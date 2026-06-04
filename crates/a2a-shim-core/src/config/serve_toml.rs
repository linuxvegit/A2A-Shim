//! Serve Shim TOML config loader (spec § 2.3).
//!
//! All `[server]`, `[server.conversations]`, `[timeouts]`, and `[logging]`
//! sections are optional and default per spec § 2.3. Only `[agent].command`
//! and `[agent].cwd` are required.
//!
//! `passthrough` permission strategy is *reserved* for v1.2 and rejected at
//! load time so operators get a clear error instead of mysterious runtime
//! behavior.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ServeConfigError {
    #[error("TOML parse error: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("`passthrough` permission strategy is reserved for v1.2 and is not implemented in MVP. Use `auto_approve` or `auto_reject`.")]
    PassthroughNotImplemented,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServeConfig {
    #[serde(default)]
    pub server: ServerConfig,
    pub agent: AgentConfig,
    #[serde(default)]
    pub timeouts: TimeoutsConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServerConfig {
    #[serde(default = "d_listen")]
    pub listen: String,
    #[serde(default)]
    pub advertised_endpoint: Option<String>,
    #[serde(default = "d_card_path")]
    pub agent_card_path: String,
    /// Per-Part body cap in bytes (ADR 0006). Default 10 MiB.
    #[serde(default = "d_max_part_bytes")]
    pub max_part_bytes: usize,
    #[serde(default)]
    pub conversations: ConversationsConfig,
    #[serde(default)]
    pub persistence: PersistenceConfig,
    #[serde(default)]
    pub caller_identity: CallerIdentityConfig,
    #[serde(default)]
    pub push_notifications: PushNotificationsConfig,
}
impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            listen: d_listen(),
            advertised_endpoint: None,
            agent_card_path: d_card_path(),
            conversations: Default::default(),
            persistence: Default::default(),
            caller_identity: Default::default(),
            push_notifications: Default::default(),
            max_part_bytes: d_max_part_bytes(),
        }
    }
}


/// SQLite-backed persistence settings (ADR 0007 / spec § 4 item #3).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PersistenceConfig {
    /// Default ON in v1.1: conversations + tasks survive Serve restart.
    /// Set false to opt back into v0.1.0 purely-in-memory behavior.
    #[serde(default = "d_persistence_enabled")]
    pub enabled: bool,
    /// SQLite file path. Default `./a2a-shim.db`. Ignored when disabled.
    #[serde(default = "d_persistence_path")]
    pub path: Option<std::path::PathBuf>,
}
impl Default for PersistenceConfig {
    fn default() -> Self {
        Self {
            enabled: d_persistence_enabled(),
            path: d_persistence_path(),
        }
    }
}
fn d_persistence_enabled() -> bool {
    true
}
fn d_persistence_path() -> Option<std::path::PathBuf> {
    Some(std::path::PathBuf::from("./a2a-shim.db"))
}

/// Caller-identity partitioning (spec § 5 item #4).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CallerIdentityConfig {
    /// When false (default): preserves v0.1.0 behavior — ConversationMap
    /// keyed only by conversation_id; caller_id never partitions.
    #[serde(default = "d_false")]
    pub enabled: bool,
    /// Fallback when no header or metadata caller_id supplied.
    #[serde(default = "d_caller_anonymous")]
    pub default_caller_id: String,
    /// When true (default), honor the X-A2A-Caller-Id request header.
    /// Set false if the shim is exposed directly to untrusted callers
    /// without an authenticating reverse proxy in front.
    #[serde(default = "d_true")]
    pub trust_header: bool,
}
impl Default for CallerIdentityConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            default_caller_id: d_caller_anonymous(),
            trust_header: true,
        }
    }
}
fn d_false() -> bool {
    false
}
fn d_true() -> bool {
    true
}
fn d_caller_anonymous() -> String {
    "anonymous".into()
}

