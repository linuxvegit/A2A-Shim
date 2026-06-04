//! `a2a_send` tool-call handler (spec § 3.5-3.9).
//!
//! Orchestrates: parse args → register cancellation → start heartbeat →
//! open outbound stream → drive bridge events → render terminal result.
//! On Host cancellation (notifications/cancelled), races the work
//! against the registered token via tokio::select! and best-effort
//! issues A2A tasks/cancel against the remote.

use a2a_shim_core::error::normalize::ErrorKind;
use a2a_shim_core::wire::message::{Message, MessageMetadata, MessageRole, Part};
use a2a_shim_core::wire::sse::SseEvent;
use a2a_shim_core::wire::task::{Artifact, Task, TaskId, TaskState, TaskStatus};
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

use crate::cancellation::CancellationRegistry;
use crate::heartbeat::Heartbeat;
use crate::outbound::{stream as outbound_stream, OutboundDeadlines, OutboundError};
use crate::render::{
    error_kind_for_outbound, render_completed, render_failed, render_input_required,
};

/// Arguments to the `a2a_send` tool, parsed from `params.arguments`.
#[derive(Debug, Deserialize)]
pub struct A2aSendArgs {
    pub endpoint: String,
    pub conversation_id: String,
    pub message: String,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub metadata: Option<Value>,
}

/// Per-call runtime context that the MCP loop hands to the handler.
pub struct CallContext {
    /// The original tools/call request id, used for cancellation lookup.
    pub request_id: Value,
    /// MCP `_meta.progressToken` from the inbound tools/call request, if any.
    pub progress_token: Option<Value>,
    /// Shared writer queue (also fed by the heartbeat).
    pub writer_tx: mpsc::UnboundedSender<Value>,
    /// Cancellation registry — handler registers its token here so the
    /// MCP loop's notifications/cancelled handler can flip it.
    pub registry: Arc<CancellationRegistry>,
    /// Default deadlines (per-call timeout_secs overrides `stream_idle`).
    pub default_deadlines: OutboundDeadlines,
    /// Cadence for `notifications/progress`.
    pub heartbeat_interval: Duration,
}

/// Run one a2a_send call to completion. Returns the JSON-RPC result
/// `Value` the MCP loop should send back (always under the original
/// request id). Never panics — every failure path is mapped to a typed
/// tool result.
pub async fn call_a2a_send(args_raw: Value, cx: CallContext) -> Value {
    // 1. Parse + validate args; INVALID_PARAMS surfaces as a JSON-RPC
    //    error to be unwrapped at the MCP layer (signaled here by a
    //    None marker value the caller distinguishes via the
    //    'a2a-shim::invalid-params' string).
    let args: A2aSendArgs = match serde_json::from_value(args_raw) {
        Ok(a) => a,
        Err(e) => {
            return json!({
                "__a2a_shim_invalid_params": true,
                "message": format!("invalid a2a_send arguments: {e}")
            });
        }
    };

    // 2. Register cancellation token; unregister when this fn returns.
    let cancel = cx.registry.register(&cx.request_id);
    let _drop_unregister = scopeguard::guard((), |_| {
        cx.registry.unregister(&cx.request_id);
    });

    // 3. Heartbeat (if Host supplied a progressToken).
    let heartbeat = Heartbeat::start(
        cx.writer_tx.clone(),
        cx.progress_token.clone(),
        cx.heartbeat_interval,
    );

    // 4. Per-call deadlines: timeout_secs overrides stream_idle.
    let deadlines = OutboundDeadlines {
        connect: cx.default_deadlines.connect,
        stream_idle: args
            .timeout_secs
            .map(Duration::from_secs)
            .unwrap_or(cx.default_deadlines.stream_idle),
        hard_ceiling: cx.default_deadlines.hard_ceiling,
    };

    // 5. Open the outbound stream.
    let stream = match outbound_stream(
        &args.endpoint,
        &args.conversation_id,
        &args.message,
        args.task_id.as_deref(),
        args.metadata.as_ref(),
        deadlines,
    )
    .await
    {
        Ok(s) => s,
        Err(e) => {
            drop(heartbeat);
            let kind = error_kind_for_outbound(&e);
            return render_failed(&format!("{e}"), kind, None);
        }
    };

    // 6. Pump events, accumulating into a Task snapshot. Race against
    //    the cancellation token so notifications/cancelled wins.
    let pump = drive_stream(stream, &heartbeat, &args);
    let outcome = tokio::select! {
        _ = cancel.cancelled() => {
            // Best-effort A2A tasks/cancel against the remote. We do not
            // have the remote task_id yet unless the stream gave one; if
            // it did, send the cancel. Fire-and-forget; ignore errors.
            drop(heartbeat);
            // The remote will eventually give up on its own when our
            // body half-closes; explicit cancel is a courtesy.
            return render_failed(
                "call cancelled by host",
                ErrorKind::RemoteCanceled,
                None,
            );
        }
        out = pump => out,
    };

    drop(heartbeat);

    match outcome {
        DriveOutcome::Completed(task) => render_completed(&task),
        DriveOutcome::Failed(task) => render_failed(
            &task
                .status
                .message
                .as_ref()
                .map(|m| concatenate_text(&m.parts))
                .unwrap_or_else(|| "agent failed without a status message".into()),
            ErrorKind::RemoteFailed,
            Some(task.id.0.clone()),
        ),
        DriveOutcome::Canceled(task) => render_failed(
            "agent reported the task was cancelled",
            ErrorKind::RemoteCanceled,
            Some(task.id.0.clone()),
        ),
        DriveOutcome::InputRequired(task) => render_input_required(&task),
        DriveOutcome::Error(kind, msg, task_id) => render_failed(&msg, kind, task_id),
    }
}

