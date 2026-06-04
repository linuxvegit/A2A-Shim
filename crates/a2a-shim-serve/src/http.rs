//! HTTP surface for the Serve Shim (spec § 2.6, § 4).
//!
//! Layout:
//!   * `ServeState` carries the shared config + AcpClient + ConversationMap +
//!     TaskRegistry the handlers need. Cloneable, Arc-internal.
//!   * `router()` mounts `GET /.well-known/agent.json` and `POST /`
//!     (JSON-RPC root). SSE branch lives inside the dispatch (Task 26).
//!
//! Dispatch table for `POST /`:
//!   * message/send  -> create-or-continue a Task, run bridge synchronously,
//!                      return the final Task snapshot. Errors map to spec § 4.6.
//!   * message/stream -> Task 26.
//!   * tasks/get     -> snapshot from TaskRegistry, or TASK_NOT_FOUND.
//!   * tasks/cancel  -> registry.cancel + best-effort AcpClient::session_cancel.
//!   * anything else -> METHOD_NOT_FOUND.

use a2a_shim_core::config::serve_toml::ServeConfig;
use a2a_shim_core::error::codes;
use a2a_shim_core::wire::envelope::{
    JsonRpcError, JsonRpcRequest, JsonRpcResponse, ResultOrError,
};
use a2a_shim_core::wire::methods::{SendMessageParams, TaskIdParams};
use a2a_shim_core::wire::task::TaskId;
use agent_client_protocol::schema::SessionId;
use axum::{
    extract::State,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde_json::Value;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use crate::acp_client::AcpClient;
use crate::agent_card::build_agent_card;
use crate::bridge;
use crate::conversation::{AcquireError, ConversationMap, NewError};
use crate::task_registry::{TaskRegistry, TransitionError};

/// Cloneable handle to all shared state the HTTP handlers need.
///
/// Constructors:
///   * `new` — config only (tests for the AgentCard endpoint).
///   * `with_client` — config + a live AcpClient, used by Task 27 wiring
///     and any integration test that goes through the JSON-RPC root.
#[derive(Clone)]
pub struct ServeState {
    pub cfg: Arc<ServeConfig>,
    pub bound: Arc<OnceLock<SocketAddr>>,
    pub acp: Option<Arc<AcpClient>>,
    pub conversations: ConversationMap,
    pub tasks: TaskRegistry,
}

impl ServeState {
    pub fn new(cfg: Arc<ServeConfig>) -> Self {
        let conv = ConversationMap::new(
            cfg.server.conversations.max_active,
            Duration::from_secs(cfg.server.conversations.idle_secs),
        );
        Self {
            cfg,
            bound: Arc::new(OnceLock::new()),
            acp: None,
            conversations: conv,
            tasks: TaskRegistry::new(),
        }
    }

    pub fn new_for_test(cfg: Arc<ServeConfig>) -> Self {
        Self::new(cfg)
    }

    pub fn with_client(cfg: Arc<ServeConfig>, client: AcpClient) -> Self {
        let mut s = Self::new(cfg);
        s.acp = Some(Arc::new(client));
        s
    }

    pub fn set_bound(&self, addr: SocketAddr) {
        let _ = self.bound.set(addr);
    }

    fn bound_str(&self) -> String {
        self.bound
            .get()
            .map(|a| a.to_string())
            .unwrap_or_else(|| self.cfg.server.listen.clone())
    }

    fn require_acp(&self) -> Result<Arc<AcpClient>, JsonRpcError> {
        self.acp.clone().ok_or_else(|| JsonRpcError {
            code: codes::INTERNAL_ERROR,
            message: "ACP client not configured".into(),
            data: None,
        })
    }
}

pub fn router(state: ServeState) -> Router {
    let card_path = state.cfg.server.agent_card_path.clone();
    Router::new()
        .route(&card_path, get(agent_card_handler))
        .route("/", post(jsonrpc_root))
        .with_state(state)
}

async fn agent_card_handler(State(state): State<ServeState>) -> impl IntoResponse {
    let bound = state.bound_str();
    let card = build_agent_card(&state.cfg, &bound);
    (
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        Json(card),
    )
}

async fn jsonrpc_root(
    State(state): State<ServeState>,
    Json(req): Json<JsonRpcRequest<Value>>,
) -> impl IntoResponse {
    let id = req.id.clone();
    let result = dispatch(state, &req).await;
    let resp = match result {
        Ok(v) => JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id,
            result_or_error: ResultOrError::from_result(v),
        },
        Err(e) => JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id,
            result_or_error: ResultOrError::from_error(e),
        },
    };
    Json(resp)
}

