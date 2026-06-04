//! `AcpClient` — handle to a spawned ACP Agent subprocess (spec § 2.6).
//!
//! Wraps `agent_client_protocol = "0.13"`'s `Client.builder().connect_with(...)`
//! pattern behind a stable, owned, control-plane API:
//!
//! ```ignore
//! let client = AcpClient::spawn(cfg).await?;
//! client.initialize().await?;
//! let sid = client.session_new(cwd).await?;
//! let mut stream = client.session_prompt(&sid, "what is 2+2?").await?;
//! while let Some(ev) = stream.next().await { /* ... */ }
//! ```
//!
//! Architecture:
//!   * `spawn` starts a background "driver" tokio task that owns the
//!     `ConnectionTo<Agent>` for the connection's lifetime. The driver
//!     runs the SDK's `connect_with` body as a loop reading `Command`s
//!     from an mpsc channel.
//!   * Public methods send `Command`s through the channel and await
//!     responses via per-call oneshot channels.
//!   * `session_prompt` additionally registers a per-session
//!     `UnboundedSender<BridgeEvent>` in a routing table. The SDK's
//!     `on_receive_notification` callback looks up the matching sender
//!     and forwards `session/update` payloads. When the prompt request
//!     resolves with a `StopReason`, the driver pushes one final
//!     `BridgeEvent::Terminal(reason)` and clears the route.
//!
//! ADR 0001: `initialize` sends `ClientCapabilities::default()` which in
//! 0.13.x gives `fs.{read,write}TextFile = false` and `terminal = false`.
//! ADR 0002: `session_new` sends `mcpServers = []` (the default for
//! `NewSessionRequest::new(cwd)`).
//!
//! Forward-compat (R6): the SDK's typed notification handler is only
//! invoked when the payload deserializes into a known `SessionUpdate`
//! variant. Unknown variants (e.g. `usage_update` from a newer agent)
//! are silently dropped at the SDK layer with an internal -32602 warning
//! that does not bring down the session. We do not need special handling
//! here; the bridge layer (Task 19) keeps the Task in Working state.

use agent_client_protocol::schema::ContentBlock;
use agent_client_protocol::schema::{
    ClientCapabilities, InitializeRequest, InitializeResponse, LoadSessionRequest,
    NewSessionRequest, PromptRequest, ProtocolVersion, ResumeSessionRequest, SessionId,
    SessionNotification, SessionUpdate, StopReason, TextContent,
};
use agent_client_protocol::{AcpAgent, Agent, ConnectionTo, Result as AcpResult};
use futures::stream::BoxStream;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};

/// Bridge-friendly enum: an SDK update notification or a terminal stop
/// reason synthesized by the driver when the prompt request resolves.
#[derive(Debug, Clone)]
pub enum BridgeEvent {
    Update(Box<SessionUpdate>),
    Terminal(StopReason),
}

/// Configuration for spawning the wrapped ACP Agent subprocess. The
/// `command` string is parsed by the SDK's `shell-words` splitter, so
/// quoted paths and arguments are honored.
#[derive(Debug, Clone)]
pub struct AcpClientConfig {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: HashMap<String, String>,
}

/// All AcpClient methods return this. Variants map cleanly onto spec § 4.6
/// JSON-RPC error codes at the next layer up.
#[derive(Debug, Error)]
pub enum AcpError {
    #[error("spawn or connect failed: {0}")]
    Spawn(String),
    #[error("transport: {0}")]
    Transport(String),
    #[error("agent error: {0}")]
    Agent(String),
    #[error("driver task gone")]
    DriverGone,
}

/// Commands sent from `AcpClient` handle methods to the driver task.
enum Command {
    Initialize {
        respond: oneshot::Sender<AcpResult<InitializeResponse>>,
    },
    SessionNew {
        cwd: PathBuf,
        respond: oneshot::Sender<AcpResult<SessionId>>,
    },
    Prompt {
        session_id: SessionId,
        content: Vec<ContentBlock>,
        /// Stream end-point: the driver pushes BridgeEvent::Update(...) for
        /// every routed session/update notification, then exactly one
        /// BridgeEvent::Terminal(stop_reason) when the prompt resolves, then
        /// drops the sender so the receiver observes None.
        sink: mpsc::UnboundedSender<std::result::Result<BridgeEvent, AcpError>>,
    },
    LoadSession {
        session_id: SessionId,
        cwd: PathBuf,
        respond: oneshot::Sender<AcpResult<()>>,
    },
    ResumeSession {
        session_id: SessionId,
        cwd: PathBuf,
        respond: oneshot::Sender<AcpResult<()>>,
    },
    Cancel {
        session_id: SessionId,
        respond: oneshot::Sender<AcpResult<()>>,
    },
}

