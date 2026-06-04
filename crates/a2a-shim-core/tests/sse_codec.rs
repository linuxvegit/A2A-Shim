use a2a_shim_core::wire::sse::{encode_sse_event, parse_sse_data_line, SseEvent};
use a2a_shim_core::wire::task::{TaskId, TaskState, TaskStatus};

#[test]
fn encode_status_update_has_final_flag() {
    let ev = SseEvent::StatusUpdate {
        task_id: TaskId::from("t-x"),
        status: TaskStatus {
            state: TaskState::Completed,
            message: None,
            timestamp: None,
        },
        final_: true,
    };
    let s = encode_sse_event(&ev);
    assert!(s.starts_with("data: "), "got: {s}");
    assert!(s.ends_with("\n\n"), "got: {s:?}");
    assert!(s.contains("\"kind\":\"status-update\""), "got: {s}");
    assert!(s.contains("\"final\":true"), "got: {s}");
}

#[test]
fn parse_status_update() {
    let line =
        r#"{"kind":"status-update","taskId":"t-x","status":{"state":"working"},"final":false}"#;
    let SseEvent::StatusUpdate {
        task_id,
        status,
        final_,
    } = parse_sse_data_line(line).unwrap()
    else {
        panic!("expected StatusUpdate");
    };
    assert_eq!(task_id.as_str(), "t-x");
    assert_eq!(status.state, TaskState::Working);
    assert!(!final_);
}

#[test]
fn parse_artifact_update() {
    let line = r#"{"kind":"artifact-update","taskId":"t-x","artifact":{"parts":[{"type":"text","text":"hi"}]},"append":false}"#;
    assert!(matches!(
        parse_sse_data_line(line).unwrap(),
        SseEvent::ArtifactUpdate { .. }
    ));
}
