//! Shared logic for the A2A-Shim project.
//!
//! This crate hosts wire types, error codes, timeout helpers, config loaders,
//! and tracing-subscriber init. Both `a2a-shim-serve` and `a2a-shim-client`
//! depend on it; the top-level `a2a-shim` binary re-exports nothing.

pub mod constants;
pub mod wire;
pub mod error;
pub mod timeout;
pub mod config;
pub mod logging;
