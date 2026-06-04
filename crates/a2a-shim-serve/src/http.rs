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
//!     return the final Task snapshot. Errors map to spec § 4.6.
//!   * message/stream -> Task 26.
//!   * tasks/get     -> snapshot from TaskRegistry, or TASK_NOT_FOUND.
//!   * tasks/cancel  -> registry.cancel + best-effort AcpClient::session_cancel.
//!   * anything else -> METHOD_NOT_FOUND.

use a2a_shim_core::config::serve_toml::ServeConfig;
use a2a_shim_core::error::codes;
use a2a_shim_core::wire::envelope::{JsonRpcError, JsonRpcRequest, JsonRpcResponse, ResultOrError};
use a2a_shim_core::wire::methods::{ListTasksParams, SendMessageParams, TaskIdParams};
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
) -> axum::response::Response {
    // message/stream needs to return an SSE body, not JSON. All other
    // methods route through the JSON dispatch + envelope path.
    if req.method == "SendStreamingMessage" || req.method == "SubscribeToTask" {
        return handle_message_stream(state, req).await;
    }
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
    Json(resp).into_response()
}

async fn dispatch(state: ServeState, req: &JsonRpcRequest<Value>) -> Result<Value, JsonRpcError> {
    match req.method.as_str() {
        "SendMessage" => handle_message_send(state, req.params.clone()).await,
        "GetTask" => handle_tasks_get(state, req.params.clone()).await,
        "CancelTask" => handle_tasks_cancel(state, req.params.clone()).await,
        "ListTasks" => handle_list_tasks(state, req.params.clone()).await,
        // SendStreamingMessage / SubscribeToTask handled above as SSE.
        // push-notif methods in Task 32; _shim/conversation/reset in Task 39.
        // message/stream is special-cased above.
        other => Err(JsonRpcError {
            code: codes::METHOD_NOT_FOUND,
            message: format!("method not found: {other}"),
            data: None,
        }),
    }
}

