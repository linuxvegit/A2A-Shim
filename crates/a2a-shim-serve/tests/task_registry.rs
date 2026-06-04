use a2a_shim_core::wire::task::{TaskId, TaskState};
use a2a_shim_serve::task_registry::{TaskRegistry, TransitionError};

#[tokio::test]
async fn submit_then_working_then_completed() {
    let reg = TaskRegistry::new();
    let id = reg.create("alice/review", "sess-1").await;
    reg.transition(&id, TaskState::Working).await.unwrap();
    reg.transition(&id, TaskState::Completed).await.unwrap();
    let snap = reg.snapshot(&id).await.unwrap();
    assert_eq!(snap.status.state, TaskState::Completed);
}

#[tokio::test]
async fn continuation_only_from_input_required() {
    let reg = TaskRegistry::new();
    let id = reg.create("c", "s").await;

    reg.transition(&id, TaskState::Working).await.unwrap();
    let err = reg.accept_continuation(&id).await.unwrap_err();
    assert!(matches!(err, TransitionError::InvalidContinuation));

    reg.transition(&id, TaskState::InputRequired).await.unwrap();
    reg.accept_continuation(&id).await.unwrap();
    let snap = reg.snapshot(&id).await.unwrap();
    assert_eq!(snap.status.state, TaskState::Working);
}

#[tokio::test]
async fn cancel_from_working_succeeds() {
    let reg = TaskRegistry::new();
    let id = reg.create("c", "s").await;
    reg.transition(&id, TaskState::Working).await.unwrap();
    reg.cancel(&id).await.unwrap();
    let snap = reg.snapshot(&id).await.unwrap();
    assert_eq!(snap.status.state, TaskState::Canceled);
}

#[tokio::test]
async fn cancel_after_terminal_rejected() {
    let reg = TaskRegistry::new();
    let id = reg.create("c", "s").await;
    reg.transition(&id, TaskState::Working).await.unwrap();
    reg.transition(&id, TaskState::Completed).await.unwrap();
    let err = reg.cancel(&id).await.unwrap_err();
    assert!(matches!(err, TransitionError::NotCancelable));
}

#[tokio::test]
async fn snapshot_of_unknown_id_is_none() {
    assert!(TaskRegistry::new()
        .snapshot(&TaskId::from("t-nope"))
        .await
        .is_none());
}

#[tokio::test]
async fn transition_unknown_id_is_not_found() {
    let reg = TaskRegistry::new();
    let err = reg
        .transition(&TaskId::from("t-nope"), TaskState::Working)
        .await
        .unwrap_err();
    assert!(matches!(err, TransitionError::NotFound));
}

#[tokio::test]
async fn illegal_transition_rejected() {
    let reg = TaskRegistry::new();
    let id = reg.create("c", "s").await;
    // Cannot jump submitted -> completed (must go via Working).
    let err = reg
        .transition(&id, TaskState::Completed)
        .await
        .unwrap_err();
    assert!(matches!(err, TransitionError::Illegal));
}

#[tokio::test]
async fn sink_handle_is_addressable() {
    let reg = TaskRegistry::new();
    let id = reg.create("c", "s").await;
    let sink = reg.sink(&id).await.expect("sink exists");
    // Just exercise the surface — sink is cloneable.
    let _sink2 = sink.clone();
}
