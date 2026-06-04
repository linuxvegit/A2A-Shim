//! v1.1 Task 39: _shim/conversation/reset.

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

async fn rpc(addr: SocketAddr, method: &str, params: Value) -> Value {
    reqwest::Client::new()
        .post(format!("http://{addr}/"))
        .json(&json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}))
        .send()
        .await
        .expect("send")
        .json()
        .await
        .expect("json")
}

#[tokio::test]
async fn reset_unknown_conv_returns_cleared_false() {
    let addr = start_server().await;
    let resp = rpc(
        addr,
        "_shim/conversation/reset",
        json!({"conversation_id": "nope"}),
    )
    .await;
    assert_eq!(resp["result"]["cleared"], false, "got: {resp}");
    let cancelled = resp["result"]["cancelled_task_ids"]
        .as_array()
        .expect("array");
    assert!(cancelled.is_empty());
}

#[tokio::test]
async fn reset_after_send_clears_conv_and_cancels_no_terminal_tasks() {
    let addr = start_server().await;
    // First SendMessage creates the conversation (and a terminal Task).
    let _ = rpc(
        addr,
        "SendMessage",
        json!({
            "message": {
                "role": "user",
                "parts": [{"text":"go"}],
                "metadata": { "x-a2a-shim/conversation": "alice/r1" }
            }
        }),
    )
    .await;
    // Reset.
    let r = rpc(
        addr,
        "_shim/conversation/reset",
        json!({"conversation_id": "alice/r1"}),
    )
    .await;
    assert_eq!(r["result"]["cleared"], true, "got: {r}");
    // Task already completed → not cancelable; cancelled list is empty.
    let cancelled = r["result"]["cancelled_task_ids"].as_array().unwrap();
    assert!(cancelled.is_empty(), "got: {r}");

    // Subsequent send under the same id should create a NEW conversation
    // (the old slot was reset). With mode=auto we just succeed.
    let r2 = rpc(
        addr,
        "SendMessage",
        json!({
            "message": {
                "role": "user",
                "parts": [{"text":"go"}],
                "metadata": { "x-a2a-shim/conversation": "alice/r1" }
            }
        }),
    )
    .await;
    assert_eq!(r2["result"]["status"]["state"], "completed");
}
