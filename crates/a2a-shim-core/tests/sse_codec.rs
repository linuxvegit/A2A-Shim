//! A2A v1.0 SSE event wrapped form (ADR 0005).
//!
//! Legacy v0.x: { "kind": "status-update", "taskId": ..., "status": ... }
//! Current v1: { "statusUpdate": { "taskId": ..., "status": ... } }
//!
//! Same shift for artifactUpdate. Inner envelopes carry taskId / status
//! / artifact / final / append exactly as before.

use a2a_shim_core::wire::sse::{encode_sse_event, parse_sse_data_line, SseEvent};
use a2a_shim_core::wire::task::{TaskId, TaskState, TaskStatus};
use serde_json::{json, Value};

#[test]
fn encode_status_update_wrapped_form() {
    let ev = SseEvent::status(
        TaskId::from("t-x"),
        TaskStatus { state: TaskState::Completed, message: None, timestamp: None },
        true,
    );
    let s = encode_sse_event(&ev);
    assert!(s.starts_with("data: "), "got: {s}");
    assert!(s.ends_with("\n\n"), "got: {s:?}");
    // No top-level "kind" key any more.
    assert!(!s.contains("\"kind\""), "v1.0 must not emit kind tag: {s}");
    // Inner wrapper key present.
    assert!(s.contains("\"statusUpdate\""), "got: {s}");
    assert!(s.contains("\"final\":true"), "got: {s}");
    assert!(s.contains("\"taskId\":\"t-x\""), "got: {s}");
}

#[test]
fn parse_status_update_wrapped_form() {
    let line = r#"{"statusUpdate":{"taskId":"t-x","status":{"state":"working"},"final":false}}"#;
    let parsed = parse_sse_data_line(line).expect("parse ok");
    let SseEvent::StatusUpdate { inner } = parsed else {
        panic!("expected StatusUpdate variant; got {parsed:?}");
    };
    assert_eq!(inner.task_id.as_str(), "t-x");
    assert_eq!(inner.status.state, TaskState::Working);
    assert!(!inner.final_);
}

#[test]
fn parse_artifact_update_wrapped_form() {
    let line = r#"{"artifactUpdate":{"taskId":"t-x","artifact":{"parts":[{"text":"hi"}]},"append":false}}"#;
    let parsed = parse_sse_data_line(line).expect("parse ok");
    let SseEvent::ArtifactUpdate { inner } = parsed else {
        panic!("expected ArtifactUpdate variant; got {parsed:?}");
    };
    assert_eq!(inner.task_id.as_str(), "t-x");
    assert_eq!(inner.artifact.parts.len(), 1);
    assert!(!inner.append);
}

#[test]
fn artifact_update_wraps_inner_v1_part() {
    // The artifact's parts use v1.0 Part shape (no `type` tag) per ADR 0005.
    use a2a_shim_core::wire::message::Part;
    use a2a_shim_core::wire::task::Artifact;

    let ev = SseEvent::artifact(
        TaskId::from("t-x"),
        Artifact {
            artifact_id: Some("a-answer".into()),
            name: Some("answer".into()),
            parts: vec![Part::Text { text: "4".into() }],
            metadata: None,
        },
        true,
    );
    let v: Value = serde_json::from_str(&encode_sse_event(&ev).trim_start_matches("data: ").trim()).unwrap();
    // The inner artifact's first part is just {"text": "4"} now.
    assert_eq!(v["artifactUpdate"]["artifact"]["parts"][0], json!({"text": "4"}));
}

#[test]
fn legacy_kind_form_no_longer_parses() {
    // ADR 0005 hard cutover: legacy v0.x kind-tagged form must not bind.
    let legacy = r#"{"kind":"status-update","taskId":"t-x","status":{"state":"working"},"final":false}"#;
    let parsed = parse_sse_data_line(legacy);
    assert!(
        parsed.is_err(),
        "legacy kind-tagged form should fail to parse under v1.0 wire; got {parsed:?}"
    );
}
