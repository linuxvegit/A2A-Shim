//! Per-Serve-Shim task registry (spec § 2.5 + § 2.6).
//!
//! Tracks every live A2A `Task`: its conversation binding, ACP session id,
//! accumulated history + artifacts, current state, and the SSE sink that
//! `message/stream` subscribers consume.
//!
//! The state machine is enforced in one place — `transition()` — so all
//! callers (bridge.rs, http.rs, cancel handler) share a single source of
//! truth for legal transitions:
//!
//! ```text
//!   Submitted ──► Working ──► Completed / Failed / Canceled (terminal)
//!                    │
//!                    └──► InputRequired ──► Working   (continuation)
//!                                        └► Canceled  (cancel allowed)
//! ```
//!
//! `cancel()` is a separate method (rather than `transition(_, Canceled)`)
//! because it has different error semantics: terminal tasks return
//! `NotCancelable` (mapping to JSON-RPC -32002), while reaching a regular
//! `Illegal` state from `transition` would map to `INTERNAL_ERROR`.

use a2a_shim_core::wire::message::Message;
use a2a_shim_core::wire::task::{Artifact, Task, TaskId, TaskState, TaskStatus};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;

use crate::sse_sink::SseSink;

/// Default capacity for each Task's SSE broadcast channel. Bigger than
/// the worst-case prompt-update burst so subscribers rarely lag-drop.
const SSE_CAPACITY: usize = 256;

#[derive(Debug, Error)]
pub enum TransitionError {
    #[error("task not found")]
    NotFound,
    #[error("task not cancelable: already in terminal state")]
    NotCancelable,
    #[error("illegal transition")]
    Illegal,
    #[error("continuation only valid from input-required")]
    InvalidContinuation,
}

/// Runtime binding for one A2A Task. Lives inside the registry; callers
/// only ever see snapshots (immutable `Task` clones) or the per-Task
/// `SseSink` handle.
struct TaskBinding {
    conversation_id: String,
    acp_session_id: String,
    state: TaskState,
    history: Vec<Message>,
    artifacts: Vec<Artifact>,
    sink: SseSink,
}

impl TaskBinding {
    fn snapshot(&self, id: &TaskId) -> Task {
        Task {
            id: id.clone(),
            context_id: Some(self.conversation_id.clone()),
            status: TaskStatus {
                state: self.state,
                message: None,
                timestamp: None,
            },
            history: self.history.clone(),
            artifacts: self.artifacts.clone(),
            metadata: None,
        }
    }
}

#[derive(Clone, Default)]
pub struct TaskRegistry {
    inner: Arc<Mutex<RegistryInner>>,
    persistence: Option<crate::persistence::Persistence>,
}

#[derive(Default)]
struct RegistryInner {
    bindings: HashMap<TaskId, TaskBinding>,
    /// Insertion order. Used by `list` to give a stable cursor.
    order: Vec<TaskId>,
}

