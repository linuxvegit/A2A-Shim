//! `a2a-shim serve` top-level entry (spec § 5 + § 6.2).
//!
//! Sequence:
//!   1. Load the TOML config (file path required from CLI or env).
//!   2. Apply CLI overrides for the few fields exposed by `ServeOpts`.
//!   3. Init tracing via `a2a_shim_core::logging::try_init_idempotent`.
//!   4. Warn if the listener is non-loopback (spec § 6.2 — the shim does
//!      not own auth/TLS; non-loopback binds rely on an external
//!      port-forward/HTTPS layer).
//!   5. Spawn the wrapped ACP Agent via `AcpClient::spawn` and call
//!      `initialize()` so the session is ready before any HTTP traffic.
//!   6. Build ServeState, the axum router, bind the listener, log the
//!      bound address (the smoke test in crates/a2a-shim parses that
//!      line to discover the port when listen = "127.0.0.1:0").
//!   7. Spawn the idle reaper: every idle_secs / 4 (clamped 30s..3600s),
//!      sweep the conversation map and best-effort cancel evicted ACP
//!      sessions.
//!   8. Serve with graceful shutdown on Ctrl-C.

use std::sync::Arc;
use std::time::Duration;

use a2a_shim_core::config::serve_toml::{ServeConfig, ServeConfigError};
use a2a_shim_core::logging::{try_init_idempotent, LogDestination, LogFormat, LoggingOptions};
use agent_client_protocol::schema::SessionId;

use crate::acp_client::{AcpClient, AcpClientConfig};
use crate::http::{router, ServeState};

