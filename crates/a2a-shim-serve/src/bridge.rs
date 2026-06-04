//! Bridge: turn an `AcpClient::session_prompt` event stream into A2A
//! `TaskRegistry` updates + per-Task SSE frames (spec § 2.5, § 2.11, § 4.5).
//!
//! Translation table:
//! | Bridge event                                    | Registry transition  | SSE emitted                       |
//! |-------------------------------------------------|----------------------|-----------------------------------|
//! | first event of any kind                         | Submitted → Working  | status-update(Working, final=false)|
//! | Update(AgentMessageChunk { text })              | (no transition)      | artifact-update(a-answer, append) |
//! | Update(other)                                   | (no transition)      | (ignored in MVP)                  |
//! | Terminal(EndTurn)                               | → Completed          | status-update(Completed, final=true)|
//! | Terminal(Cancelled)                             | → Canceled           | status-update(Canceled, final=true) |
//! | Terminal(Refusal \| MaxTokens \| MaxTurnRequests)| → Failed             | status-update(Failed, final=true)   |
//! | Err(AcpError) at any point                      | → Failed             | status-update(Failed, final=true)   |
//! | stream ends without Terminal                    | → Failed             | status-update(Failed, final=true)   |
//!
//! Forward-compat (R6): the AcpClient swallows unknown SessionUpdate
//! variants at the SDK layer, so they never reach us. The "stream ends
//! without Terminal" path is also where we'd land if the agent dies mid-
//! turn — both produce a clean Failed Task with a descriptive message.

use a2a_shim_core::wire::message::Part;
use a2a_shim_core::wire::sse::SseEvent;
use a2a_shim_core::wire::task::{Artifact, TaskId, TaskState, TaskStatus};
use agent_client_protocol::schema::{ContentBlock, SessionUpdate, StopReason};
use futures::StreamExt;
use thiserror::Error;

use crate::acp_client::{AcpError, BridgeEvent};
use crate::sse_sink::SseSink;
use crate::task_registry::{TaskRegistry, TransitionError};

/// Canonical artifact id under which the bridge accumulates the agent's
/// streamed answer text. Stable identifier so SSE clients can correlate.
pub const ANSWER_ARTIFACT_ID: &str = "a-answer";
pub const ANSWER_ARTIFACT_NAME: &str = "answer";

