//! v1.1 Task 12: bridge passes non-text ContentBlocks through translate
//! and emits matching artifact-update SSE frames.

use a2a_shim_core::wire::message::Part;
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
    assert!(bin.exists());
    bin
}

#[tokio::test]
async fn multimodal_script_emits_text_and_image_parts() {
    let cfg = AcpClientConfig {
        command: mock_bin().to_string_lossy().into_owned(),
        args: vec!["--script".into(), "multimodal".into()],
        cwd: std::env::temp_dir(),
        env: HashMap::new(),
    };
    let client = AcpClient::spawn(cfg).await.expect("spawn");
    client.initialize().await.expect("init");
    let sid = client
        .session_new(std::env::temp_dir())
        .await
        .expect("session/new");

    let registry = TaskRegistry::new();
    let task_id = registry.create("alice/multi", sid.0.as_ref()).await;
    let sink = registry.sink(&task_id).await.expect("sink");
    let mut rx = sink.subscribe().expect("sink open");

    let stream = client
        .session_prompt(&sid, "show me an image")
        .await
        .expect("prompt");
    bridge::run_session(task_id.clone(), registry.clone(), stream)
        .await
        .expect("bridge ok");

    let mut frames = Vec::new();
    loop {
        match timeout(Duration::from_secs(2), rx.recv()).await {
            Ok(Ok(f)) => frames.push(f),
            Ok(Err(_)) => break,
            Err(_) => panic!("timed out after {} frames", frames.len()),
        }
    }

    // Expect:
    //   1. status(Working)
    //   2. artifact-update with Text part "Here is an image: "
    //   3. artifact-update with File part {mediaType=image/png, raw=<base64>}
    //   4. status(Completed final=true)
    assert!(
        frames.len() >= 4,
        "expected >= 4 frames, got {}: {frames:?}",
        frames.len()
    );

    let mut saw_text = false;
    let mut saw_image = false;
    for f in &frames {
        if let SseFrame::Event(a2a_shim_core::wire::sse::SseEvent::ArtifactUpdate { inner }) = f {
            for p in &inner.artifact.parts {
                match p {
                    Part::Text { text } if text.contains("image") => saw_text = true,
                    Part::File {
                        raw: Some(_),
                        media_type,
                        ..
                    } if media_type == "image/png" => saw_image = true,
                    _ => {}
                }
            }
        }
    }
    assert!(saw_text, "missing text artifact part: {frames:?}");
    assert!(saw_image, "missing image File part: {frames:?}");

    let snap = registry.snapshot(&task_id).await.expect("snap");
    assert_eq!(snap.status.state, TaskState::Completed);
}
