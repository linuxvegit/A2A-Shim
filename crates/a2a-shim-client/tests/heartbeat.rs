use a2a_shim_client::heartbeat::Heartbeat;
use serde_json::Value;
use std::time::Duration;
use tokio::sync::mpsc;

#[tokio::test(start_paused = true)]
async fn three_frames_in_3500ms_with_1s_interval() {
    let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
    let guard = Heartbeat::start(tx, Some(Value::from("tok-42")), Duration::from_secs(1));

    // Give the spawned task a chance to enter its first sleep.
    tokio::task::yield_now().await;
    // Step through 3 sleep periods, yielding between each so the spawned
    // task's awakened branch actually runs before we move time again.
    for _ in 0..3 {
        tokio::time::advance(Duration::from_millis(1100)).await;
        tokio::task::yield_now().await;
    }

    drop(guard); // stops the task; remaining tx in spawned task is dropped

    let mut frames = Vec::new();
    while let Some(v) = rx.recv().await {
        frames.push(v);
    }
    assert_eq!(frames.len(), 3, "expected 3 progress frames, got: {frames:?}");

    // Each must be a notifications/progress with progressToken + monotone progress.
    let mut last = -1i64;
    for f in &frames {
        assert_eq!(f["jsonrpc"], "2.0");
        assert_eq!(f["method"], "notifications/progress");
        assert_eq!(f["params"]["progressToken"], "tok-42");
        let p = f["params"]["progress"].as_i64().expect("progress is int");
        assert!(p > last, "expected monotonic, got {p} after {last}");
        last = p;
    }
}

#[tokio::test(start_paused = true)]
async fn update_summary_changes_next_message() {
    let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
    let guard = Heartbeat::start(tx, Some(Value::from("tok")), Duration::from_secs(1));

    tokio::time::advance(Duration::from_millis(1100)).await;
    let first = rx.recv().await.expect("first frame");
    assert_eq!(first["params"]["message"], Value::Null);

    guard.update_summary("phase 2".to_string());
    tokio::time::advance(Duration::from_millis(1100)).await;
    let second = rx.recv().await.expect("second frame");
    assert_eq!(second["params"]["message"], "phase 2");

    drop(guard);
}

#[tokio::test(start_paused = true)]
async fn progress_token_none_emits_nothing() {
    let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
    let guard = Heartbeat::start(tx, None, Duration::from_millis(100));
    tokio::time::advance(Duration::from_secs(3)).await;
    drop(guard);
    assert!(rx.recv().await.is_none(), "expected no frames, got something");
}

#[tokio::test(start_paused = true)]
async fn drop_guard_stops_emissions() {
    let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
    let guard = Heartbeat::start(tx, Some(Value::from("tok")), Duration::from_millis(100));
    tokio::time::advance(Duration::from_millis(250)).await;
    drop(guard);
    // Drain any in-flight frames already queued.
    let mut before = 0;
    while rx.try_recv().is_ok() {
        before += 1;
    }
    // After the guard drop the channel is closed; recv yields None.
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(rx.recv().await.is_none(), "frames after drop: started with {before}");
}
