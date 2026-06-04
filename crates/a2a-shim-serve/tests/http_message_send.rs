//! Integration test: end-to-end message/send + tasks/get + tasks/cancel
//! through the JSON-RPC root, driving the real AcpClient + mock_acp_agent
//! pipeline. Also covers all the error-mapping branches in spec § 4.6.

use a2a_shim_core::config::serve_toml::ServeConfig;
use a2a_shim_core::error::codes;
use a2a_shim_serve::acp_client::{AcpClient, AcpClientConfig};
use a2a_shim_serve::http;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::time::timeout;

fn mock_bin() -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("current_exe");
    let target_dir = exe
        .parent()
        .and_then(|p| p.parent())
        .expect("two parents up");
    let mut bin = target_dir.join("mock_acp_agent");
    if cfg!(windows) {
        bin.set_extension("exe");
    }
    assert!(bin.exists(), "mock_acp_agent missing at {}", bin.display());
    bin
}

fn cfg(max_active: u32) -> Arc<ServeConfig> {
    let toml = format!(
        r#"
[server.conversations]
max_active = {max_active}
idle_secs = 3600
[agent]
command = "claude-agent-acp"
cwd = "/tmp"
"#
    );
    Arc::new(ServeConfig::from_toml_str(&toml).unwrap())
}

/// Spawn one shared AcpClient against mock_acp_agent so the test does not
/// pay subprocess startup time per call.
async fn fresh_client() -> AcpClient {
    let cfg = AcpClientConfig {
        command: mock_bin().to_string_lossy().into_owned(),
        args: vec!["--script".into(), "happy".into()],
        cwd: std::env::temp_dir(),
        env: HashMap::new(),
    };
    let c = AcpClient::spawn(cfg).await.expect("spawn AcpClient");
    c.initialize().await.expect("initialize");
    c
}

async fn start_server(cfg: Arc<ServeConfig>, client: AcpClient) -> SocketAddr {
    let state = http::ServeState::with_client(cfg, client);
    let app = http::router(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    addr
}

async fn rpc(addr: SocketAddr, method: &str, params: Value) -> Value {
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params
    });
    let url = format!("http://{addr}/");
    timeout(
        Duration::from_secs(10),
        reqwest::Client::new().post(&url).json(&body).send(),
    )
    .await
    .expect("rpc timeout")
    .expect("reqwest ok")
    .json::<Value>()
    .await
    .expect("parse json")
}

fn send_params(conv_id: &str, text: &str) -> Value {
    json!({
        "message": {
            "role": "user",
            "parts": [{ "type": "text", "text": text }],
            "metadata": { "x-a2a-shim/conversation": conv_id }
        }
    })
}

#[tokio::test]
async fn message_send_happy_returns_completed_task() {
    let addr = start_server(cfg(8), fresh_client().await).await;
    let resp = rpc(
        addr,
        "SendMessage",
        send_params("alice/test", "what is 2+2?"),
    )
    .await;
    let task = &resp["result"];
    assert_eq!(task["status"]["state"], "completed", "got: {resp}");
    let parts = &task["artifacts"][0]["parts"];
    let text = parts[0]["text"].as_str().unwrap();
    assert_eq!(text, "4");
}

#[tokio::test]
async fn message_send_without_conversation_metadata_is_invalid_params() {
    let addr = start_server(cfg(8), fresh_client().await).await;
    let bad = json!({
        "message": {
            "role": "user",
            "parts": [{ "type": "text", "text": "no conv" }]
            // metadata field omitted entirely
        }
    });
    let resp = rpc(addr, "SendMessage", bad).await;
    assert_eq!(resp["error"]["code"], codes::INVALID_PARAMS, "got: {resp}");
}

#[tokio::test]
async fn tasks_get_unknown_id_returns_task_not_found() {
    let addr = start_server(cfg(8), fresh_client().await).await;
    let resp = rpc(addr, "GetTask", json!({ "id": "t-does-not-exist" })).await;
    assert_eq!(resp["error"]["code"], codes::TASK_NOT_FOUND);
}

#[tokio::test]
async fn tasks_cancel_after_completed_is_not_cancelable() {
    let addr = start_server(cfg(8), fresh_client().await).await;
    let send = rpc(addr, "SendMessage", send_params("c/cancel", "2+2")).await;
    let task_id = send["result"]["id"].as_str().unwrap().to_string();

    let resp = rpc(addr, "CancelTask", json!({ "id": task_id })).await;
    assert_eq!(
        resp["error"]["code"],
        codes::TASK_NOT_CANCELABLE,
        "got: {resp}"
    );
}

#[tokio::test]
async fn unknown_method_is_method_not_found() {
    let addr = start_server(cfg(8), fresh_client().await).await;
    let resp = rpc(addr, "Frobnicate", json!({})).await;
    assert_eq!(resp["error"]["code"], codes::METHOD_NOT_FOUND);
}

#[tokio::test]
async fn message_send_over_max_active_returns_conversation_limit_reached() {
    // max_active = 1; the first send creates conversation "a"; the second
    // send under a different conversation id "b" must trip the limit.
    let addr = start_server(cfg(1), fresh_client().await).await;
    rpc(addr, "SendMessage", send_params("a/x", "go")).await; // creates "a"
    let resp = rpc(addr, "SendMessage", send_params("b/x", "go")).await;
    assert_eq!(
        resp["error"]["code"],
        codes::CONVERSATION_LIMIT_REACHED,
        "got: {resp}"
    );
}
