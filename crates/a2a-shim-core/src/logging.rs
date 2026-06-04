//! Centralized `tracing_subscriber` init.
//!
//! Client Shim hard rule: logs MUST NEVER go to stdout — stdout is the MCP
//! transport and a stray log line corrupts the wire. `LogDestination` only
//! exposes `Stderr` and `File` variants for that reason.
//!
//! Idempotency is delegated to `tracing_subscriber::try_init`, which returns
//! `Err(_)` if a global subscriber is already installed. We map that into
//! `TracingInitError::AlreadyInit`, and `try_init` returns `Ok(())` if the
//! caller has explicitly suppressed it via `try_init_idempotent`.

use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::Arc;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

#[derive(Debug, Clone)]
pub struct LoggingOptions {
    pub level: String,
    pub format: LogFormat,
    pub destination: LogDestination,
}

impl Default for LoggingOptions {
    fn default() -> Self {
        Self {
            level: "info".into(),
            format: LogFormat::Compact,
            destination: LogDestination::Stderr,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum LogFormat {
    Compact,
    Json,
    Pretty,
}

#[derive(Debug, Clone)]
pub enum LogDestination {
    Stderr,
    File(PathBuf),
}

/// Install the global subscriber. Returns `Err(AlreadyInit)` if one is
/// already installed; use [`try_init_idempotent`] if that should be a no-op
/// (typical for unit tests that may run in any order).
pub fn try_init(opts: LoggingOptions) -> Result<(), TracingInitError> {
    let filter =
        EnvFilter::try_new(&opts.level).map_err(|e| TracingInitError::Filter(e.to_string()))?;
    let writer = match &opts.destination {
        LogDestination::Stderr => BoxedWriter::Stderr,
        LogDestination::File(p) => {
            let f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)
                .map_err(|e| TracingInitError::OpenFile(p.clone(), e.to_string()))?;
            BoxedWriter::File(Arc::new(Mutex::new(f)))
        }
    };
    let layer = fmt::layer().with_writer(writer);
    let reg = tracing_subscriber::registry().with(filter);
    let r = match opts.format {
        LogFormat::Compact => reg.with(layer.compact()).try_init(),
        LogFormat::Json => reg.with(layer.json()).try_init(),
        LogFormat::Pretty => reg.with(layer.pretty()).try_init(),
    };
    r.map_err(|e| TracingInitError::AlreadyInit(e.to_string()))
}

/// Like [`try_init`] but treats "already initialized" as success — handy
/// when the same test binary may invoke init multiple times across tests.
pub fn try_init_idempotent(opts: LoggingOptions) -> Result<(), TracingInitError> {
    match try_init(opts) {
        Ok(()) | Err(TracingInitError::AlreadyInit(_)) => Ok(()),
        Err(e) => Err(e),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TracingInitError {
    #[error("invalid log filter: {0}")]
    Filter(String),
    #[error("failed to open log file {0}: {1}")]
    OpenFile(PathBuf, String),
    #[error("tracing already initialized: {0}")]
    AlreadyInit(String),
}

#[derive(Clone)]
enum BoxedWriter {
    Stderr,
    File(Arc<Mutex<std::fs::File>>),
}

impl<'a> fmt::MakeWriter<'a> for BoxedWriter {
    type Writer = Box<dyn std::io::Write + Send>;
    fn make_writer(&'a self) -> Self::Writer {
        match self {
            BoxedWriter::Stderr => Box::new(std::io::stderr()),
            // parking_lot::Mutex has no poisoning, so .lock() returns the guard directly.
            BoxedWriter::File(a) => Box::new(a.lock().try_clone().expect("clone fd")),
        }
    }
}