type RouteMap = Arc<
    Mutex<HashMap<SessionId, mpsc::UnboundedSender<std::result::Result<BridgeEvent, AcpError>>>>,
>;

#[derive(Clone)]
pub struct AcpClient {
    cmd_tx: mpsc::UnboundedSender<Command>,
    routes: RouteMap,
}

impl AcpClient {
    /// Spawn the agent subprocess and run the SDK connection driver in a
    /// background tokio task. Returns once the driver is ready to accept
    /// the first command (which is enforced by the fact that command
    /// dispatch happens inside the connect_with closure body, after the
    /// SDK transport is fully wired).
    pub async fn spawn(cfg: AcpClientConfig) -> Result<Self, AcpError> {
        let command_line = if cfg.args.is_empty() {
            cfg.command.clone()
        } else {
            // shell-quote each arg via shell-words style: simple — paths
            // may contain spaces on Windows, so always quote.
            let mut s = format!("\"{}\"", cfg.command);
            for a in &cfg.args {
                s.push(' ');
                s.push('"');
                s.push_str(a);
                s.push('"');
            }
            s
        };
        let agent = AcpAgent::from_str(&command_line)
            .map_err(|e| AcpError::Spawn(format!("parse command {command_line:?}: {e}")))?;

        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<Command>();
        let routes: RouteMap = Arc::new(Mutex::new(HashMap::new()));

        // Spawn the driver. The SDK's connect_with future runs until the
        // closure body returns; the closure body is our command loop that
        // exits cleanly when all AcpClient handles are dropped.
        let driver_routes = Arc::clone(&routes);
        tokio::spawn(async move {
            let _ = agent_client_protocol::Client
                .builder()
                .name("a2a-shim")
                .on_receive_notification(
                    {
                        let routes = Arc::clone(&driver_routes);
                        async move |n: SessionNotification, _cx| {
                            // Route the typed update to whatever session
                            // owns it. If no one is listening, drop it —
                            // happens for unsolicited mid-cancel updates
                            // or for sessions whose prompt has already
                            // terminated. (R6: forward-compat unknown
                            // variants never reach this callback at all;
                            // the SDK swallows them upstream.)
                            let sender = routes.lock().get(&n.session_id).cloned();
                            if let Some(tx) = sender {
                                let _ = tx.send(Ok(BridgeEvent::Update(Box::new(n.update))));
                            }
                            Ok(())
                        }
                    },
                    agent_client_protocol::on_receive_notification!(),
                )
                .connect_with(agent, move |conn: ConnectionTo<Agent>| async move {
                    drive(conn, cmd_rx, driver_routes).await;
                    Ok::<_, agent_client_protocol::Error>(())
                })
                .await;
            // When connect_with returns, the connection is closed. Any
            // remaining prompt streams will observe their sender being
            // dropped; that is the correct "agent went away" signal.
        });

        Ok(Self { cmd_tx, routes })
    }

