# A2A-Shim — Design Specification

- **Date:** 2026-06-03
- **Status:** Approved for implementation planning
- **Author(s):** Design session (interactive)
- **Audience:** Implementers, reviewers, future maintainers

---

## Table of Contents

1. [Project Definition & Dual-Mode Architecture](#1-project-definition--dual-mode-architecture)
2. [Serve Mode — A2A Server + ACP Client + Task State Machine](#2-serve-mode--a2a-server--acp-client--task-state-machine)
3. [Client Mode — MCP Server + Outbound A2A Client](#3-client-mode--mcp-server--outbound-a2a-client)
4. [A2A Wire Protocol — Shared Layer](#4-a2a-wire-protocol--shared-layer)
5. [Configuration, CLI, Logging, Observability](#5-configuration-cli-logging-observability)
6. [Testing Strategy, MVP Milestones, v1.1 Roadmap](#6-testing-strategy-mvp-milestones-v11-roadmap)
- [Appendix A — Decision Trail](#appendix-a--decision-trail)
- [Appendix B — Incident Memos](#appendix-b--incident-memos)
- [Appendix C — Anti-Patterns Accepted in MVP](#appendix-c--anti-patterns-accepted-in-mvp)

---

## 1. Project Definition & Dual-Mode Architecture

### 1.1 Project Definition

**A2A-Shim** is a single Rust binary providing two complementary subcommands
that let any pair of agents communicate over the Google A2A protocol **without
modifying either agent's source code**:

- **`a2a-shim client`** — runs as an MCP server embedded into a host agent
  (started by the host, e.g., Claude Code). It exposes a single tool,
  `a2a_send`, which the host's LLM can call to reach a remote A2A endpoint.

- **`a2a-shim serve`** — spawns an ACP-compatible agent subprocess (e.g.,
  `claude-agent-acp`, `codex-acp`, `gemini-cli`) and wraps it as an HTTP server
  speaking standard A2A.

The two modes share the A2A wire-format implementation, error normalization,
and the configuration/logging framework. **The shim itself implements no
"intelligence" or orchestration** — discussion flow, convergence strategy, and
final synthesis are the host agent's responsibilities.

### 1.2 Core Metaphors

> **The shim is a bidirectional translator.**
> - Client mode: translates the host agent's MCP tool call into an outbound A2A
>   HTTP request.
> - Serve mode: translates inbound A2A HTTP requests into ACP `session/prompt`
>   calls, and ACP `session/update` notifications back into A2A responses/SSE.

> **The two ends are asymmetric in responsibility but meet over standard A2A.**
> - Active end (A): the agent starts itself; the shim is its tool.
> - Passive end (B): the shim starts itself; the agent is its subprocess.
> - The two sides only meet over the standard A2A HTTP protocol and remain
>   unaware of each other's implementation details.
>
> **The server end also binds to loopback by default** — the shim never faces
> the public network directly; all external exposure is routed through the
> operator's port-forwarding software.

> **"Remote" means a localhost port number.**
> - Real cross-network transport, authentication, and TLS are handled by the
>   operator's port-forwarding software (kubectl / tailscale / cloudflared /
>   SSH tunnel).
> - The shim never touches TLS, auth, or cross-network concerns.

### 1.3 Dual-Mode Topology

```
┌────── Active end (operator's dev machine) ───────┐    ┌────── Passive end (any machine) ─────────┐
│                                                   │    │                                            │
│  Operator starts:                                  │    │  Operator starts:                          │
│  $ claude   (regular Claude Code; not --acp mode) │    │  $ a2a-shim serve \                        │
│                                                   │    │      --listen 7001 \                       │
│  ┌──────────────────────────────────────┐         │    │      --spawn "npx -y \                     │
│  │ Agent A (Claude Code)                │         │    │              @agentclientprotocol/         │
│  │                                       │         │    │              claude-agent-acp"            │
│  │  MCP config registers:                │         │    │                                            │
│  │  ┌────────────────────────────────┐   │         │    │  ┌──────────────────────────────────┐    │
│  │  │ a2a-shim (CLIENT MODE)          │   │  stdio  │    │  │ a2a-shim sidecar (SERVE MODE)    │    │
│  │  │ ─────────────────────────────── │   │  MCP    │    │  │                                  │    │
│  │  │ Embedded MCP server             │   │ JSON-RPC│    │  │ ┌──────────────┐ ┌────────────┐ │    │
│  │  │ Exposes tool:                    │   │   ↓     │    │  │ │ A2A Server   │ │ ACP Client │ │    │
│  │  │   a2a_send(port, message,        │   │┌──────┐│    │  │ │ (axum)       │ │ (off. crate)│ │    │
│  │  │            conversation?)        │   ││ Shim ││    │  │ │ :7001        │ │             │ │    │
│  │  └──────────┬─────────────────────┘   │└──────┘│    │  │ └──────┬───────┘ └─────┬──────┘ │    │
│  └─────────────┼──────────────────────────┘         │    │  │        │                │        │    │
│                │ MCP tool call                       │    │  │        ▼                ▼        │    │
│                ▼                                     │    │  │ ┌──────────────────────────────┐ │    │
│         ┌──────────────────────────────────────┐    │    │  │ │ Task Manager + ConvMap        │ │    │
│         │ A2A HTTP client (reqwest)            │    │    │  │ │ A2A task ↔ ACP prompt turn   │ │    │
│         │ POST http://localhost:7001/          │    │    │  │ │ Multiple convs share/isolate │ │    │
│         │ method = "message/stream" (SSE)      │ ──────────┼─►│ ACP sessions                  │ │    │
│         │ - consume progress events            │    │    │  │ └──────────────────────────────┘ │    │
│         │ - return full result to MCP at end   │    │    │  │ ┌──────────────────────────────┐ │    │
│         └──────────────────────────────────────┘    │    │  │ │ Agent B subprocess           │ │    │
│                ▲                                     │    │  │ │ (e.g., claude-agent-acp)     │ │    │
└────────────────┼─────────────────────────────────────┘    │  │ │ stdio ↔ ACP JSON-RPC         │ │    │
                 │                                          │  │ └──────────────────────────────┘ │    │
                 │ HTTP via port-forwarding software        │  └─────────────────────────────────┘    │
                 │ (cloudflared / tailscale / kubectl / ssh)│                                          │
                 └─────────────────────────────────────────►│                                          │
                                                            └──────────────────────────────────────────┘
```

### 1.4 Mode Responsibility Comparison

| Dimension | `a2a-shim client` | `a2a-shim serve` |
|---|---|---|
| Started by | The host agent (Agent A), via the host's MCP configuration (stdio) | The operator (systemd, docker, manual launch) |
| External surface | MCP server (stdio JSON-RPC) consumed by the host agent | A2A HTTP server consumed by remote A2A clients |
| Internal surface | A2A HTTP client (outbound calls to `localhost:port`) | ACP client (spawns and drives an agent subprocess) |
| Stateful concerns | None (stateless per call; γ1) | Task state machine, conversation map, ACP session map |
| Lifecycle | Short (lives with the host agent) | Long (independent resident process) |
| Crash impact | Next MCP call from the host fails; host itself is unaffected | All in-flight tasks fail; supervisor restarts the sidecar |
| Configuration source | CLI flags + environment variables (no TOML in MVP) | TOML config file + CLI override |
| Key timeouts | Outbound connect-idle 2 min; stream-idle 10 min; hard ceiling 24 h | Same set applied to ACP `session/prompt`; plus input-required wait |

### 1.5 Single Binary + Cargo Workspace Layout

```
a2a-shim/                          # Cargo workspace root
├── Cargo.toml                     # [workspace] definition
├── Cargo.lock                     # Committed to git
├── crates/
│   ├── a2a-shim/                  # Main binary crate
│   │   ├── src/main.rs            # CLI entry, clap subcommand dispatch
│   │   ├── src/cli.rs             # `client` / `serve` subcommand schemas
│   │   └── Cargo.toml
│   ├── a2a-shim-core/             # Shared logic
│   │   └── src/
│   │       ├── wire/              # A2A wire format codec
│   │       ├── error/             # Error normalization (Section 7d shape)
│   │       ├── timeout/           # Idle / hard-ceiling helpers
│   │       ├── config/            # TOML schema
│   │       └── logging/           # tracing initialization
│   ├── a2a-shim-client/           # Client-mode implementation
│   │   └── src/
│   │       ├── mcp_server.rs      # Exposes the a2a_send tool
│   │       └── outbound.rs        # A2A HTTP client + internal SSE consumption
│   └── a2a-shim-serve/            # Serve-mode implementation
│       └── src/
│           ├── a2a_server.rs      # axum HTTP server (A2A entry point)
│           ├── acp_client.rs      # Wraps agent-client-protocol crate
│           ├── task_manager.rs    # A2A task state machine
│           ├── conversation.rs    # ConversationMap + ACP session reuse
│           └── bridge.rs          # A2A ↔ ACP translation
├── docs/
│   └── superpowers/specs/
│       └── 2026-06-03-a2a-shim-design.md   # This document
└── README.md
```

Rationale:

- A single binary is easy to distribute (`cargo install a2a-shim` provides
  both subcommands).
- Workspace-internal crates enforce clear code boundaries: `core` depends on
  no mode-specific crate; `client` and `serve` do not depend on each other.
- Either mode-specific crate can be feature-gated off later without
  restructuring.

### 1.6 Shared vs. Mode-Specific Boundary

**Shared (`a2a-shim-core`):**

- A2A JSON-RPC envelope codec
- A2A method schemas (params + result) for `message/send`, `message/stream`,
  `tasks/get`, `tasks/cancel`
- A2A wire-level error structure and the 7d-style normalized error shape
- Idle timer and hard-ceiling utilities
- TOML configuration loader
- `tracing` log formatting conventions and span field standards
- The conversation metadata key constant: `x-a2a-shim/conversation`

**Client mode only:**

- MCP server implementation (transport, `initialize`, `tools/list`,
  `tools/call`)
- Outbound A2A HTTP client (reqwest + eventsource-stream)
- `a2a_send` tool handler
- MCP-level `notifications/cancelled` handling

**Serve mode only:**

- A2A HTTP server (axum)
- ACP client built on `agent-client-protocol` crate
- Agent subprocess lifecycle management
- A2A task state machine + per-task history
- ConversationMap (per-`conversation_id` ACP session reuse)
- Permission strategy implementation (auto-approve, auto-reject,
  deny-tool-kinds filter)

### 1.7 Implementation Language: Rust

Rationale:

- Long-running sidecars must be lean on cold start and memory.
- The workload (async I/O, stdio pumping, HTTP/SSE, JSON-RPC state machines)
  fits the `tokio` + `axum` + `serde` sweet spot.
- Single static binary; cross-platform compilation; no runtime dependencies.
- The official `agent-client-protocol` crate (v0.13) is directly reusable for
  the ACP wire layer, `Client` trait, and the `mcp_server` module.

### 1.8 Out of Scope (Explicit Exclusions)

- Multi-agent orchestration, meeting rooms, or consensus protocols (the agents'
  own responsibility).
- Agent registration or discovery services (operator uses prompt + port number
  configuration).
- Authentication, TLS, or cross-network transport — the port-forwarding layer
  owns these by design.
- Task state persistence (MVP is in-memory; serve mode v1.1 will add SQLite).
- High availability, clustering, or load balancing (single instance is
  sufficient).
- Source modifications to either end's agent (client end uses MCP; serve end
  uses ACP — both standards permit zero-modification integration).
- Real streaming `a2a_send` (G2 deferred to v1.1).
- Concurrent `a2a_send` calls within one conversation (H2 deferred to v1.1).
- Multi-modal A2A parts (v1.1).
- Push notifications, AgentCard auth schemes (v1.1).

### 1.9 One-Line Summary

> A2A-Shim is a single Rust binary with two subcommands. `client` augments an
> already-running agent with an MCP tool so it can call out. `serve` wraps an
> ACP-compatible agent as an A2A HTTP server so it can be called. The two ends
> meet over standard A2A, and neither agent's source needs to change.

---

## 2. Serve Mode — A2A Server + ACP Client + Task State Machine

### 2.1 Definition

> Serve mode wraps an ACP-compatible agent subprocess as an outward-facing
> A2A HTTP service.

Responsibilities:

1. Accept inbound A2A HTTP requests (`message/send`, `message/stream`,
   `tasks/get`, `tasks/cancel`, plus the AgentCard at
   `/.well-known/agent.json`).
2. Route each A2A task to an ACP `session/prompt`; reuse or create ACP sessions
   according to the request's `conversation_id` metadata.
3. Translate the ACP `session/update` stream back into A2A responses or SSE
   events.

### 2.2 Startup Sequence

```
$ a2a-shim serve --listen 127.0.0.1:7001 \
                 --spawn "npx -y @agentclientprotocol/claude-agent-acp" \
                 --cwd /work/project
```

1. Parse CLI and load TOML → produce `ServeConfig`.
2. Initialize `tracing` subscriber.
3. Construct an ACP `Client` via `agent-client-protocol`:
   - spawn the agent subprocess (stdio takeover, stderr tee'd to logs)
   - send ACP `initialize` (protocolVersion=1; advertise client capabilities)
   - verify the protocolVersion in the response
   - cache the agent's capabilities (`promptCapabilities`, `loadSession`,
     `sessionCapabilities`, etc.)
4. Render the outward AgentCard once (see 2.7).
4.5 If `listen` is not a loopback address (not 127.0.0.1, ::1, or a Unix
   socket), log a `WARN`:

   > "Listening on non-loopback address {addr}. A2A protocol does not perform
   > authentication; ensure an external auth/tunnel layer is in place. Set
   > listen to 127.0.0.1 if not intentional."

5. Start the axum HTTP server on `listen`, mounting:
   - `POST /` (A2A JSON-RPC)
   - `GET /.well-known/agent.json` (AgentCard)
   - `GET /health` (health check; see 5.5)
6. Register a SIGTERM/SIGINT shutdown hook (see 2.4).
7. Ready — block until shutdown signal.

**MVP starts the agent eagerly** (not lazily on first request) so that spawn
failures surface immediately and the first A2A request does not pay cold
start.

### 2.3 Configuration Schema (TOML + CLI Overrides)

```toml
# a2a-shim-serve.toml

[server]
listen = "127.0.0.1:7001"
# Optional override for the AgentCard.url; falls back to `listen` when omitted.
# advertised_endpoint = "https://my-agent.tunnel.example.com"
agent_card_path = "/.well-known/agent.json"

[server.conversations]
idle_secs = 86400              # 24h with no activity → swept
max_active = 64
default_conversation_id = "default"

[agent]
# Recommended: install once with `npm install -g
# @agentclientprotocol/claude-agent-acp`, then use a direct command.
command = "claude-agent-acp"
args = []
cwd = "/work/project"          # Required
env = { ANTHROPIC_API_KEY = "..." }

[agent.card]
name = "claude-code-sidecar"
description = "Claude Agent exposed as an A2A endpoint"
version = "0.1.0"
# Capabilities are derived from the ACP initialize response, not from config.

[agent.permissions]
strategy = "auto_approve"      # auto_approve | auto_reject (passthrough → v1.2)
deny_tool_kinds = []           # e.g. ["delete", "execute"]

[timeouts]
agent_sync_idle_secs = 120
agent_stream_idle_secs = 600
agent_hard_ceiling_secs = 86400
input_required_wait_secs = 86400   # reserved; rarely used under A3
shutdown_grace_secs = 5

[logging]
level = "info"                 # trace | debug | info | warn | error
format = "compact"             # compact | json | pretty
# file = "/var/log/a2a-shim-serve.log"
```

CLI flags override TOML: `--listen`, `--spawn`, `--cwd`, `--config <path>`,
`--advertised-endpoint`, `--permission-strategy`, `--log-file`.

Configuration lookup order:

1. `--config <path>` (must exist)
2. `$A2A_SHIM_CONFIG` env var
3. `./a2a-shim-serve.toml`
4. Platform user config (`$XDG_CONFIG_HOME/a2a-shim/serve.toml` or
   `%APPDATA%\a2a-shim\serve.toml`)
5. Built-in defaults (with `agent.command` and `agent.cwd` mandatory)

Precedence (high → low): CLI > env > config file > defaults.

### 2.4 Agent Subprocess Lifecycle

**Spawn.** Use `agent-client-protocol::Client` + `Stdio` transport. The crate
handles JSON-RPC framing. Stderr is forwarded to the shim's tracing log with
the prefix `[agent stderr]`.

**Health monitoring.** No active health checks. Each `session/prompt` call
exercises the connection; failures surface naturally.

**Crash handling.**

```rust
tokio::select! {
    exit_status = child.wait() => {
        for task in task_registry.lock().iter_in_flight() {
            task.fail("agent process exited unexpectedly");
        }
        broadcast_close_to_subscribers();
        std::process::exit(exit_status.code().unwrap_or(1));
    }
    _ = shutdown_signal.recv() => {
        graceful_shutdown().await;
    }
}
```

**Graceful shutdown** on SIGTERM/SIGINT:

1. Stop accepting new A2A inbound requests (axum graceful shutdown).
2. Send `session/cancel` notifications to every active ACP session.
3. Wait `shutdown_grace_secs` seconds (default 5).
4. SIGTERM the agent.
5. Wait 2 more seconds, then SIGKILL.
6. Close stdio.
7. Exit 0.

### 2.5 A2A Task State Machine

```
submitted ──┐
            ├─→ working ──┬─→ completed (terminal; NOT revivable in MVP)
            │             ├─→ failed    (terminal)
            │             ├─→ canceled  (terminal)
            │             └─→ input-required  (rare under A3)
            │                      ↑   ↓
            │                      └───┘   (continuation via message/send)
            │
            └─→ canceled (terminal; cancel arrived before work started)
```

| A2A trigger | A2A transition | ACP action |
|---|---|---|
| `message/send` first call (no `taskId`) | `submitted` → `working` | look up or create the ACP session for the `conversation_id`; then `session/prompt` |
| `message/send` continuation (`taskId` present, state = `input-required`) | `input-required` → `working` | `session/prompt` on the same ACP session |
| `message/send` continuation (`taskId` present, state ≠ `input-required`) | rejected | return `TaskNotCancelable` (or equivalent) |
| `tasks/cancel` (state = `submitted` or `input-required`) | → `canceled` | none (agent is not running) |
| `tasks/cancel` (state = `working`) | → `canceled` | send ACP `session/cancel` notification |
| `tasks/cancel` (terminal) | unchanged | return `TaskNotCancelable` |
| ACP `session/prompt` returns `end_turn` | `working` → `completed` | terminal |
| ACP `session/prompt` returns `cancelled` | `working` → `canceled` | terminal |
| ACP `session/prompt` returns `refusal` / `max_tokens` / `max_turn_requests` | `working` → `failed` (reason indicates which stop reason) | terminal |
| Agent subprocess crashes | every non-terminal task → `failed` | sidecar exits |
| Idle or hard-ceiling timeout | `working` → `failed` | send `session/cancel` best-effort |
| Input-required wait timeout | `input-required` → `failed` | send `session/cancel` if applicable |

**MVP constraint: `completed` is not revivable.** Continuation of a discussion
that has already ended must use a *new* A2A task (no `taskId`), and the
caller is responsible for embedding relevant prior context in the new
`message`. This is the natural consequence of decision A3 (no input-required
simulation) combined with decision γ1 (client mode is stateless).

**Task data structure:**

```rust
struct TaskBinding {
    a2a_task_id: TaskId,
    acp_session_id: Option<SessionId>,
    conversation_id: String,
    state: TaskState,
    history: Vec<A2AMessage>,
    artifacts: Vec<A2AArtifact>,
    a2a_subscribers: Vec<SseSink>,
    created_at: Instant,
    last_activity_at: Arc<RwLock<Instant>>,
    cancel_token: CancellationToken,
}

type TaskRegistry = Arc<RwLock<HashMap<TaskId, Arc<RwLock<TaskBinding>>>>>;
```

### 2.6 Conversation Routing (per-`conversation_id` ACP Session Reuse)

A single serve-mode sidecar with a single agent subprocess serves multiple
independent discussions. The `conversation_id` carried in
`message.metadata["x-a2a-shim/conversation"]` (also mirrored to
`task.context_id`, see 4.4) determines which ACP session a given A2A request
maps to.

**Routing logic on each `message/send` or `message/stream`:**

1. Extract `conversation_id` from `message.metadata`; default to `"default"`
   if absent.
2. Look up `ConversationMap[conversation_id]`:
   - Hit → reuse the recorded `acpSessionId`; send `session/prompt`.
   - Miss → call `session/new`, store the result, then send `session/prompt`.
3. Continue through the task state machine; an `end_turn` from ACP maps to
   A2A `completed` (per A3).

**Conversation data structure:**

```rust
struct Conversation {
    id: String,                              // e.g. "api-design"
    acp_session_id: SessionId,
    created_at: Instant,
    last_used_at: Arc<RwLock<Instant>>,
    in_flight_prompt: Arc<RwLock<bool>>,     // H1 serial guard
}

type ConversationMap = Arc<RwLock<HashMap<String, Arc<Conversation>>>>;
```

**Concurrency model.**

| Scenario | Concurrency safety |
|---|---|
| Different conversations served concurrently | Fully concurrent (each ACP session is independent) |
| Same conversation receives two requests in quick succession | The second receives `ConversationBusy` immediately (code `-32010`); the caller decides whether to retry |

Returning an error is preferred over silently queuing because the caller can
detect the state and back off; queuing risks opaque client timeouts.

**Lifecycle.**

| Event | Effect |
|---|---|
| First sight of a new `conversation_id` | Create + `session/new` |
| Subsequent request with same `conversation_id` | Reuse, update `last_used_at` |
| Conversation idle past `conversation_idle_secs` (default 24 h) | Background sweep: `session/close` (if supported) + remove from map |
| `max_active` reached when trying to create | Return `ConversationLimitReached` (code `-32011`) |
| Sidecar shutdown | Send `session/close` to all conversations, then exit |
| Agent crash | All conversations invalidated; sidecar exits (per 2.4) |

**Security boundary.** Different conversations share the same agent process.
Context is isolated by ACP `sessionId`, but filesystem effects, API keys,
and rate limits are shared. For stronger isolation, deploy multiple
serve-mode instances on different ports (out of scope for this shim; v1.2
considers per-conversation process isolation as a feature).

### 2.7 Permission Strategy (P4, default P1)

When the ACP agent issues `session/request_permission`, the sidecar handles
it according to `[agent.permissions].strategy`:

| Strategy | Behavior |
|---|---|
| `auto_approve` (default) | Immediately reply `{outcome: selected, optionId: "allow-once"}`, unless the `toolCall.kind` is in `deny_tool_kinds`, in which case reply `{outcome: selected, optionId: "reject-once"}` and log a `WARN`. |
| `auto_reject` | Immediately reply `{outcome: selected, optionId: "reject-once"}`. Tools that require approval will fail; suitable only for pure-reasoning agents. |
| `passthrough` | Reserved for v1.2; currently returns a configuration error at startup. Will translate the permission request into an A2A `input-required` event and wait for the calling end to decide. |

Default `auto_approve` is justified by the deployment assumption that the
operator controls both ends of the connection, the network is isolated by
the port-forwarding layer, and the agent itself runs in a sandboxed
environment (e.g., a container with a scoped filesystem). README MUST
document the implications and recommend `deny_tool_kinds = ["delete",
"execute"]` as a safer default in shared environments.

### 2.8 Elicitation Handling

When the ACP agent issues `elicitation/create` (RFD, not yet stable), the
sidecar MUST respond with a method-not-implemented JSON-RPC error. The
agent will fall back to embedding the question in an `agent_message_chunk`
and ending the turn — which serve mode translates to A2A `completed` with
the question as the final message text. The calling end's LLM interprets the
question and answers via a new `a2a_send`.

This is an MVP anti-pattern (see Appendix C) tracked for v1.1 once
`elicitation/create` stabilizes.

### 2.9 ACP `session/update` → A2A Event Translation

| ACP `sessionUpdate` variant | A2A translation |
|---|---|
| `agent_message_chunk` (text part) | Append to the task's `history` agent message; broadcast as `TaskStatusUpdateEvent` SSE |
| `agent_thought_chunk` | Dropped in MVP (no A2A equivalent) |
| `user_message_chunk` (replay during `session/load`) | Ignored (no load in MVP) |
| `tool_call` (name != `a2a_send`) | Recorded internally for debugging; not propagated to A2A |
| `tool_call_update` (matching above) | Same |
| `tool_call` (name == `a2a_send`) | Surfaced as `TaskArtifactUpdateEvent` metadata so the remote A2A client can observe the cross-agent call |
| `plan` | Surfaced as `TaskStatusUpdateEvent.metadata.plan` |
| `available_commands_update` | Dropped in MVP |
| `current_mode_update` | Dropped in MVP |

**`session/prompt` terminal response → task terminal state:**

- `end_turn` → `completed`
- `cancelled` → `canceled`
- `refusal` → `failed` (`reason = "agent refused"`)
- `max_tokens` → `failed` (`reason = "agent hit max tokens"`)
- `max_turn_requests` → `failed` (`reason = "agent exceeded max turn requests"`)

### 2.10 AgentCard Rendering

Rendered once at startup; served from memory at `/.well-known/agent.json`.

```json
{
  "name": "claude-code-sidecar",
  "description": "Claude Agent exposed as an A2A endpoint",
  "version": "0.1.0",
  "url": "http://127.0.0.1:7001/",
  "capabilities": {
    "streaming": true,
    "pushNotifications": false,
    "stateTransitionHistory": true
  },
  "defaultInputModes":  ["text/plain"],
  "defaultOutputModes": ["text/plain"],
  "skills": [],
  "metadata": {
    "x-a2a-shim/conversations": {
      "supported": true,
      "metadataKey": "x-a2a-shim/conversation",
      "contextIdAlias": true,
      "maxActive": 64,
      "idleSecs": 86400
    }
  }
}
```

The `url` field uses `advertised_endpoint` if configured, otherwise the
`listen` address. When `advertised_endpoint` is omitted and `listen` is a
loopback address, the AgentCard URL is technically not reachable by remote
A2A clients — this is intentional: the operator's port-forwarding layer
provides the actual public URL, and the operator MUST either set
`advertised_endpoint` or accept that the AgentCard URL is descriptive only.

### 2.11 Error Responses

All A2A errors follow JSON-RPC 2.0 plus A2A and shim-specific codes
(see 4.6 for the full table). Responses are always well-formed JSON-RPC; no
HTTP 500 + empty body.

### 2.12 Non-Functional Requirements

| Requirement | Target |
|---|---|
| Cold start (sidecar ready) | ≤ 2 s (excluding agent spawn) |
| Steady-state RSS | ≤ 50 MB (sidecar only; agent subprocess is separate) |
| Concurrent tasks (MVP) | ≥ 16 (bounded by the agent's concurrency) |
| SSE keepalive | Emit `: keepalive\n\n` every 30 s on every active SSE stream |
| Network bind default | 127.0.0.1; non-loopback triggers WARN |
| AgentCard endpoint | `advertised_endpoint` if set, else `listen` |
| Logging | Per-task `task_id=...` tracing span; structured JSON option |
| Metrics | MVP: structured logs only; v1.1 will add `/metrics` |

### 2.13 v1.1 TODO Anchors (Serve Mode)

```rust
// TODO(v1.1): session/load + session/resume → persist TaskRegistry and
//   ConversationMap to SQLite so the sidecar can resume after restart.
//   Requires agent's loadSession / sessionCapabilities.resume capability.

// TODO(v1.1): Push notifications → implement tasks/pushNotificationConfig/*
//   for long-running tasks to call back via webhook.

// TODO(v1.1): Multi-modal parts → bidirectional translation between A2A
//   Part variants and ACP ContentBlock variants.

// TODO(v1.1): Skills derivation → populate AgentCard.skills from ACP
//   slash_commands / agentCapabilities.

// TODO(v1.1): AgentCard auth scheme declaration + actual validation in the
//   A2A server entry; MVP has no authentication.

// TODO(v1.1): Prometheus /metrics endpoint exposing task counts, durations,
//   SSE subscriber counts, agent restart events.

// TODO(v1.1): Explicit conversation reset via A2A custom method
//   `_shim/conversation/reset {conversation_id}`.

// TODO(v1.2): Per-conversation agent isolation mode (spawn one agent
//   subprocess per conversation) for stronger isolation.

// TODO(v1.2): Permission passthrough strategy — translate ACP
//   `session/request_permission` into A2A `input-required`.

// TODO(v1.2): Elicitation bridging once ACP `elicitation/create` stabilizes.
```

### 2.14 One-Line Summary

> Serve mode is an ACP-to-A2A bidirectional translator: A2A tasks map 1:1 to
> ACP prompt turns; multiple tasks sharing a `conversation_id` reuse a single
> ACP session for memory continuity, while different `conversation_id` values
> isolate independent discussions. `completed` is terminal and not revivable.
> Agent crash takes the sidecar with it; an external supervisor restarts.

---

## 3. Client Mode — MCP Server + Outbound A2A Client

### 3.1 Definition

> Client mode runs as a stdio MCP server spawned by a host agent. It exposes
> a single tool, `a2a_send`, which the host's LLM uses to consult remote A2A
> agents. Internally each call uses A2A `message/stream` (SSE) for connection
> health, but exposes a blocking synchronous result to MCP (decision G1-SSE).

Responsibilities:

1. Be spawned by the host agent via stdio MCP configuration.
2. Implement MCP `initialize`, `tools/list`, `tools/call`, and
   `notifications/cancelled`.
3. On `a2a_send`, perform an outbound A2A `message/stream` call against
   `http://localhost:{port}/`, consume the SSE stream, and return the final
   result synchronously to MCP.

### 3.2 Startup: Spawned by the Host

Client mode is **not** a long-running daemon. It is spawned per host-agent
session via the host's MCP configuration. Example for Claude Code:

```json
{
  "mcpServers": {
    "a2a": {
      "command": "a2a-shim",
      "args": ["client", "--log-file", "/tmp/a2a-shim.log"],
      "env": {}
    }
  }
}
```

**Configuration philosophy.** Client mode reads no TOML file. All settings
are CLI flags or environment variables (`A2A_SHIM_*`). This keeps the host
agent's MCP configuration as the single source of truth for the shim's
lifecycle.

**CLI options:**

```
a2a-shim client [OPTIONS]

OPTIONS:
    --connect-timeout-secs <N>   Outbound idle before SSE established (default 120)
    --stream-idle-secs <N>       Outbound SSE idle after established (default 600)
    --hard-ceiling-secs <N>      Single a2a_send hard ceiling (default 86400)
    --log-file <PATH>            Write logs to file instead of stderr
    --log-level <LEVEL>          (default info)
```

### 3.3 Stdio Discipline

| Stream | Permitted content | Violation consequence |
|---|---|---|
| `stdin` | MCP JSON-RPC messages from host | Parse error → graceful failure |
| `stdout` | MCP JSON-RPC messages to host **only** | Host MCP parser corrupted; session dies |
| `stderr` | Logs (default) | Host typically tees or ignores |
| File via `--log-file` | All log output when set | Recommended for production |

A unit test in CI MUST verify that no log line ever reaches stdout. This is
non-negotiable per MCP specification.

### 3.4 MCP Server Implementation

**Crate selection.** Phase 0 will validate whether the
`agent-client-protocol::mcp_server` module can be reused stand-alone for the
MCP server role here. If not, fall back to the official Anthropic MCP Rust
SDK (`rmcp`). This decision is deferred to Phase 0 and does not block the
specification.

**`initialize` response:**

```json
{
  "protocolVersion": "2024-11-05",
  "capabilities": { "tools": { "listChanged": false } },
  "serverInfo": { "name": "a2a-shim-client", "version": "0.1.0" }
}
```

Only `tools` capability is advertised — no resources, prompts, sampling,
or roots.

**`tools/list` response:**

```json
{
  "tools": [
    {
      "name": "a2a_send",
      "description": "Send a message to a remote A2A agent on localhost. Use this when you need to consult, ask, or collaborate with another agent. The remote agent's identity is fully determined by its localhost port. Each call is INDEPENDENT — the remote agent does not remember previous calls UNLESS you reuse the same `conversation` value across calls. Use the same `conversation` to keep continuity; use different `conversation` values for unrelated topics. Returns the remote agent's complete response synchronously.",
      "inputSchema": {
        "type": "object",
        "properties": {
          "port": {
            "type": "integer",
            "description": "Localhost port where the remote A2A agent is mapped.",
            "minimum": 1, "maximum": 65535
          },
          "message": {
            "type": "string",
            "description": "The message to send. Plain text or markdown."
          },
          "conversation": {
            "type": "string",
            "description": "Optional conversation thread id. Same value across calls = same memory thread on the remote agent. Different values = independent threads. Omit or use 'default' for a single shared thread per port.",
            "default": "default"
          }
        },
        "required": ["port", "message"]
      }
    }
  ]
}
```

### 3.5 `tools/call` Handler

```
1. Validate arguments (port range, non-empty message).
   On failure → MCP error: invalid arguments.
2. Compute outbound metadata:
   metadata["x-a2a-shim/conversation"] = arguments.conversation
                                          OR "default"
3. Issue outbound A2A request (see 3.6).
4. Consume the SSE stream until a final event arrives.
5. Serialize the final task into an MCP tool result (see 3.7).
6. Return.
```

There is **no SessionMap** in client mode — every call is stateless. The
host agent's LLM is responsible for re-supplying conversation context (via
the message body) when continuity matters; the `conversation` parameter
enables the **remote** agent's memory continuity through ConversationMap on
the serve side.

### 3.6 Outbound A2A Call (G1-SSE)

**Protocol choice: always use `message/stream`** even though the MCP call
is synchronous. Rationale:

- Keeps the HTTP connection alive via SSE traffic, defeating idle timeouts
  in port-forwarding layers (cloudflared default 100 s; many others
  similar).
- Lets the shim apply the stream-idle timeout (10 min default), which
  accurately distinguishes "agent is working" from "agent stuck".

**Request shape:**

```http
POST http://127.0.0.1:7001/ HTTP/1.1
Content-Type: application/json
Accept: text/event-stream

{
  "jsonrpc": "2.0",
  "id": "out-<uuid>",
  "method": "message/stream",
  "params": {
    "message": {
      "role": "user",
      "parts": [{"type": "text", "text": "<message>"}],
      "metadata": {
        "x-a2a-shim/conversation": "<conversation_id>"
      }
    }
  }
}
```

**Timeout model:**

| State | Timeout | On trigger |
|---|---|---|
| Between HTTP request sent and first SSE event | `connect_timeout_secs` (120 s default) | `a2a_send` fails: `error.kind = "remote_timeout"` |
| Between successive SSE events after the first | `stream_idle_secs` (600 s default) | `a2a_send` fails: `error.kind = "remote_timeout"` |
| Total call duration | `hard_ceiling_secs` (24 h default) | `a2a_send` fails: `error.kind = "remote_timeout"` |

**SSE consumption loop:**

```rust
let mut buffer_artifacts = vec![];
let mut last_status = None;
while let Some(event) = sse_stream.next().await {
    idle_guard.reset();
    match event? {
        SseEvent::StatusUpdate { status, final_, .. } => {
            last_status = Some(status);
            if final_ { break; }
        }
        SseEvent::ArtifactUpdate { artifact, .. } => {
            buffer_artifacts.push(artifact);
        }
        // SSE comment lines (": keepalive") are auto-skipped by the parser.
    }
}
construct_mcp_tool_result(last_status, buffer_artifacts)
```

### 3.7 MCP Tool Result Serialization

| Remote terminal state | MCP tool result |
|---|---|
| `completed` with artifacts | `content: [{type: "text", text: <artifacts rendered as markdown>}]` |
| `completed` without artifacts | `content: [{type: "text", text: <last agent message>}]` |
| `input-required` | `content: [{type: "text", text: "[Remote is asking for more input]\n\n" + <last message>}]`; `isError` not set |
| `failed` | `content: [{type: "text", text: <error description>}]`, `isError: true` |
| `canceled` | `content: [{type: "text", text: "[Remote task was canceled]"}]`, `isError: true` |
| Network or protocol error | `content: [{type: "text", text: <serialized error JSON>}]`, `isError: true` |

**Multimodal degradation (MVP).** Non-text artifact parts are reduced to
placeholder text such as:

```
[image: image/png, 12345 bytes — omitted in MVP. v1.1 will surface inline.]
```

The metadata is preserved; the binary payload is not. v1.1 will translate
to native MCP content types.

### 3.8 Error Normalization (Section 7d Applied)

Failures returned to MCP carry a structured JSON in the text body:

```json
{
  "error": {
    "kind": "remote_timeout",
    "message": "Remote agent at localhost:7001 did not respond within 120 seconds",
    "remote_task_id": null
  }
}
```

`error.kind` enumeration:

- `network_error` — DNS, connect refused, TLS, mid-stream disconnect
- `remote_timeout` — any timeout dimension fired
- `remote_failed` — the remote task ended in `failed` state
- `remote_canceled` — the remote task ended in `canceled` state
- `protocol_error` — the remote response did not conform to A2A
- `invalid_request` — local argument error (defense in depth; schema
  validation should normally catch this)
- `concurrent_call_not_supported` — H1 serial guard tripped (reserved;
  MVP does not actually serialize within one MCP session, but the
  enumeration is in place for H2 work)

### 3.9 Resource & Cancellation

**Memory.** SessionMap is removed (γ1). Per-call buffer ≈ accumulated
artifact size. No artifact size cap in MVP; v1.1 will add `--max-artifact-bytes`.

**MCP-level cancellation.** When the host sends `notifications/cancelled`
matching an in-flight `a2a_send`:

1. Abort the outbound HTTP/SSE connection (drop the reqwest response).
2. Issue an A2A `tasks/cancel` to the remote, best-effort, fire-and-forget.
3. Clear the in-flight marker.
4. Do not return a tool result (the host has already cancelled it).

**Host death.** Stdin EOF → MCP server loop exits → for each in-flight
`a2a_send`, best-effort `tasks/cancel` to the remote → process exit.

### 3.10 Non-Functional Requirements

| Requirement | Target |
|---|---|
| Cold start (responding to `initialize`) | ≤ 200 ms |
| Steady-state RSS, idle | ≤ 10 MB |
| Concurrency (MVP) | H1: per-MCP-session-and-port serial guard reserved; in practice MCP tool calls are serial by convention |
| Log destination | Default stderr; `--log-file` redirects to a file |
| Binary size | ≤ 15 MB (release, strip, panic=abort) |

### 3.11 v1.1 TODO Anchors (Client Mode)

```rust
// TODO(v1.1, G2): Real streaming a2a_send. Forward remote progress events
//   as MCP tool_call progress notifications. Requires verifying host MCP
//   client (Claude Code, etc.) support for progress.

// TODO(v1.1, H2): Allow concurrent a2a_send to different ports (or even
//   the same port) within one MCP session.

// TODO(v1.1): Multimodal pass-through — propagate image/file/data artifacts
//   as MCP content blocks instead of placeholder text.

// TODO(v1.1): --max-artifact-bytes flag to bound per-call buffer.

// TODO(v1.1): Optional --allowlist <ports> for defense-in-depth, even though
//   the port-mapping layer is the authoritative auth point.
```

### 3.12 One-Line Summary

> Client mode is a stdio MCP server spawned by the host agent. It exposes
> only `a2a_send`. Each call is independent; the optional `conversation`
> parameter triggers memory continuity on the remote side. Internally each
> call uses A2A `message/stream` (for SSE health and idle-timeout
> distinction) but appears synchronous to MCP. No configuration files, no
> allowlists — remote identity is fully expressed by the `port` argument.

---

## 4. A2A Wire Protocol — Shared Layer

This section defines `a2a-shim-core`: the A2A wire-format primitives shared
by both modes. It contains no business logic.

### 4.1 Scope

| In scope | Out of scope |
|---|---|
| A2A JSON-RPC envelope codec | Task state machine (Section 2.5) |
| A2A method schemas (params + result) | MCP protocol handling (Section 3) |
| A2A error model + normalization | ACP protocol handling (Section 2) |
| SSE event format and parsing | Business routing (Sections 2 & 3) |
| Conversation metadata key constant | — |

### 4.2 A2A Method Coverage (MVP)

| Method | Purpose | Serve | Client |
|---|---|---|---|
| `message/send` | Synchronous send, await terminal | ✅ | ❌ (always uses stream internally) |
| `message/stream` | Streaming send (SSE) | ✅ | ✅ |
| `tasks/get` | Snapshot a task by id | ✅ | ❌ (γ1 has no need) |
| `tasks/cancel` | Cancel a task | ✅ | ✅ (triggered by MCP cancel) |
| AgentCard at `/.well-known/agent.json` | Self-description metadata | ✅ | ❌ |

Out of scope for MVP (deferred to v1.1):

- `tasks/pushNotificationConfig/*`
- `tasks/resubscribe`
- Any auth-related extension

### 4.3 JSON-RPC Envelope

```rust
#[derive(Serialize, Deserialize)]
struct JsonRpcRequest<P> {
    jsonrpc: &'static str,       // "2.0"
    id: serde_json::Value,       // String or Number
    method: String,
    params: P,
}

#[derive(Serialize, Deserialize)]
struct JsonRpcResponse<R> {
    jsonrpc: &'static str,       // "2.0"
    id: serde_json::Value,
    #[serde(flatten)]
    result_or_error: ResultOrError<R>,
}

#[derive(Serialize, Deserialize)]
enum ResultOrError<R> {
    #[serde(rename = "result")] Result(R),
    #[serde(rename = "error")]  Error(JsonRpcError),
}

#[derive(Serialize, Deserialize)]
struct JsonRpcError {
    code: i32,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<serde_json::Value>,
}
```

The shim writes its envelope code from scratch (~150 lines) rather than
pulling a third-party JSON-RPC crate, to keep error-code semantics and
metadata handling under direct control.

### 4.4 Method Schemas

**`message/send` / `message/stream` params:**

```rust
#[derive(Serialize, Deserialize)]
struct SendMessageParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<TaskId>,          // continuation only
    message: Message,
    #[serde(skip_serializing_if = "Option::is_none")]
    configuration: Option<serde_json::Value>,  // accepted but ignored in MVP
}

#[derive(Serialize, Deserialize)]
struct Message {
    role: MessageRole,
    parts: Vec<Part>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<MessageMetadata>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum MessageRole { User, Agent }

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum Part {
    Text { text: String },
    File {
        name: Option<String>,
        mime_type: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")] bytes: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")] uri: Option<String>,
    },
    Data { data: serde_json::Value },
}

#[derive(Serialize, Deserialize, Default)]
struct MessageMetadata {
    #[serde(rename = "x-a2a-shim/conversation",
            skip_serializing_if = "Option::is_none")]
    conversation: Option<String>,

    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}
```

**Multimodal handling (MVP).** On both ends, non-text Parts are degraded
to placeholder text. The original Part type and metadata (name, mime, size)
are preserved in the placeholder so that downstream consumers know what was
omitted.

**`message/send` result (A2A Task object):**

```rust
#[derive(Serialize, Deserialize)]
struct Task {
    id: TaskId,
    #[serde(skip_serializing_if = "Option::is_none")]
    context_id: Option<String>,  // mirrors conversation_id
    status: TaskStatus,
    history: Vec<Message>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    artifacts: Vec<Artifact>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<serde_json::Value>,
}

#[derive(Serialize, Deserialize)]
struct TaskStatus {
    state: TaskState,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum TaskState {
    Submitted, Working, InputRequired, Completed, Failed, Canceled,
}

#[derive(Serialize, Deserialize)]
struct Artifact {
    #[serde(skip_serializing_if = "Option::is_none")]
    artifact_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    parts: Vec<Part>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<serde_json::Value>,
}
```

**Conversation ID dual-encoding.** Serve mode populates both:

- `task.context_id` = `conversation_id` (standard A2A field; visible to
  any A2A client)
- `message.metadata["x-a2a-shim/conversation"]` = `conversation_id` (shim's
  preferred key during the transition period)

When the A2A specification standardizes conversation/thread semantics, the
shim will migrate to the standard key while keeping the `x-a2a-shim` alias
for backward compatibility (v1.2 task).

**`tasks/get` and `tasks/cancel`:**

```rust
struct TaskIdParams { id: TaskId }
```

Both methods return a `Task`.

### 4.5 SSE Event Format

```rust
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum SseEvent {
    StatusUpdate {
        task_id: TaskId,
        status: TaskStatus,
        #[serde(default, rename = "final")] final_: bool,
    },
    ArtifactUpdate {
        task_id: TaskId,
        artifact: Artifact,
        #[serde(default)] append: bool,
    },
}
```

**Wire encoding:**

```
data: {"kind":"status-update","taskId":"t-x","status":{"state":"working"},"final":false}\n\n

: keepalive\n\n

data: {"kind":"artifact-update","taskId":"t-x","artifact":{...},"append":false}\n\n

data: {"kind":"status-update","taskId":"t-x","status":{"state":"completed"},"final":true}\n\n
```

The final event MUST carry `"final": true`. The serve side uses
`axum::response::Sse` + a tokio broadcast channel; the client side uses
the `eventsource-stream` crate for parsing.

### 4.6 Error Code Table

| Code | Name | Trigger |
|---|---|---|
| `-32700` | Parse error | Request body is not valid JSON |
| `-32600` | Invalid Request | Not a valid JSON-RPC 2.0 envelope |
| `-32601` | Method not found | Unknown method |
| `-32602` | Invalid params | Params failed schema validation |
| `-32603` | Internal error | Sidecar internal exception |
| `-32001` | TaskNotFoundError | `taskId` does not exist |
| `-32002` | TaskNotCancelableError | Cancel on terminal task or continuation in wrong state |
| `-32010` | **ConversationBusy** (shim extension) | In-flight prompt exists for the same conversation |
| `-32011` | **ConversationLimitReached** (shim extension) | `max_active` reached |

Shim extensions use the JSON-RPC server-defined range `-32000` to `-32099`.

### 4.7 AgentCard Schema

See 2.10 for the rendered example. Type definitions:

```rust
#[derive(Serialize, Deserialize)]
struct AgentCard {
    name: String,
    description: String,
    version: String,
    url: String,                       // advertised_endpoint or listen
    capabilities: AgentCapabilities,
    default_input_modes: Vec<String>,
    default_output_modes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    skills: Vec<AgentSkill>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<AgentCardMetadata>,
}

#[derive(Serialize, Deserialize)]
struct AgentCapabilities {
    streaming: bool,                   // true
    push_notifications: bool,          // false (v1.1)
    state_transition_history: bool,    // true
}

#[derive(Serialize, Deserialize)]
struct AgentCardMetadata {
    #[serde(rename = "x-a2a-shim/conversations")]
    conversations: Option<ConversationsCapability>,
}

#[derive(Serialize, Deserialize)]
struct ConversationsCapability {
    supported: bool,
    metadata_key: String,              // "x-a2a-shim/conversation"
    context_id_alias: bool,            // true
    max_active: u32,
    idle_secs: u64,
}
```

### 4.8 Public API Surface of `a2a-shim-core`

```rust
pub mod wire {
    pub use envelope::{JsonRpcRequest, JsonRpcResponse, JsonRpcError};
    pub use methods::{SendMessageParams, TaskIdParams};
    pub use task::{Task, TaskStatus, TaskState, TaskId, Artifact};
    pub use message::{Message, MessageRole, Part, MessageMetadata};
    pub use sse::{SseEvent, encode_sse_line, parse_sse_event};
    pub use card::{AgentCard, AgentCapabilities, AgentCardMetadata,
                   ConversationsCapability};
}

pub mod error {
    pub use codes::{TASK_NOT_FOUND, TASK_NOT_CANCELABLE,
                    CONVERSATION_BUSY, CONVERSATION_LIMIT_REACHED, ...};
    pub use normalize::{NormalizedError, ErrorKind, normalize_outbound};
}

pub mod timeout {
    pub use idle::IdleGuard;
    pub use ceiling::HardCeiling;
}

pub mod constants {
    pub const CONVERSATION_METADATA_KEY: &str = "x-a2a-shim/conversation";
    pub const SSE_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);
    pub const PROTOCOL_VERSION: &str = "0.1";
}
```

### 4.9 v1.1 TODO Anchors (Wire Layer)

```rust
// TODO(v1.1): Multi-modal pass-through — preserve File/Data parts end to
//   end instead of degrading to placeholder text.

// TODO(v1.1): tasks/pushNotificationConfig/* + tasks/resubscribe.

// TODO(v1.1): Auth scheme on AgentCard + actual enforcement in the A2A
//   server entry.

// TODO(v1.2): Standardize on the A2A spec's conversation key when defined,
//   keeping `x-a2a-shim/conversation` as an alias.
```

---

## 5. Configuration, CLI, Logging, Observability

### 5.1 Top-Level CLI

```
a2a-shim 0.1.0
Bidirectional shim between ACP agents and Google A2A protocol.

USAGE: a2a-shim <SUBCOMMAND>

SUBCOMMANDS:
    serve     Run as an A2A HTTP server, spawning an ACP agent subprocess
    client    Run as an MCP server (stdio), exposing a2a_send tool
    help      Print help info
    version   Print version info
```

Top-level flags applicable to either subcommand:

```
-v, --verbose...           Increase log level (-v info, -vv debug, -vvv trace)
-q, --quiet                Only warn and above
    --log-format <fmt>     compact | json | pretty (default compact)
```

### 5.2 `serve` Subcommand

CLI form, TOML schema, and lookup order are covered in 2.3. Precedence:
CLI > env > TOML > defaults.

### 5.3 `client` Subcommand

```
a2a-shim client [OPTIONS]

OPTIONS:
    --connect-timeout-secs <N>  default 120
    --stream-idle-secs <N>      default 600
    --hard-ceiling-secs <N>     default 86400
    --log-file <PATH>           write logs to file (strongly recommended)
    --log-level <LEVEL>         default info
```

No TOML file is read in client mode. The host agent's MCP configuration is
the single source of truth for the shim's lifecycle and arguments.

### 5.4 Logging

**Crate:** `tracing` + `tracing-subscriber`.

**Formats:**

| Format | Use case | Example |
|---|---|---|
| `compact` (default) | Terminal reading | `2026-06-03T10:23:45Z INFO task=t-abc serve: prompt received` |
| `pretty` | Development | Multi-line, color, field names |
| `json` | Production aggregation | `{"ts":"...","level":"INFO","task":"t-abc","msg":"prompt received"}` |

**Mandatory span fields:**

| Span | Fields |
|---|---|
| `task` (serve) | `task_id`, `conversation_id`, `acp_session_id` |
| `prompt` (serve) | `task_id`, `prompt_turn_n` |
| `outbound` (client) | `mcp_session`, `port`, `conversation`, `outbound_id` |
| `agent_subprocess` | `pid` |

**Level conventions:**

| Level | Use |
|---|---|
| ERROR | Task failed; agent crash; config load failure; ACP initialize failure |
| WARN | Non-loopback listen; timeout fired; deny_tool_kinds matched |
| INFO | Startup ready; conversation create/sweep; task state transitions; outbound start/end |
| DEBUG | Per-SSE-event; per-ACP-message; idle-timer resets |
| TRACE | Full wire dumps (JSON-RPC bodies) |

**Sensitive content rules.** Message text content is never logged below
DEBUG (only its length or hash). Environment variable values are never
logged (only key names). Full payloads appear only at TRACE, which the docs
warn is "debug only".

### 5.5 Observability (MVP)

**Health check (serve mode):** HTTP `GET /health` returns:

```json
{
  "status": "ok",
  "uptime_secs": 12345,
  "agent": {
    "pid": 9876,
    "command": "claude-agent-acp",
    "initialized_at": "2026-06-03T10:00:00Z"
  },
  "conversations": { "active": 3, "max_active": 64 }
}
```

- `200 OK` when the agent is healthy.
- `503 Service Unavailable` reserved for partial failure modes (currently
  unreachable, because per 2.4 an agent crash takes the sidecar with it).

**No metrics endpoint in MVP.** All required signals are emitted as
structured logs and can be aggregated via Loki/Promtail or equivalents.
v1.1 will add a Prometheus `/metrics` endpoint.

**Client mode** has no health endpoint (short-lived subprocess; the host
agent's supervisor monitors the host).

### 5.6 Error Messaging

All user-visible errors (CLI stderr, HTTP responses, MCP tool results)
satisfy three rules:

1. **What happened** — e.g., "Failed to spawn agent process".
2. **Why** — e.g., "command 'claude-agent-acp' not found in PATH".
3. **How to fix** — e.g., "Set [agent.command] to an absolute path or
   install the binary".

Implementation uses the `miette` crate for colored, sourced, cause-chained
diagnostics.

### 5.7 Signal Handling

| Signal | Serve mode | Client mode |
|---|---|---|
| SIGTERM / SIGINT (Ctrl+C) | Graceful shutdown per 2.4 | Close stdio, best-effort cancel of in-flight calls, exit |
| SIGHUP | Ignored in MVP (restart to reload config) | Equivalent to SIGTERM |
| Windows Ctrl+Break | Equivalent to SIGTERM | Equivalent to SIGTERM |

### 5.8 Binary Build

```toml
[profile.release]
opt-level = 3
lto = "thin"
codegen-units = 1
strip = true
panic = "abort"
```

Target binary size ≤ 15 MB.

**CI matrix:**

| Platform | Triple | Artifact |
|---|---|---|
| Linux x86_64 | `x86_64-unknown-linux-musl` | `a2a-shim-linux-x86_64` |
| Linux aarch64 | `aarch64-unknown-linux-musl` | `a2a-shim-linux-aarch64` |
| macOS x86_64 | `x86_64-apple-darwin` | `a2a-shim-macos-x86_64` |
| macOS aarch64 | `aarch64-apple-darwin` | `a2a-shim-macos-aarch64` |
| Windows x86_64 | `x86_64-pc-windows-msvc` | `a2a-shim-windows-x86_64.exe` |

Linux builds use musl for static linking, avoiding libc version dependencies.

### 5.9 v1.1 TODO (Operations)

```rust
// TODO(v1.1): Prometheus /metrics endpoint.
// TODO(v1.1): Config hot reload via SIGHUP (timeouts/log level only).
// TODO(v1.1): Structured audit log — separate stream for permission
//   decisions, conversation lifecycle events, outbound A2A calls.
// TODO(v1.1): OpenTelemetry tracing export alongside or replacing tracing
//   subscriber.
```

---

## 6. Testing Strategy, MVP Milestones, v1.1 Roadmap

### 6.1 Testing Pyramid

```
                ┌──────────────────────────┐
                │  E2E (small set)         │  Real agent + real LLM
                ├──────────────────────────┤
                │  Integration (~20)       │  Mock ACP agent / mock A2A peer
                ├──────────────────────────┤
                │  Unit (~100+)            │  Wire codec, state machines, config
                └──────────────────────────┘
```

E2E is intentionally small. Real LLM calls are slow, expensive, and
non-deterministic. Use them only to verify integration assumptions; verify
detail at the integration and unit layers.

### 6.2 Unit Tests

Per-crate target ≥ 80% line coverage. Highlights:

**`a2a-shim-core`:**
- JSON-RPC envelope round-trip (proptest)
- A2A method schemas (params + result) round-trip from fixtures
- Error code mapping (table-driven)
- SSE encode/decode (including keepalive comment lines, events split across
  chunks)
- Multimodal degradation (File/Data → placeholder)
- `MessageMetadata.extra` field round-trip
- AgentCard rendering with/without `advertised_endpoint`
- Constant values for conversation key

**`a2a-shim-serve`:**
- Task state machine — one test per transition edge
- `completed` non-revivability
- ConversationMap behavior (first-sight, reuse, idle sweep, max_active cap)
- ConversationBusy guard (concurrent same-conv requests)
- Permission strategies × deny_tool_kinds
- Timeout timers under `tokio::time::pause()`
- AgentCard capability synthesis at startup

**`a2a-shim-client`:**
- MCP `initialize` / `tools/list` / `tools/call` parsing and responses
- `a2a_send` argument validation
- SSE consumption state machine (mid-stream status updates silently absorbed;
  `final: true` triggers terminal)
- MCP tool result serialization (each remote terminal state)
- Error normalization for every `error.kind`
- **Stdio discipline:** binary CI test asserts no log output reaches stdout

### 6.3 Integration Tests

**Serve mode — mock ACP agent.** A minimal Rust mock implementing the ACP
`Agent` trait, scriptable per fixture. Verifies:

- Full task lifecycle (synchronous and streaming)
- Cancel behavior in each state
- Conversation reuse and isolation
- ConversationBusy triggering
- Permission strategies in action
- Agent crash → all in-flight tasks fail → sidecar exits
- `elicitation/create` returns method-not-implemented

**Client mode — mock A2A server.** A minimal axum-based mock. Verifies:

- Successful `a2a_send` path
- Each error branch (connection refused, timeout, SSE interruption, remote
  failed, remote canceled)
- Multimodal artifact degradation in tool result
- MCP `notifications/cancelled` triggers outbound abort + remote `tasks/cancel`
- Host death triggers remote-side notification + exit

**Dual-end loopback.** Run `a2a-shim serve` (with mock ACP agent) and
`a2a-shim client` simultaneously; have the client call the serve. Catches
the majority of bilateral contract bugs.

### 6.4 E2E Tests (Real Stack)

Run in a CI optional job, requiring `ANTHROPIC_API_KEY`.

- **E2E #1: Smoke test.** Serve with real `claude-agent-acp`. Test driver
  uses `a2a_send` to ask "What is 1+1?" and asserts the answer contains "2".
- **E2E #2: Cross-conversation isolation.** Same as #1, but uses two
  different `conversation` values; asserts the "other" conversation has no
  knowledge of the first one's content.
- **E2E #3: Multi-agent discussion (manual).** Two serve instances + one
  client connected to real Claude Code; human-prompted discussion. Not in
  CI; documented as a manual release acceptance step.

### 6.5 Testing Infrastructure

| Item | Choice |
|---|---|
| Runner | `cargo test` (unit) + `cargo nextest` (integration) |
| Mocks | Hand-rolled (avoid mock-framework magic) |
| Fixtures | JSON under `tests/fixtures/` |
| Time control | `tokio::time::pause()` |
| Coverage | `cargo llvm-cov`; CI warns at <80% |
| CI OS matrix | Linux x86_64 + macOS aarch64 + Windows x86_64 for unit + integration |

### 6.6 MVP Milestones

**Phase 0 — Dependency reality check.**

- [ ] Spawn `claude-agent-acp` via `agent-client-protocol` 0.13 in a throwaway
      crate; round-trip `initialize` + `session/new` + `session/prompt` + one
      `end_turn`.
- [ ] Validate the `mcp_server` module can inject a tool into the agent's
      `mcpServers` configuration as required. If not, evaluate the `rmcp`
      fallback.
- [ ] Empirically observe `session/request_permission` frequency under
      claude-agent-acp.
- [ ] Empirically observe whether `elicitation/create` is currently emitted
      by claude-agent-acp.

**Deliverable:** Phase 0 report confirming the architecture's assumptions
or listing required spec amendments.

**Phase 1 — Wire layer (`a2a-shim-core`).**

- [ ] All MVP method schemas with round-trip tests
- [ ] Multimodal degradation
- [ ] SSE codec with keepalive
- [ ] AgentCard renderer
- [ ] Error code table + normalization
- [ ] Timeout helpers
- [ ] Config schema and loader

**Deliverable:** `cargo test -p a2a-shim-core` green at ≥ 80% coverage.

**Phase 2 — Serve mode.**

- [ ] Crate-based spawn + initialize
- [ ] Task state machine
- [ ] ConversationMap with per-conversation ACP session reuse
- [ ] axum HTTP server (`POST /`, `/.well-known/agent.json`, `/health`)
- [ ] ACP update → A2A SSE translation
- [ ] Permission strategy
- [ ] `elicitation/create` → method-not-implemented
- [ ] Cancel handling across states
- [ ] Graceful shutdown + agent-crash handling
- [ ] Integration tests against the mock ACP agent

**Deliverable:** `cargo test -p a2a-shim-serve` green; manual `curl` walks
through a streaming task.

**Phase 3 — Client mode.**

- [ ] MCP server (`initialize`, `tools/list`, `tools/call`)
- [ ] `a2a_send` with `conversation` parameter
- [ ] Outbound via `message/stream` with SSE consumption
- [ ] Error normalization in MCP tool result
- [ ] `notifications/cancelled` handling
- [ ] CI test enforcing stdio discipline
- [ ] Integration tests against the mock A2A server

**Deliverable:** `cargo test -p a2a-shim-client` green; an echo MCP test
driver completes one `a2a_send`.

**Phase 4 — Dual-end + cross-platform CI.**

- [ ] Dual-end integration test (serve + client + mock ACP agent, no LLM)
- [ ] CI matrix green across Linux/macOS/Windows
- [ ] musl static-link binary produced
- [ ] README, example configs, troubleshooting docs

**Deliverable:** Release `v0.1.0` binaries usable.

**Phase 5 — E2E and real-world validation.**

- [ ] E2E #1 and #2 pass in CI optional job
- [ ] Manual E2E #3 completed and documented
- [ ] All issues discovered are triaged and fixed

**Deliverable:** Public MVP release `v0.1.0`.

### 6.7 v1.1 Roadmap

**v1.1.0**

1. Multi-modal full support (File/Data parts end to end)
2. G2 streaming `a2a_send` (remote progress → MCP `tool_call_update`)
3. Conversation persistence + `session/resume` (serve mode restart-safe)

**v1.1.1**

4. Push notifications (`tasks/pushNotificationConfig/*`)
5. Prometheus `/metrics`
6. Explicit conversation reset (A2A custom method)

**v1.2.0**

7. Elicitation bridging (once ACP `elicitation/create` is stable)
8. Permission passthrough (ACP permission → A2A input-required)
9. H2 concurrent `a2a_send`

**Out of v1.x scope (v2 candidates):**

- In-shim multi-agent orchestration (per 1.8, deliberately excluded)
- Auth schemes (deliberately delegated to the port-forwarding layer)
- HA / clustering

### 6.8 One-Line Summary

> Mocks first, E2E last; MVP in five phases, with Phase 0 being a critical
> reality check on the ACP crate and `claude-agent-acp` behavior; v1.1
> prioritizes multimodal, streaming, and persistence; the decision trail and
> incident memos are preserved in the appendices.

---

## Appendix A — Decision Trail

| ID | Question | Decision | Rationale |
|---|---|---|---|
| Q1 | Scope | B: Heterogeneous agent interoperability | Best fits project intent |
| Q2 | Protocol stance | A: Align with Google A2A | Project name and ecosystem maturity |
| Q3 | Deployment shape | B: Sidecar | Language-neutral, fault-isolated |
| Q4 | Local transport (later revised) | A → revised to ACP after Q-D | See incident B.2 |
| Q5 | MVP scope | A2A core 8 + multi-turn input-required (later revised by A3) | Operator-selected baseline |
| Q6 | Task state ownership | A: Sidecar owns task; agent stateless | Aligns with LLM call pattern |
| Q7a | Timeouts (revised) | Sync idle 2 m / stream idle 10 m / hard 24 h / input-required 24 h | Stream idle prevents middle-layer disconnects |
| Q7b | Cancel semantics | Best-effort; cancel notification on working tasks | Matches A2A spec |
| Q7c | Crash handling | Fail-fast on agent death; sidecar exits | 12-factor standard |
| Q7d | Outbound error shape | HTTP 200 + structured `{ok, error.kind}` | Friendly to all language clients |
| Q-A | input-required strategy | A3: ACP `end_turn` → A2A `completed`; no simulation | ACP elicitation not stable; calling end LLM drives continuation |
| Q-B | Agent lifecycle | B1: Long-lived, single instance per sidecar | Honors ACP design intent; avoids cold-start spam |
| Q-C | Task ↔ session mapping (revised) | ❸: Per-conversation ACP session via `conversation_id` metadata | Enables multi-discussion parallelism with memory continuity |
| Q-D | Outbound mechanism | D2: MCP tool bridge | Zero-modification across all ACP agents |
| Q-E | Outbound visibility | E1: Visible via tool_call notification | Observability for multi-agent discussions |
| Q-F | Remote addressing | localhost ports; no allowlist | Port-forwarding layer owns auth |
| Q-G | Outbound call mode (revised) | G1-SSE: Synchronous to MCP; SSE internally | Keepalive + accurate health monitoring |
| Q-H | Concurrent calls | H1: Conversation-Busy error on overlap | No silent queuing; H2 in v1.1 |
| Q-α | Binary structure | α1: Single binary, two subcommands | Easy distribution + clean code boundary |
| Q-β | MVP build order | β1: Serve first, then client | Server can be tested via curl before client exists |
| Q-γ | Client mode state (final) | γ1: Stateless per call | A3 already routes continuity via new tasks |
| Permission | Permission policy | P4 with default P1 (`auto_approve`) + `deny_tool_kinds` | Operator owns isolation; safer default suggested |
| Network | Default bind address | 127.0.0.1; non-loopback warns; `advertised_endpoint` override | Defense by default |
| Agent runtime | How to spawn the agent | Spawn whatever the operator configures (e.g., `claude-agent-acp` via npm) | Shim is agent-neutral |

## Appendix B — Incident Memos

### B.1 `claude --acp` does not exist (2026-06-03)

Early drafts of this specification assumed the sidecar could spawn
`claude --acp` as an ACP-compatible agent. **This was incorrect.** Claude
Code does not provide an `--acp` entry point. The ACP ecosystem's "Claude"
agent is the independent npm package `@agentclientprotocol/claude-agent-acp`,
which is the official ACP adapter for the Claude Agent SDK, maintained by
Anthropic.

**Lesson:** When introducing support for any specific agent, verify the
actual ACP entry point (binary name, arguments, runtime requirements) before
committing examples or code paths. Do not assume — read the agent's own
documentation and the ACP registry listing.

**Resolution:** All documented examples now use
`npx -y @agentclientprotocol/claude-agent-acp` (or a globally installed
`claude-agent-acp`). The shim itself depends on no specific agent.

### B.2 Local transport revised mid-design (2026-06-03)

The initial design used a custom local HTTP+SSE channel between the sidecar
and the user's agent. This precluded zero-modification integration with
CLI-style agents such as Claude Code, which do not listen on network ports.
The operator pointed out this gap; the design pivoted to ACP (stdio
JSON-RPC) as the local transport, which is the explicit purpose of the
ACP standard. The pivot simplified the design net-net by reusing the
official `agent-client-protocol` crate for wire format, transport, and
bidirectional RPC scaffolding.

## Appendix C — Anti-Patterns Accepted in MVP

| Anti-pattern | Why accepted | Upgrade path |
|---|---|---|
| Using `agent_message_chunk` + `end_turn` to simulate elicitation | ACP `elicitation/create` is RFD-stage; this is the current ecosystem norm | v1.2 — once `elicitation/create` is stable, implement bidirectional bridging |
| `completed` not revivable; continuation requires a new task with caller-supplied context | Aligns with A3 + γ1; calling end's LLM naturally carries discussion context | None planned — this is consistent with A2A's "task = one bounded exchange" semantics |
| `auto_approve` as the default permission strategy | Operator's deployment assumes sandboxed agent + trusted endpoints | v1.2 — `passthrough` strategy enables human-in-the-loop via A2A input-required |
| Multimodal Parts degraded to text placeholders on both ends | MCP content type mapping work is non-trivial; placeholder retains metadata | v1.1 — full bidirectional Part / ContentBlock translation |
| No state persistence — sidecar restart loses all tasks and conversations | In-memory storage keeps MVP simple; supervisor handles restart | v1.1 — SQLite persistence + ACP `session/resume` |
| Internal A2A SSE consumption produces synchronous MCP tool result; progress events are silently absorbed | G1-SSE compromise: gain network keepalive without MCP streaming complexity | v1.1 — G2 forwards SSE events as MCP `tool_call_update` |

---

*End of specification.*