#[derive(Debug, Error)]
pub enum BridgeError {
    #[error("task not in registry")]
    NotFound,
    #[error("registry transition: {0}")]
    Transition(#[from] TransitionError),
}

/// Drive the per-Task pump. Returns once the input stream is exhausted
/// (or yields a Terminal / Err event) and the matching terminal SSE has
/// been published. Always leaves the Task in a terminal state and the
/// SseSink closed.
pub async fn run_session<S>(
    task_id: TaskId,
    registry: TaskRegistry,
    mut stream: S,
) -> Result<(), BridgeError>
where
    S: futures::Stream<Item = Result<BridgeEvent, AcpError>> + Unpin,
{
    let sink = registry.sink(&task_id).await.ok_or(BridgeError::NotFound)?;

    let mut first_seen = false;
    let mut artifact_seen = false;
    let mut saw_terminal = false;

    while let Some(item) = stream.next().await {
        if !first_seen {
            first_seen = true;
            registry.transition(&task_id, TaskState::Working).await?;
            sink.publish_event(working_event(&task_id));
        }

        match item {
            Ok(BridgeEvent::Update(boxed_update)) => match *boxed_update {
                SessionUpdate::AgentMessageChunk(chunk) => {
                    if let ContentBlock::Text(t) = chunk.content {
                        let part = Part::Text { text: t.text };
                        let append = artifact_seen;
                        artifact_seen = true;
                        // Persist into registry history (merging by artifact_id).
                        let artifact = Artifact {
                            artifact_id: Some(ANSWER_ARTIFACT_ID.into()),
                            name: Some(ANSWER_ARTIFACT_NAME.into()),
                            parts: vec![part.clone()],
                            metadata: None,
                        };
                        let _ = registry.upsert_artifact(&task_id, artifact).await?;
                        // Publish just the delta as an artifact-update with append.
                        sink.publish_event(SseEvent::artifact(
                            task_id.clone(),
                            Artifact {
                                artifact_id: Some(ANSWER_ARTIFACT_ID.into()),
                                name: Some(ANSWER_ARTIFACT_NAME.into()),
                                parts: vec![part],
                                metadata: None,
                            },
                            append,
                        ));
                    }
                }
                _ => {
                    // Tool calls, thoughts, plan updates, etc. — out of MVP
                    // scope. Tracing-only so test output stays readable.
                    tracing::debug!("ignoring non-text session update");
                }
            },
            Ok(BridgeEvent::Terminal(reason)) => {
                saw_terminal = true;
                publish_terminal_for_stop(&task_id, &registry, &sink, reason).await?;
                break;
            }
            Err(e) => {
                saw_terminal = true;
                publish_failed(&task_id, &registry, &sink, format!("agent error: {e}")).await?;
                break;
            }
        }
    }

    if !saw_terminal {
        // Stream ended without a Terminal or Err. Treat as Failed; this
        // catches agent-process-died-mid-turn and schema-skew-killed-
        // stream cases consistently.
        publish_failed(
            &task_id,
            &registry,
            &sink,
            "agent stream ended without terminal stop reason".to_string(),
        )
        .await?;
    }
    Ok(())
}

fn working_event(task_id: &TaskId) -> SseEvent {
    SseEvent::status(
        task_id.clone(),
        TaskStatus {
            state: TaskState::Working,
            message: None,
            timestamp: None,
        },
        false,
    )
}

async fn publish_terminal_for_stop(
    task_id: &TaskId,
    registry: &TaskRegistry,
    sink: &SseSink,
    stop: StopReason,
) -> Result<(), BridgeError> {
    let new_state = match stop {
        StopReason::EndTurn => TaskState::Completed,
        StopReason::Cancelled => TaskState::Canceled,
        // MaxTokens / Refusal / MaxTurnRequests all collapse to Failed
        // with a human-readable status message embedded below.
        _ => TaskState::Failed,
    };
    registry.transition(task_id, new_state).await?;
    let status = TaskStatus {
        state: new_state,
        message: status_message_for_stop(stop),
        timestamp: None,
    };
    sink.publish_final(SseEvent::status(task_id.clone(), status, true));
    Ok(())
}

async fn publish_failed(
    task_id: &TaskId,
    registry: &TaskRegistry,
    sink: &SseSink,
    reason: String,
) -> Result<(), BridgeError> {
    registry.transition(task_id, TaskState::Failed).await?;
    sink.publish_final(SseEvent::status(
        task_id.clone(),
        TaskStatus {
            state: TaskState::Failed,
            message: Some(text_message(reason)),
            timestamp: None,
        },
        true,
    ));
    Ok(())
}

fn status_message_for_stop(stop: StopReason) -> Option<a2a_shim_core::wire::message::Message> {
    match stop {
        StopReason::EndTurn | StopReason::Cancelled => None,
        StopReason::Refusal => Some(text_message("agent refused to complete the turn".into())),
        StopReason::MaxTokens => Some(text_message("agent stopped: max tokens".into())),
        StopReason::MaxTurnRequests => {
            Some(text_message("agent stopped: max turn requests".into()))
        }
        // SDK is #[non_exhaustive]-friendly: future variants funnel through
        // Failed with a generic reason.
        _ => Some(text_message(
            "agent stopped for an unrecognized reason".into(),
        )),
    }
}

fn text_message(text: String) -> a2a_shim_core::wire::message::Message {
    use a2a_shim_core::wire::message::{Message, MessageRole};
    Message {
        role: MessageRole::Agent,
        parts: vec![Part::Text { text }],
        metadata: None,
    }
}