    pub async fn initialize(&self) -> Result<InitializeResponse, AcpError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::Initialize { respond: tx })
            .map_err(|_| AcpError::DriverGone)?;
        rx.await
            .map_err(|_| AcpError::DriverGone)?
            .map_err(|e| AcpError::Agent(e.to_string()))
    }

    pub async fn session_new(&self, cwd: PathBuf) -> Result<SessionId, AcpError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::SessionNew { cwd, respond: tx })
            .map_err(|_| AcpError::DriverGone)?;
        rx.await
            .map_err(|_| AcpError::DriverGone)?
            .map_err(|e| AcpError::Agent(e.to_string()))
    }

    /// Start a prompt with plain text content. Convenience wrapper over
    /// `session_prompt_blocks` for the common case.
    pub async fn session_prompt(
        &self,
        session_id: &SessionId,
        text: &str,
    ) -> Result<BoxStream<'static, Result<BridgeEvent, AcpError>>, AcpError> {
        let blocks = vec![ContentBlock::Text(TextContent::new(text))];
        self.session_prompt_blocks(session_id, blocks).await
    }

    /// Start a prompt with arbitrary ContentBlocks (multi-modal).
    pub async fn session_prompt_blocks(
        &self,
        session_id: &SessionId,
        content: Vec<ContentBlock>,
    ) -> Result<BoxStream<'static, Result<BridgeEvent, AcpError>>, AcpError> {
        let (event_tx, event_rx) = mpsc::unbounded_channel::<Result<BridgeEvent, AcpError>>();
        // Register the route BEFORE sending the Prompt command so any
        // updates the agent emits early are not dropped.
        self.routes
            .lock()
            .insert(session_id.clone(), event_tx.clone());

        let send_res = self.cmd_tx.send(Command::Prompt {
            session_id: session_id.clone(),
            content,
            sink: event_tx,
        });
        if send_res.is_err() {
            self.routes.lock().remove(session_id);
            return Err(AcpError::DriverGone);
        }

        let stream = tokio_stream::wrappers::UnboundedReceiverStream::new(event_rx);
        Ok(Box::pin(stream))
    }

    /// session/load (ADR 0007 + Spike A): re-attach to an existing
    /// session id after restart. The agent replays prior turns as
    /// session/update notifications immediately; subscribers see them
    /// through the route_map if registered before this call returns.
    pub async fn session_load(
        &self,
        session_id: &SessionId,
        cwd: PathBuf,
    ) -> Result<(), AcpError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::LoadSession {
                session_id: session_id.clone(),
                cwd,
                respond: tx,
            })
            .map_err(|_| AcpError::DriverGone)?;
        rx.await
            .map_err(|_| AcpError::DriverGone)?
            .map_err(|e| AcpError::Agent(e.to_string()))
    }

    /// session/resume (ADR 0007 + Spike A): resume a previously-cancelled
    /// session. Mostly symmetric to session_load; the agent decides what
    /// resume means for its own internal state.
    pub async fn session_resume(
        &self,
        session_id: &SessionId,
        cwd: PathBuf,
    ) -> Result<(), AcpError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::ResumeSession {
                session_id: session_id.clone(),
                cwd,
                respond: tx,
            })
            .map_err(|_| AcpError::DriverGone)?;
        rx.await
            .map_err(|_| AcpError::DriverGone)?
            .map_err(|e| AcpError::Agent(e.to_string()))
    }

    pub async fn session_cancel(&self, session_id: &SessionId) -> Result<(), AcpError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::Cancel {
                session_id: session_id.clone(),
                respond: tx,
            })
            .map_err(|_| AcpError::DriverGone)?;
        rx.await
            .map_err(|_| AcpError::DriverGone)?
            .map_err(|e| AcpError::Agent(e.to_string()))
    }
}

/// Driver loop body. Owns the connection, reads commands, dispatches.
async fn drive(
    conn: ConnectionTo<Agent>,
    mut cmd_rx: mpsc::UnboundedReceiver<Command>,
    routes: RouteMap,
) {
    while let Some(cmd) = cmd_rx.recv().await {
        match cmd {
            Command::Initialize { respond } => {
                // ADR 0001: explicit default capabilities (fs disabled, terminal false).
                let req = InitializeRequest::new(ProtocolVersion::V1)
                    .client_capabilities(ClientCapabilities::default());
                let result = conn.send_request(req).block_task().await;
                let _ = respond.send(result);
            }
            Command::SessionNew { cwd, respond } => {
                // ADR 0002: NewSessionRequest::new defaults mcp_servers = [].
                let req = NewSessionRequest::new(cwd);
                let result = conn
                    .send_request(req)
                    .block_task()
                    .await
                    .map(|r| r.session_id);
                let _ = respond.send(result);
            }
            Command::Prompt {
                session_id,
                content,
                sink,
            } => {
                let req = PromptRequest::new(session_id.clone(), content);
                let prompt_result = conn.send_request(req).block_task().await;
                // After the prompt resolves, emit terminal (or error) on
                // the stream and clear the route. Routing is best-effort:
                // if the receiver is gone the send fails silently.
                match prompt_result {
                    Ok(response) => {
                        let _ = sink.send(Ok(BridgeEvent::Terminal(response.stop_reason)));
                    }
                    Err(e) => {
                        let _ = sink.send(Err(AcpError::Agent(e.to_string())));
                    }
                }
                routes.lock().remove(&session_id);
                // Dropping `sink` is implicit at end of scope; subscribers
                // observe end-of-stream.
            }
            Command::LoadSession {
                session_id,
                cwd,
                respond,
            } => {
                let req = LoadSessionRequest::new(session_id, cwd);
                let result = conn.send_request(req).block_task().await.map(|_| ());
                let _ = respond.send(result);
            }
            Command::ResumeSession {
                session_id,
                cwd,
                respond,
            } => {
                let req = ResumeSessionRequest::new(session_id, cwd);
                let result = conn.send_request(req).block_task().await.map(|_| ());
                let _ = respond.send(result);
            }
            Command::Cancel {
                session_id,
                respond,
            } => {
                // CancelNotification is a notification (no response), but
                // we still wrap into AcpResult<()> so the public API can
                // surface transport errors uniformly.
                let result: AcpResult<()> = conn
                    .send_notification(agent_client_protocol::schema::CancelNotification::new(
                        session_id,
                    ))
                    .map(|_| ());
                let _ = respond.send(result);
            }
        }
    }
    // cmd_rx ended → all handles dropped → return from the closure body so
    // the SDK shuts the connection down cleanly.
}
