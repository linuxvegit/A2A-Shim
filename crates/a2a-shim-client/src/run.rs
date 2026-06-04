//! `a2a-shim client` top-level entry (spec § 5 + § 3.1).
//!
//! Sequence:
//!   1. Init tracing. Destination is File(opts.log_file) if set, else
//!      Stderr. **Never stdout** — stdout is the MCP transport.
//!   2. Build ClientRuntime from ClientRunOpts (deadlines + heartbeat).
//!   3. Build ServerState and call mcp_server::serve_loop(stdin, stdout, state).
//!
//! The hard rule from spec § 3.4: a single stray println / log line on
//! stdout corrupts the JSON-RPC stream. The LoggingOptions type does
//! not even expose a Stdout variant by construction.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use a2a_shim_core::logging::{try_init_idempotent, LogDestination, LogFormat, LoggingOptions};

use crate::mcp_server::{serve_loop, ClientRuntime, ServerState};
use crate::outbound::OutboundDeadlines;

/// Clap-free mirror of `a2a-shim::cli::ClientOpts` so this module is
/// testable without dragging clap in.
#[derive(Debug, Clone)]
pub struct ClientRunOpts {
    pub connect_timeout_secs: u64,
    pub stream_idle_secs: u64,
    pub hard_ceiling_secs: u64,
    pub heartbeat_secs: u64,
    pub log_file: Option<PathBuf>,
    pub log_level: String,
    pub log_format: Option<String>,
}

impl Default for ClientRunOpts {
    fn default() -> Self {
        Self {
            connect_timeout_secs: 120,
            stream_idle_secs: 600,
            hard_ceiling_secs: 86400,
            heartbeat_secs: 30,
            log_file: None,
            log_level: "info".into(),
            log_format: None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("init tracing: {0}")]
    Tracing(#[from] a2a_shim_core::logging::TracingInitError),
    #[error("MCP loop io: {0}")]
    Loop(#[from] std::io::Error),
}

pub async fn run(opts: ClientRunOpts) -> Result<(), RunError> {
    // 1. Tracing — Stderr or File, never Stdout.
    let log_opts = LoggingOptions {
        level: opts.log_level.clone(),
        format: match opts.log_format.as_deref().unwrap_or("compact") {
            "json" => LogFormat::Json,
            "pretty" => LogFormat::Pretty,
            _ => LogFormat::Compact,
        },
        destination: match opts.log_file.clone() {
            Some(p) => LogDestination::File(p),
            None => LogDestination::Stderr,
        },
    };
    try_init_idempotent(log_opts)?;

    // 2. Runtime config.
    let runtime = Arc::new(ClientRuntime {
        deadlines: OutboundDeadlines {
            connect: Duration::from_secs(opts.connect_timeout_secs),
            stream_idle: Duration::from_secs(opts.stream_idle_secs),
            hard_ceiling: Duration::from_secs(opts.hard_ceiling_secs),
        },
        heartbeat_interval: Duration::from_secs(opts.heartbeat_secs),
    });
    let state = ServerState::new(runtime);

    // 3. Serve. stdin / stdout are the MCP transport.
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    serve_loop(stdin, stdout, state).await?;
    Ok(())
}
