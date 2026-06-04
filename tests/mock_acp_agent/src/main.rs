//! Scripted ACP-over-stdio mock agent for `a2a-shim-serve` integration tests.
//!
//! Not shipped. Speaks the wire format via `agent-client-protocol = "0.13"`
//! as an Agent role so wire-shape drift fails the build, not at test time.
//!
//! Phase 2 scripts (added on demand):
//!   * `happy`        — answers any prompt with "4" then EndTurn.
//!
//! Future scripts to add when bridge tests (Task 19) need them:
//!   * `slow` `refusal` `tool-error` `crash` `noop-cancel`.
//!
//! Usage: `mock_acp_agent --script happy` (default: happy).

use agent_client_protocol::schema::{
    AgentCapabilities, ContentBlock, ContentChunk, InitializeRequest, InitializeResponse,
    NewSessionRequest, NewSessionResponse, PromptRequest, PromptResponse, SessionId,
    SessionNotification, SessionUpdate, StopReason, TextContent,
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
    #[arg(long, default_value = "happy", value_parser = ["happy"])]
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
        // initialize — echo back the requested protocol version with a
        // minimal agent capabilities advertisement.
        .on_receive_request(
            async move |req: InitializeRequest, responder, _conn| {
                responder.respond(
                    InitializeResponse::new(req.protocol_version)
                        .agent_capabilities(AgentCapabilities::new()),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        // session/new — fabricate a deterministic session id.
        .on_receive_request(
            async move |_req: NewSessionRequest, responder, _conn| {
                let n = SESSION_COUNTER.fetch_add(1, Ordering::Relaxed);
                let id = SessionId::from(format!("mock-sess-{n}"));
                responder.respond(NewSessionResponse::new(id))
            },
            agent_client_protocol::on_receive_request!(),
        )
        // session/prompt — dispatch to script.
        .on_receive_request(
            {
                let script = script.clone();
                async move |req: PromptRequest, responder, conn: ConnectionTo<Client>| {
                    let stop = run_script(&script, req.session_id.clone(), conn).await;
                    responder.respond(PromptResponse::new(stop))
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        // Catch-all for anything else (e.g. cancel notifications, unknown methods):
        // respond with internal_error so the SDK doesn't hang the test.
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

async fn run_script(script: &str, session_id: SessionId, conn: ConnectionTo<Client>) -> StopReason {
    match script {
        "happy" => script_happy(session_id, conn).await,
        other => {
            // Should be unreachable due to clap's value_parser, but fall back
            // to a clean EndTurn so the runner doesn't deadlock if we mis-add.
            eprintln!("mock_acp_agent: unknown script '{other}', defaulting to no-op EndTurn");
            StopReason::EndTurn
        }
    }
}

/// Emit one `agent_message_chunk` containing "4" then return `EndTurn`.
async fn script_happy(session_id: SessionId, conn: ConnectionTo<Client>) -> StopReason {
    let chunk = SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(
        TextContent::new("4"),
    )));
    let _ = conn.send_notification(SessionNotification::new(session_id, chunk));
    StopReason::EndTurn
}
