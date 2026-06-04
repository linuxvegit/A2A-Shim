//! Wire-format types for A2A and JSON-RPC traffic.
//!
//! Spec § 4. Each submodule is self-contained and round-trip tested.

pub mod envelope;
pub mod message;
pub mod task;
pub mod methods;
pub mod sse;
pub mod card;
