//! Integration test: drive `AcpClient` against the `mock_acp_agent` binary.
//!
//! Validates the full happy path: spawn → initialize → session_new →
//! session_prompt streams an agent_message_chunk('4') then terminates with
//! StopReason::EndTurn. Also implicitly validates the mock binary itself,
//! since this is the first thing that talks to it end-to-end.

use a2a_shim_serve::acp_client::{AcpClient, AcpClientConfig, BridgeEvent};
use agent_client_protocol::schema::{ContentBlock, SessionUpdate, StopReason};
use futures::StreamExt;
use std::collections::HashMap;
use std::time::Duration;
use tokio::time::timeout;

fn mock_bin() -> std::path::PathBuf {
    // Tests live in `target/<profile>/deps/<testbin>.exe`. The mock binary
    // lives in `target/<profile>/mock_acp_agent[.exe]`. Walk up two
    // directories from the current test binary to find it.
    let exe = std::env::current_exe().expect("current_exe");
    let target_dir = exe
        .parent()
        .and_then(|p| p.parent())
        .expect("two parents up from test binary");
    let mut bin = target_dir.join("mock_acp_agent");
    if cfg!(windows) {
        bin.set_extension("exe");
    }
    assert!(
        bin.exists(),
        "mock_acp_agent not found at {} — run `cargo build -p mock_acp_agent` first",
        bin.display()
    );
    bin
}

fn cfg() -> AcpClientConfig {
    AcpClientConfig {
        command: mock_bin().to_string_lossy().into_owned(),
        args: vec!["--script".into(), "happy".into()],
        cwd: std::env::temp_dir(),
        env: HashMap::new(),
    }
}

#[tokio::test]
async fn smoke_initialize_new_prompt_happy_path() {
    let client = AcpClient::spawn(cfg()).await.expect("spawn ok");

    client.initialize().await.expect("initialize ok");

    let sid = client
        .session_new(std::env::temp_dir())
        .await
        .expect("session_new ok");
    assert!(!sid.0.is_empty(), "empty session id");

    let mut stream = client
        .session_prompt(&sid, "what is 2+2?")
        .await
        .expect("session_prompt ok");

    let mut saw_chunk_text: Option<String> = None;
    let mut terminal: Option<StopReason> = None;

    // Drain the stream with a generous timeout — the mock is local and
    // should be near-instant; 5s is purely an anti-hang guard.
    let drain = async {
        while let Some(ev) = stream.next().await {
            match ev.expect("stream item ok") {
                BridgeEvent::Update(boxed) => {
                    if let SessionUpdate::AgentMessageChunk(chunk) = *boxed {
                        if let ContentBlock::Text(t) = chunk.content {
                            saw_chunk_text = Some(t.text);
                        }
                    }
                }
                BridgeEvent::Terminal(reason) => {
                    terminal = Some(reason);
                    break;
                }
            }
        }
    };
    timeout(Duration::from_secs(5), drain)
        .await
        .expect("drain timed out");

    assert_eq!(saw_chunk_text.as_deref(), Some("4"));
    assert!(matches!(terminal, Some(StopReason::EndTurn)));
}

#[tokio::test]
async fn dropping_client_shuts_down_driver_cleanly() {
    // Smoke: spawn + drop must not panic and must not leak the child
    // process indefinitely. We can't easily assert on process death from
    // here, but a panic in the driver task would surface as a tokio
    // runtime warning printed to stderr; the cargo test runner does not
    // fail on those, so this test exists primarily as a regression
    // tripwire if drop semantics break in a future refactor.
    let client = AcpClient::spawn(cfg()).await.expect("spawn ok");
    client.initialize().await.expect("initialize ok");
    drop(client);
    // Give the driver a moment to wind down so any panic surfaces in the
    // test output rather than leaking into a later test.
    tokio::time::sleep(Duration::from_millis(200)).await;
}