async fn dispatch(
    state: ServeState,
    req: &JsonRpcRequest<Value>,
) -> Result<Value, JsonRpcError> {
    match req.method.as_str() {
        "message/send" => handle_message_send(state, req.params.clone()).await,
        "tasks/get" => handle_tasks_get(state, req.params.clone()).await,
        "tasks/cancel" => handle_tasks_cancel(state, req.params.clone()).await,
        // message/stream lands in Task 26.
        other => Err(JsonRpcError {
            code: codes::METHOD_NOT_FOUND,
            message: format!("method not found: {other}"),
            data: None,
        }),
    }
}

async fn handle_message_send(
    state: ServeState,
    params: Value,
) -> Result<Value, JsonRpcError> {
    let parsed: SendMessageParams = serde_json::from_value(params).map_err(invalid_params)?;
    let conv_id = parsed
        .message
        .metadata
        .as_ref()
        .and_then(|m| m.conversation.clone())
        .ok_or_else(|| JsonRpcError {
            code: codes::INVALID_PARAMS,
            message: format!(
                "missing required metadata key '{}' (spec § 2.6)",
                a2a_shim_core::constants::CONVERSATION_METADATA_KEY
            ),
            data: None,
        })?;

    let acp = state.require_acp()?;
    let cwd = state.cfg.agent.cwd.clone();

    // Get-or-create the conversation. The closure runs at most once per
    // conversation id; ConversationMap serializes new-session creation.
    let (conv, _created) = state
        .conversations
        .get_or_create(&conv_id, || async {
            let acp = Arc::clone(&acp);
            let cwd: PathBuf = cwd;
            // Map the AcpError into a String so NewError::Spawn carries
            // something serde-friendly without leaking AcpError shape.
            acp.session_new(cwd)
                .await
                .map(|sid| sid.0.as_ref().to_string())
                .map_err(|e| e.to_string())
        })
        .await
        .map_err(map_new_error)?;

    // Acquire the H1 in-flight guard before we even create a Task so
    // overlap is reported as CONVERSATION_BUSY, not as a state-machine
    // failure halfway through.
    let _permit = state
        .conversations
        .acquire_in_flight(&conv_id)
        .await
        .map_err(map_acquire_error)?;

    // Continuation path: caller supplied a task_id. Only legal from
    // input-required (spec § 2.5).
    let task_id = if let Some(existing_id) = parsed.id.clone() {
        state
            .tasks
            .accept_continuation(&existing_id)
            .await
            .map_err(map_transition_error)?;
        existing_id
    } else {
        state.tasks.create(&conv_id, &conv.acp_session_id).await
    };

    // Extract the user prompt text. Concatenate all text parts so multi-
    // part prompts work; non-text parts ignored in MVP.
    let prompt_text = parsed
        .message
        .parts
        .iter()
        .filter_map(|p| match p {
            a2a_shim_core::wire::message::Part::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");

    // Start the prompt stream and pump through the bridge synchronously.
    let session_id = SessionId::from(conv.acp_session_id.clone());
    let stream = acp
        .session_prompt(&session_id, &prompt_text)
        .await
        .map_err(|e| JsonRpcError {
            code: codes::INTERNAL_ERROR,
            message: format!("session/prompt failed: {e}"),
            data: None,
        })?;
    bridge::run_session(task_id.clone(), state.tasks.clone(), stream)
        .await
        .map_err(|e| JsonRpcError {
            code: codes::INTERNAL_ERROR,
            message: format!("bridge: {e}"),
            data: None,
        })?;

    let snap = state
        .tasks
        .snapshot(&task_id)
        .await
        .ok_or_else(|| JsonRpcError {
            code: codes::INTERNAL_ERROR,
            message: "task vanished mid-prompt".into(),
            data: None,
        })?;
    Ok(serde_json::to_value(snap).expect("Task serializes"))
}

async fn handle_tasks_get(
    state: ServeState,
    params: Value,
) -> Result<Value, JsonRpcError> {
    let parsed: TaskIdParams = serde_json::from_value(params).map_err(invalid_params)?;
    let snap = state.tasks.snapshot(&parsed.id).await.ok_or_else(|| {
        JsonRpcError {
            code: codes::TASK_NOT_FOUND,
            message: format!("task not found: {}", parsed.id),
            data: None,
        }
    })?;
    Ok(serde_json::to_value(snap).expect("Task serializes"))
}

async fn handle_tasks_cancel(
    state: ServeState,
    params: Value,
) -> Result<Value, JsonRpcError> {
    let parsed: TaskIdParams = serde_json::from_value(params).map_err(invalid_params)?;
    state
        .tasks
        .cancel(&parsed.id)
        .await
        .map_err(map_transition_error)?;

    // Best-effort: also tell the underlying ACP session to stop. Failures
    // here are logged but do not bubble up because the A2A-side Task is
    // already Canceled in our registry.
    if let Some(acp) = state.acp.as_ref() {
        if let Some(snap) = state.tasks.snapshot(&parsed.id).await {
            if let Some(conv_id) = snap.context_id {
                if let Some(conv) = state.conversations.get(&conv_id).await {
                    let sid = SessionId::from(conv.acp_session_id.clone());
                    if let Err(e) = acp.session_cancel(&sid).await {
                        tracing::warn!(
                            error = %e,
                            task = %parsed.id,
                            "best-effort ACP session_cancel failed"
                        );
                    }
                }
            }
        }
    }

    let snap = state
        .tasks
        .snapshot(&parsed.id)
        .await
        .ok_or_else(|| JsonRpcError {
            code: codes::INTERNAL_ERROR,
            message: "task vanished mid-cancel".into(),
            data: None,
        })?;
    Ok(serde_json::to_value(snap).expect("Task serializes"))
}

fn invalid_params(e: serde_json::Error) -> JsonRpcError {
    JsonRpcError {
        code: codes::INVALID_PARAMS,
        message: format!("invalid params: {e}"),
        data: None,
    }
}

fn map_new_error(e: NewError<String>) -> JsonRpcError {
    match e {
        NewError::LimitReached => JsonRpcError {
            code: codes::CONVERSATION_LIMIT_REACHED,
            message: "max active conversations reached".into(),
            data: None,
        },
        NewError::Spawn(msg) => JsonRpcError {
            code: codes::INTERNAL_ERROR,
            message: format!("session/new failed: {msg}"),
            data: None,
        },
    }
}

fn map_acquire_error(e: AcquireError) -> JsonRpcError {
    match e {
        AcquireError::Busy => JsonRpcError {
            code: codes::CONVERSATION_BUSY,
            message: "conversation already has a prompt in flight".into(),
            data: None,
        },
        AcquireError::NotFound => JsonRpcError {
            code: codes::INTERNAL_ERROR,
            message: "conversation vanished between create and acquire".into(),
            data: None,
        },
    }
}

fn map_transition_error(e: TransitionError) -> JsonRpcError {
    match e {
        TransitionError::NotFound => JsonRpcError {
            code: codes::TASK_NOT_FOUND,
            message: "task not found".into(),
            data: None,
        },
        TransitionError::NotCancelable => JsonRpcError {
            code: codes::TASK_NOT_CANCELABLE,
            message: "task is already in a terminal state".into(),
            data: None,
        },
        TransitionError::InvalidContinuation => JsonRpcError {
            code: codes::INVALID_PARAMS,
            message: "continuation only valid from input-required".into(),
            data: None,
        },
        TransitionError::Illegal => JsonRpcError {
            code: codes::INTERNAL_ERROR,
            message: "illegal task transition".into(),
            data: None,
        },
    }
}

// Used by Task 26.
#[allow(dead_code)]
fn _task_id_helper(id: &str) -> TaskId {
    TaskId::from(id.to_string())
}
