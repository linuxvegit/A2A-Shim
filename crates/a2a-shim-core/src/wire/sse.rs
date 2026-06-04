//! Server-Sent Events for `SendStreamingMessage` (A2A v1.0.1 spec § 4.2;
//! ADR 0005 wire upgrade).
//!
//! v0.x form (legacy, no longer accepted):
//!     { "kind": "status-update", "taskId": ..., "status": ... }
//!
//! v1.0 form (current):
//!     { "statusUpdate":   { "taskId": ..., "status": ..., "final": ... } }
//!     { "artifactUpdate": { "taskId": ..., "artifact": ..., "append": ... } }
//!
//! The outer wrapping is via `#[serde(untagged)]` + a single mandatory
//! key per variant (`statusUpdate` / `artifactUpdate`); the inner
//! envelope carries the v0.x field set unchanged.

use serde::{Deserialize, Serialize};

use super::task::{Artifact, TaskId, TaskStatus};

/// Outer A2A SSE event. Variants are discriminated by which wrapper
/// key the payload has.
///
/// Order matters under `#[serde(untagged)]`, but since the two
/// variants have disjoint required keys (`statusUpdate` vs
/// `artifactUpdate`), order is mostly cosmetic. We keep `StatusUpdate`
/// first to match the ordering operators see in spec § 4.2.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum SseEvent {
    StatusUpdate {
        #[serde(rename = "statusUpdate")]
        inner: TaskStatusEnvelope,
    },
    ArtifactUpdate {
        #[serde(rename = "artifactUpdate")]
        inner: TaskArtifactEnvelope,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskStatusEnvelope {
    #[serde(rename = "taskId")]
    pub task_id: TaskId,
    #[serde(rename = "contextId", skip_serializing_if = "Option::is_none", default)]
    pub context_id: Option<String>,
    pub status: TaskStatus,
    /// Marks the terminal frame. Serializes as `"final"` (Rust
    /// keyword) under `#[serde(rename)]`.
    #[serde(default, rename = "final")]
    pub final_: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskArtifactEnvelope {
    #[serde(rename = "taskId")]
    pub task_id: TaskId,
    #[serde(rename = "contextId", skip_serializing_if = "Option::is_none", default)]
    pub context_id: Option<String>,
    pub artifact: Artifact,
    #[serde(default)]
    pub append: bool,
}

impl SseEvent {
    /// Convenience constructor for a status-update event.
    pub fn status(task_id: TaskId, status: TaskStatus, final_: bool) -> Self {
        Self::StatusUpdate {
            inner: TaskStatusEnvelope {
                task_id,
                context_id: None,
                status,
                final_,
            },
        }
    }

    /// Convenience constructor for an artifact-update event.
    pub fn artifact(task_id: TaskId, artifact: Artifact, append: bool) -> Self {
        Self::ArtifactUpdate {
            inner: TaskArtifactEnvelope {
                task_id,
                context_id: None,
                artifact,
                append,
            },
        }
    }
}

/// Encode one event as a single SSE `data:` record terminated by a
/// blank line.
pub fn encode_sse_event(ev: &SseEvent) -> String {
    let json = serde_json::to_string(ev).expect("SseEvent serialization is infallible");
    format!("data: {json}\n\n")
}

/// Parse the JSON payload of one `data:` line back into a typed event.
pub fn parse_sse_data_line(line: &str) -> Result<SseEvent, serde_json::Error> {
    serde_json::from_str(line)
}