impl TaskRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct with an optional Persistence handle. When Some, every
    /// task create / transition / cancel is written through to SQLite
    /// (ADR 0007).
    pub fn with_persistence(persistence: Option<crate::persistence::Persistence>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(RegistryInner::default())),
            persistence,
        }
    }

    /// Create a fresh Task in `Submitted` and return its newly minted id.
    pub async fn create(&self, conversation_id: &str, acp_session_id: &str) -> TaskId {
        let id = TaskId::new_random();
        let sink = SseSink::new(SSE_CAPACITY);
        let binding = TaskBinding {
            conversation_id: conversation_id.to_string(),
            acp_session_id: acp_session_id.to_string(),
            state: TaskState::Submitted,
            history: Vec::new(),
            artifacts: Vec::new(),
            sink,
        };
        {
            let mut inner = self.inner.lock();
            inner.bindings.insert(id.clone(), binding);
            inner.order.push(id.clone());
        }
        if let Some(p) = self.persistence.as_ref() {
            if let Err(e) = p
                .record_task(id.as_str(), conversation_id, "submitted")
                .await
            {
                tracing::warn!(task = %id, error = %e, "persistence record_task(create) failed");
            }
        }
        id
    }

    /// Move a Task into a new state, enforcing the spec § 2.5 transition
    /// graph. Use `cancel()` instead of `transition(_, Canceled)` for
    /// user-initiated cancels so the error mapping comes out right.
    pub async fn transition(&self, id: &TaskId, to: TaskState) -> Result<(), TransitionError> {
        let conv_id = {
            let mut map = self.inner.lock();
            let binding = map.bindings.get_mut(id).ok_or(TransitionError::NotFound)?;
            if !is_legal(binding.state, to) {
                return Err(TransitionError::Illegal);
            }
            binding.state = to;
            binding.conversation_id.clone()
        };
        if let Some(p) = self.persistence.as_ref() {
            if let Err(e) = p.record_task(id.as_str(), &conv_id, state_str(to)).await {
                tracing::warn!(task = %id, error = %e, "persistence record_task(transition) failed");
            }
        }
        Ok(())
    }

    /// Continuation: a client `message/send` arrives carrying an existing
    /// Task id. Only legal when the Task is in `InputRequired`; on success
    /// the Task moves back to `Working`.
    pub async fn accept_continuation(&self, id: &TaskId) -> Result<(), TransitionError> {
        let mut map = self.inner.lock();
        let binding = map.bindings.get_mut(id).ok_or(TransitionError::NotFound)?;
        if binding.state != TaskState::InputRequired {
            return Err(TransitionError::InvalidContinuation);
        }
        binding.state = TaskState::Working;
        Ok(())
    }

    /// User-initiated cancel. Rejects with `NotCancelable` if the Task is
    /// already terminal (spec maps to JSON-RPC -32002 TASK_NOT_CANCELABLE).
    pub async fn cancel(&self, id: &TaskId) -> Result<(), TransitionError> {
        let conv_id = {
            let mut map = self.inner.lock();
            let binding = map.bindings.get_mut(id).ok_or(TransitionError::NotFound)?;
            if binding.state.is_terminal() {
                return Err(TransitionError::NotCancelable);
            }
            binding.state = TaskState::Canceled;
            binding.conversation_id.clone()
        };
        if let Some(p) = self.persistence.as_ref() {
            if let Err(e) = p.record_task(id.as_str(), &conv_id, "canceled").await {
                tracing::warn!(task = %id, error = %e, "persistence record_task(cancel) failed");
            }
        }
        Ok(())
    }

    pub async fn snapshot(&self, id: &TaskId) -> Option<Task> {
        self.inner.lock().bindings.get(id).map(|b| b.snapshot(id))
    }

    pub async fn sink(&self, id: &TaskId) -> Option<SseSink> {
        self.inner.lock().bindings.get(id).map(|b| b.sink.clone())
    }

    /// Accessor used by the bridge to record the ACP session_id when
    /// turning incoming notifications into history/artifact updates.
    /// Returns None if the task is gone (e.g. swept).
    pub async fn acp_session_id(&self, id: &TaskId) -> Option<String> {
        self.inner
            .lock()
            .bindings
            .get(id)
            .map(|b| b.acp_session_id.clone())
    }

    /// Append a Message to the task's history. Used by the bridge to
    /// record incoming user prompts and outgoing agent answers so
    /// `tasks/get` returns a coherent transcript.
    pub async fn push_history(&self, id: &TaskId, m: Message) -> Result<(), TransitionError> {
        let mut map = self.inner.lock();
        let binding = map.bindings.get_mut(id).ok_or(TransitionError::NotFound)?;
        binding.history.push(m);
        Ok(())
    }

    /// Replace-or-append an Artifact keyed by `artifactId`. Returns the
    /// final, merged Artifact so the bridge can re-emit it on the wire.
    pub async fn upsert_artifact(
        &self,
        id: &TaskId,
        artifact: Artifact,
    ) -> Result<Artifact, TransitionError> {
        let mut map = self.inner.lock();
        let binding = map.bindings.get_mut(id).ok_or(TransitionError::NotFound)?;
        if let Some(existing_idx) = artifact.artifact_id.as_ref().and_then(|aid| {
            binding
                .artifacts
                .iter()
                .position(|a| a.artifact_id.as_deref() == Some(aid))
        }) {
            // Merge text parts by appending the new parts to existing.
            binding.artifacts[existing_idx]
                .parts
                .extend(artifact.parts.clone());
            Ok(binding.artifacts[existing_idx].clone())
        } else {
            binding.artifacts.push(artifact.clone());
            Ok(artifact)
        }
    }

    /// List Task snapshots in insertion order. `after` is the cursor:
    /// returned snapshots are those whose insertion index is strictly
    /// greater than the position of `after` (or all of them if `after`
    /// is None). `limit` is capped at 1000 to bound response size; pass
    /// 0 to use the default of 50 (spec A2A v1.0 § 9.4.4 leaves the
    /// default to implementations).
    ///
    /// Returns `(tasks, next_cursor)` where `next_cursor` is `Some(id)`
    /// of the last returned task if there are more, else `None`.
    pub async fn list(&self, after: Option<&TaskId>, limit: usize) -> (Vec<Task>, Option<TaskId>) {
        let effective_limit = if limit == 0 { 50 } else { limit.min(1000) };
        let inner = self.inner.lock();
        let start_idx = match after {
            None => 0,
            Some(cursor) => {
                match inner.order.iter().position(|id| id == cursor) {
                    Some(pos) => pos + 1,
                    None => return (Vec::new(), None), // unknown cursor -> empty page
                }
            }
        };
        let end_idx = (start_idx + effective_limit).min(inner.order.len());
        let mut tasks = Vec::with_capacity(end_idx - start_idx);
        for id in &inner.order[start_idx..end_idx] {
            if let Some(b) = inner.bindings.get(id) {
                tasks.push(b.snapshot(id));
            }
        }
        let next_cursor = if end_idx < inner.order.len() {
            tasks.last().map(|t| t.id.clone())
        } else {
            None
        };
        (tasks, next_cursor)
    }
}

/// Spec § 2.5 transition rules. `transition()` only handles the
/// non-cancel arrows; cancel has its own method.
fn is_legal(from: TaskState, to: TaskState) -> bool {
    use TaskState::*;
    match (from, to) {
        // Forward flow
        (Submitted, Working) => true,
        (Working, Completed) | (Working, Failed) | (Working, InputRequired) => true,
        // Continuations are handled by accept_continuation, NOT this fn.
        // Direct Canceled transition from any non-terminal is allowed so
        // the bridge can synthesize a Canceled state when the agent
        // reports cancellation; user-initiated cancel goes through
        // cancel() for the right error mapping.
        (Submitted | Working | InputRequired, Canceled) => true,
        _ => false,
    }
}

/// Wire-format string for a TaskState (matches the kebab-case serde
/// representation in a2a_shim_core::wire::task::TaskState).
fn state_str(s: TaskState) -> &'static str {
    use TaskState::*;
    match s {
        Submitted => "submitted",
        Working => "working",
        InputRequired => "input-required",
        Completed => "completed",
        Failed => "failed",
        Canceled => "canceled",
    }
}
