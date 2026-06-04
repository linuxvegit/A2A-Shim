//! End-to-end test of `bridge::run_session` against the real `AcpClient`
//! driving the `mock_acp_agent` 'happy' script.
//!
//! Validates that one happy-path prompt produces, in order:
//!   1. status-update Working (not final)
//!   2. artifact-update for the 'a-answer' artifact carrying "4"
//!   3. status-update Completed (final = true), after which the sink is closed
//!
//! Also asserts the TaskRegistry snapshot lands in Completed with the
//! accumulated answer text in artifacts[0].

use a2a_shim_core::wire::sse::SseEvent;
use a2a_shim_core::wire::task::TaskState;
use a2a_shim_serve::acp_client::{AcpClient, AcpClientConfig};
use a2a_shim_serve::bridge;
use a2a_shim_serve::sse_sink::SseFrame;
use a2a_shim_serve::task_registry::TaskRegistry;
use std::collections::HashMap;
use std::time::Duration;
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

#[tokio::test]
async fn happy_path_chunk_then_completed() {
    let cfg = AcpClientConfig {
        command: mock_bin().to_string_lossy().into_owned(),
        args: vec!["--script".into(), "happy".into()],
        cwd: std::env::temp_dir(),
        env: HashMap::new(),
    };
    let client = AcpClient::spawn(cfg).await.expect("spawn");
    client.initialize().await.expect("initialize");
    let sid = client
        .session_new(std::env::temp_dir())
        .await
        .expect("session/new");

    let registry = TaskRegistry::new();
    let task_id = registry.create("alice/review", sid.0.as_ref()).await;
    let sink = registry.sink(&task_id).await.expect("sink");
    let mut rx = sink.subscribe().expect("sink open");

    let stream = client
        .session_prompt(&sid, "what is 2+2?")
        .await
        .expect("session/prompt");

    bridge::run_session(task_id.clone(), registry.clone(), stream)
        .await
        .expect("bridge ok");

    // Drain frames with a per-recv timeout to make hangs surface fast.
    let mut frames = Vec::new();
    loop {
        match timeout(Duration::from_secs(2), rx.recv()).await {
            Ok(Ok(f)) => frames.push(f),
            Ok(Err(_)) => break, // channel closed (publish_final dropped sender)
            Err(_) => panic!("recv timed out after {} frames", frames.len()),
        }
    }

    // Expected: Working, ArtifactUpdate("4"), Completed.
    assert!(
        frames.len() >= 3,
        "expected at least 3 frames, got {}: {frames:?}",
        frames.len()
    );

    let SseFrame::Event(SseEvent::StatusUpdate { inner: first_inner }) = &frames[0]
    else {
        panic!("first frame should be StatusUpdate, got {:?}", frames[0]);
    };
    assert_eq!(first_inner.status.state, TaskState::Working);
    assert!(!first_inner.final_, "first status should not be final");

    // Find the artifact-update with text "4" and the terminal completed.
    let mut saw_answer = false;
    let mut saw_terminal_completed = false;
    for f in &frames[1..] {
        match f {
            SseFrame::Event(SseEvent::ArtifactUpdate { inner }) => {
                if let Some(a2a_shim_core::wire::message::Part::Text { text }) =
                    inner.artifact.parts.first()
                {
                    if text == "4" {
                        saw_answer = true;
                    }
                }
            }
            SseFrame::Event(SseEvent::StatusUpdate { inner })
                if inner.status.state == TaskState::Completed && inner.final_ =>
            {
                saw_terminal_completed = true;
            }
            _ => {}
        }
    }
    assert!(saw_answer, "did not see artifact-update with text '4'");
    assert!(saw_terminal_completed, "did not see final completed status");

    let snap = registry.snapshot(&task_id).await.expect("snapshot");
    assert_eq!(snap.status.state, TaskState::Completed);
    assert_eq!(snap.artifacts.len(), 1, "expected one accumulated artifact");
}
