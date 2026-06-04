//! A2A HTTP server wrapping a spawned ACP Agent.
//!
//! Module surface is built up incrementally across Phase 2 tasks.

pub mod acp_client;
pub mod agent_card;
pub mod bridge;
pub mod conversation;
pub mod elicitation;
pub mod http;
pub mod metrics;
pub mod permission;
pub mod persistence;
pub mod push_delivery;
pub mod run;
pub mod sse_sink;
pub mod task_registry;
pub mod translate;
