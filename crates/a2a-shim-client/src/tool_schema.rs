//! `a2a_send` tool definition advertised to the Host via MCP `tools/list`
//! (spec § 3.2-3.3).
//!
//! The schema is the public contract with Hosts (Claude Code et al.), so
//! we encode it once with `serde_json::json!` and round-trip the literal.
//! Any change here is a wire-visible change.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Minimal typed view of an MCP tool definition. Mirrors the subset of
/// the MCP `Tool` schema the Host actually consumes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolDefinition {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

/// Build the canonical `a2a_send` tool definition. Lives in a function
/// (not a static) so the description string can stay readable.
pub fn tool_definition() -> McpToolDefinition {
    McpToolDefinition {
        name: "a2a_send".into(),
        description: "Send a message to a remote A2A agent and stream back its full reply. \
             Each call is bound to a `conversation_id`; reuse the same id across \
             calls to keep state on the remote agent's side (recommended format: \
             \"<host>/<topic>\"). Returns the agent's complete reply as text plus \
             the full A2A Task object via _meta.a2aTask. Use task_id to continue \
             a Task currently waiting on input-required."
            .into(),
        input_schema: json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["endpoint", "conversation_id", "message"],
            "properties": {
                "endpoint": {
                    "type": "string",
                    "format": "uri",
                    "description": "Base URL of the remote A2A endpoint, e.g. http://127.0.0.1:7001"
                },
                "conversation_id": {
                    "type": "string",
                    "description": "Opaque conversation identifier (ADR 0004). Reusing the same id keeps the remote agent's session warm."
                },
                "message": {
                    "type": "string",
                    "description": "The message body to send to the agent. Plain text or Markdown."
                },
                "task_id": {
                    "type": "string",
                    "description": "Optional. Continuation id for an existing Task currently in input-required."
                },
                "timeout_secs": {
                    "type": "number",
                    "minimum": 1,
                    "description": "Optional per-call timeout in seconds. Defaults to the Client Shim's --stream-idle-secs."
                },
                "metadata": {
                    "type": "object",
                    "description": "Optional. Extra metadata merged into the outbound A2A Message.metadata.",
                    "additionalProperties": true
                },
                "caller_id": {
                    "type": "string",
                    "description": "Optional caller identity (v1.1 item #4). Forwarded as metadata['x-a2a-shim/caller_id']. Remote Serve Shim uses this to partition conversations across callers when [server.caller_identity].enabled."
                },
                "conversation_mode": {
                    "type": "string",
                    "enum": ["new", "continue", "auto"],
                    "default": "auto",
                    "description": "Optional conversation mode (v1.1 item #5). 'new' rejects if the conversation_id already exists (CONVERSATION_EXISTS). 'continue' rejects if it doesn't (CONVERSATION_LOST). 'auto' (default) silently reuses or creates."
                }
            }
        }),
    }
}
