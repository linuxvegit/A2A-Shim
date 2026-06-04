//! A2A HTTP server wrapping a spawned ACP Agent.
//!
//! Module surface is built up incrementally across Phase 2 tasks.

pub mod sse_sink;
pub mod conversation;
pub mod acp_client;
pub mod task_registry;
pub mod permission;
pub mod elicitation;
pub mod agent_card;
pub mod bridge;
pub mod http;
pub mod run;
