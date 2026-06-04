//! Build the `/.well-known/agent.json` payload for the Serve Shim
//! (spec § 2.10 + § 4.7).
//!
//! Pure function: takes the loaded `ServeConfig` and the actual bound
//! `SocketAddr`-style string the listener wound up on, and produces the
//! typed `AgentCard` for serialization.
//!
//! URL precedence: `cfg.server.advertised_endpoint` wins if set (operator
//! is fronting the Shim behind a port-forward or HTTPS terminator).
//! Otherwise fall back to `http://<bound>/`.

use a2a_shim_core::config::serve_toml::ServeConfig;
use a2a_shim_core::wire::card::{
    AgentCapabilities, AgentCard, AgentCardMetadata, ConversationsCapability,
};

/// Default content-type modes — text/plain only in MVP per spec § 2.10.
const DEFAULT_MODES: &[&str] = &["text/plain"];

pub fn build_agent_card(cfg: &ServeConfig, bound: &str) -> AgentCard {
    let url = cfg
        .server
        .advertised_endpoint
        .clone()
        .unwrap_or_else(|| format!("http://{bound}/"));

    AgentCard {
        name: cfg.agent.card.name.clone(),
        description: cfg.agent.card.description.clone(),
        version: cfg.agent.card.version.clone(),
        url,
        capabilities: AgentCapabilities {
            streaming: true,
            push_notifications: false,
            state_transition_history: true,
        },
        default_input_modes: DEFAULT_MODES.iter().map(|s| s.to_string()).collect(),
        default_output_modes: DEFAULT_MODES.iter().map(|s| s.to_string()).collect(),
        skills: Vec::new(),
        metadata: Some(AgentCardMetadata {
            conversations: Some(ConversationsCapability {
                supported: true,
                metadata_key: a2a_shim_core::constants::CONVERSATION_METADATA_KEY.to_string(),
                context_id_alias: true,
                max_active: cfg.server.conversations.max_active,
                idle_secs: cfg.server.conversations.idle_secs,
            }),
        }),
    }
}
