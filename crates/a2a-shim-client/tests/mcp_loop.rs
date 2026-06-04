//! In-memory tests for the MCP stdio dispatcher. Drives `serve_loop` over
//! `tokio::io::duplex` pipes so we don't pay subprocess overhead.

use a2a_shim_client::mcp_server::{serve_loop, ServerState};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

async fn run_with_lines(lines: Vec<Value>) -> Vec<Value> {
    // 64 KiB ought to be plenty for unit-test request/response sizes.
    let (mut client_in, server_in) = tokio::io::duplex(64 * 1024);
    let (server_out, client_out) = tokio::io::duplex(64 * 1024);

    // Drive the loop in a background task.
    let state = ServerState::default();
    let server = tokio::spawn(async move {
        let _ = serve_loop(server_in, server_out, state).await;
    });

    // Write each request line + newline.
    for v in lines {
        let mut s = serde_json::to_string(&v).unwrap();
        s.push('\n');
        client_in.write_all(s.as_bytes()).await.unwrap();
    }
    // Close stdin → loop returns Ok(()) and the writer task drops.
    drop(client_in);
    // Wait for the loop to finish so all output is flushed.
    let _ = server.await;

    // Collect newline-delimited JSON from server_out.
    let mut reader = BufReader::new(client_out);
    let mut out = Vec::new();
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => {
                let v: Value =
                    serde_json::from_str(line.trim()).expect("server emitted non-JSON line");
                out.push(v);
            }
            Err(_) => break,
        }
    }
    out
}

#[tokio::test]
async fn initialize_returns_protocol_version_and_tools_capability() {
    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "test-host", "version": "0.0.0" }
        }
    });
    let out = run_with_lines(vec![req]).await;
    assert_eq!(out.len(), 1, "expected exactly one response: {out:?}");
    let r = &out[0];
    assert_eq!(r["id"], 1);
    assert_eq!(r["jsonrpc"], "2.0");
    assert!(
        r["result"]["capabilities"]["tools"].is_object(),
        "expected tools capability object, got {r}"
    );
    assert_eq!(
        r["result"]["capabilities"]["tools"]["listChanged"], false,
        "spec: a2a-shim's tool list is static"
    );
    assert!(
        r["result"]["serverInfo"]["name"]
            .as_str()
            .map(|s| s.contains("a2a-shim"))
            .unwrap_or(false),
        "expected serverInfo.name to mention a2a-shim, got {r}"
    );
}

#[tokio::test]
async fn tools_list_returns_a2a_send() {
    let init = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {}
    });
    let list = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {}
    });
    let out = run_with_lines(vec![init, list]).await;
    assert_eq!(out.len(), 2, "got: {out:?}");
    let tools = out[1]["result"]["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], "a2a_send");
}

#[tokio::test]
async fn unknown_method_returns_method_not_found() {
    let req = json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": "frobnicate",
        "params": {}
    });
    let out = run_with_lines(vec![req]).await;
    assert_eq!(out.len(), 1);
    assert_eq!(out[0]["error"]["code"], -32601, "got: {:?}", out[0]);
    assert_eq!(out[0]["id"], 7);
}

#[tokio::test]
async fn notifications_carry_no_id_and_emit_no_response() {
    // Per JSON-RPC 2.0, requests without an `id` field are notifications;
    // the server must NOT respond. Send one + a regular request and assert
    // exactly one response comes back.
    let notif = json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": { "requestId": 999 }
    });
    let ping = json!({
        "jsonrpc": "2.0",
        "id": 42,
        "method": "tools/list",
        "params": {}
    });
    let out = run_with_lines(vec![notif, ping]).await;
    assert_eq!(
        out.len(),
        1,
        "notification should produce no response: {out:?}"
    );
    assert_eq!(out[0]["id"], 42);
}

#[tokio::test]
async fn malformed_json_returns_parse_error_with_null_id() {
    let (mut client_in, server_in) = tokio::io::duplex(4096);
    let (server_out, mut client_out) = tokio::io::duplex(4096);
    let server = tokio::spawn(async move {
        let _ = serve_loop(server_in, server_out, ServerState::default()).await;
    });

    client_in
        .write_all(b"{this is not valid json\n")
        .await
        .unwrap();
    drop(client_in);
    let _ = server.await;

    let mut buf = String::new();
    use tokio::io::AsyncReadExt;
    let _ = client_out.read_to_string(&mut buf).await;
    let first_line = buf.lines().next().expect("got at least one response line");
    let v: Value = serde_json::from_str(first_line).expect("response is JSON");
    assert_eq!(v["error"]["code"], -32700, "got: {v}");
    assert!(
        v["id"].is_null(),
        "id should be null for parse errors, got: {v}"
    );
}
