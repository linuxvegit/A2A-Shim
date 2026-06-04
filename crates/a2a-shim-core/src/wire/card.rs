//! AgentCard for `/.well-known/agent.json` (spec § 2.10 + § 4.7).
//!
//! Carries A2A-Shim-specific `x-a2a-shim/conversations` capability under
//! `metadata` so vanilla A2A clients ignore it gracefully while
//! conversation-aware clients can opt in.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCard {
    pub name: String,
    pub description: String,
    pub version: String,
    pub url: String,
    pub capabilities: AgentCapabilities,
    #[serde(rename = "defaultInputModes")]
    pub default_input_modes: Vec<String>,
    #[serde(rename = "defaultOutputModes")]
    pub default_output_modes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<AgentSkill>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<AgentCardMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCapabilities {
    pub streaming: bool,
    #[serde(rename = "pushNotifications")]
    pub push_notifications: bool,
    #[serde(rename = "stateTransitionHistory")]
    pub state_transition_history: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSkill {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCardMetadata {
    #[serde(
        rename = "x-a2a-shim/conversations",
        skip_serializing_if = "Option::is_none"
    )]
    pub conversations: Option<ConversationsCapability>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationsCapability {
    pub supported: bool,
    #[serde(rename = "metadataKey")]
    pub metadata_key: String,
    #[serde(rename = "contextIdAlias")]
    pub context_id_alias: bool,
    #[serde(rename = "maxActive")]
    pub max_active: u32,
    #[serde(rename = "idleSecs")]
    pub idle_secs: u64,
}
