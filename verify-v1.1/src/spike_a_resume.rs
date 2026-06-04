//! v1.1 Phase 0 Spike A — does `claude-agent-acp@0.40.0` actually
//! advertise and answer `session/load` and `session/resume`?
//!
//! The schema crate (agent-client-protocol-schema = 0.13.5) ships
//! LoadSessionRequest / ResumeSessionRequest + the matching capability
//! flags (verified statically by reading the crate source on
//! 2026-06-04). What this probe still needs to answer:
//!
//!   1. Does the agent set `load_session: true` or
//!      `session_capabilities.resume: Some(_)` in InitializeResponse?
//!   2. If load_session: does `session/load` against a fresh
//!      `session/new`-minted session id succeed?
//!   3. If resume: does `session/resume` against the same id succeed?
//!
//! The answer drives v1.1 item #3 (persistence). Three observable outcomes:
//!   * Both flags + both methods work → full restart-resume story.
//!   * Only flags advertised (or one of them) → partial; degrade gracefully.
//!   * Neither → persistence becomes forensic-only (read-back of past
//!     Task snapshots; no live re-attach).

use agent_client_protocol::schema::{
    ContentBlock, InitializeRequest, LoadSessionRequest, NewSessionRequest, PromptRequest,
    ProtocolVersion, ResumeSessionRequest, TextContent,
};
use agent_client_protocol::{AcpAgent, Agent, ConnectionTo};
use std::path::PathBuf;
use std::str::FromStr;

const DEFAULT_AGENT_CMD_FRAGMENT: &str =
    "@agentclientprotocol/claude-agent-acp/dist/index.js";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let agent_command = resolve_agent_command();
    eprintln!("[spawn] {agent_command}");
    let agent = AcpAgent::from_str(&agent_command)?;

    agent_client_protocol::Client
        .builder()
        .on_receive_notification(
            async move |n: agent_client_protocol::schema::SessionNotification, _cx| {
                let summary = format!("{:?}", n.update);
                eprintln!(
                    "[notif] {}",
                    if summary.len() > 200 {
                        format!("{}…", &summary[..200])
                    } else {
                        summary
                    }
                );
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .connect_with(agent, |conn: ConnectionTo<Agent>| async move {
            run_probe(conn).await
        })
        .await?;

    eprintln!("\n=== spike_a_resume exited cleanly ===");
    Ok(())
}

async fn run_probe(conn: ConnectionTo<Agent>) -> agent_client_protocol::Result<()> {
    // 1. Initialize and read the capability flags.
    eprintln!("\n[init] sending InitializeRequest::new(V1) (defaults)");
    let init = conn
        .send_request(InitializeRequest::new(ProtocolVersion::V1))
        .block_task()
        .await?;
    eprintln!("[init] agent_info       = {:?}", init.agent_info);
    eprintln!("[init] agent_capabs FULL = {:?}", init.agent_capabilities);
    eprintln!();
    let caps = &init.agent_capabilities;
    let load_advertised = caps.load_session;
    // SessionCapabilities and SessionResumeCapabilities are optional sub-
    // structures; we only care that resume is Some(_) to call it.
    let resume_advertised = caps.session_capabilities.resume.is_some();
    eprintln!(
        "[caps] load_session advertised  = {}",
        yn(load_advertised)
    );
    eprintln!(
        "[caps] session.resume advertised = {}",
        yn(resume_advertised)
    );

    // 2. Open a fresh session so we have an id to test load/resume against.
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    eprintln!("\n[session/new] cwd = {}", cwd.display());
    let new_resp = conn
        .send_request(NewSessionRequest::new(cwd.clone()))
        .block_task()
        .await?;
    let session_id = new_resp.session_id;
    eprintln!("[session/new] session_id = {session_id:?}");

    // Do one tiny prompt so the session has some history to load.
    eprintln!("\n[prompt] one trivial prompt so the session has history");
    let prompt = PromptRequest::new(
        session_id.clone(),
        vec![ContentBlock::Text(TextContent::new("Say 'spike-a probing'."))],
    );
    match conn.send_request(prompt).block_task().await {
        Ok(r) => eprintln!("[prompt] stop_reason = {:?}", r.stop_reason),
        Err(e) => eprintln!("[prompt] FAILED (continuing): {e:?}"),
    }

    // 3. Try session/load against the same id.
    eprintln!("\n[session/load] attempting against id={session_id:?}");
    let load_req = LoadSessionRequest::new(session_id.clone(), cwd.clone());
    match conn.send_request(load_req).block_task().await {
        Ok(resp) => {
            eprintln!("[session/load] OK — response = {resp:?}");
        }
        Err(e) => {
            eprintln!("[session/load] REJECTED — {e:?}");
        }
    }

    // 4. Try session/resume against the same id.
    eprintln!("\n[session/resume] attempting against id={session_id:?}");
    let resume_req = ResumeSessionRequest::new(session_id.clone(), cwd.clone());
    match conn.send_request(resume_req).block_task().await {
        Ok(resp) => {
            eprintln!("[session/resume] OK — response = {resp:?}");
        }
        Err(e) => {
            eprintln!("[session/resume] REJECTED — {e:?}");
        }
    }

    // 5. Final summary.
    eprintln!("\n==================================================================");
    eprintln!("SPIKE A SUMMARY (paste into verify-v1.1/REPORT.md)");
    eprintln!("==================================================================");
    eprintln!("load_session advertised   : {}", yn(load_advertised));
    eprintln!("session.resume advertised : {}", yn(resume_advertised));
    eprintln!("session/load call         : see [session/load] line above");
    eprintln!("session/resume call       : see [session/resume] line above");
    eprintln!("==================================================================");

    Ok(())
}

/// Resolve agent launch command. Same Windows-compat trick as
/// verify/src/main.rs in v0.1.0 — npm's .cmd shim defeats Command::new
/// so we run node + abs path to the JS entry.
fn resolve_agent_command() -> String {
    if let Ok(c) = std::env::var("A2A_VERIFY_AGENT_CMD") {
        if !c.trim().is_empty() {
            return c;
        }
    }
    if let Some(js_entry) = find_js_entry() {
        return format!("node \"{}\"", js_entry.display());
    }
    "claude-agent-acp".to_string()
}

fn find_js_entry() -> Option<PathBuf> {
    let out = std::process::Command::new(if cfg!(windows) { "npm.cmd" } else { "npm" })
        .args(["root", "-g"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let root = std::str::from_utf8(&out.stdout).ok()?.trim();
    let p = PathBuf::from(root).join(DEFAULT_AGENT_CMD_FRAGMENT);
    p.exists().then_some(p)
}

fn yn(b: bool) -> &'static str {
    if b {
        "YES"
    } else {
        "NO"
    }
}