/// Push-notification settings (ADR 0008).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PushNotificationsConfig {
    /// Default ON in v1.1. When false, the four push-notif methods
    /// return PUSH_NOTIFICATIONS_NOT_SUPPORTED (-32030) and the
    /// AgentCard advertises pushNotifications: false.
    #[serde(default = "d_true")]
    pub enabled: bool,
    #[serde(default = "d_max_attempts")]
    pub max_attempts: usize,
    #[serde(default = "d_backoff_base_secs")]
    pub backoff_base_secs: u64,
    #[serde(default = "d_backoff_factor")]
    pub backoff_factor: u64,
    /// Delete the config after M consecutive permanent failures
    /// (4xx other than 408/429). ADR 0008.
    #[serde(default = "d_perm_failure_threshold")]
    pub permanent_failure_threshold: u32,
    #[serde(default = "d_http_connect_secs")]
    pub http_connect_timeout_secs: u64,
    #[serde(default = "d_http_req_secs")]
    pub http_request_timeout_secs: u64,
}
impl Default for PushNotificationsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_attempts: d_max_attempts(),
            backoff_base_secs: d_backoff_base_secs(),
            backoff_factor: d_backoff_factor(),
            permanent_failure_threshold: d_perm_failure_threshold(),
            http_connect_timeout_secs: d_http_connect_secs(),
            http_request_timeout_secs: d_http_req_secs(),
        }
    }
}
fn d_max_attempts() -> usize {
    3
}
fn d_backoff_base_secs() -> u64 {
    1
}
fn d_backoff_factor() -> u64 {
    3
}
fn d_perm_failure_threshold() -> u32 {
    10
}
fn d_http_connect_secs() -> u64 {
    5
}
fn d_http_req_secs() -> u64 {
    10
}
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ConversationsConfig {
    #[serde(default = "d_idle_secs")]
    pub idle_secs: u64,
    #[serde(default = "d_max_active")]
    pub max_active: u32,
}
impl Default for ConversationsConfig {
    fn default() -> Self {
        Self {
            idle_secs: d_idle_secs(),
            max_active: d_max_active(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AgentConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub cwd: PathBuf,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub card: AgentCardConfig,
    #[serde(default)]
    pub permissions: PermissionsConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct AgentCardConfig {
    #[serde(default = "d_card_name")]
    pub name: String,
    #[serde(default = "d_card_desc")]
    pub description: String,
    #[serde(default = "d_card_version")]
    pub version: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PermissionsConfig {
    #[serde(default)]
    pub strategy: PermissionStrategy,
    #[serde(default)]
    pub deny_tool_kinds: Vec<String>,
}
impl Default for PermissionsConfig {
    fn default() -> Self {
        Self {
            strategy: PermissionStrategy::AutoApprove,
            deny_tool_kinds: vec![],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PermissionStrategy {
    #[default]
    AutoApprove,
    AutoReject,
    Passthrough,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TimeoutsConfig {
    #[serde(default = "d_sync_idle")]
    pub agent_sync_idle_secs: u64,
    #[serde(default = "d_stream_idle")]
    pub agent_stream_idle_secs: u64,
    #[serde(default = "d_hard_ceil")]
    pub agent_hard_ceiling_secs: u64,
    #[serde(default = "d_input_wait")]
    pub input_required_wait_secs: u64,
    #[serde(default = "d_shutdown")]
    pub shutdown_grace_secs: u64,
}
impl Default for TimeoutsConfig {
    fn default() -> Self {
        Self {
            agent_sync_idle_secs: d_sync_idle(),
            agent_stream_idle_secs: d_stream_idle(),
            agent_hard_ceiling_secs: d_hard_ceil(),
            input_required_wait_secs: d_input_wait(),
            shutdown_grace_secs: d_shutdown(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LoggingConfig {
    #[serde(default = "d_log_level")]
    pub level: String,
    #[serde(default = "d_log_format")]
    pub format: String,
    #[serde(default)]
    pub file: Option<PathBuf>,
}
impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: d_log_level(),
            format: d_log_format(),
            file: None,
        }
    }
}

fn d_listen() -> String {
    "127.0.0.1:7001".into()
}
fn d_card_path() -> String {
    "/.well-known/agent.json".into()
}
fn d_idle_secs() -> u64 {
    86400
}
fn d_max_active() -> u32 {
    64
}
fn d_max_part_bytes() -> usize {
    10 * 1024 * 1024 // 10 MiB
}
fn d_card_name() -> String {
    "a2a-shim-serve".into()
}
fn d_card_desc() -> String {
    "ACP Agent exposed as A2A endpoint via a2a-shim".into()
}
fn d_card_version() -> String {
    "0.1.0".into()
}
fn d_sync_idle() -> u64 {
    120
}
fn d_stream_idle() -> u64 {
    600
}
fn d_hard_ceil() -> u64 {
    86400
}
fn d_input_wait() -> u64 {
    86400
}
fn d_shutdown() -> u64 {
    5
}
fn d_log_level() -> String {
    "info".into()
}
fn d_log_format() -> String {
    "compact".into()
}

impl ServeConfig {
    pub fn from_toml_str(s: &str) -> Result<Self, ServeConfigError> {
        let cfg: ServeConfig = toml::from_str(s)?;
        if cfg.agent.permissions.strategy == PermissionStrategy::Passthrough {
            return Err(ServeConfigError::PassthroughNotImplemented);
        }
        Ok(cfg)
    }
}
