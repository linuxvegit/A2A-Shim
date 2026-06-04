//! Permission policy (spec § 2.7).
//!
//! Pure decision-maker: given an inbound `RequestPermissionRequest`, the
//! configured strategy, and the deny-list of tool kinds, produce a
//! `Decision` of `Approve(option_id)` or `Reject`. AcpClient's permission
//! callback (Task 17 follow-up) translates the Decision into a wire
//! `RequestPermissionResponse`.
//!
//! Strategy semantics (spec § 2.7 + § 2.3):
//!   * `AutoApprove` — approve unless the requested tool's kind is in
//!     `deny_tool_kinds`, in which case reject. When approving, prefer
//!     the `AllowOnce` option (least-trust grant), else fall back to the
//!     first option offered.
//!   * `AutoReject` — always reject. The deny list is therefore moot.
//!   * `Passthrough` — reserved for v1.2; rejected at config load. As
//!     defense-in-depth, this evaluator treats it as `AutoReject` so a
//!     hand-constructed value cannot accidentally grant.
//!
//! If the agent offers zero options, the policy returns `Reject`.

use a2a_shim_core::config::serve_toml::PermissionStrategy;
use agent_client_protocol::schema::{
    PermissionOptionId, PermissionOptionKind, RequestPermissionRequest, ToolKind,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Approve(PermissionOptionId),
    Reject,
}

/// Evaluate the policy. `deny_tool_kinds` carries the lower-cased,
/// dotted-name forms of ToolKind variants that must be force-rejected
/// (e.g. "delete", "execute"); comparison is case-insensitive.
pub fn evaluate(
    req: &RequestPermissionRequest,
    strategy: PermissionStrategy,
    deny_tool_kinds: &[String],
) -> Decision {
    // AutoReject / Passthrough → reject regardless of options.
    if matches!(
        strategy,
        PermissionStrategy::AutoReject | PermissionStrategy::Passthrough
    ) {
        return Decision::Reject;
    }

    // AutoApprove with deny-list match → reject.
    if !deny_tool_kinds.is_empty() {
        // tool_call is a ToolCallUpdate; the kind is optional because
        // updates may patch any subset of fields. When unspecified we
        // cannot match a deny rule, so default to allowing through to the
        // option-picker below.
        if let Some(kind) = req.tool_call.fields.kind {
            let kind_name = tool_kind_name(kind);
            if deny_tool_kinds
                .iter()
                .any(|k| k.eq_ignore_ascii_case(kind_name))
            {
                tracing::warn!(
                    tool = ?kind,
                    "permission auto-rejected by deny list"
                );
                return Decision::Reject;
            }
        }
    }

    // AutoApprove with no objection → pick AllowOnce if present, else first.
    let chosen = req
        .options
        .iter()
        .find(|o| o.kind == PermissionOptionKind::AllowOnce)
        .or_else(|| req.options.first());

    match chosen {
        Some(opt) => {
            tracing::warn!(
                option = ?opt.option_id,
                tool = ?req.tool_call.fields.kind,
                "permission auto-approved"
            );
            Decision::Approve(opt.option_id.clone())
        }
        None => Decision::Reject,
    }
}

/// Canonical lower-snake name for a ToolKind, matching the wire form so
/// operators can write `deny_tool_kinds = ["delete", "execute"]` in TOML.
fn tool_kind_name(k: ToolKind) -> &'static str {
    match k {
        ToolKind::Read => "read",
        ToolKind::Edit => "edit",
        ToolKind::Delete => "delete",
        ToolKind::Move => "move",
        ToolKind::Search => "search",
        ToolKind::Execute => "execute",
        ToolKind::Think => "think",
        ToolKind::Fetch => "fetch",
        ToolKind::SwitchMode => "switch_mode",
        ToolKind::Other => "other",
        // ToolKind is #[non_exhaustive] in the SDK so future variants
        // (e.g. a newly minted "network" kind) fall through to "other"
        // for deny-list matching purposes. Operators who care must
        // upgrade their deny list once the SDK exposes the new name.
        _ => "other",
    }
}
