use a2a_shim_core::wire::sse::SseEvent;
use a2a_shim_core::wire::task::{TaskId, TaskState, TaskStatus};
use a2a_shim_serve::sse_sink::{SseFrame, SseSink};
use std::time::Duration;
use tokio::time::timeout;

fn status(state: TaskState, final_: bool) -> SseEvent {
    SseEvent::StatusUpdate {
        task_id: TaskId::from("t-1"),
        status: TaskStatus {
            state,
            message: None,
            timestamp: None,
        },
        final_,
    }
}

#[tokio::test]
async fn subscriber_receives_event_then_final_closes_channel() {
    let sink = SseSink::new(8);
    let mut rx = sink.subscribe();

    sink.publish_event(status(TaskState::Working, false));
    sink.publish_final(status(TaskState::Completed, true));

    // First frame: working
    let first = timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("first recv timed out")
        .expect("first frame is Ok");
    assert!(matches!(
        first,
        SseFrame::Event(SseEvent::StatusUpdate {
            status: TaskStatus {
                state: TaskState::Working,
                ..
            },
            ..
        })
    ));

    // Second frame: completed final
    let second = timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("second recv timed out")
        .expect("second frame is Ok");
    assert!(matches!(
        second,
        SseFrame::Event(SseEvent::StatusUpdate {
            status: TaskStatus {
                state: TaskState::Completed,
                ..
            },
            final_: true,
            ..
        })
    ));

    // Channel must be closed now.
    let after = timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("third recv timed out");
    assert!(after.is_err(), "expected Closed; got {after:?}");
}

#[tokio::test(start_paused = true)]
async fn keepalive_emitted_at_interval() {
    let sink = SseSink::new(8);
    let mut rx = sink.subscribe();
    sink.start_keepalive(Duration::from_secs(1));
    tokio::time::advance(Duration::from_millis(1100)).await;
    let frame = rx.recv().await.expect("recv ok");
    assert!(matches!(frame, SseFrame::Keepalive), "got {frame:?}");
}

#[tokio::test]
async fn publish_after_final_is_noop() {
    // Defensive: caller might mistakenly publish more events after the
    // terminal one. We must not panic and must not deliver them.
    let sink = SseSink::new(8);
    let mut rx = sink.subscribe();
    sink.publish_final(status(TaskState::Completed, true));
    let _ = timeout(Duration::from_secs(1), rx.recv()).await.unwrap();
    // Channel closed.
    sink.publish_event(status(TaskState::Working, false));
    // Drop the closed-receiver path explicitly.
    let after = timeout(Duration::from_secs(1), rx.recv()).await.unwrap();
    assert!(after.is_err(), "expected Closed; got {after:?}");
}
