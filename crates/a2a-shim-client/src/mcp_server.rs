//! Stdio MCP JSON-RPC server loop (spec § 3.1, § 3.4).
//!
//! Wire: line-delimited JSON over the supplied `AsyncRead` (stdin in
//! production) and `AsyncWrite` (stdout). The whole reason we own a
//! single writer task is to guarantee no log line ever lands on stdout
//! and no two emitters can interleave partial frames — the heartbeat
//! (Task 31) and the eventual tools/call result (Task 32) both go
//! through `writer_tx`.

use a2a_shim_core::error::codes;
use a2a_shim_core::wire::envelope::{JsonRpcError, JsonRpcResponse, ResultOrError};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use crate::call_handler::{call_a2a_send, CallContext};
use crate::cancellation::CancellationRegistry;
use crate::outbound::OutboundDeadlines;
use crate::tool_schema::tool_definition;

pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

/// Runtime configuration for outbound calls. The MCP loop does not look
/// at these directly; it threads them into each CallContext.
#[derive(Debug, Clone)]
pub struct ClientRuntime {
    pub deadlines: OutboundDeadlines,
    pub heartbeat_interval: Duration,
}

impl Default for ClientRuntime {
    fn default() -> Self {
        Self {
            deadlines: OutboundDeadlines {
                connect: Duration::from_secs(120),
                stream_idle: Duration::from_secs(600),
                hard_ceiling: Duration::from_secs(86400),
            },
            heartbeat_interval: Duration::from_secs(30),
        }
    }
}

#[derive(Clone)]
pub struct ServerState {
    pub runtime: Arc<ClientRuntime>,
    pub registry: Arc<CancellationRegistry>,
}

impl ServerState {
    pub fn new(runtime: Arc<ClientRuntime>) -> Self {
        Self {
            runtime,
            registry: Arc::new(CancellationRegistry::new()),
        }
    }
}

impl Default for ServerState {
    fn default() -> Self {
        Self::new(Arc::new(ClientRuntime::default()))
    }
}

/// Run the stdio MCP loop until EOF on the reader.
pub async fn serve_loop<R, W>(
    reader: R,
    mut writer: W,
    state: ServerState,
) -> std::io::Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (tx, mut rx) = mpsc::unbounded_channel::<Value>();

    // Single writer task. Drops rx → drains in-flight frames → returns.
    let writer_task = tokio::spawn(async move {
        while let Some(v) = rx.recv().await {
            let mut s = match serde_json::to_string(&v) {
                Ok(s) => s,
                Err(_) => continue,
            };
            s.push('\n');
            if writer.write_all(s.as_bytes()).await.is_err() {
                break;
            }
            if writer.flush().await.is_err() {
                break;
            }
        }
    });

    let mut lines = BufReader::new(reader).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let parsed: Result<Value, _> = serde_json::from_str(&line);
        match parsed {
            Ok(v) => {
                let method = v
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                // Notifications have no `id`; act on them and produce no
                // response.
                if v.get("id").is_none() {
                    handle_notification(&method, &v, &state);
                    continue;
                }
                // Special-case tools/call so we can spawn a handler that
                // streams progress through writer_tx independently.
                if method == "tools/call" {
                    spawn_tools_call(v, state.clone(), tx.clone());
                    continue;
                }
                // Everything else is a synchronous in-loop dispatch.
                let resp = dispatch_sync(v);
                let _ = tx.send(serde_json::to_value(&resp).expect("response serializes"));
            }
            Err(e) => {
                let resp = JsonRpcResponse::<Value> {
                    jsonrpc: "2.0".into(),
                    id: Value::Null,
                    result_or_error: ResultOrError::from_error(JsonRpcError {
                        code: codes::PARSE_ERROR,
                        message: format!("parse error: {e}"),
                        data: None,
                    }),
                };
                let _ = tx.send(serde_json::to_value(&resp).expect("response serializes"));
            }
        }
    }

    drop(tx);
    let _ = writer_task.await;
    Ok(())
}

fn handle_notification(method: &str, msg: &Value, state: &ServerState) {
    match method {
        "notifications/cancelled" => {
            if let Some(id) = msg.get("params").and_then(|p| p.get("requestId")) {
                tracing::debug!(req_id = ?id, "cancelling in-flight a2a_send");
                state.registry.cancel(id);
            }
        }
        _ => tracing::debug!(method, "unhandled notification (dropped)"),
    }
}

fn dispatch_sync(req: Value) -> JsonRpcResponse<Value> {
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let method = req
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    let result: Result<Value, JsonRpcError> = match method.as_str() {
        "initialize" => Ok(initialize_result()),
        "tools/list" => Ok(tools_list_result()),
        other => Err(JsonRpcError {
            code: codes::METHOD_NOT_FOUND,
            message: format!("method not found: {other}"),
            data: None,
        }),
    };

    match result {
        Ok(v) => JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id,
            result_or_error: ResultOrError::from_result(v),
        },
        Err(e) => JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id,
            result_or_error: ResultOrError::from_error(e),
        },
    }
}

fn spawn_tools_call(req: Value, state: ServerState, writer_tx: mpsc::UnboundedSender<Value>) {
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let params = req.get("params").cloned().unwrap_or(Value::Null);
    let tool_name = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
    let progress_token = req
        .get("params")
        .and_then(|p| p.get("_meta"))
        .and_then(|m| m.get("progressToken"))
        .cloned();

    // Unknown tool name -> isError tool result, NOT JSON-RPC error.
    if tool_name != "a2a_send" {
        let resp = JsonRpcResponse::<Value> {
            jsonrpc: "2.0".into(),
            id,
            result_or_error: ResultOrError::from_result(json!({
                "isError": true,
                "content": [{
                    "type": "text",
                    "text": format!("unknown tool: {tool_name}")
                }]
            })),
        };
        let _ = writer_tx.send(serde_json::to_value(&resp).expect("response serializes"));
        return;
    }

    tokio::spawn(async move {
        let cx = CallContext {
            request_id: id.clone(),
            progress_token,
            writer_tx: writer_tx.clone(),
            registry: state.registry.clone(),
            default_deadlines: state.runtime.deadlines,
            heartbeat_interval: state.runtime.heartbeat_interval,
        };
        let result = call_a2a_send(arguments, cx).await;

        // The call handler signals INVALID_PARAMS via a sentinel object
        // because the JSON-RPC error envelope is the MCP loop's job.
        let resp: JsonRpcResponse<Value> = if result
            .get("__a2a_shim_invalid_params")
            .and_then(Value::as_bool)
            == Some(true)
        {
            let msg = result
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("invalid params")
                .to_string();
            JsonRpcResponse {
                jsonrpc: "2.0".into(),
                id,
                result_or_error: ResultOrError::from_error(JsonRpcError {
                    code: codes::INVALID_PARAMS,
                    message: msg,
                    data: None,
                }),
            }
        } else {
            JsonRpcResponse {
                jsonrpc: "2.0".into(),
                id,
                result_or_error: ResultOrError::from_result(result),
            }
        };
        let _ = writer_tx.send(serde_json::to_value(&resp).expect("response serializes"));
    });
}

fn initialize_result() -> Value {
    json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "capabilities": {
            "tools": { "listChanged": false }
        },
        "serverInfo": {
            "name": "a2a-shim",
            "version": env!("CARGO_PKG_VERSION")
        }
    })
}

fn tools_list_result() -> Value {
    json!({ "tools": [tool_definition()] })
}
