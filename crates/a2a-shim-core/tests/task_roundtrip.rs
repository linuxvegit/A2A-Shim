use a2a_shim_core::wire::message::{Message, MessageRole, Part};
use a2a_shim_core::wire::task::{Artifact, Task, TaskId, TaskState, TaskStatus};

#[test]
fn state_serializes_kebab_case() {
    use TaskState::*;
    for (s, lit) in [
        (Submitted, "\"submitted\""),
        (Working, "\"working\""),
        (InputRequired, "\"input-required\""),
        (Completed, "\"completed\""),
        (Failed, "\"failed\""),
        (Canceled, "\"canceled\""),
    ] {
        assert_eq!(serde_json::to_string(&s).unwrap(), lit);
    }
}

#[test]
fn terminal_classifier_matches_spec_2_5() {
    use TaskState::*;
    for s in [Submitted, Working, InputRequired] {
        assert!(!s.is_terminal(), "{s:?} should not be terminal");
    }
    for s in [Completed, Failed, Canceled] {
        assert!(s.is_terminal(), "{s:?} should be terminal");
    }
}

#[test]
fn task_full_roundtrip() {
    let task = Task {
        id: TaskId::from("t-abc"),
        context_id: Some("alice/review".into()),
        status: TaskStatus {
            state: TaskState::Completed,
            message: None,
            timestamp: Some("2026-06-03T10:00:00Z".into()),
        },
        history: vec![Message {
            role: MessageRole::User,
            parts: vec![Part::Text { text: "hi".into() }],
            metadata: None,
        }],
        artifacts: vec![Artifact {
            artifact_id: Some("a-1".into()),
            name: Some("answer".into()),
            parts: vec![Part::Text { text: "ok".into() }],
            metadata: None,
        }],
        metadata: None,
    };
    let back: Task = serde_json::from_str(&serde_json::to_string(&task).unwrap()).unwrap();
    assert_eq!(back.id.as_str(), "t-abc");
    assert_eq!(back.context_id.as_deref(), Some("alice/review"));
    assert!(back.status.state.is_terminal());
    assert_eq!(back.history.len(), 1);
    assert_eq!(back.artifacts.len(), 1);
}

#[test]
fn task_id_random_shape() {
    let id = TaskId::new_random();
    assert!(id.as_str().starts_with("t-"), "got: {id}");
    assert!(id.as_str().len() > 5, "got: {id}");
}
