//! A2A HTTP server wrapping a spawned ACP Agent.
//!
//! Module surface is built up incrementally across Phase 2 tasks.

pub mod sse_sink;
pub mod conversation;
pub mod acp_client;
