//! Render an A2A `Task` (or normalized error) into an MCP `tools/call`
//! result payload (spec § 3.5 + § 3.8).
//!
//! Three terminal shapes:
//!   * `Completed`        -> content[].text = concatenated artifact texts;
//!     `_meta.a2aTask` = full Task JSON. `isError=false`.
//!   * `Failed|Canceled`  -> `isError=true`; content[].text = human reason;
//!     `_meta.error` = NormalizedError envelope.
//!   * `InputRequired`    -> `isError=false`; content[].text = last agent
//!     message text; `_meta.{taskId, state}` so the
//!     Host can re-invoke with `task_id` to continue.

use a2a_shim_core::error::normalize::{ErrorKind, NormalizedError};
use a2a_shim_core::wire::message::Part;
use a2a_shim_core::wire::task::Task;
use serde_json::{json, Value};

/// Build a Completed tool result.
pub fn render_completed(task: &Task) -> Value {
    let text = concatenate_artifact_text(task);
    json!({
        "isError": false,
        "content": [{
            "type": "text",
            "text": text
        }],
        "_meta": { "a2aTask": serde_json::to_value(task).expect("Task serializes") }
    })
}

/// Build a Failed / Canceled tool result. The reason string is shown to
/// the Host user; the _meta.error envelope is for programmatic consumers.
pub fn render_failed(reason: &str, kind: ErrorKind, remote_task_id: Option<String>) -> Value {
    let err = NormalizedError {
        kind,
        message: reason.to_string(),
        remote_task_id,
    };
    json!({
        "isError": true,
        "content": [{
            "type": "text",
            "text": reason
        }],
        "_meta": { "error": err }
    })
}

/// Build an Input-Required tool result. NOT an error — the caller is
/// expected to follow up with another `a2a_send` carrying `task_id`.
pub fn render_input_required(task: &Task) -> Value {
    let prompt_text = task
        .status
        .message
        .as_ref()
        .map(|m| concatenate_message_text(&m.parts))
        .unwrap_or_else(|| "(agent is waiting on input but did not include a prompt)".to_string());
    json!({
        "isError": false,
        "content": [{
            "type": "text",
            "text": prompt_text,
        }],
        "_meta": {
            "taskId": task.id,
            "state": "input-required",
            "hint": "Call a2a_send again with task_id set to this task to provide the requested input.",
            "a2aTask": serde_json::to_value(task).expect("Task serializes")
        }
    })
}

/// Concatenate all text parts of all artifacts of a Task into one string.
/// Non-text parts are skipped silently.
fn concatenate_artifact_text(task: &Task) -> String {
    let mut out = String::new();
    for a in &task.artifacts {
        out.push_str(&concatenate_message_text(&a.parts));
    }
    if out.is_empty() {
        out.push_str("(agent returned no text content)");
    }
    out
}

fn concatenate_message_text(parts: &[Part]) -> String {
    let mut out = String::new();
    for p in parts {
        if let Part::Text { text } = p {
            out.push_str(text);
        }
    }
    out
}

/// Map an `OutboundError` to the right `(kind, message)` for the
/// `render_failed` helper. Centralized so callers stay uncluttered.
pub fn error_kind_for_outbound(err: &crate::outbound::OutboundError) -> ErrorKind {
    use crate::outbound::OutboundError::*;
    match err {
        NetworkError(_) => ErrorKind::NetworkError,
        RemoteFailed { .. } => ErrorKind::RemoteFailed,
        RemoteTimeout { .. } => ErrorKind::RemoteTimeout,
        HardCeilingExceeded { .. } => ErrorKind::RemoteTimeout,
        ProtocolError(_) => ErrorKind::ProtocolError,
    }
}
