//! Normalized error envelope returned by Client Shim tool results when the
//! remote A2A call fails (spec § 3.8). Distinct from JSON-RPC error codes
//! because MCP `tools/call` results carry an `isError: true` payload, not a
//! JSON-RPC error, and the Host UI renders the payload directly.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    NetworkError,
    RemoteTimeout,
    RemoteFailed,
    RemoteCanceled,
    ProtocolError,
    InvalidRequest,
    ConcurrentCallNotSupported,
    // v1.1 additions
    ConversationLost,
    ConversationExists,
    PushNotificationsNotSupported,
    InvalidPushNotificationConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalizedError {
    pub kind: ErrorKind,
    pub message: String,
    pub remote_task_id: Option<String>,
}

/// Wraps `NormalizedError` under an outer `"error"` key so the on-wire shape is
/// `{"error": {...}}`. This is what gets stuffed into the MCP tool result
/// `_meta.error` field.
#[derive(Debug, Clone)]
pub struct NormalizedErrorEnvelope(pub NormalizedError);

impl Serialize for NormalizedErrorEnvelope {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut m = ser.serialize_map(Some(1))?;
        m.serialize_entry("error", &self.0)?;
        m.end()
    }
}

impl<'de> Deserialize<'de> for NormalizedErrorEnvelope {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Wrap {
            error: NormalizedError,
        }
        Ok(NormalizedErrorEnvelope(Wrap::deserialize(de)?.error))
    }
}