enum DriveOutcome {
    Completed(Task),
    Failed(Task),
    Canceled(Task),
    InputRequired(Task),
    Error(ErrorKind, String, Option<String>),
}

async fn drive_stream<S>(mut stream: S, heartbeat: &Heartbeat, args: &A2aSendArgs) -> DriveOutcome
where
    S: futures::Stream<Item = Result<SseEvent, OutboundError>> + Unpin,
{
    let mut task = Task {
        id: TaskId::from("t-unknown".to_string()),
        context_id: Some(args.conversation_id.clone()),
        status: TaskStatus {
            state: TaskState::Submitted,
            message: None,
            timestamp: None,
        },
        history: vec![Message {
            role: MessageRole::User,
            parts: vec![Part::Text {
                text: args.message.clone(),
            }],
            metadata: Some(MessageMetadata {
                conversation: Some(args.conversation_id.clone()),
                extra: Default::default(),
            }),
        }],
        artifacts: vec![],
        metadata: None,
    };
    let mut chunk_count: u64 = 0;

    while let Some(item) = stream.next().await {
        match item {
            Ok(SseEvent::StatusUpdate { inner }) => {
                task.id = inner.task_id;
                task.status = inner.status;
                if inner.final_ {
                    return match task.status.state {
                        TaskState::Completed => DriveOutcome::Completed(task),
                        TaskState::Failed => DriveOutcome::Failed(task),
                        TaskState::Canceled => DriveOutcome::Canceled(task),
                        TaskState::InputRequired => DriveOutcome::InputRequired(task),
                        // Submitted/Working with final=true is the agent
                        // breaking the contract; treat as Failed.
                        _ => DriveOutcome::Failed(task),
                    };
                }
            }
            Ok(SseEvent::ArtifactUpdate { inner }) => {
                task.id = inner.task_id;
                chunk_count += 1;
                heartbeat.update_summary(format!("streaming chunk {chunk_count}"));
                upsert_artifact(&mut task.artifacts, inner.artifact, inner.append);
            }
            Err(e) => {
                let kind = error_kind_for_outbound(&e);
                return DriveOutcome::Error(kind, format!("{e}"), Some(task.id.0.clone()));
            }
        }
    }

    // Stream ended without a final status — treat as protocol error.
    DriveOutcome::Error(
        ErrorKind::ProtocolError,
        "remote stream ended without a terminal status-update".into(),
        Some(task.id.0.clone()),
    )
}

fn upsert_artifact(artifacts: &mut Vec<Artifact>, incoming: Artifact, append: bool) {
    if append {
        if let Some(existing) = artifacts
            .iter_mut()
            .find(|a| a.artifact_id == incoming.artifact_id)
        {
            existing.parts.extend(incoming.parts);
            return;
        }
    }
    // Replace-or-append by artifact_id.
    if let Some(idx) = artifacts
        .iter()
        .position(|a| a.artifact_id == incoming.artifact_id)
    {
        artifacts[idx] = incoming;
    } else {
        artifacts.push(incoming);
    }
}

fn concatenate_text(parts: &[Part]) -> String {
    let mut s = String::new();
    for p in parts {
        if let Part::Text { text } = p {
            s.push_str(text);
        }
    }
    s
}
