//! A2A v1.0 `Message`, `Part`, and `MessageMetadata` (spec § 4.4 + § 2.6;
//! ADR 0005 wire upgrade).
//!
//! Discrimination by member presence (`#[serde(untagged)]`), no `type`
//! tag. Field renames vs v0.x legacy:
//!
//!   File: `bytes` → `raw`, `mimeType` → `mediaType`,
//!         `uri` → `url`, `name` → `filename`
//!   Data: bare `data` becomes `{data, mediaType}` (mediaType REQUIRED in v1.0)
//!
//! Variant ordering matters: `Text` first so `{"text":"..."}` binds to
//! Text and not as a malformed File missing its required `mediaType`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    User,
    Agent,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum Part {
    /// Plain text. Bound when payload contains a `text` member.
    /// MUST be first under `#[serde(untagged)]` so `{"text":"..."}` does
    /// not accidentally bind to File (which has all-optional `raw`/`url`).
    Text { text: String },
    /// Structured data. Bound when payload contains a `data` member.
    /// MUST come before File because File's required-only field
    /// (`mediaType`) is also present here; without this ordering, an
    /// `{"data":...,"mediaType":"..."}` payload would bind to File.
    Data {
        data: Value,
        #[serde(rename = "mediaType")]
        media_type: String,
    },
    /// Binary or referenced file. Bound when payload contains a
    /// `mediaType` member and (`raw` OR `url`) but no `data` member.
    File {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        #[serde(rename = "mediaType")]
        media_type: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filename: Option<String>,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MessageMetadata {
    /// `x-a2a-shim/conversation` — the conversation id binding A2A
    /// traffic to one ACP session. Absent on initial sends; present
    /// after that.
    #[serde(
        rename = "x-a2a-shim/conversation",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub conversation: Option<String>,
    /// Any other metadata keys are preserved verbatim across
    /// round-trips so downstream consumers' extensions are not dropped
    /// silently.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub role: MessageRole,
    pub parts: Vec<Part>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<MessageMetadata>,
}
