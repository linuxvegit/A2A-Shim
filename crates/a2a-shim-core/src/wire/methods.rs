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

/// Parameters for A2A v1.0 § 9.4.4 `ListTasks`. Both fields optional.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListTasksParams {
    /// Maximum tasks to return in this page. 0 / missing => server default.
    #[serde(default, rename = "pageSize", skip_serializing_if = "Option::is_none")]
    pub page_size: Option<usize>,
    /// Opaque cursor from a previous response's `nextPageToken`.
    #[serde(default, rename = "pageToken", skip_serializing_if = "Option::is_none")]
    pub page_token: Option<String>,
}
