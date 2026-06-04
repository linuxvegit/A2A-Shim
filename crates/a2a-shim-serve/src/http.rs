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
    pub persistence: Option<crate::persistence::Persistence>,
    pub push_registry: crate::push_delivery::PushConfigRegistry,
    pub push_tx: Option<tokio::sync::mpsc::UnboundedSender<crate::push_delivery::DeliveryJob>>,
    pub metrics_handle: Option<metrics_exporter_prometheus::PrometheusHandle>,
}

impl ServeState {
    pub fn new(cfg: Arc<ServeConfig>) -> Self {
        Self::new_with_persistence(cfg, None)
    }

    /// Variant that wires a Persistence handle through to ConversationMap
    /// and TaskRegistry so writes survive Serve restart (ADR 0007).
    pub fn new_with_persistence(
        cfg: Arc<ServeConfig>,
        persistence: Option<crate::persistence::Persistence>,
    ) -> Self {
        let conv = ConversationMap::with_persistence(
            cfg.server.conversations.max_active,
            Duration::from_secs(cfg.server.conversations.idle_secs),
            persistence.clone(),
        );
        let push_registry = crate::push_delivery::PushConfigRegistry::new(
            persistence.clone(),
            cfg.server.push_notifications.permanent_failure_threshold,
        );
        Self {
            cfg,
            bound: Arc::new(OnceLock::new()),
            acp: None,
            conversations: conv,
            tasks: TaskRegistry::with_persistence(persistence.clone()),
            persistence,
            push_registry,
            push_tx: None,
            metrics_handle: None,
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

    /// Production constructor: config + ACP client + Persistence wired
    /// through to all in-memory state holders. Used by serve::run.
    pub fn with_client_and_persistence(
        cfg: Arc<ServeConfig>,
        client: AcpClient,
        persistence: Option<crate::persistence::Persistence>,
    ) -> Self {
        let mut s = Self::new_with_persistence(cfg, persistence);
        s.acp = Some(Arc::new(client));
        s
    }

    /// Wire the push delivery worker mpsc sender onto state.
    pub fn set_push_tx(
        &mut self,
        tx: tokio::sync::mpsc::UnboundedSender<crate::push_delivery::DeliveryJob>,
    ) {
        self.push_tx = Some(tx);
    }

    /// Wire the Prometheus handle so the /metrics route can render.
    pub fn set_metrics_handle(&mut self, h: metrics_exporter_prometheus::PrometheusHandle) {
        self.metrics_handle = Some(h);
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
    let metrics_enabled = state.cfg.server.metrics.enabled;
    let mut r = Router::new()
        .route(&card_path, get(agent_card_handler))
        .route("/", post(jsonrpc_root));
    if metrics_enabled {
        r = r.route("/metrics", get(metrics_handler));
    }
    r.with_state(state)
}

async fn metrics_handler(State(state): State<ServeState>) -> impl IntoResponse {
    match state.metrics_handle.as_ref() {
        Some(h) => (
            [(
                axum::http::header::CONTENT_TYPE,
                "text/plain; version=0.0.4; charset=utf-8",
            )],
            h.render(),
        )
            .into_response(),
        None => (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "metrics recorder not initialized",
        )
            .into_response(),
    }
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
    headers: axum::http::HeaderMap,
    Json(req): Json<JsonRpcRequest<Value>>,
) -> axum::response::Response {
    // Resolve caller_id once per request. Used by SendMessage /
    // SendStreamingMessage handlers when [server.caller_identity].enabled.
    let header_caller = if state.cfg.server.caller_identity.trust_header {
        headers
            .get("X-A2A-Caller-Id")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
    } else {
        None
    };
    // message/stream needs to return an SSE body, not JSON. All other
    // methods route through the JSON dispatch + envelope path.
    if req.method == "SendStreamingMessage" || req.method == "SubscribeToTask" {
        return handle_message_stream(state, req, header_caller).await;
    }
    let id = req.id.clone();
    let method_for_metric = req.method.clone();
    let result = dispatch(state, &req, header_caller).await;
    crate::metrics::record_message(&method_for_metric, result.is_ok());
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

async fn dispatch(
    state: ServeState,
    req: &JsonRpcRequest<Value>,
    header_caller: Option<String>,
) -> Result<Value, JsonRpcError> {
    match req.method.as_str() {
        "SendMessage" => handle_message_send(state, req.params.clone(), header_caller).await,
        "GetTask" => handle_tasks_get(state, req.params.clone()).await,
        "CancelTask" => handle_tasks_cancel(state, req.params.clone()).await,
        "ListTasks" => handle_list_tasks(state, req.params.clone()).await,
        "CreateTaskPushNotificationConfig" => handle_push_create(state, req.params.clone()).await,
        "GetTaskPushNotificationConfig" => handle_push_get(state, req.params.clone()).await,
        "ListTaskPushNotificationConfigs" => handle_push_list(state, req.params.clone()).await,
        "DeleteTaskPushNotificationConfig" => handle_push_delete(state, req.params.clone()).await,
        "_shim/conversation/reset" => {
            handle_conversation_reset(state, req.params.clone(), header_caller).await
        }
        // SendStreamingMessage / SubscribeToTask handled above as SSE.
        other => Err(JsonRpcError {
            code: codes::METHOD_NOT_FOUND,
            message: format!("method not found: {other}"),
            data: None,
        }),
    }
}

/// Resolve effective caller_id with priority: header > metadata > config default.
/// Returns None if caller_identity is disabled (no partitioning applied).
pub(crate) fn resolve_caller_id(
    cfg: &a2a_shim_core::config::serve_toml::CallerIdentityConfig,
    header_caller: Option<&str>,
    metadata_caller: Option<&str>,
) -> Option<String> {
    if !cfg.enabled {
        return None;
    }
    if let Some(h) = header_caller {
        return Some(h.to_string());
    }
    if let Some(m) = metadata_caller {
        return Some(m.to_string());
    }
    Some(cfg.default_caller_id.clone())
}

/// Convert (caller_id, conversation_id) into the actual map key used by
/// ConversationMap. When caller_id is None (feature disabled), the key
/// is the conversation_id verbatim — preserving v0.1.0 behavior.
pub(crate) fn partition_key(caller_id: Option<&str>, conversation_id: &str) -> String {
    match caller_id {
        Some(c) => format!("{c}\u{1f}{conversation_id}"),
        None => conversation_id.to_string(),
    }
}

/// Apply v1.1 item #5 conversation_mode semantics. Reads
/// `params._shim_conversation_mode` (set by Client Shim outbound when
/// the operator passed `conversation_mode`). Returns:
///   - Ok(()) when mode is auto/missing OR mode is satisfied
///   - Err(CONVERSATION_EXISTS) when mode='new' but the key is present
///   - Err(CONVERSATION_LOST) when mode='continue' but the key is absent
///   - Err(INVALID_PARAMS) when mode is set to an unknown string
pub(crate) async fn check_conversation_mode(
    state: &ServeState,
    params: &Value,
    conv_key: &str,
) -> Result<(), JsonRpcError> {
    let mode = params
        .get("_shim_conversation_mode")
        .and_then(|v| v.as_str())
        .unwrap_or("auto");
    match mode {
        "auto" => Ok(()),
        "new" => {
            if state.conversations.get(conv_key).await.is_some() {
                Err(JsonRpcError {
                    code: codes::CONVERSATION_EXISTS,
                    message: format!("conversation '{}' already exists (mode=new)", conv_key),
                    data: None,
                })
            } else {
                Ok(())
            }
        }
        "continue" => {
            if state.conversations.get(conv_key).await.is_none() {
                Err(JsonRpcError {
                    code: codes::CONVERSATION_LOST,
                    message: format!("conversation '{}' does not exist (mode=continue)", conv_key),
                    data: None,
                })
            } else {
                Ok(())
            }
        }
        other => Err(JsonRpcError {
            code: codes::INVALID_PARAMS,
            message: format!("unknown conversation_mode: {other}"),
            data: None,
        }),
    }
}

async fn handle_message_send(
    state: ServeState,
    params: Value,
    header_caller: Option<String>,
) -> Result<Value, JsonRpcError> {
    let raw_params = params.clone();
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

    // Resolve effective caller_id; None when caller_identity is disabled.
    let metadata_caller = parsed
        .message
        .metadata
        .as_ref()
        .and_then(|m| m.extra.get("x-a2a-shim/caller_id"))
        .and_then(|v| v.as_str());
    let caller_id = resolve_caller_id(
        &state.cfg.server.caller_identity,
        header_caller.as_deref(),
        metadata_caller,
    );
    let conv_key = partition_key(caller_id.as_deref(), &conv_id);
    check_conversation_mode(&state, &raw_params, &conv_key).await?;

    let acp = state.require_acp()?;
    let cwd = state.cfg.agent.cwd.clone();

    // Get-or-create the conversation. The closure runs at most once per
    // conv_key; ConversationMap serializes new-session creation.
    let (conv, _created) = state
        .conversations
        .get_or_create_with_meta(
            &conv_key,
            &cwd.display().to_string(),
            caller_id.as_deref().unwrap_or("anonymous"),
            || async {
                let acp = Arc::clone(&acp);
                let cwd: PathBuf = cwd;
                acp.session_new(cwd)
                    .await
                    .map(|sid| sid.0.as_ref().to_string())
                    .map_err(|e| e.to_string())
            },
        )
        .await
        .map_err(map_new_error)?;

    // Acquire the H1 in-flight guard before we even create a Task so
    // overlap is reported as CONVERSATION_BUSY, not as a state-machine
    // failure halfway through.
    let _permit = state
        .conversations
        .acquire_in_flight(&conv_key)
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
        state.tasks.create(&conv_key, &conv.acp_session_id).await
    };

    // Translate inbound A2A Parts -> ACP ContentBlocks per ADR 0006.
    // PartCaps defaults to all-off in v1.1; the cap-cache from agent's
    // initialize response wires through in v1.2. Today: Text + ResourceLink
    // always pass; Image/Audio/EmbeddedResource/Data require caps=on
    // (none today) and drop with a tracing warn.
    crate::translate::validate_parts(&parsed.message.parts, state.cfg.server.max_part_bytes)
        .map_err(|e| JsonRpcError {
            code: codes::INVALID_PARAMS,
            message: format!("{e}"),
            data: None,
        })?;
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
    // v1.1 item #6: on terminal transition, enqueue push delivery
    // jobs for any registered configs (ADR 0008 trigger).
    if snap.status.state.is_terminal() {
        enqueue_push_for_terminal(&state, &snap);
    }
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
    header_caller: Option<String>,
) -> axum::response::Response {
    if req.method == "SubscribeToTask" {
        return handle_subscribe_to_task(state, req).await;
    }
    match prepare_prompt(state.clone(), req.params.clone(), header_caller).await {
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

async fn prepare_prompt(
    state: ServeState,
    params: Value,
    header_caller: Option<String>,
) -> Result<PromptHandle, JsonRpcError> {
    let raw_params = params.clone();
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
    let metadata_caller = parsed
        .message
        .metadata
        .as_ref()
        .and_then(|m| m.extra.get("x-a2a-shim/caller_id"))
        .and_then(|v| v.as_str());
    let caller_id = resolve_caller_id(
        &state.cfg.server.caller_identity,
        header_caller.as_deref(),
        metadata_caller,
    );
    let conv_key = partition_key(caller_id.as_deref(), &conv_id);
    check_conversation_mode(&state, &raw_params, &conv_key).await?;
    let acp = state.require_acp()?;
    let cwd = state.cfg.agent.cwd.clone();

    let (conv, _created) = state
        .conversations
        .get_or_create_with_meta(
            &conv_key,
            &cwd.display().to_string(),
            caller_id.as_deref().unwrap_or("anonymous"),
            || async {
                let acp = Arc::clone(&acp);
                let cwd: PathBuf = cwd;
                acp.session_new(cwd)
                    .await
                    .map(|sid| sid.0.as_ref().to_string())
                    .map_err(|e| e.to_string())
            },
        )
        .await
        .map_err(map_new_error)?;

    let permit = state
        .conversations
        .acquire_in_flight(&conv_key)
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
        state.tasks.create(&conv_key, &conv.acp_session_id).await
    };

    crate::translate::validate_parts(&parsed.message.parts, state.cfg.server.max_part_bytes)
        .map_err(|e| JsonRpcError {
            code: codes::INVALID_PARAMS,
            message: format!("{e}"),
            data: None,
        })?;
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
///     EOFs immediately because publish_final already closed the
///     broadcast channel. Operators wanting the cached snapshot use
///     GetTask instead.
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

// ─────────────────────── Push Notification Config CRUD ────────────────────

/// JSON body shape for the four push-notif methods (spec A2A v1.0.1 § 3.1.7).
#[derive(serde::Deserialize)]
struct PushConfigParams {
    #[serde(rename = "taskId")]
    task_id: Option<String>,
    #[serde(rename = "configId")]
    config_id: Option<String>,
    #[serde(rename = "pushNotificationConfig")]
    push_notification_config: Option<PushNotificationConfigWire>,
}

#[derive(Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct PushNotificationConfigWire {
    id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tenant: Option<String>,
    #[serde(rename = "taskId", skip_serializing_if = "Option::is_none")]
    task_id: Option<String>,
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    authentication: Option<AuthenticationInfo>,
}

#[derive(Clone, serde::Deserialize, serde::Serialize)]
struct AuthenticationInfo {
    scheme: String,
    credentials: String,
}

fn require_push_enabled(state: &ServeState) -> Result<(), JsonRpcError> {
    if !state.cfg.server.push_notifications.enabled {
        return Err(JsonRpcError {
            code: codes::PUSH_NOTIFICATIONS_NOT_SUPPORTED,
            message: "push notifications disabled by config".into(),
            data: None,
        });
    }
    Ok(())
}

async fn handle_push_create(state: ServeState, params: Value) -> Result<Value, JsonRpcError> {
    require_push_enabled(&state)?;
    let parsed: PushConfigParams = serde_json::from_value(params).map_err(invalid_params)?;
    let cfg = parsed
        .push_notification_config
        .ok_or_else(|| JsonRpcError {
            code: codes::INVALID_PUSH_NOTIFICATION_CONFIG,
            message: "missing pushNotificationConfig".into(),
            data: None,
        })?;
    let task_id = cfg
        .task_id
        .clone()
        .or(parsed.task_id)
        .ok_or_else(|| JsonRpcError {
            code: codes::INVALID_PUSH_NOTIFICATION_CONFIG,
            message: "missing taskId".into(),
            data: None,
        })?;
    let config_id = cfg
        .id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string());
    let (auth_scheme, auth_credentials) = cfg
        .authentication
        .as_ref()
        .map(|a| (Some(a.scheme.clone()), Some(a.credentials.clone())))
        .unwrap_or((None, None));
    let pcfg = crate::push_delivery::PushNotificationConfig {
        config_id: config_id.clone(),
        task_id: task_id.clone(),
        url: cfg.url.clone(),
        token: cfg.token.clone(),
        auth_scheme,
        auth_credentials,
        tenant: cfg.tenant.clone(),
    };
    state
        .push_registry
        .insert(pcfg)
        .await
        .map_err(|e| JsonRpcError {
            code: codes::INTERNAL_ERROR,
            message: format!("push registry insert: {e}"),
            data: None,
        })?;
    let mut out = cfg;
    out.id = Some(config_id);
    out.task_id = Some(task_id);
    Ok(serde_json::to_value(&out).expect("serialize"))
}

async fn handle_push_get(state: ServeState, params: Value) -> Result<Value, JsonRpcError> {
    require_push_enabled(&state)?;
    let parsed: PushConfigParams = serde_json::from_value(params).map_err(invalid_params)?;
    let cid = parsed.config_id.ok_or_else(|| JsonRpcError {
        code: codes::INVALID_PARAMS,
        message: "missing configId".into(),
        data: None,
    })?;
    let cfg = state
        .push_registry
        .get(&cid)
        .await
        .map_err(|e| JsonRpcError {
            code: codes::INTERNAL_ERROR,
            message: format!("push registry get: {e}"),
            data: None,
        })?
        .ok_or_else(|| JsonRpcError {
            code: codes::TASK_NOT_FOUND,
            message: format!("push config not found: {cid}"),
            data: None,
        })?;
    Ok(push_config_to_wire(&cfg))
}

async fn handle_push_list(state: ServeState, params: Value) -> Result<Value, JsonRpcError> {
    require_push_enabled(&state)?;
    let parsed: PushConfigParams = serde_json::from_value(params).map_err(invalid_params)?;
    let task_id = parsed.task_id.ok_or_else(|| JsonRpcError {
        code: codes::INVALID_PARAMS,
        message: "missing taskId".into(),
        data: None,
    })?;
    let cfgs = state.push_registry.list_for_task(&task_id);
    Ok(serde_json::json!({
        "configs": cfgs.iter().map(push_config_to_wire).collect::<Vec<_>>()
    }))
}

async fn handle_push_delete(state: ServeState, params: Value) -> Result<Value, JsonRpcError> {
    require_push_enabled(&state)?;
    let parsed: PushConfigParams = serde_json::from_value(params).map_err(invalid_params)?;
    let cid = parsed.config_id.ok_or_else(|| JsonRpcError {
        code: codes::INVALID_PARAMS,
        message: "missing configId".into(),
        data: None,
    })?;
    state
        .push_registry
        .delete(&cid)
        .await
        .map_err(|e| JsonRpcError {
            code: codes::INTERNAL_ERROR,
            message: format!("push registry delete: {e}"),
            data: None,
        })?;
    Ok(serde_json::json!({ "deleted": true }))
}

fn push_config_to_wire(cfg: &crate::push_delivery::PushNotificationConfig) -> Value {
    let auth = match (cfg.auth_scheme.as_ref(), cfg.auth_credentials.as_ref()) {
        (Some(s), Some(c)) => Some(serde_json::json!({ "scheme": s, "credentials": c })),
        _ => None,
    };
    serde_json::json!({
        "id": cfg.config_id,
        "taskId": cfg.task_id,
        "url": cfg.url,
        "token": cfg.token,
        "tenant": cfg.tenant,
        "authentication": auth,
    })
}

/// Build + enqueue delivery jobs for every push config registered against
/// the Task whose snapshot just hit a terminal state. Best-effort:
/// failures log at debug.
pub(crate) fn enqueue_push_for_terminal(
    state: &ServeState,
    snap: &a2a_shim_core::wire::task::Task,
) {
    let Some(tx) = state.push_tx.as_ref() else {
        return;
    };
    let cfgs = state.push_registry.list_for_task(snap.id.as_str());
    if cfgs.is_empty() {
        return;
    }
    let state_str = match snap.status.state {
        a2a_shim_core::wire::task::TaskState::Completed => "completed",
        a2a_shim_core::wire::task::TaskState::Failed => "failed",
        a2a_shim_core::wire::task::TaskState::Canceled => "canceled",
        _ => return, // non-terminal — caller is supposed to check
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let payload =
        crate::push_delivery::build_status_update_payload(snap.id.as_str(), state_str, now_ms);
    for cfg in cfgs {
        let job = crate::push_delivery::DeliveryJob {
            config: cfg,
            payload: payload.clone(),
        };
        if let Err(e) = tx.send(job) {
            tracing::debug!(error = %e, "push delivery channel closed");
        }
    }
}

// ──────────────────── _shim/conversation/reset (Task 39) ────────────────────

#[derive(serde::Deserialize)]
struct ConversationResetParams {
    conversation_id: String,
    #[serde(default)]
    caller_id: Option<String>,
}

async fn handle_conversation_reset(
    state: ServeState,
    params: Value,
    header_caller: Option<String>,
) -> Result<Value, JsonRpcError> {
    let parsed: ConversationResetParams = serde_json::from_value(params).map_err(invalid_params)?;
    let caller = resolve_caller_id(
        &state.cfg.server.caller_identity,
        header_caller.as_deref(),
        parsed.caller_id.as_deref(),
    );
    let conv_key = partition_key(caller.as_deref(), &parsed.conversation_id);

    let conv = state.conversations.get(&conv_key).await;
    let conv_exists = conv.is_some();

    // Cancel every non-terminal Task that points at this conv_key.
    let (tasks, _) = state.tasks.list(None, 1000).await;
    let mut cancelled: Vec<String> = Vec::new();
    for t in tasks {
        if t.context_id.as_deref() == Some(conv_key.as_str())
            && !t.status.state.is_terminal()
            && state.tasks.cancel(&t.id).await.is_ok()
        {
            cancelled.push(t.id.as_str().to_string());
        }
    }

    // Tell the ACP agent to drop the session.
    if let (Some(conv), Some(acp)) = (conv.as_ref(), state.acp.as_ref()) {
        let sid = agent_client_protocol::schema::SessionId::from(conv.acp_session_id.clone());
        if let Err(e) = acp.session_cancel(&sid).await {
            tracing::warn!(
                conv = %conv_key,
                error = %e,
                "ACP session/cancel failed during conversation reset"
            );
        }
    }

    // Drop the in-mem entry + persistence row (CASCADE removes tasks +
    // push configs).
    if conv_exists {
        if let Some(p) = state.persistence.as_ref() {
            if let Err(e) = p.delete_conversation(&conv_key).await {
                tracing::warn!(
                    conv = %conv_key,
                    error = %e,
                    "persistence delete during conversation reset failed"
                );
            }
        }
        // Best-effort: there's no direct ConversationMap remove method
        // today, so trigger via sweep_idle by zeroing the timestamp.
        // Simpler v1.1 path: rely on idle reaper to evict; or expose a
        // remove. We do a manual remove via internal access.
        state.conversations.remove(&conv_key).await;
    }

    Ok(serde_json::json!({
        "cleared": conv_exists,
        "cancelled_task_ids": cancelled,
    }))
}
