//! Integration test: POST a message/stream request and consume the
//! SSE response. Validates Task 26 — that the bridge frames pump out
//! as `data: {...}\n\n` records and the channel closes after the
//! terminal status-update.

use a2a_shim_core::config::serve_toml::ServeConfig;
use a2a_shim_core::wire::sse::{parse_sse_data_line, SseEvent};
use a2a_shim_core::wire::task::TaskState;
use a2a_shim_serve::acp_client::{AcpClient, AcpClientConfig};
use a2a_shim_serve::http;
use serde_json::json;
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
command = "claude-agent-acp"
cwd = "/tmp"
"#,
        )
        .unwrap(),
    );
    let state = http::ServeState::with_client(cfg, fresh_client().await);
    let app = http::router(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    addr
}

#[tokio::test]
async fn message_stream_emits_working_artifact_completed() {
    let addr = start_server().await;
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "message/stream",
        "params": {
            "message": {
                "role": "user",
                "parts": [{ "type": "text", "text": "2+2" }],
                "metadata": { "x-a2a-shim/conversation": "alice/stream" }
            }
        }
    });
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/"))
        .json(&body)
        .send()
        .await
        .expect("post ok");
    assert_eq!(resp.status(), 200);
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        ct.starts_with("text/event-stream"),
        "expected text/event-stream, got '{ct}'"
    );

    // Buffer the whole body — easy because the stream terminates fast.
    let raw = timeout(Duration::from_secs(5), resp.bytes())
        .await
        .expect("body recv timeout")
        .expect("bytes ok");
    let text = String::from_utf8_lossy(&raw).to_string();

    // Parse out each `data: ...` line and JSON-decode.
    let events: Vec<SseEvent> = text
        .lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .filter_map(|payload| parse_sse_data_line(payload).ok())
        .collect();
    assert!(
        events.len() >= 3,
        "expected at least 3 events; got {}: {text}",
        events.len()
    );

    // First: status Working
    assert!(matches!(
        &events[0],
        SseEvent::StatusUpdate { status, final_, .. }
            if status.state == TaskState::Working && !final_
    ));
    // Somewhere: artifact-update carrying "4"
    let mut saw_answer = false;
    let mut saw_final = false;
    for e in &events {
        match e {
            SseEvent::ArtifactUpdate { artifact, .. } => {
                if let Some(a2a_shim_core::wire::message::Part::Text { text }) =
                    artifact.parts.first()
                {
                    if text == "4" {
                        saw_answer = true;
                    }
                }
            }
            SseEvent::StatusUpdate { status, final_, .. }
                if status.state == TaskState::Completed && *final_ =>
            {
                saw_final = true;
            }
            _ => {}
        }
    }
    assert!(saw_answer, "no artifact-update with text '4' in: {text}");
    assert!(saw_final, "no terminal completed status in: {text}");
}

#[tokio::test]
async fn message_stream_without_conversation_metadata_returns_400_or_error_frame() {
    // The dispatcher should reject malformed params before opening the
    // stream. The exact transport (JSON-RPC error body vs HTTP 400 vs
    // immediate failed SSE) is implementation-defined; we just assert
    // we don't get a 200 OK with a Working frame.
    let addr = start_server().await;
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "message/stream",
        "params": {
            "message": {
                "role": "user",
                "parts": [{ "type": "text", "text": "nope" }]
            }
        }
    });
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/"))
        .json(&body)
        .send()
        .await
        .expect("post ok");
    // Either the dispatcher caught it (non-2xx or JSON error body), OR
    // it opened the stream and emitted a Failed-only sequence — but
    // there must not be a Working frame because no Task was created.
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    let ok_with_working = status == 200
        && text.contains("\"state\":\"working\"")
        && !text.contains("\"state\":\"failed\"");
    assert!(
        !ok_with_working,
        "stream emitted Working without conversation metadata: status={status} body={text}"
    );
}
