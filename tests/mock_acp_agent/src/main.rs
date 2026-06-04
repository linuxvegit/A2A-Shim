//! Scripted ACP-over-stdio mock agent for `a2a-shim-serve` integration tests.
//!
//! Not shipped. Speaks the wire format via `agent-client-protocol = "0.13"`
//! as an Agent role so wire-shape drift fails the build, not at test time.
//!
//! Scripts:
//!   * `happy`      — one chunk "4" then EndTurn (used since v0.1.0).
//!   * `streamy`    — 5 small chunks 50ms apart for G2 streaming tests.
//!   * `multimodal` — one Text + one Image ContentBlock then EndTurn.
//!   * `resumable`  — same as happy on first prompt; accepts LoadSession
//!                    against any previously-issued session id; on
//!                    subsequent prompts to a loaded session, replies "ok".
//!
//! Usage: `mock_acp_agent --script <name>` (default: happy).

use agent_client_protocol::schema::{
    AgentCapabilities, ContentBlock, ContentChunk, ImageContent, InitializeRequest,
    InitializeResponse, LoadSessionRequest, LoadSessionResponse, NewSessionRequest,
    NewSessionResponse, PromptRequest, PromptResponse, ResumeSessionRequest,
    ResumeSessionResponse, SessionId, SessionNotification, SessionUpdate, StopReason,
    TextContent,
};
use agent_client_protocol::{Agent, Client, ConnectionTo, Dispatch, Result, Stdio};
use clap::Parser;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Parser, Debug, Clone)]
#[command(
    name = "mock_acp_agent",
    about = "Scripted ACP-over-stdio mock for tests"
)]
struct Args {
    /// Behaviour script to run.
    #[arg(
        long,
        default_value = "happy",
        value_parser = ["happy", "streamy", "multimodal", "resumable", "echo"]
    )]
    script: String,
}

static SESSION_COUNTER: AtomicU64 = AtomicU64::new(0);

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let script = args.script;

    Agent
        .builder()
        .name("mock_acp_agent")
        .on_receive_request(
            async move |req: InitializeRequest, responder, _conn| {
                responder.respond(
                    InitializeResponse::new(req.protocol_version)
                        .agent_capabilities(AgentCapabilities::new()),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |_req: NewSessionRequest, responder, _conn| {
                let n = SESSION_COUNTER.fetch_add(1, Ordering::Relaxed);
                let id = SessionId::from(format!("mock-sess-{n}"));
                responder.respond(NewSessionResponse::new(id))
            },
            agent_client_protocol::on_receive_request!(),
        )
        // session/load — `resumable` script accepts any session id; other
        // scripts also accept it as a no-op so tests can probe the call
        // without crashing the mock.
        .on_receive_request(
            async move |_req: LoadSessionRequest, responder, _conn| {
                responder.respond(LoadSessionResponse::new())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |_req: ResumeSessionRequest, responder, _conn| {
                responder.respond(ResumeSessionResponse::new())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let script = script.clone();
                async move |req: PromptRequest, responder, conn: ConnectionTo<Client>| {
                    let stop = run_script(&script, req.session_id.clone(), req.prompt, conn).await;
                    responder.respond(PromptResponse::new(stop))
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_dispatch(
            async move |message: Dispatch, cx: ConnectionTo<Client>| {
                message.respond_with_error(
                    agent_client_protocol::util::internal_error("unhandled by mock_acp_agent"),
                    cx,
                )
            },
            agent_client_protocol::on_receive_dispatch!(),
        )
        .connect_to(Stdio::new())
        .await
}

async fn run_script(
    script: &str,
    session_id: SessionId,
    prompt: Vec<ContentBlock>,
    conn: ConnectionTo<Client>,
) -> StopReason {
    match script {
        "happy" | "resumable" => script_happy(session_id, conn).await,
        "streamy" => script_streamy(session_id, conn).await,
        "multimodal" => script_multimodal(session_id, conn).await,
        "echo" => script_echo(session_id, prompt, conn).await,
        other => {
            eprintln!("mock_acp_agent: unknown script '{other}', defaulting to no-op EndTurn");
            let _ = prompt;
            StopReason::EndTurn
        }
    }
}

/// One chunk "4" then EndTurn.
async fn script_happy(session_id: SessionId, conn: ConnectionTo<Client>) -> StopReason {
    let chunk = SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(
        TextContent::new("4"),
    )));
    let _ = conn.send_notification(SessionNotification::new(session_id, chunk));
    StopReason::EndTurn
}

/// 5 small chunks 50ms apart — gives G2 streaming tests room to observe
/// accumulating text in `notifications/progress.message`.
async fn script_streamy(session_id: SessionId, conn: ConnectionTo<Client>) -> StopReason {
    for piece in ["The ", "quick ", "brown ", "fox ", "jumps."] {
        let chunk = SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(
            TextContent::new(piece),
        )));
        let _ = conn.send_notification(SessionNotification::new(session_id.clone(), chunk));
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    StopReason::EndTurn
}

/// One Text + one Image ContentBlock then EndTurn.
/// Image is a 1×1 transparent PNG (base64) — smallest valid payload.
async fn script_multimodal(session_id: SessionId, conn: ConnectionTo<Client>) -> StopReason {
    const TINY_PNG_B64: &str =
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";
    let text_chunk = SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(
        TextContent::new("Here is an image: "),
    )));
    let _ = conn.send_notification(SessionNotification::new(
        session_id.clone(),
        text_chunk,
    ));
    let image_chunk = SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Image(
        ImageContent::new(TINY_PNG_B64, "image/png"),
    )));
    let _ = conn.send_notification(SessionNotification::new(session_id, image_chunk));
    StopReason::EndTurn
}

/// Echoes back a text chunk summarizing what kinds of ContentBlock the
/// agent received in the prompt. Used by inbound multi-modal tests to
/// prove that translate::a2a_to_acp actually delivered the parts.
async fn script_echo(
    session_id: SessionId,
    prompt: Vec<ContentBlock>,
    conn: ConnectionTo<Client>,
) -> StopReason {
    let mut summary = String::from("received:");
    for block in &prompt {
        let label = match block {
            ContentBlock::Text(_) => "text",
            ContentBlock::Image(_) => "image",
            ContentBlock::Audio(_) => "audio",
            ContentBlock::ResourceLink(_) => "resource_link",
            ContentBlock::Resource(_) => "resource",
            _ => "unknown",
        };
        summary.push(' ');
        summary.push_str(label);
    }
    let chunk = SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(
        TextContent::new(summary),
    )));
    let _ = conn.send_notification(SessionNotification::new(session_id, chunk));
    StopReason::EndTurn
}
