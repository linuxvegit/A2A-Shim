//! Project-wide constants pinned by the design spec.

use std::time::Duration;

/// A2A `Message.metadata` key carrying `conversation_id` (spec §2.6, §4.4).
pub const CONVERSATION_METADATA_KEY: &str = "x-a2a-shim/conversation";

/// SSE keepalive cadence emitted by the Serve Shim (spec §2.12).
pub const SSE_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);

/// Client Shim `notifications/progress` heartbeat cadence (ADR 0003).
pub const MCP_PROGRESS_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

/// Wire-protocol revision independent of crate version.
pub const PROTOCOL_VERSION: &str = "0.1";
