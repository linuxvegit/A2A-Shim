use a2a_shim_core::config::serve_toml::PermissionStrategy;
use a2a_shim_serve::permission::{evaluate, Decision};
use agent_client_protocol::schema::{
    PermissionOption, PermissionOptionId, PermissionOptionKind, RequestPermissionRequest,
    SessionId, ToolCall, ToolCallId, ToolKind,
};

fn opt(id: &str, kind: PermissionOptionKind) -> PermissionOption {
    PermissionOption::new(
        PermissionOptionId::from(id.to_string()),
        id.to_string(),
        kind,
    )
}

fn req(tool_kind: ToolKind, options: Vec<PermissionOption>) -> RequestPermissionRequest {
    RequestPermissionRequest::new(
        SessionId::from("sess".to_string()),
        ToolCall::new(ToolCallId::from("tc-1".to_string()), "test-tool")
            .kind(tool_kind)
            .into(),
        options,
    )
}

#[test]
fn auto_approve_with_no_deny_returns_approve_picking_allow_once() {
    let r = req(
        ToolKind::Read,
        vec![
            opt("o-always", PermissionOptionKind::AllowAlways),
            opt("o-once", PermissionOptionKind::AllowOnce),
            opt("o-rej", PermissionOptionKind::RejectOnce),
        ],
    );
    let d = evaluate(&r, PermissionStrategy::AutoApprove, &[]);
    let Decision::Approve(id) = d else {
        panic!("expected Approve, got {d:?}");
    };
    assert_eq!(&*id.0, "o-once", "expected to pick the allow_once option");
}

#[test]
fn auto_approve_falls_back_to_first_option_when_no_allow_once() {
    let r = req(
        ToolKind::Read,
        vec![
            opt("o-first", PermissionOptionKind::AllowAlways),
            opt("o-rej", PermissionOptionKind::RejectOnce),
        ],
    );
    let d = evaluate(&r, PermissionStrategy::AutoApprove, &[]);
    let Decision::Approve(id) = d else {
        panic!("got {d:?}");
    };
    assert_eq!(&*id.0, "o-first");
}

#[test]
fn auto_approve_with_deny_matching_kind_returns_reject() {
    let r = req(
        ToolKind::Delete,
        vec![
            opt("o-once", PermissionOptionKind::AllowOnce),
            opt("o-rej-once", PermissionOptionKind::RejectOnce),
        ],
    );
    let d = evaluate(&r, PermissionStrategy::AutoApprove, &["delete".to_string()]);
    assert!(matches!(d, Decision::Reject), "got {d:?}");
}

#[test]
fn auto_reject_always_rejects() {
    let r = req(
        ToolKind::Read,
        vec![opt("o-once", PermissionOptionKind::AllowOnce)],
    );
    let d = evaluate(&r, PermissionStrategy::AutoReject, &[]);
    assert!(matches!(d, Decision::Reject), "got {d:?}");
}

#[test]
fn no_options_returns_reject() {
    let r = req(ToolKind::Read, vec![]);
    let d = evaluate(&r, PermissionStrategy::AutoApprove, &[]);
    assert!(matches!(d, Decision::Reject), "got {d:?}");
}

#[test]
fn passthrough_strategy_falls_back_to_reject() {
    // Passthrough is rejected at config load (Task 13), but defense-in-depth:
    // if someone constructs it directly, evaluate() must still be safe.
    let r = req(
        ToolKind::Read,
        vec![opt("o-once", PermissionOptionKind::AllowOnce)],
    );
    let d = evaluate(&r, PermissionStrategy::Passthrough, &[]);
    assert!(matches!(d, Decision::Reject), "got {d:?}");
}
