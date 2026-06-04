//! HTTP surface for the Serve Shim (spec § 2.6, § 4).
//!
//! Built up incrementally:
//!   * Task 24 (this file): ServeState plus `GET /.well-known/agent.json`.
//!   * Task 25: `POST /` JSON-RPC dispatch (message/send, tasks/get, tasks/cancel).
//!   * Task 26: SSE branch for `message/stream`.
//!
//! `ServeState` is `Arc`-shared between the axum router and the run-time
//! task that owns the listener. The bound `SocketAddr` is published into a
//! `OnceLock` *after* bind() so the AgentCard handler can render the URL
//! correctly even if the operator did not set advertised_endpoint.

use a2a_shim_core::config::serve_toml::ServeConfig;
use axum::{extract::State, response::IntoResponse, routing::get, Json, Router};
use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};

use crate::agent_card::build_agent_card;

#[derive(Clone)]
pub struct ServeState {
    pub cfg: Arc<ServeConfig>,
    pub bound: Arc<OnceLock<SocketAddr>>,
}

impl ServeState {
    pub fn new(cfg: Arc<ServeConfig>) -> Self {
        Self {
            cfg,
            bound: Arc::new(OnceLock::new()),
        }
    }

    /// Convenience constructor for tests that do not need to track the
    /// bound address (the AgentCard URL will render with the bound port
    /// once `set_bound` is called, but tests that only check capabilities
    /// can skip that and just rely on the fallback formatting).
    pub fn new_for_test(cfg: Arc<ServeConfig>) -> Self {
        Self::new(cfg)
    }

    /// Record the actual bound socket so the AgentCard URL renders right.
    /// Called from `serve::run` exactly once after `TcpListener::bind`.
    /// Subsequent calls are silently ignored.
    pub fn set_bound(&self, addr: SocketAddr) {
        let _ = self.bound.set(addr);
    }

    fn bound_str(&self) -> String {
        self.bound
            .get()
            .map(|a| a.to_string())
            .unwrap_or_else(|| self.cfg.server.listen.clone())
    }
}

pub fn router(state: ServeState) -> Router {
    let card_path = state.cfg.server.agent_card_path.clone();
    Router::new()
        .route(&card_path, get(agent_card_handler))
        .with_state(state)
}

async fn agent_card_handler(State(state): State<ServeState>) -> impl IntoResponse {
    let bound = state.bound_str();
    let card = build_agent_card(&state.cfg, &bound);
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "application/json",
        )],
        Json(card),
    )
}
