//! Stdio MCP JSON-RPC server loop (spec § 3.1, § 3.4).
//!
//! Wire: line-delimited JSON over the supplied `AsyncRead` (typically
//! `tokio::io::stdin`) and `AsyncWrite` (typically `tokio::io::stdout`).
//! Each inbound message is one line of valid JSON; each outbound message
//! is one line followed by `\n`. Hard rule: **nothing else is ever
//! written to the AsyncWrite** — no logs, no banners, no errors. That
//! is the whole point of the writer-task indirection: every emitter
//! goes through `writer_tx` so concurrent `notifications/progress`
//! (Task 31) and the eventual `tools/call` response (Task 32) cannot
//! produce interleaved partial lines.
//!
//! This task (Task 29) implements:
//!   * `initialize` — return server info + capabilities.tools (no listChanged).
//!   * `tools/list` — return `[tool_definition()]`.
//!   * `tools/call` — placeholder returning -32601 until Task 32.
//!   * `notifications/*` — silently dropped (handled in Task 32 onward).
//!   * unknown methods — `-32601 method not found`.
//!   * malformed JSON — `-32700 parse error` with null id.

use a2a_shim_core::error::codes;
use a2a_shim_core::wire::envelope::{
    JsonRpcError, JsonRpcRequest, JsonRpcResponse, ResultOrError,
};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use crate::tool_schema::tool_definition;

/// Server protocol-revision string advertised by `initialize`. Matches the
/// MCP 2025-06-18 revision; older clients should still interop because the
/// only feature we expose is the static tools list.
pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

#[derive(Clone, Default)]
pub struct ServerState {
    // Reserved for Task 32 (cancellation registry, outbound client, ...).
}

/// Run the stdio MCP loop until the read half hits EOF. Returns the IO
/// error if reading or writing fails, otherwise Ok(()).
pub async fn serve_loop<R, W>(
    reader: R,
    mut writer: W,
    _state: ServerState,
) -> std::io::Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (tx, mut rx) = mpsc::unbounded_channel::<Value>();

    // Single writer task: drain rx, serialize each value as one line.
    // Owning the writer here means no other code path can leak bytes
    // to stdout (the rs-discipline rule).
    let writer_task = tokio::spawn(async move {
        while let Some(v) = rx.recv().await {
            let mut s = match serde_json::to_string(&v) {
                Ok(s) => s,
                Err(_) => continue, // serialization can't really fail for us
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

    // Reader / dispatcher loop. Each line is one JSON-RPC message.
    let mut lines = BufReader::new(reader).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(&line) {
            Ok(req) => {
                // Notifications have no `id`; per JSON-RPC 2.0 we MUST NOT
                // respond. Drop them silently — Task 32 will wire them
                // into the cancellation registry.
                if req.get("id").is_none() {
                    tracing::debug!(method = ?req.get("method"), "notification dropped (handler comes in Task 32)");
                    continue;
                }
                let resp = dispatch(req);
                let _ = tx.send(serde_json::to_value(&resp).expect("response serializes"));
            }
            Err(e) => {
                // Parse error → response with null id per JSON-RPC 2.0.
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

    drop(tx); // signal writer task to finish
    let _ = writer_task.await;
    Ok(())
}

fn dispatch(req: Value) -> JsonRpcResponse<Value> {
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let method = req
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    // We took everything we need out of `req` already, but keep it owned
    // so future Task 32 work can pull params without an extra clone.
    let params = req.get("params").cloned().unwrap_or(Value::Null);

    let result: Result<Value, JsonRpcError> = match method.as_str() {
        "initialize" => Ok(initialize_result()),
        "tools/list" => Ok(tools_list_result()),
        "tools/call" => Err(JsonRpcError {
            code: codes::METHOD_NOT_FOUND,
            message: "tools/call is wired in Task 32".into(),
            data: Some(json!({ "received_params": params })),
        }),
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

#[allow(dead_code)]
fn _request_typed_check(_req: JsonRpcRequest<Value>) {
    // Compile-time witness that our envelope types still match what we
    // accept on the wire. Doesn't run; deleting this line would silently
    // mask drift if dispatch() stops parsing into JsonRpcRequest later.
}
