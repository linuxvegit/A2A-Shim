//! Elicitation handler (spec § 2.8).
//!
//! `elicitation/create` is the ACP method an agent uses to ask its client
//! for additional structured input mid-turn. In an A2A-Shim deployment
//! there is no human in front of the Serve Shim — the requester is an
//! HTTP client that has already sent the only message it intends to send
//! — so we cannot fulfill the request.
//!
//! Two layers of defense:
//!   1. AcpClient's `on_receive_dispatch` catch-all (Task 17) already
//!      returns a JSON-RPC `internal_error` for any unhandled method
//!      including `elicitation/create`. This keeps the agent's protocol
//!      loop alive.
//!   2. When the elicitation arrives mid-prompt, the bridge layer
//!      (Task 19) inspects the synthesized `BridgeEvent::Terminal(...)`
//!      that arrives once the agent's prompt resolves with `Refusal` /
//!      `ToolError`, and embeds [`error_message()`] into the resulting
//!      A2A Task's `status.message` so the caller sees why the Task
//!      failed.
//!
//! Phase 0 V7 verified that `claude-agent-acp` never emits
//! `elicitation/create` for low-risk prompts, so this code path is
//! exercised purely as defense in depth. If a future agent starts
//! sending elicitations, the catch-all -32601 path is what triggers,
//! and bridge.rs (Task 19) will already be mapping that into `Failed`.

/// Canonical human-readable reason embedded into `TaskStatus.message` when
/// an elicitation triggers a `Failed` transition. Centralized here so the
/// wording is testable and consistent across bridge + integration tests.
pub fn error_message() -> &'static str {
    "Agent requested mid-turn elicitation (elicitation/create), which the \
     A2A-Shim Serve role cannot fulfill: there is no human at this endpoint. \
     This MVP returns method-not-implemented; a future protocol revision may \
     surface elicitations as an A2A `input-required` Task instead."
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_is_stable_and_descriptive() {
        let m = error_message();
        assert!(m.contains("elicitation"), "got: {m}");
        assert!(m.contains("input-required"), "got: {m}");
    }
}
