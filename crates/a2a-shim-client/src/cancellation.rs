//! In-flight call cancellation registry (spec § 3.9).
//!
//! Keyed by the MCP request id of the inbound `tools/call`. The MCP
//! loop registers a fresh `CancellationToken` per call; the
//! `notifications/cancelled { requestId }` notification looks it up and
//! cancels. The call handler races its work against the token via
//! `tokio::select!`.

use parking_lot::Mutex;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Default)]
pub struct CancellationRegistry {
    inner: Arc<Mutex<HashMap<String, CancellationToken>>>,
}

impl CancellationRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a fresh token for `id`. If something is already keyed
    /// here we replace it — duplicate ids on the wire are a Host bug we
    /// cannot fix.
    pub fn register(&self, id: &Value) -> CancellationToken {
        let key = id.to_string();
        let token = CancellationToken::new();
        self.inner.lock().insert(key, token.clone());
        token
    }

    /// Drop the registration. Idempotent.
    pub fn unregister(&self, id: &Value) {
        let key = id.to_string();
        self.inner.lock().remove(&key);
    }

    /// Cancel the call keyed by `id`. No-op if unknown.
    pub fn cancel(&self, id: &Value) {
        let key = id.to_string();
        if let Some(tok) = self.inner.lock().remove(&key) {
            tok.cancel();
        }
    }
}
