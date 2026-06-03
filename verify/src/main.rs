//! Phase 0 reality-check probe for A2A-Shim.
//!
//! This is throwaway code — clarity over elegance. It answers the eight V
//! questions in `docs/superpowers/specs/2026-06-03-a2a-shim-design.md` §6.6
//! by exercising `agent-client-protocol = "0.13"` against the reference ACP
//! Agent `claude-agent-acp`. Output goes to stderr; `run.log` captures it.
//!
//! What we exercise (and the V it informs):
//!   V1, V3 — initialize + new-session + prompt against `claude-agent-acp`
//!            with the *default* (fs-disabled, terminal=false) capabilities.
//!   V6, V7 — log every `SessionNotification` so we can eyeball the kinds
//!            of updates the agent emits (permission frequency, any
//!            `elicitation/create` traffic).
//!   V8     — after the first prompt, send `SessionCancelNotification`
//!            and try a second `PromptRequest` on the same session id.
//!
//! V2 (which MCP-server crate to use for the Client Shim) is answered by the
//! crate's own `mcp_server` module — no runtime check needed, just inspection.
//! V4 / V5 (MCP `_meta.progressToken` behavior in Claude Code) are answered
//! against Claude Code, not against this agent; they are recorded as DEFERRED
//! here and verified in Phase 3 integration.

use agent_client_protocol::schema::{
    CancelNotification, ContentBlock, InitializeRequest, NewSessionRequest, PromptRequest,
    ProtocolVersion, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome, SessionNotification, TextContent,
};
use agent_client_protocol::{AcpAgent, Agent, ConnectionTo};
use std::path::PathBuf;
use std::str::FromStr;

/// Override at runtime: `set A2A_VERIFY_AGENT_CMD=python my_agent.py` etc.
/// Default resolves the `claude-agent-acp` JS entry under `npm root -g` and
/// runs it via `node`, which sidesteps Windows's `.cmd` shim that defeats
/// `tokio::process::Command`'s direct PATH lookup.
const DEFAULT_AGENT_CMD_FRAGMENT: &str = "@agentclientprotocol/claude-agent-acp/dist/index.js";
const PROMPT_ONE: &str = "What is 2+2? Reply with just the number, nothing else.";
const PROMPT_TWO: &str = "And what is 3+3? Reply with just the number.";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    eprintln!("=== Phase 0 reality-check probe ===");
    eprintln!("agent-client-protocol crate: 0.13 (resolved at build time)");
    eprintln!();

    let agent_command = resolve_agent_command();
    eprintln!("[spawn] command line: {agent_command}");
    let agent = AcpAgent::from_str(&agent_command)?;
    eprintln!("[spawn] AcpAgent parsed");

    agent_client_protocol::Client
        .builder()
        .on_receive_notification(
            async move |n: SessionNotification, _cx| {
                // V6/V7 evidence: print every session update verbatim.
                eprintln!("[notification] {:?}", n.update);
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            async move |req: RequestPermissionRequest, responder, _conn| {
                // V6 evidence: log the permission request shape, then auto-approve.
                eprintln!("[permission] auto-approving: {req:?}");
                let id = req.options.first().map(|o| o.option_id.clone());
                if let Some(id) = id {
                    responder.respond(RequestPermissionResponse::new(
                        RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(id)),
                    ))
                } else {
                    eprintln!("[permission] no options offered — cancelling");
                    responder.respond(RequestPermissionResponse::new(
                        RequestPermissionOutcome::Cancelled,
                    ))
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(agent, |conn: ConnectionTo<Agent>| async move {
            run_probe(conn).await
        })
        .await?;

    eprintln!();
    eprintln!("=== probe exited cleanly ===");
    Ok(())
}

async fn run_probe(conn: ConnectionTo<Agent>) -> agent_client_protocol::Result<()> {
    // --- Step 1: initialize (V1, V3) ---
    eprintln!("[init] sending InitializeRequest::new(V1) with default capabilities");
    eprintln!(
        "       (ADR 0001: defaults give fs.{{read,write}}TextFile=false, terminal=false)"
    );
    let init = conn
        .send_request(InitializeRequest::new(ProtocolVersion::V1))
        .block_task()
        .await?;
    eprintln!("[init] OK — agent_info = {:?}", init.agent_info);
    eprintln!("[init] agent capabilities = {:?}", init.agent_capabilities);

    // --- Step 2: new session ---
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    eprintln!("[session/new] cwd = {}", cwd.display());
    let new_resp = conn
        .send_request(NewSessionRequest::new(cwd.clone()))
        .block_task()
        .await?;
    let session_id = new_resp.session_id;
    eprintln!("[session/new] OK — session_id = {session_id:?}");

    // --- Step 3: first prompt (V1, V6, V7) ---
    eprintln!("[prompt #1] {PROMPT_ONE:?}");
    let prompt1 = conn
        .send_request(PromptRequest::new(
            session_id.clone(),
            vec![ContentBlock::Text(TextContent::new(PROMPT_ONE))],
        ))
        .block_task()
        .await?;
    eprintln!("[prompt #1] stop_reason = {:?}", prompt1.stop_reason);

    // --- Step 4: cancel, then second prompt on same session id (V8) ---
    eprintln!("[cancel] sending SessionCancelNotification (V8 probe setup)");
    match conn.send_notification(CancelNotification::new(session_id.clone())) {
        Ok(()) => eprintln!("[cancel] notification dispatched"),
        Err(e) => eprintln!("[cancel] notification send error: {e:?}"),
    }

    // Brief breath so the agent has a chance to act on the cancel.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    eprintln!("[prompt #2] (V8) {PROMPT_TWO:?}");
    let prompt2 = conn
        .send_request(PromptRequest::new(
            session_id.clone(),
            vec![ContentBlock::Text(TextContent::new(PROMPT_TWO))],
        ))
        .block_task()
        .await;
    match prompt2 {
        Ok(r) => {
            eprintln!("[prompt #2] V8 PASS — agent accepted reprompt; stop_reason = {:?}", r.stop_reason);
        }
        Err(e) => {
            eprintln!("[prompt #2] V8 FAIL — agent rejected reprompt: {e:?}");
        }
    }

    Ok(())
}

/// Resolve the agent launch command.
///
/// Priority: `A2A_VERIFY_AGENT_CMD` env var (verbatim, shell-split by the SDK)
/// → `node <npm-root>/<DEFAULT_AGENT_CMD_FRAGMENT>` when npm and the package
/// are present → bare `claude-agent-acp` (works on Unix where there is no
/// `.cmd` shim issue).
fn resolve_agent_command() -> String {
    if let Ok(c) = std::env::var("A2A_VERIFY_AGENT_CMD") {
        if !c.trim().is_empty() {
            return c;
        }
    }
    if let Some(js_entry) = find_js_entry() {
        // shell-words splits on whitespace; quote the path so spaces survive.
        return format!("node \"{}\"", js_entry.display());
    }
    "claude-agent-acp".to_string()
}

fn find_js_entry() -> Option<std::path::PathBuf> {
    let out = std::process::Command::new(if cfg!(windows) { "npm.cmd" } else { "npm" })
        .args(["root", "-g"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let root = std::str::from_utf8(&out.stdout).ok()?.trim();
    let path = std::path::PathBuf::from(root).join(DEFAULT_AGENT_CMD_FRAGMENT);
    path.exists().then_some(path)
}
