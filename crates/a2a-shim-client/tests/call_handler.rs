//! Integration test for the a2a_send tools/call handler.
//!
//! Spins up a tiny axum SSE server (same pattern as outbound.rs tests)
//! plus the MCP loop over duplex pipes, sends a tools/call, and asserts
//! the rendered MCP tool result.

use a2a_shim_client::mcp_server::{serve_loop, ClientRuntime, ServerState};
use a2a_shim_client::outbound::OutboundDeadlines;
use axum::{
    response::sse::{Event, KeepAlive, Sse},
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

/// Spin up a fake A2A endpoint returning a 3-event happy stream.
async fn happy_endpoint() -> SocketAddr {
    let app = Router::new().route(
        "/",
        post(|Json(_): Json<Value>| async {
            let events = vec![
                json!({
                    "kind": "status-update",
                    "taskId": "t-fake",
                    "status": { "state": "working" },
                    "final": false
                }),
                json!({
                    "kind": "artifact-update",
                    "taskId": "t-fake",
                    "artifact": {
                        "artifactId": "a-answer",
                        "parts": [{ "type": "text", "text": "Hello!" }]
                    },
                    "append": false
                }),
                json!({
                    "kind": "status-update",
                    "taskId": "t-fake",
                    "status": { "state": "completed" },
                    "final": true
                }),
            ];
            let stream = async_stream::stream! {
                for e in events {
                    yield Ok::<_, Infallible>(Event::default().data(serde_json::to_string(&e).unwrap()));
                }
            };
            Sse::new(stream).keep_alive(KeepAlive::default())
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    addr
}

/// Spin up a fake that returns HTTP 500 immediately.
async fn failing_endpoint() -> SocketAddr {
    let app = Router::new().route(
        "/",
        post(|Json(_): Json<Value>| async { axum::http::StatusCode::INTERNAL_SERVER_ERROR }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    addr
}

fn runtime() -> ClientRuntime {
    ClientRuntime {
        deadlines: OutboundDeadlines {
            connect: Duration::from_secs(5),
            stream_idle: Duration::from_secs(10),
            hard_ceiling: Duration::from_secs(60),
        },
        heartbeat_interval: Duration::from_secs(30),
    }
}

async fn drive_loop(lines: Vec<Value>, state: ServerState) -> Vec<Value> {
    let (mut client_in, server_in) = tokio::io::duplex(64 * 1024);
    let (server_out, client_out) = tokio::io::duplex(256 * 1024);
    let server = tokio::spawn(async move {
        let _ = serve_loop(server_in, server_out, state).await;
    });
    for v in lines {
        let mut s = serde_json::to_string(&v).unwrap();
        s.push('\n');
        client_in.write_all(s.as_bytes()).await.unwrap();
    }
    drop(client_in);
    let _ = server.await;
    let mut reader = BufReader::new(client_out);
    let mut out = Vec::new();
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => out.push(serde_json::from_str(line.trim()).expect("json line")),
            Err(_) => break,
        }
    }
    out
}

#[tokio::test]
async fn tools_call_happy_returns_text_and_a2a_task_meta() {
    let addr = happy_endpoint().await;
    let state = ServerState::new(Arc::new(runtime()));
    let init = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} });
    let call = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "a2a_send",
            "arguments": {
                "endpoint": format!("http://{addr}"),
                "conversation_id": "alice/x",
                "message": "hi"
            }
        }
    });
    let out = drive_loop(vec![init, call], state).await;
    // Expect [init resp, tools/call resp]; progress notifications might
    // appear between them if heartbeat_interval is short enough, but our
    // runtime sets it to 30s so none should fire here.
    let call_resp = out
        .iter()
        .find(|v| v["id"] == 2)
        .expect("tools/call response not found");
    let content = &call_resp["result"]["content"];
    assert!(content.is_array(), "got: {call_resp}");
    let text = content[0]["text"].as_str().unwrap_or("");
    assert!(
        text.contains("Hello!"),
        "expected answer text, got '{text}'"
    );
    let task = &call_resp["result"]["_meta"]["a2aTask"];
    assert_eq!(task["status"]["state"], "completed");
    assert_eq!(call_resp["result"]["isError"], false);
}

#[tokio::test]
async fn tools_call_remote_500_yields_iserror_with_normalized_error() {
    let addr = failing_endpoint().await;
    let state = ServerState::new(Arc::new(runtime()));
    let init = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} });
    let call = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "a2a_send",
            "arguments": {
                "endpoint": format!("http://{addr}"),
                "conversation_id": "c/err",
                "message": "x"
            }
        }
    });
    let out = drive_loop(vec![init, call], state).await;
    let resp = out
        .iter()
        .find(|v| v["id"] == 2)
        .expect("tools/call response");
    assert_eq!(resp["result"]["isError"], true, "got: {resp}");
    let kind = resp["result"]["_meta"]["error"]["kind"]
        .as_str()
        .unwrap_or("");
    assert_eq!(kind, "remote_failed");
}

#[tokio::test]
async fn tools_call_invalid_args_returns_invalid_params() {
    let state = ServerState::new(Arc::new(runtime()));
    let init = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} });
    let bad_call = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "a2a_send",
            "arguments": {
                // missing endpoint, conversation_id, message
                "irrelevant": true
            }
        }
    });
    let out = drive_loop(vec![init, bad_call], state).await;
    let resp = out.iter().find(|v| v["id"] == 2).expect("response");
    assert_eq!(
        resp["error"]["code"], -32602,
        "expected INVALID_PARAMS, got {resp}"
    );
}

#[tokio::test]
async fn tools_call_unknown_tool_returns_method_not_found_style() {
    let state = ServerState::new(Arc::new(runtime()));
    let init = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} });
    let call = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "nonexistent_tool",
            "arguments": {}
        }
    });
    let out = drive_loop(vec![init, call], state).await;
    let resp = out.iter().find(|v| v["id"] == 2).expect("response");
    // MCP wraps unknown-tool errors as isError=true tool results, not
    // JSON-RPC errors. Either shape is accepted but the failure must be
    // visible.
    let is_error_result = resp["result"]["isError"].as_bool().unwrap_or(false);
    let has_error = resp["error"].is_object();
    assert!(
        is_error_result || has_error,
        "expected error indication, got: {resp}"
    );
}
