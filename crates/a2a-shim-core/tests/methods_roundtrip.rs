use a2a_shim_core::wire::message::{Message, MessageRole, Part};
use a2a_shim_core::wire::methods::{SendMessageParams, TaskIdParams};
use a2a_shim_core::wire::task::TaskId;

fn hello() -> Message {
    Message {
        role: MessageRole::User,
        parts: vec![Part::Text { text: "hi".into() }],
        metadata: None,
    }
}

#[test]
fn new_task_omits_id() {
    let p = SendMessageParams {
        id: None,
        message: hello(),
        configuration: None,
    };
    let s = serde_json::to_string(&p).unwrap();
    assert!(!s.contains("\"id\""), "expected no id field; got: {s}");
}

#[test]
fn continuation_includes_id() {
    let p = SendMessageParams {
        id: Some(TaskId::from("t-1")),
        message: hello(),
        configuration: None,
    };
    let s = serde_json::to_string(&p).unwrap();
    assert!(s.contains(r#""id":"t-1""#), "got: {s}");
}

#[test]
fn task_id_params_is_lean() {
    let s = serde_json::to_string(&TaskIdParams {
        id: TaskId::from("t-zzz"),
    })
    .unwrap();
    assert_eq!(s, r#"{"id":"t-zzz"}"#);
}
