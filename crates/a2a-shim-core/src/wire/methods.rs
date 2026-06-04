//! Method-specific param types for the A2A JSON-RPC surface (spec § 4.4).

use super::message::Message;
use super::task::TaskId;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Parameters for `message/send` and `message/stream`.
///
/// `id` absent ⇒ create a new Task. `id` present ⇒ continuation; spec § 2.5
/// only allows this when the named Task is in `input-required`. The server
/// must reject continuations on `working` Tasks with `INVALID_PARAMS`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendMessageParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<TaskId>,
    pub message: Message,
    /// Accepted on the wire but ignored in MVP (spec § 4.4).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configuration: Option<Value>,
}

/// Parameters for `tasks/get` and `tasks/cancel`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskIdParams {
    pub id: TaskId,
}