/// Runtime options for `serve::run`. Mirrors the CLI surface in
/// `crates/a2a-shim/src/cli.rs::ServeOpts` but kept dep-free so this
/// module can be exercised from tests without pulling clap.
#[derive(Debug, Clone, Default)]
pub struct ServeRuntimeOpts {
    pub config_path: Option<std::path::PathBuf>,
    pub listen_override: Option<String>,
    pub advertised_endpoint_override: Option<String>,
    pub cwd_override: Option<std::path::PathBuf>,
    pub log_file: Option<std::path::PathBuf>,
    pub log_format: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("missing required --config or A2A_SHIM_CONFIG (no inline default in MVP)")]
    MissingConfig,
    #[error("read config {0}: {1}")]
    ReadConfig(std::path::PathBuf, std::io::Error),
    #[error("parse config: {0}")]
    ParseConfig(#[from] ServeConfigError),
    #[error("init tracing: {0}")]
    Tracing(#[from] a2a_shim_core::logging::TracingInitError),
    #[error("spawn ACP agent: {0}")]
    SpawnAgent(String),
    #[error("ACP agent initialize: {0}")]
    InitAgent(String),
    #[error("bind {0}: {1}")]
    Bind(String, std::io::Error),
    #[error("serve: {0}")]
    Serve(#[source] std::io::Error),
}

pub async fn run(opts: ServeRuntimeOpts) -> Result<(), RunError> {
    // ----- 1+2: load + override -----
    let cfg_path = opts.config_path.clone().ok_or(RunError::MissingConfig)?;
    let raw = std::fs::read_to_string(&cfg_path)
        .map_err(|e| RunError::ReadConfig(cfg_path.clone(), e))?;
    let mut cfg = ServeConfig::from_toml_str(&raw)?;
    if let Some(listen) = opts.listen_override.clone() {
        cfg.server.listen = listen;
    }
    if let Some(adv) = opts.advertised_endpoint_override.clone() {
        cfg.server.advertised_endpoint = Some(adv);
    }
    if let Some(cwd) = opts.cwd_override.clone() {
        cfg.agent.cwd = cwd;
    }

    // ----- 3: tracing -----
    let log_opts = LoggingOptions {
        level: cfg.logging.level.clone(),
        format: match opts.log_format.as_deref().unwrap_or(cfg.logging.format.as_str()) {
            "json" => LogFormat::Json,
            "pretty" => LogFormat::Pretty,
            _ => LogFormat::Compact,
        },
        destination: match opts.log_file.clone().or_else(|| cfg.logging.file.clone()) {
            Some(p) => LogDestination::File(p),
            None => LogDestination::Stderr,
        },
    };
    try_init_idempotent(log_opts)?;

    // ----- 4: non-loopback warning (spec § 6.2) -----
    if !is_loopback_listen(&cfg.server.listen) {
        tracing::warn!(
            listen = %cfg.server.listen,
            "a2a-shim is binding a non-loopback address. The shim does NOT \
             own authentication or TLS — rely on an external port-forward \
             or HTTPS terminator (spec § 6.2)."
        );
    }

    // ----- 5: spawn the ACP agent + initialize -----
    let acp_cfg = AcpClientConfig {
        command: cfg.agent.command.clone(),
        args: cfg.agent.args.clone(),
        cwd: cfg.agent.cwd.clone(),
        env: cfg.agent.env.clone(),
    };
    let acp = AcpClient::spawn(acp_cfg).await.map_err(|e| RunError::SpawnAgent(e.to_string()))?;
    acp.initialize().await.map_err(|e| RunError::InitAgent(e.to_string()))?;

    // ----- 6: bind + log -----
    let cfg_arc = Arc::new(cfg);
    let state = ServeState::with_client(cfg_arc.clone(), acp);
    let listener = tokio::net::TcpListener::bind(&cfg_arc.server.listen)
        .await
        .map_err(|e| RunError::Bind(cfg_arc.server.listen.clone(), e))?;
    let bound = listener.local_addr().map_err(RunError::Serve)?;
    state.set_bound(bound);
    tracing::info!(%bound, "serve listening on {bound}");

    // ----- 7: idle reaper -----
    spawn_idle_reaper(state.clone(), cfg_arc.server.conversations.idle_secs);

    // ----- 8: serve with graceful shutdown -----
    let app = router(state);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(RunError::Serve)?;
    Ok(())
}

fn is_loopback_listen(listen: &str) -> bool {
    // Best-effort: parse "host:port" then check the host. If we cannot
    // parse, assume non-loopback so the warning fires conservatively.
    let host = listen.rsplit_once(':').map(|(h, _)| h).unwrap_or(listen);
    matches!(host, "127.0.0.1" | "localhost" | "::1" | "[::1]")
}

fn spawn_idle_reaper(state: ServeState, idle_secs: u64) {
    // Sweep cadence: idle_secs / 4, clamped to a sane band. Idle eviction
    // is best-effort so we do not need exact timing.
    let sweep_period = Duration::from_secs(idle_secs / 4).clamp(
        Duration::from_secs(30),
        Duration::from_secs(3600),
    );
    let conversations = state.conversations.clone();
    let acp = state.acp.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(sweep_period);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        ticker.tick().await; // first tick fires immediately; skip it
        loop {
            ticker.tick().await;
            let dropped = conversations.sweep_idle().await;
            if dropped.is_empty() {
                continue;
            }
            tracing::info!(?dropped, "idle reaper swept conversations");
            if let Some(acp) = acp.as_ref() {
                for conv_id in dropped {
                    // We have only the conversation id at this point; the
                    // session id was dropped with the entry. In MVP we
                    // accept that ACP sessions tied to evicted
                    // conversations become orphaned until the agent
                    // process is restarted; v1.2 will track session ids
                    // separately so the reaper can cancel them. For now
                    // log the gap so operators see it.
                    tracing::debug!(
                        conv = %conv_id,
                        "evicted conversation; corresponding ACP session not \
                         explicitly cancelled (MVP limitation, v1.2)"
                    );
                    let _ = acp; // suppress unused-warning if branch goes away
                    break;
                }
            }
        }
    });
}

async fn shutdown_signal() {
    // Cross-platform: Ctrl-C only. SIGTERM is Unix-only and Windows
    // does not have a portable equivalent, so we document Ctrl-C as the
    // supported shutdown trigger (spec § 6.5 + operating-notes.md).
    if let Err(e) = tokio::signal::ctrl_c().await {
        tracing::warn!(error = %e, "failed to install Ctrl-C handler; running until killed");
        std::future::pending::<()>().await;
    }
    tracing::info!("Ctrl-C received; shutting down gracefully");
}

/// Suppress the `SessionId` import being flagged unused once the idle
/// reaper drops its `acp` reference in MVP. Keeping this here makes the
/// future v1.2 work obvious; remove when the reaper learns to cancel.
#[allow(dead_code)]
fn _unused_session_id_marker(s: SessionId) -> SessionId {
    s
}
