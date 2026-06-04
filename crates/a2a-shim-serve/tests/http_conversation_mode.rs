//! v1.1 Task 29: conversation_mode semantics + ConversationLost/Exists wire.

use a2a_shim_core::config::serve_toml::ServeConfig;
use a2a_shim_serve::acp_client::{AcpClient, AcpClientConfig};
use a2a_shim_serve::http;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;

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
    assert!(bin.exists());
    bin
}

async fn fresh_client() -> AcpClient {
    let cfg = AcpClientConfig {
        command: mock_bin().to_string_lossy().into_owned(),
        args: vec!["--script".into(), "happy".into()],
        cwd: std::env::temp_dir(),
        env: HashMap::new(),
    };
    let c = AcpClient::spawn(cfg).await.expect("spawn");
    c.initialize().await.expect("init");
    c
}

async fn start_server() -> SocketAddr {
    let cfg = Arc::new(
        ServeConfig::from_toml_str(
            r#"
[agent]
command = "x"
cwd = "/x"
"#,
        )
        .unwrap(),
    );
    let state = http::ServeState::with_client(cfg, fresh_client().await);
    let app = http::router(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    addr
}

fn send_body(conv: &str, mode: Option<&str>) -> Value {
    let mut params = serde_json::Map::new();
    if let Some(m) = mode {
        params.insert(
            "_shim_conversation_mode".to_string(),
            Value::String(m.to_string()),
        );
    }
    params.insert(
        "message".to_string(),
        json!({
            "role": "user",
            "parts": [{ "text": "go" }],
            "metadata": { "x-a2a-shim/conversation": conv }
        }),
    );
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "SendMessage",
        "params": Value::Object(params)
    })
}

async fn rpc(addr: SocketAddr, body: Value) -> Value {
    reqwest::Client::new()
        .post(format!("http://{addr}/"))
        .json(&body)
        .send()
        .await
        .expect("send")
        .json()
        .await
        .expect("json")
}

#[tokio::test]
async fn mode_continue_on_missing_returns_conversation_lost() {
    let addr = start_server().await;
    let resp = rpc(addr, send_body("never/exists", Some("continue"))).await;
    assert_eq!(resp["error"]["code"], -32013, "got: {resp}");
    let msg = resp["error"]["message"].as_str().unwrap_or("");
    assert!(msg.contains("continue"), "got: {msg}");
}

#[tokio::test]
async fn mode_new_on_existing_returns_conversation_exists() {
    let addr = start_server().await;
    // Create the conversation first with mode=auto.
    let _ = rpc(addr, send_body("alice/dup", Some("auto"))).await;
    // Now try mode=new on the same id.
    let resp = rpc(addr, send_body("alice/dup", Some("new"))).await;
    assert_eq!(resp["error"]["code"], -32012, "got: {resp}");
}

#[tokio::test]
async fn mode_auto_default_creates_and_reuses() {
    let addr = start_server().await;
    // First call creates.
    let r1 = rpc(addr, send_body("alice/auto", None)).await;
    assert_eq!(r1["result"]["status"]["state"], "completed");
    // Second call reuses.
    let r2 = rpc(addr, send_body("alice/auto", Some("auto"))).await;
    assert_eq!(r2["result"]["status"]["state"], "completed");
}

#[tokio::test]
async fn mode_new_on_fresh_id_succeeds() {
    let addr = start_server().await;
    let resp = rpc(addr, send_body("alice/brand-new", Some("new"))).await;
    assert_eq!(
        resp["result"]["status"]["state"], "completed",
        "got: {resp}"
    );
}

#[tokio::test]
async fn unknown_mode_returns_invalid_params() {
    let addr = start_server().await;
    let resp = rpc(addr, send_body("c", Some("bogus"))).await;
    assert_eq!(resp["error"]["code"], -32602, "got: {resp}");
    let msg = resp["error"]["message"].as_str().unwrap_or("");
    assert!(msg.contains("bogus"));
}
