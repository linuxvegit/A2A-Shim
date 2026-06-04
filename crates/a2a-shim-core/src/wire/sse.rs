//! Server-Sent Events for `message/stream` (spec § 4.5).
//!
//! Two event kinds — `status-update` and `artifact-update` — discriminated by
//! a top-level `kind` field. The encoder produces a single SSE `data:` record
//! terminated by a blank line. Keepalive frames (`: keepalive\n\n`) are emitted
//! separately by the SSE sink in `a2a-shim-serve`.

use super::task::{Artifact, TaskId, TaskStatus};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SseEvent {
    StatusUpdate {
        #[serde(rename = "taskId")]
        task_id: TaskId,
        status: TaskStatus,
        #[serde(default, rename = "final")]
        final_: bool,
    },
    ArtifactUpdate {
        #[serde(rename = "taskId")]
        task_id: TaskId,
        artifact: Artifact,
        #[serde(default)]
        append: bool,
    },
}

/// Encode one event as a single SSE `data:` record terminated by a blank line.
pub fn encode_sse_event(ev: &SseEvent) -> String {
    let json = serde_json::to_string(ev).expect("SseEvent serialization is infallible");
    format!("data: {json}\n\n")
}

/// Parse the JSON payload of one `data:` line back into a typed event.
pub fn parse_sse_data_line(line: &str) -> Result<SseEvent, serde_json::Error> {
    serde_json::from_str(line)
}
