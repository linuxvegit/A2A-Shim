//! Stdio MCP server exposing the `a2a_send` tool.
//!
//! Module surface filled out across Phase 3 tasks 28-33.

pub mod tool_schema;
pub mod mcp_server;
pub mod outbound;
pub mod heartbeat;
pub mod cancellation;
pub mod render;
pub mod call_handler;
