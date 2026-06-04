//! Outbound A2A streaming over HTTP/SSE (spec § 3.6).
//!
//! `stream(endpoint, conversation_id, message, task_id, metadata, deadlines)`
//! POSTs a JSON-RPC `message/stream` request, opens the SSE response body,
//! and returns a typed stream of `Result<SseEvent, OutboundError>`. An
//! IdleGuard wraps every poll: if `stream_idle` elapses without a new
//! frame, the next poll yields `RemoteTimeout` and ends. A HardCeiling
//! does the same for absolute wall-clock budget.

use a2a_shim_core::constants::CONVERSATION_METADATA_KEY;
use a2a_shim_core::timeout::ceiling::HardCeiling;
use a2a_shim_core::timeout::idle::IdleGuard;
use a2a_shim_core::wire::sse::SseEvent;
use eventsource_stream::Eventsource;
use futures::stream::{BoxStream, StreamExt};
use serde_json::{json, Value};
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Clone, Copy)]
pub struct OutboundDeadlines {
    pub connect: Duration,
    pub stream_idle: Duration,
    pub hard_ceiling: Duration,
}

#[derive(Debug, Error)]
pub enum OutboundError {
    #[error("network error: {0}")]
    NetworkError(String),
    #[error("remote returned HTTP {status}: {body}")]
    RemoteFailed { status: u16, body: String },
    #[error("remote did not send a frame within the idle window")]
    RemoteTimeout { elapsed: Duration },
    #[error("hard ceiling exceeded ({limit:?})")]
    HardCeilingExceeded { limit: Duration },
    #[error("protocol error: {0}")]
    ProtocolError(String),
}

/// Open the SSE stream against `endpoint`. The returned stream yields
/// `Result<SseEvent, OutboundError>` and ends after the terminal
/// `status-update final=true`, on error, or on timeout.
// REASON: 8 args matches the A2A SendMessage param surface; refactoring
// to a builder pattern adds noise without clarity.
#[allow(clippy::too_many_arguments)]
pub async fn stream(
    endpoint: &str,
    conversation_id: &str,
    message: &str,
    task_id: Option<&str>,
    metadata: Option<&Value>,
    caller_id: Option<&str>,
    conversation_mode: Option<&str>,
    deadlines: OutboundDeadlines,
) -> Result<BoxStream<'static, Result<SseEvent, OutboundError>>, OutboundError> {
    let client = reqwest::Client::builder()
        .connect_timeout(deadlines.connect)
        .build()
        .map_err(|e| OutboundError::NetworkError(e.to_string()))?;

    let body = build_request_body(
        conversation_id,
        message,
        task_id,
        metadata,
        caller_id,
        conversation_mode,
    );

    let resp = client
        .post(endpoint)
        .header("accept", "text/event-stream")
        .json(&body)
        .send()
        .await
        .map_err(|e| OutboundError::NetworkError(e.to_string()))?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(OutboundError::RemoteFailed {
            status: status.as_u16(),
            body,
        });
    }

    // Wrap the byte stream with eventsource-stream, then map each SSE
    // frame's `data:` payload into a typed SseEvent.
    let byte_stream = resp.bytes_stream().eventsource();

    let idle_window = deadlines.stream_idle;
    let ceiling = HardCeiling::new(deadlines.hard_ceiling);
    let typed = async_stream::stream! {
        let mut byte_stream = Box::pin(byte_stream);
        let mut idle = IdleGuard::new(idle_window);
        loop {
            // Race the next event against the idle window.
            let timeout = tokio::time::sleep(idle_window);
            tokio::pin!(timeout);
            tokio::select! {
                _ = &mut timeout => {
                    yield Err(OutboundError::RemoteTimeout { elapsed: idle_window });
                    break;
                }
                next = byte_stream.next() => {
                    match next {
                        None => break,
                        Some(Err(e)) => {
                            yield Err(OutboundError::NetworkError(e.to_string()));
                            break;
                        }
                        Some(Ok(ev)) => {
                            idle.reset();
                            if ceiling.exceeded() {
                                yield Err(OutboundError::HardCeilingExceeded {
                                    limit: deadlines.hard_ceiling,
                                });
                                break;
                            }
                            match serde_json::from_str::<SseEvent>(&ev.data) {
                                Ok(typed) => {
                                    let is_final = matches!(
                                        &typed,
                                        SseEvent::StatusUpdate { inner } if inner.final_
                                    );
                                    yield Ok(typed);
                                    if is_final { break; }
                                }
                                Err(e) => {
                                    yield Err(OutboundError::ProtocolError(format!(
                                        "non-SseEvent data line: {e} (raw: {})",
                                        ev.data
                                    )));
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        }
    };
    Ok(typed.boxed())
}

fn build_request_body(
    conversation_id: &str,
    message: &str,
    task_id: Option<&str>,
    metadata: Option<&Value>,
    caller_id: Option<&str>,
    conversation_mode: Option<&str>,
) -> Value {
    let mut md = serde_json::Map::new();
    md.insert(
        CONVERSATION_METADATA_KEY.to_string(),
        Value::String(conversation_id.to_string()),
    );
    if let Some(c) = caller_id {
        md.insert(
            "x-a2a-shim/caller_id".to_string(),
            Value::String(c.to_string()),
        );
    }
    if let Some(Value::Object(m)) = metadata {
        for (k, v) in m {
            // Caller-supplied metadata never overrides our conversation
            // or caller key — otherwise the Host could break routing.
            if k != CONVERSATION_METADATA_KEY && k != "x-a2a-shim/caller_id" {
                md.insert(k.clone(), v.clone());
            }
        }
    }

    let mut params = serde_json::Map::new();
    if let Some(id) = task_id {
        params.insert("id".into(), Value::String(id.to_string()));
    }
    if let Some(mode) = conversation_mode {
        params.insert(
            "_shim_conversation_mode".into(),
            Value::String(mode.to_string()),
        );
    }
    params.insert(
        "message".into(),
        json!({
            "role": "user",
            "parts": [{ "text": message }],
            "metadata": md,
        }),
    );

    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "SendStreamingMessage",
        "params": Value::Object(params),
    })
}
