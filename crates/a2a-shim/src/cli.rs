//! Command-line surface for the `a2a-shim` binary.
//!
//! Two subcommands: `serve` (wraps an ACP Agent and exposes A2A over HTTP)
//! and `client` (runs a stdio MCP server exposing the `a2a_send` tool).
//!
//! Both subcommands accept config via flags **and** environment variables
//! (`A2A_SHIM_*`). Flags win over env vars; env vars win over defaults.
//! Full surface is spec § 5.1-5.3.

use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "a2a-shim",
    version,
    about = "Bidirectional shim between ACP agents and Google A2A protocol"
)]
pub struct Cli {
    /// Increase log verbosity (`-v` info, `-vv` debug, `-vvv` trace).
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,
    /// Suppress all logs below WARN.
    #[arg(short, long, global = true)]
    pub quiet: bool,
    /// Log format.
    #[arg(long, global = true, default_value = "compact", value_parser = ["compact", "json", "pretty"])]
    pub log_format: String,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Spawn an ACP Agent and serve A2A over HTTP.
    Serve(ServeOpts),
    /// Run as stdio MCP server exposing `a2a_send`.
    Client(ClientOpts),
}

#[derive(Args, Debug)]
pub struct ServeOpts {
    /// Path to the TOML config file. Most fields are also override-able via flags.
    #[arg(short, long, env = "A2A_SHIM_CONFIG")]
    pub config: Option<PathBuf>,
    /// Bind address, e.g. `127.0.0.1:7001`.
    #[arg(long)]
    pub listen: Option<String>,
    /// URL to advertise in `/.well-known/agent.json` if different from the bind address.
    #[arg(long)]
    pub advertised_endpoint: Option<String>,
    /// Whitespace-split command line for the wrapped ACP Agent (e.g. `claude-agent-acp`).
    /// First token becomes `[agent].command`; the rest becomes `[agent].args`.
    #[arg(long)]
    pub spawn: Option<String>,
    /// Working directory for the spawned Agent.
    #[arg(long)]
    pub cwd: Option<PathBuf>,
    /// `auto_approve` or `auto_reject`. `passthrough` is reserved for v1.2 and rejected.
    #[arg(long, value_parser = ["auto_approve", "auto_reject"])]
    pub permission_strategy: Option<String>,
    /// Write logs here instead of stderr.
    #[arg(long)]
    pub log_file: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct ClientOpts {
    /// HTTP connect timeout for remote A2A endpoints, in seconds.
    #[arg(long, default_value_t = 120, env = "A2A_SHIM_CONNECT_TIMEOUT_SECS")]
    pub connect_timeout_secs: u64,
    /// Per-stream idle timeout. If no SSE event arrives for this many seconds, abort.
    #[arg(long, default_value_t = 600, env = "A2A_SHIM_STREAM_IDLE_SECS")]
    pub stream_idle_secs: u64,
    /// Absolute ceiling per `a2a_send` call (default 24 h).
    #[arg(long, default_value_t = 86400, env = "A2A_SHIM_HARD_CEILING_SECS")]
    pub hard_ceiling_secs: u64,
    /// MCP `notifications/progress` heartbeat cadence (ADR 0003).
    #[arg(long, default_value_t = 30, env = "A2A_SHIM_HEARTBEAT_SECS")]
    pub heartbeat_secs: u64,
    /// Path to write logs. Logs NEVER go to stdout — stdout is the MCP transport.
    #[arg(long, env = "A2A_SHIM_LOG_FILE")]
    pub log_file: Option<PathBuf>,
    /// Log level (`error|warn|info|debug|trace`), or a richer `tracing_subscriber::EnvFilter` directive.
    #[arg(long, default_value = "info", env = "A2A_SHIM_LOG_LEVEL")]
    pub log_level: String,
}
