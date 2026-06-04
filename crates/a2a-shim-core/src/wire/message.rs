//! A2A `Message`, `Part`, and `MessageMetadata` (spec § 4.4 + § 2.6).
//!
//! The conversation key is carried inside `metadata` rather than as a
//! top-level field; ADR 0004 fixes the key string. Any unknown metadata
//! keys are preserved on round-trip via `#[serde(flatten)]`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    User,
    Agent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Part {
    Text {
        text: String,
    },
    File {
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        bytes: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        uri: Option<String>,
    },
    Data {
        data: Value,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MessageMetadata {
    /// `x-a2a-shim/conversation` — the conversation id binding A2A traffic
    /// to one ACP session. Absent on initial sends; present after that.
    #[serde(
        rename = "x-a2a-shim/conversation",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub conversation: Option<String>,
    /// Any other metadata keys are preserved verbatim across round-trips so
    /// downstream consumers' extensions are not dropped silently.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: MessageRole,
    pub parts: Vec<Part>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<MessageMetadata>,
}
