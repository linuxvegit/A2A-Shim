//! v1.1 Task 14: --max-part-bytes cap (server-side per-part check).
//! ADR 0006 + spec § 2.

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

async fn start_server_with_max(max_bytes: usize) -> SocketAddr {
    let toml_str = format!(
        r#"
[server]
max_part_bytes = {max_bytes}

[agent]
command = "claude-agent-acp"
cwd = "/tmp"
"#
    );
    let cfg = Arc::new(ServeConfig::from_toml_str(&toml_str).expect("parse cfg"));
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

#[tokio::test]
async fn small_part_passes() {
    let addr = start_server_with_max(1024).await;
    let small = "A".repeat(100); // 100 bytes
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "SendMessage",
        "params": {
            "message": {
                "role": "user",
                "parts": [
                    { "text": "go" },
                    { "raw": small, "mediaType": "application/octet-stream" }
                ],
                "metadata": { "x-a2a-shim/conversation": "alice/small" }
            }
        }
    });
    let resp: Value = reqwest::Client::new()
        .post(format!("http://{addr}/"))
        .json(&body)
        .send()
        .await
        .expect("send")
        .json()
        .await
        .expect("json");
    assert_eq!(
        resp["result"]["status"]["state"], "completed",
        "got: {resp}"
    );
}

#[tokio::test]
async fn oversized_part_returns_invalid_params() {
    let addr = start_server_with_max(1024).await;
    let big = "A".repeat(2048); // 2 KiB > 1 KiB cap
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "SendMessage",
        "params": {
            "message": {
                "role": "user",
                "parts": [
                    { "text": "go" },
                    { "raw": big, "mediaType": "application/octet-stream" }
                ],
                "metadata": { "x-a2a-shim/conversation": "alice/big" }
            }
        }
    });
    let resp: Value = reqwest::Client::new()
        .post(format!("http://{addr}/"))
        .json(&body)
        .send()
        .await
        .expect("send")
        .json()
        .await
        .expect("json");
    // Expect INVALID_PARAMS (-32602) with a message mentioning the cap.
    assert_eq!(
        resp["error"]["code"], -32602,
        "expected INVALID_PARAMS, got {resp}"
    );
    let msg = resp["error"]["message"].as_str().unwrap_or("");
    assert!(
        msg.to_lowercase().contains("part") && msg.contains("bytes"),
        "expected cap message, got: {msg}"
    );
}

#[tokio::test]
async fn default_cap_is_10mib() {
    // No max_part_bytes in TOML -> default 10 MiB. A 100KB part passes.
    let cfg = Arc::new(
        ServeConfig::from_toml_str(
            r#"
[agent]
command = "claude-agent-acp"
cwd = "/tmp"
"#,
        )
        .unwrap(),
    );
    assert_eq!(cfg.server.max_part_bytes, 10 * 1024 * 1024);
}