async fn handle_message_send(state: ServeState, params: Value) -> Result<Value, JsonRpcError> {
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

    // Translate inbound A2A Parts -> ACP ContentBlocks per ADR 0006.
    // PartCaps defaults to all-off in v1.1; the cap-cache from agent's
    // initialize response wires through in v1.2. Today: Text + ResourceLink
    // always pass; Image/Audio/EmbeddedResource/Data require caps=on
    // (none today) and drop with a tracing warn.
    let caps = crate::translate::PartCaps::default();
    let content = crate::translate::a2a_to_acp(&parsed.message.parts, &caps);

    // Start the prompt stream and pump through the bridge synchronously.
    let session_id = SessionId::from(conv.acp_session_id.clone());
    let stream = acp
        .session_prompt_blocks(&session_id, content)
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

async fn handle_tasks_get(state: ServeState, params: Value) -> Result<Value, JsonRpcError> {
    let parsed: TaskIdParams = serde_json::from_value(params).map_err(invalid_params)?;
    let snap = state
        .tasks
        .snapshot(&parsed.id)
        .await
        .ok_or_else(|| JsonRpcError {
            code: codes::TASK_NOT_FOUND,
            message: format!("task not found: {}", parsed.id),
            data: None,
        })?;
    Ok(serde_json::to_value(snap).expect("Task serializes"))
}

async fn handle_list_tasks(state: ServeState, params: Value) -> Result<Value, JsonRpcError> {
    let parsed: ListTasksParams = serde_json::from_value(params).map_err(invalid_params)?;
    let after = parsed.page_token.map(TaskId::from);
    let limit = parsed.page_size.unwrap_or(0);
    let (tasks, next_cursor) = state.tasks.list(after.as_ref(), limit).await;
    Ok(serde_json::json!({
        "tasks": tasks,
        "nextPageToken": next_cursor.as_ref().map(|c| c.as_str()),
    }))
}

async fn handle_tasks_cancel(state: ServeState, params: Value) -> Result<Value, JsonRpcError> {
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

// ───────────────────────── SSE: message/stream ─────────────────────────

use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures::stream::StreamExt;

use crate::conversation::InFlightGuard;
use crate::sse_sink::SseFrame;

async fn handle_message_stream(
    state: ServeState,
    req: JsonRpcRequest<Value>,
) -> axum::response::Response {
    if req.method == "SubscribeToTask" {
        return handle_subscribe_to_task(state, req).await;
    }
    match prepare_prompt(state.clone(), req.params.clone()).await {
        Ok(PromptHandle {
            task_id,
            sink,
            permit,
            stream,
        }) => {
            // Spawn the bridge so it pumps SseSink while we hand the
            // subscriber receiver out as the HTTP response body. The
            // permit moves into the bridge task so it lives until the
            // bridge finishes (terminal event published, sink closed).
            let registry = state.tasks.clone();
            let bridge_task_id = task_id.clone();
            tokio::spawn(async move {
                let _permit = permit;
                if let Err(e) = bridge::run_session(bridge_task_id, registry, stream).await {
                    tracing::warn!(error = %e, "bridge::run_session failed in stream branch");
                }
            });
            // Per-Task SSE keepalive (spec § 2.12) handled by axum's
            // built-in KeepAlive layer at 30s.
            let rx = sink
                .subscribe()
                .expect("sink is open at message-stream subscribe time");
            let body =
                tokio_stream::wrappers::BroadcastStream::new(rx).filter_map(|item| async move {
                    match item {
                        Ok(SseFrame::Event(ev)) => {
                            // encode_sse_event returns the full "data: …\n\n"
                            // block, but axum's Sse Event type only takes
                            // the JSON body — re-serialize from the typed
                            // form.
                            let json = serde_json::to_string(&ev).ok()?;
                            Some(Ok::<_, std::convert::Infallible>(
                                Event::default().data(json),
                            ))
                        }
                        Ok(SseFrame::Keepalive) => {
                            // SseSink emits its own keepalive frames, but
                            // we let axum's KeepAlive layer handle the
                            // wire-level comment line so we don't double
                            // up. Dropping our frame is correct.
                            None
                        }
                        Err(_) => None, // lagged or closed -> end stream
                    }
                });
            Sse::new(body)
                .keep_alive(
                    KeepAlive::new()
                        .interval(a2a_shim_core::constants::SSE_KEEPALIVE_INTERVAL)
                        .text("keepalive"),
                )
                .into_response()
        }
        Err(e) => {
            // Prep failed: return JSON-RPC error envelope at HTTP 200.
            let resp: JsonRpcResponse<Value> = JsonRpcResponse {
                jsonrpc: "2.0".into(),
                id: req.id.clone(),
                result_or_error: ResultOrError::from_error(e),
            };
            let mut r = Json(resp).into_response();
            // Keep status 200 so JSON-RPC error envelope semantics are
            // preserved (clients parse the body to discover the error).
            *r.status_mut() = StatusCode::OK;
            r
        }
    }
}

/// Output of the conversation + task + permit + stream bootstrap shared
/// by message/send and message/stream.
struct PromptHandle {
    task_id: TaskId,
    sink: crate::sse_sink::SseSink,
    permit: InFlightGuard,
    stream: futures::stream::BoxStream<
        'static,
        Result<crate::acp_client::BridgeEvent, crate::acp_client::AcpError>,
    >,
}

async fn prepare_prompt(state: ServeState, params: Value) -> Result<PromptHandle, JsonRpcError> {
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

    let (conv, _created) = state
        .conversations
        .get_or_create(&conv_id, || async {
            let acp = Arc::clone(&acp);
            let cwd: PathBuf = cwd;
            acp.session_new(cwd)
                .await
                .map(|sid| sid.0.as_ref().to_string())
                .map_err(|e| e.to_string())
        })
        .await
        .map_err(map_new_error)?;

    let permit = state
        .conversations
        .acquire_in_flight(&conv_id)
        .await
        .map_err(map_acquire_error)?;

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

    let caps = crate::translate::PartCaps::default();
    let content = crate::translate::a2a_to_acp(&parsed.message.parts, &caps);

    let session_id = SessionId::from(conv.acp_session_id.clone());
    let stream = acp
        .session_prompt_blocks(&session_id, content)
        .await
        .map_err(|e| JsonRpcError {
            code: codes::INTERNAL_ERROR,
            message: format!("session/prompt failed: {e}"),
            data: None,
        })?;
    let sink = state
        .tasks
        .sink(&task_id)
        .await
        .ok_or_else(|| JsonRpcError {
            code: codes::INTERNAL_ERROR,
            message: "task vanished before sink fetch".into(),
            data: None,
        })?;

    Ok(PromptHandle {
        task_id,
        sink,
        permit,
        stream,
    })
}

/// `SubscribeToTask` re-attaches to an existing Task's per-Task SseSink
/// without consuming the H1 in-flight permit. Spec A2A v1.0.1 § 9.4.6.
///
/// Errors:
///   * unknown task id     -> TASK_NOT_FOUND  (-32001)
///   * terminal task       -> still returns SSE; the response body
///                            EOFs immediately because publish_final
///                            already closed the broadcast channel.
///                            Operators wanting the cached snapshot
///                            use GetTask instead.
async fn handle_subscribe_to_task(
    state: ServeState,
    req: JsonRpcRequest<Value>,
) -> axum::response::Response {
    let parsed: Result<TaskIdParams, _> = serde_json::from_value(req.params.clone());
    let params = match parsed {
        Ok(p) => p,
        Err(e) => return jsonrpc_error_response(req.id, invalid_params(e)),
    };
    let sink = match state.tasks.sink(&params.id).await {
        Some(s) => s,
        None => {
            return jsonrpc_error_response(
                req.id,
                JsonRpcError {
                    code: codes::TASK_NOT_FOUND,
                    message: format!("task not found: {}", params.id),
                    data: None,
                },
            );
        }
    };
    let rx = match sink.subscribe() {
        Some(rx) => rx,
        None => {
            // Sink already closed (terminal task). Return empty SSE
            // body — caller's connection EOFs cleanly.
            return Sse::new(futures::stream::empty::<
                Result<Event, std::convert::Infallible>,
            >())
            .keep_alive(
                KeepAlive::new()
                    .interval(a2a_shim_core::constants::SSE_KEEPALIVE_INTERVAL)
                    .text("keepalive"),
            )
            .into_response();
        }
    };
    let body = tokio_stream::wrappers::BroadcastStream::new(rx).filter_map(|item| async move {
        match item {
            Ok(SseFrame::Event(ev)) => {
                let json = serde_json::to_string(&ev).ok()?;
                Some(Ok::<_, std::convert::Infallible>(
                    Event::default().data(json),
                ))
            }
            Ok(SseFrame::Keepalive) => None,
            Err(_) => None,
        }
    });
    Sse::new(body)
        .keep_alive(
            KeepAlive::new()
                .interval(a2a_shim_core::constants::SSE_KEEPALIVE_INTERVAL)
                .text("keepalive"),
        )
        .into_response()
}

/// Small helper to render an error envelope as an axum Response so the
/// caller doesn't need to repeat the JSON-RPC framing.
fn jsonrpc_error_response(id: Value, err: JsonRpcError) -> axum::response::Response {
    let resp: JsonRpcResponse<Value> = JsonRpcResponse {
        jsonrpc: "2.0".into(),
        id,
        result_or_error: ResultOrError::from_error(err),
    };
    let mut r = Json(resp).into_response();
    *r.status_mut() = StatusCode::OK;
    r
}
