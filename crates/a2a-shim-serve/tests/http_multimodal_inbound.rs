//! v1.1 Task 13: inbound multi-modal Parts on SendMessage reach the agent
//! as the right ContentBlock variants.
//!
//! Drives the mock 'echo' script which emits back a text chunk
//! summarizing what ContentBlock kinds it received. The test then
//! asserts the agent saw the kinds the test sent.

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
    assert!(bin.exists(), "mock_acp_agent missing");
    bin
}

async fn fresh_client_echo() -> AcpClient {
    let cfg = AcpClientConfig {
        command: mock_bin().to_string_lossy().into_owned(),
        args: vec!["--script".into(), "echo".into()],
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
command = "claude-agent-acp"
cwd = "/tmp"
"#,
        )
        .unwrap(),
    );
    let state = http::ServeState::with_client(cfg, fresh_client_echo().await);
    let app = http::router(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    addr
}

async fn send(addr: SocketAddr, body: Value) -> Value {
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

fn agent_summary(resp: &Value) -> String {
    resp["result"]["artifacts"][0]["parts"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

#[tokio::test]
async fn text_only_part_reaches_agent_as_text() {
    let addr = start_server().await;
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "SendMessage",
        "params": {
            "message": {
                "role": "user",
                "parts": [ { "text": "hi" } ],
                "metadata": { "x-a2a-shim/conversation": "alice/echo-text" }
            }
        }
    });
    let resp = send(addr, body).await;
    let s = agent_summary(&resp);
    assert_eq!(s, "received: text", "got: {s:?} (full resp: {resp})");
}

#[tokio::test]
async fn image_part_reaches_agent_as_image_block() {
    let addr = start_server().await;
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "SendMessage",
        "params": {
            "message": {
                "role": "user",
                "parts": [
                    { "text": "look:" },
                    { "raw": "AAAA", "mediaType": "image/png" }
                ],
                "metadata": { "x-a2a-shim/conversation": "alice/echo-image" }
            }
        }
    });
    let resp = send(addr, body).await;
    let s = agent_summary(&resp);
    // PartCaps default is all-OFF, so image SHOULD be dropped → "text" only.
    // After Task 12+13 wires the cap cache properly (Task 12 ships
    // outbound only — inbound capability source is still default-off in
    // v1.1.0; v1.2 wires the cache from initialize). Document the
    // current expectation:
    //
    // Today: image is gated because PartCaps::default() => image=false.
    // Therefore the agent sees only the text block.
    assert_eq!(
        s, "received: text",
        "image part should be cap-dropped (PartCaps default=none); got {s:?}"
    );
}

#[tokio::test]
async fn data_part_drops_when_embedded_context_cap_off() {
    let addr = start_server().await;
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "SendMessage",
        "params": {
            "message": {
                "role": "user",
                "parts": [
                    { "text": "look:" },
                    { "data": {"k":1}, "mediaType": "application/json" }
                ],
                "metadata": { "x-a2a-shim/conversation": "alice/echo-data" }
            }
        }
    });
    let resp = send(addr, body).await;
    let s = agent_summary(&resp);
    assert_eq!(s, "received: text", "data part should be cap-dropped");
}

#[tokio::test]
async fn resource_link_passes_without_cap_gate() {
    // ResourceLink (url-only File part) is NOT gated by embedded_context
    // per ADR 0006. So even with all-caps-off it should reach the agent.
    let addr = start_server().await;
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "SendMessage",
        "params": {
            "message": {
                "role": "user",
                "parts": [
                    { "text": "see:" },
                    { "url": "https://example.com/x.pdf", "mediaType": "application/pdf" }
                ],
                "metadata": { "x-a2a-shim/conversation": "alice/echo-link" }
            }
        }
    });
    let resp = send(addr, body).await;
    let s = agent_summary(&resp);
    assert_eq!(s, "received: text resource_link", "got: {s:?}");
}
