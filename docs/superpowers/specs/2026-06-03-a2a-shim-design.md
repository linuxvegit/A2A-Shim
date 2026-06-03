# A2A-Shim — Design Specification

- **Date:** 2026-06-03 (revised after grilling session same day)
- **Status:** Approved for implementation planning
- **Audience:** Implementers, reviewers, future maintainers

## How to read this document

This specification uses the canonical domain terms defined in
[`CONTEXT.md`](../../../CONTEXT.md). The most load-bearing distinctions:

- **`Client Shim`** — a running instance of `a2a-shim client`. Acts as a
  stdio MCP server toward its `Host`. Does not participate in ACP.
- **`Serve Shim`** — a running instance of `a2a-shim serve`. Acts as an
  A2A HTTP server and as the `ACP Client role` toward a spawned
  `ACP Agent` subprocess.
- **`Host`** — the program that spawns a `Client Shim` (e.g., Claude
  Code).
- **`ACP Agent`** — the subprocess spawned by a `Serve Shim` (e.g.,
  `claude-agent-acp`).
- **`Caller`** — whoever initiates an A2A interaction. In client-mode
  flow the `Caller` is the `Host`; in serve-mode flow it is the remote
  party hitting the A2A endpoint.
- **`Conversation`** — a caller-declared series of `a2a_send` calls
  expected to share remote-agent memory. Identified by
  `conversation_id`.
- **`Turn`** — one request/response cycle. 1 `Turn` ↔ 1 A2A `Task` ↔ 1
  ACP `session/prompt` call. `Task`s are short-lived; "long-running"
  applies to `Turn`s or `Conversation`s, never to `Task`s.
- **`Workspace`** — the filesystem and resources visible to one
  `ACP Agent`. **`Workspace`s are not shared between `Caller` and
  `ACP Agent`.**

The bare word `shim` is not used on its own — always specified as
`Client Shim` or `Serve Shim`. The capitalized bare word `Client` or
`Agent` is reserved for ACP roles (`ACP Client role`, `ACP Agent role`).

Architectural decisions with surprising consequences are captured in
[`docs/adr/`](../../adr/). This document references them by id where
relevant; the ADRs themselves carry the full rationale.

---

## Table of Contents

1. [Project Definition & Dual-Mode Architecture](#1-project-definition--dual-mode-architecture)
2. [Serve Mode](#2-serve-mode)
3. [Client Mode](#3-client-mode)
4. [A2A Wire Protocol — Shared Layer](#4-a2a-wire-protocol--shared-layer)
5. [Configuration, CLI, Logging, Observability](#5-configuration-cli-logging-observability)
6. [Testing Strategy, MVP Milestones, v1.1 Roadmap](#6-testing-strategy-mvp-milestones-v11-roadmap)
- [Appendix A — Decision Trail](#appendix-a--decision-trail)
- [Appendix B — Incident Memos](#appendix-b--incident-memos)
- [Appendix C — Anti-Patterns Accepted in MVP](#appendix-c--anti-patterns-accepted-in-mvp)

---

## 1. Project Definition & Dual-Mode Architecture

### 1.1 Project Definition

**A2A-Shim** is a single Rust binary providing two complementary
subcommands that let any pair of agents communicate over the Google A2A
protocol **without modifying either agent's source code**:

- **`a2a-shim client`** — runs as a `Client Shim`. The `Host` spawns it
  via MCP configuration. It exposes a single tool, `a2a_send`, that the
  `Host`'s `LLM` can call to reach a remote A2A endpoint.

- **`a2a-shim serve`** — runs as a `Serve Shim`. It spawns an
  ACP-compatible `ACP Agent` subprocess (e.g., `claude-agent-acp`,
  `codex-acp`, `gemini-cli`) and wraps it as an HTTP server speaking
  standard A2A.

The two modes share the A2A wire-format implementation, error
normalization, and the configuration/logging framework. **The shim
itself implements no "intelligence" or orchestration** — discussion
flow, convergence strategy, and final synthesis are the `Host`'s
responsibilities.

### 1.2 Core Metaphors

> **The shim binary is a bidirectional translator.**
> - `Client Shim`: translates the `Host`'s MCP tool call into an
>   outbound A2A HTTP request.
> - `Serve Shim`: translates inbound A2A HTTP requests into ACP
>   `session/prompt` calls, and ACP `session/update` notifications back
>   into A2A responses / SSE events.

> **The two ends are asymmetric in responsibility but meet over
> standard A2A.**
> - Active end (the `Caller`): the `Host` starts itself; the
>   `Client Shim` is its tool.
> - Passive end (the responder): the `Serve Shim` starts itself; the
>   `ACP Agent` is its subprocess.
> - Both sides interoperate only via standard A2A and remain unaware
>   of each other's implementation details.
>
> The `Serve Shim` binds to loopback by default — it never faces the
> public network directly; all external exposure is routed through the
> operator's port-forwarding software.

> **"Remote" means a localhost port number.**
> - Cross-network transport, authentication, and TLS are handled by
>   the operator's port-forwarding software (kubectl / tailscale /
>   cloudflared / SSH tunnel).
> - Neither shim touches TLS, auth, or cross-network concerns.

### 1.3 Operational Pattern: Cross-Expert Discussions are Host-Mediated

Per [ADR 0002](../../adr/0002-serve-shim-mcp-servers-empty.md), the
`Serve Shim` injects no MCP tools into the `ACP Agent`'s session. As a
direct consequence, the multi-agent discussion topology in MVP is a
**single-direction tree**:

```
                   Host
                   │
                   ▼  a2a_send
        ┌──────────┴──────────┐
        │                     │
        ▼                     ▼
   Serve Shim B          Serve Shim C
        │                     │
        ▼                     ▼
   ACP Agent B           ACP Agent C
```

`ACP Agent B` cannot directly `a2a_send` to `ACP Agent C`. If the
`Host`'s `LLM` wants B and C to converse, it shuttles their replies
back and forth — relaying each side's contribution into the next call's
`message`. This works correctly but means the `Host`'s working context
grows on the order of `O(participants × Turns)`. Mesh topologies are a
v1.1 opt-in (see ADR 0002 and section 6.7).

### 1.4 Dual-Mode Topology

```
┌────── Active end (operator's dev machine) ──┐  ┌────── Passive end (any machine) ────┐
│                                              │  │                                       │
│  Operator starts:                             │  │  Operator starts:                     │
│  $ claude    ← regular Claude Code            │  │  $ a2a-shim serve \                   │
│                                              │  │      --listen 127.0.0.1:7001 \        │
│  ┌──────────────────────────────────────┐    │  │      --spawn "claude-agent-acp"       │
│  │ Host (Claude Code)                   │    │  │                                       │
│  │                                      │    │  │  ┌───────────────────────────────┐   │
│  │  MCP config registers:                │    │  │  │ Serve Shim                    │   │
│  │  ┌────────────────────────────────┐  │    │  │  │  - A2A HTTP server (axum)     │   │
│  │  │ Client Shim                     │  │stdio │  │  - ACP Client role             │   │
│  │  │  - MCP server                   │  │MCP   │  │  - ConversationMap            │   │
│  │  │  - Tool: a2a_send(port, msg,    │  │ ↓    │  │                               │   │
│  │  │          conversation?)         │  │┌────┐│  │  ┌──────────────────────────┐ │   │
│  │  └──────────┬─────────────────────┘  ││Shim ││  │  │ ACP Agent subprocess     │ │   │
│  └─────────────┼──────────────────────────┘└────┘│  │  │ (claude-agent-acp /      │ │   │
│                │ MCP tool call                   │  │  │  codex-acp / etc.)       │ │   │
│                ▼                                 │  │  │  stdio ↔ ACP JSON-RPC    │ │   │
│         ┌────────────────────────────────────┐   │  │  └──────────────────────────┘ │   │
│         │ A2A HTTP client (reqwest)          │   │  └───────────────────────────────┘   │
│         │ POST http://localhost:7001/         │ ─────►                                   │
│         │ method=message/stream (SSE)         │   │                                       │
│         │  - consume progress events          │   │                                       │
│         │  - emit 30s notifications/progress  │   │                                       │
│         │    heartbeat to Host (ADR 0003)     │   │                                       │
│         │  - return final result to MCP       │   │                                       │
│         └────────────────────────────────────┘   │                                       │
│                ▲                                  │                                       │
└────────────────┼──────────────────────────────────┘                                       │
                 │ HTTP via port-forwarding software (cloudflared / tailscale / ...)        │
                 │                                                                          │
                 └─────────────────────────────────────────────────────────────────────────►
```

### 1.5 Mode Responsibility Comparison

| Dimension | `Client Shim` | `Serve Shim` |
|---|---|---|
| Started by | The `Host`, via MCP configuration (stdio) | The operator (systemd, docker, manual launch) |
| External surface | MCP server (stdio JSON-RPC) consumed by the `Host` | A2A HTTP server consumed by remote `Caller`s |
| Internal surface | A2A HTTP client (outbound calls to `localhost:port`) | ACP `Client role` (spawns and drives an `ACP Agent`) |
| Stateful concerns | None (stateless per call; see γ1 in Appendix A) | `Task` state machine, `ConversationMap`, ACP session map |
| Lifecycle | Short (lives with the `Host`) | Long (independent resident process) |
| Crash impact | Next MCP call from the `Host` fails; the `Host` itself is unaffected | All in-flight `Task`s fail; supervisor restarts |
| Configuration source | CLI flags + environment variables (no TOML in MVP) | TOML config file + CLI override |
| Key timeouts | Outbound connect-idle 2 min; stream-idle 10 min; hard ceiling 24 h | Same set applied to ACP `session/prompt`; plus input-required wait |
| Liveness signaling | Emits `notifications/progress` every 30 s during in-flight calls (ADR 0003) | Emits SSE `: keepalive` every 30 s on every open stream |

### 1.6 Single Binary + Cargo Workspace Layout

```
a2a-shim/
├── Cargo.toml                  # [workspace] definition
├── Cargo.lock                  # committed
├── crates/
│   ├── a2a-shim/               # main binary; CLI subcommand dispatch
│   ├── a2a-shim-core/          # shared: wire codec, error normalization,
│   │                           # timeouts, config, logging
│   ├── a2a-shim-client/        # Client Shim implementation
│   └── a2a-shim-serve/         # Serve Shim implementation
├── docs/
│   ├── adr/                    # numbered architectural decisions
│   └── superpowers/specs/
│       └── 2026-06-03-a2a-shim-design.md
├── CONTEXT.md                  # canonical glossary
└── README.md
```

Rationale unchanged from prior draft: single binary for distribution,
workspace-internal crates enforce code boundaries, either mode crate
can be feature-gated off without restructuring.

### 1.7 Shared vs. Mode-Specific Boundary

**Shared (`a2a-shim-core`):**
- A2A JSON-RPC envelope codec
- A2A method schemas for `message/send`, `message/stream`, `tasks/get`,
  `tasks/cancel`
- Error structure and the normalized error shape (Appendix A row 7d)
- Idle timer and hard-ceiling utilities
- TOML configuration loader
- `tracing` log formatting conventions
- `CONVERSATION_METADATA_KEY = "x-a2a-shim/conversation"`

**`Client Shim` only:**
- MCP server implementation (`initialize`, `tools/list`, `tools/call`,
  `notifications/cancelled`)
- 30 s `notifications/progress` heartbeat (ADR 0003)
- Outbound A2A HTTP client (reqwest + eventsource-stream)
- `a2a_send` tool handler

**`Serve Shim` only:**
- A2A HTTP server (axum)
- ACP `Client role` built on `agent-client-protocol` crate v0.13
- `ACP Agent` subprocess lifecycle management
- `Task` state machine and per-`Task` history
- `ConversationMap` (per-`conversation_id` ACP session reuse)
- Permission strategy implementation (per ADR-deferred policy; see 2.7)

### 1.8 Implementation Language: Rust

- Long-running sidecars need lean cold start and small RSS.
- Workload (async I/O, stdio pumping, HTTP/SSE, JSON-RPC state machines)
  is `tokio` + `axum` + `serde` territory.
- Single static binary, cross-platform, no runtime deps.
- Official `agent-client-protocol` crate (v0.13) provides ACP wire
  format, `Client` trait, and the `mcp_server` helper module.

### 1.9 Out of Scope (Explicit Exclusions)

- Multi-agent orchestration, meeting rooms, consensus protocols.
- Agent registration or discovery services.
- Authentication, TLS, or cross-network transport (delegated to the
  port-forwarding layer).
- `Task` state persistence (MVP in-memory; `Serve Shim` v1.1 adds
  SQLite).
- High availability, clustering, load balancing.
- Source modifications to either end's agent.
- Real streaming `a2a_send` to MCP (G2 → v1.1).
- Concurrent `a2a_send` within one MCP session (H2 → v1.1).
- Multi-modal A2A parts (v1.1).
- Push notifications, AgentCard auth schemes (v1.1).
- Mesh discussions (`ACP Agent` initiates outbound) — see ADR 0002, v1.1.
- ACP `fs/*` and `terminal/*` capabilities on the `Serve Shim` side —
  see ADR 0001.

### 1.10 One-Line Summary

> A2A-Shim is a single Rust binary with two subcommands. `client`
> augments a running `Host` with an MCP tool so it can call out.
> `serve` wraps an `ACP Agent` as an A2A HTTP server so it can be
> called. The two ends meet over standard A2A; neither agent's source
> needs to change.

---

## 2. Serve Mode

### 2.1 Definition

> The `Serve Shim` wraps an `ACP Agent` subprocess as an outward-facing
> A2A HTTP service.

Responsibilities:

1. Accept inbound A2A HTTP requests (`message/send`, `message/stream`,
   `tasks/get`, `tasks/cancel`, AgentCard at
   `/.well-known/agent.json`).
2. Route each `Task` to an ACP `session/prompt`; reuse or create ACP
   sessions per the request's `conversation_id` metadata.
3. Translate the ACP `session/update` stream back into A2A responses or
   SSE events.

### 2.2 Startup Sequence

```
$ a2a-shim serve --listen 127.0.0.1:7001 \
                 --spawn "claude-agent-acp" \
                 --cwd /work/api-design-workspace
```

1. Parse CLI and load TOML → `ServeConfig`.
2. Initialize `tracing` subscriber.
3. Construct an ACP `Client` via `agent-client-protocol`:
   - Spawn the `ACP Agent` subprocess (stdio takeover; stderr tee'd
     to logs with `[acp-agent stderr]` prefix).
   - Send ACP `initialize` (protocolVersion=1) with the following
     **`clientCapabilities` declaration** (per ADR 0001):

     ```json
     {
       "fs": { "readTextFile": false, "writeTextFile": false },
       "terminal": false
     }
     ```

     The `Serve Shim` deliberately offers no fs/terminal reverse
     capabilities; the `ACP Agent` must perform such work in-process
     against its own `Workspace`.
   - Verify the protocolVersion in the response.
   - Cache the `ACP Agent`'s capabilities (`promptCapabilities`,
     `loadSession`, `sessionCapabilities`, etc.).
4. Render the outward AgentCard once (see 2.10).
4.5. If `listen` is not a loopback address, log a `WARN`:

   > "Listening on non-loopback address {addr}. A2A protocol does not
   > perform authentication; ensure an external auth/tunnel layer is in
   > place. Set listen to 127.0.0.1 if not intentional."

5. Start the axum HTTP server on `listen`, mounting:
   - `POST /` (A2A JSON-RPC)
   - `GET /.well-known/agent.json` (AgentCard)
   - `GET /health` (health check; see 5.5)
6. Register SIGTERM/SIGINT shutdown hook (see 2.4).
7. Ready — block until shutdown.

**Eager startup.** Spawn the `ACP Agent` at boot, not lazily on first
request, so spawn failures surface immediately and the first A2A
request does not pay cold start.

### 2.3 Configuration Schema (TOML + CLI Override)

```toml
# a2a-shim-serve.toml

[server]
listen = "127.0.0.1:7001"
# Optional override for the AgentCard.url; falls back to `listen`.
# advertised_endpoint = "https://my-agent.tunnel.example.com"
agent_card_path = "/.well-known/agent.json"

[server.conversations]
idle_secs = 86400              # 24h with no activity → swept
max_active = 64

[agent]
# Recommended: install once with `npm install -g
# @agentclientprotocol/claude-agent-acp`, then use the direct command.
command = "claude-agent-acp"
args = []
cwd = "/work/project"          # Required; defines the ACP Agent's Workspace
env = { ANTHROPIC_API_KEY = "..." }

[agent.card]
name = "claude-code-sidecar"
description = "Claude Agent exposed as an A2A endpoint"
version = "0.1.0"

[agent.permissions]
strategy = "auto_approve"      # auto_approve | auto_reject (passthrough → v1.2)
deny_tool_kinds = []           # e.g. ["delete", "execute"]

[timeouts]
agent_sync_idle_secs = 120
agent_stream_idle_secs = 600
agent_hard_ceiling_secs = 86400
input_required_wait_secs = 86400   # reserved (rarely used under A3)
shutdown_grace_secs = 5

[logging]
level = "info"
format = "compact"
# file = "/var/log/a2a-shim-serve.log"
```

CLI flags override TOML: `--listen`, `--spawn`, `--cwd`, `--config
<path>`, `--advertised-endpoint`, `--permission-strategy`, `--log-file`.

Configuration lookup order: `--config` > `$A2A_SHIM_CONFIG` >
`./a2a-shim-serve.toml` > platform user config > built-in defaults
(with `agent.command` and `agent.cwd` mandatory).

Precedence: CLI > env > TOML > defaults.

### 2.4 `ACP Agent` Subprocess Lifecycle

**Spawn.** Use `agent-client-protocol::Client` + `Stdio` transport. The
crate handles JSON-RPC framing.

**Health monitoring.** No active health checks. Each `session/prompt`
exercises the connection; failures surface naturally.

**Crash handling.**

```rust
tokio::select! {
    exit_status = child.wait() => {
        for task in task_registry.lock().iter_in_flight() {
            task.fail("ACP Agent subprocess exited unexpectedly");
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
2. Send `session/cancel` to every active ACP session.
3. Wait `shutdown_grace_secs` seconds.
4. SIGTERM the `ACP Agent`.
5. Wait 2 more seconds, then SIGKILL.
6. Close stdio.
7. Exit 0.

### 2.5 A2A `Task` State Machine

```
submitted ──┐
            ├─→ working ──┬─→ completed (terminal; NOT revivable in MVP)
            │             ├─→ failed    (terminal)
            │             ├─→ canceled  (terminal)
            │             └─→ input-required  (rare under A3)
            │                      ↑   ↓
            │                      └───┘   continuation via message/send
            │
            └─→ canceled (terminal; cancel arrived before work started)
```

Each `Task` represents exactly one `Turn`. `Task` is intrinsically
short-lived; for long-running work say "long-running `Turn`" or "long
`Conversation`" (see CONTEXT.md `Task` and `Turn`).

| A2A trigger | A2A transition | ACP action |
|---|---|---|
| `message/send` first call (no `taskId`) | `submitted` → `working` | look up or create the ACP session for the `conversation_id`; then `session/prompt` |
| `message/send` continuation (`taskId` present, state = `input-required`) | `input-required` → `working` | `session/prompt` on the same ACP session |
| `message/send` continuation (`taskId` present, state ≠ `input-required`) | rejected | return `TaskNotCancelable` (or equivalent) |
| `tasks/cancel` (state = `submitted` or `input-required`) | → `canceled` | none (`ACP Agent` not running) |
| `tasks/cancel` (state = `working`) | → `canceled` | send ACP `session/cancel` notification |
| `tasks/cancel` (terminal) | unchanged | return `TaskNotCancelable` |
| ACP `session/prompt` returns `end_turn` | `working` → `completed` | terminal |
| ACP `session/prompt` returns `cancelled` | `working` → `canceled` | terminal |
| ACP `session/prompt` returns `refusal` / `max_tokens` / `max_turn_requests` | `working` → `failed` | terminal |
| `ACP Agent` subprocess crashes | every non-terminal `Task` → `failed` | `Serve Shim` exits |
| Idle or hard-ceiling timeout | `working` → `failed` | send `session/cancel` best-effort |
| Input-required wait timeout | `input-required` → `failed` | send `session/cancel` if applicable |

**MVP constraint: `completed` is not revivable.** Continuation must use
a *new* `Task` (no `taskId`); the `Caller` re-supplies relevant prior
context in the new `message`. This combines decisions A3 (no
input-required simulation) and γ1 (`Client Shim` is stateless).

**`Task` data structure:**

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

### 2.6 Conversation Routing

A single `Serve Shim` with a single `ACP Agent` subprocess serves
multiple independent `Conversation`s. The `conversation_id` in
`message.metadata["x-a2a-shim/conversation"]` (also mirrored to
`task.context_id`, see 4.4) selects which ACP session a request maps
to.

**Continuity guarantees are best-effort, not contractual.** See the
`Conversation` entry in CONTEXT.md for the full enumeration of failure
modes (`Serve Shim` restart, `ACP Agent` crash, idle sweep).

**The `conversation_id` namespace is flat and unpartitioned** in MVP
(per ADR 0004). Two unrelated `Caller`s picking the same id will share
an ACP session and pollute each other's context. The defense is the
`a2a_send` tool description and CONTEXT.md naming guidance; no
enforcement.

**Routing logic on each `message/send` or `message/stream`:**

1. Extract `conversation_id` from `message.metadata`. If absent, the
   `Serve Shim` treats this as a fresh isolated `Conversation` and
   generates a unique id internally — same default semantics that the
   `Client Shim` enforces (CONTEXT.md `Conversation` defaulting rule).
2. Look up `ConversationMap[conversation_id]`:
   - Hit → reuse `acpSessionId`; send `session/prompt`.
   - Miss → call `session/new` with **`mcpServers = []`** (per ADR
     0002), store result, then `session/prompt`.
3. Continue through the `Task` state machine; an `end_turn` from ACP
   maps to A2A `completed` (per A3).

**Conversation data structure:**

```rust
struct Conversation {
    id: String,
    acp_session_id: SessionId,
    created_at: Instant,
    last_used_at: Arc<RwLock<Instant>>,
    in_flight_prompt: Arc<RwLock<bool>>,     // H1 serial guard
}
type ConversationMap = Arc<RwLock<HashMap<String, Arc<Conversation>>>>;
```

**Concurrency model:**

| Scenario | Concurrency safety |
|---|---|
| Different `Conversation`s served concurrently | Fully concurrent (each ACP session is independent) |
| Same `Conversation` receives two requests in quick succession | The second receives `ConversationBusy` (code `-32010`) immediately; caller decides whether to retry |

**Lifecycle:**

| Event | Effect |
|---|---|
| First sight of a new `conversation_id` | Create + `session/new` (`mcpServers = []`) |
| Subsequent request with same `conversation_id` | Reuse, update `last_used_at` |
| `Conversation` idle past `conversation_idle_secs` (24 h default) | Background sweep: `session/close` (if supported) + remove |
| `max_active` reached when trying to create | Return `ConversationLimitReached` (code `-32011`) |
| `Serve Shim` shutdown | Send `session/close` to all `Conversation`s, then exit |
| `ACP Agent` crash | All `Conversation`s invalidated; `Serve Shim` exits (per 2.4) |

**Security boundary.** Different `Conversation`s share the same
`ACP Agent` process. Context is isolated by ACP `sessionId`, but
filesystem effects, API keys, and rate limits are shared. For stronger
isolation, deploy multiple `Serve Shim` instances on different ports.

### 2.7 Permission Strategy (P4, default P1)

When the `ACP Agent` issues `session/request_permission`, the
`Serve Shim` handles it according to `[agent.permissions].strategy`:

| Strategy | Behavior |
|---|---|
| `auto_approve` (default) | Reply `{outcome: selected, optionId: "allow-once"}`, unless the `toolCall.kind` is in `deny_tool_kinds` — then reply `reject-once` and log a `WARN`. |
| `auto_reject` | Reply `reject-once` unconditionally. Suitable only for pure-reasoning agents. |
| `passthrough` | Reserved for v1.2; currently returns a configuration error at startup. Will translate the permission request into A2A `input-required` so the `Caller` decides. |

README MUST document the deployment implications and recommend
`deny_tool_kinds = ["delete", "execute"]` as a safer default in
shared environments.

### 2.8 Elicitation Handling

When the `ACP Agent` issues `elicitation/create` (RFD, not yet stable),
the `Serve Shim` MUST respond with a method-not-implemented JSON-RPC
error. The `ACP Agent` will fall back to embedding the question in an
`agent_message_chunk` and ending the `Turn` — which `Serve Shim`
translates to A2A `completed` with the question as the final message
text. The `Host`'s `LLM` interprets the question and answers via a new
`a2a_send`.

Tracked in Appendix C as an MVP anti-pattern; v1.1 bridging is planned.

### 2.9 ACP `session/update` → A2A Event Translation

| ACP `sessionUpdate` variant | A2A translation |
|---|---|
| `agent_message_chunk` (text) | Append to `Task.history`; broadcast as `TaskStatusUpdateEvent` SSE |
| `agent_thought_chunk` | Dropped in MVP |
| `user_message_chunk` (during `session/load` replay) | Ignored (no load in MVP) |
| `tool_call` (name ≠ `a2a_send`) | Recorded internally; not propagated to A2A |
| `tool_call_update` (matching) | Same |
| `tool_call` (name == `a2a_send`) | This case is **impossible in MVP** because `ACP Agent` is offered no `a2a_send` tool (ADR 0002). v1.1 mesh discussions revisit. |
| `plan` | Surfaced as `TaskStatusUpdateEvent.metadata.plan` |
| `available_commands_update` | Dropped in MVP |
| `current_mode_update` | Dropped in MVP |

**`session/prompt` terminal response → `Task` terminal state:**

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

`url` uses `advertised_endpoint` if configured, otherwise the `listen`
address. When `advertised_endpoint` is omitted and `listen` is a
loopback address, the AgentCard URL is descriptive only — the operator
must either set `advertised_endpoint` or accept this.

### 2.11 Error Responses

All A2A errors follow JSON-RPC 2.0 plus A2A and shim-specific codes
(see 4.6). Responses are always well-formed JSON-RPC; no HTTP 500 with
empty body.

### 2.12 Non-Functional Requirements

| Requirement | Target |
|---|---|
| Cold start (`Serve Shim` ready) | ≤ 2 s (excluding `ACP Agent` spawn) |
| Steady-state RSS | ≤ 50 MB (`Serve Shim` only) |
| Concurrent `Task`s (MVP) | ≥ 16 (bounded by the `ACP Agent`'s concurrency) |
| SSE keepalive | Emit `: keepalive\n\n` every 30 s on every active SSE stream |
| Network bind default | 127.0.0.1; non-loopback triggers WARN |
| AgentCard endpoint | `advertised_endpoint` if set, else `listen` |
| Logging | Per-`Task` `task_id=...` tracing span; structured JSON option |
| Metrics | MVP: structured logs only; v1.1 adds `/metrics` |

### 2.13 v1.1+ TODO Anchors (Serve Mode)

```rust
// TODO(v1.1): session/load + session/resume — persist TaskRegistry and
//   ConversationMap to SQLite so the Serve Shim can resume after restart.
//   Required to fulfill the "best-effort continuity" hardening promise
//   in CONTEXT.md `Conversation`.

// TODO(v1.1): Push notifications — implement tasks/pushNotificationConfig/*.

// TODO(v1.1): Multi-modal Parts — bidirectional translation between A2A
//   Part variants and ACP ContentBlock variants.

// TODO(v1.1): Skills derivation — populate AgentCard.skills from ACP
//   slash_commands / agentCapabilities.

// TODO(v1.1): AgentCard auth scheme declaration + enforcement in A2A server.

// TODO(v1.1): Prometheus /metrics endpoint.

// TODO(v1.1): Explicit conversation reset via A2A custom method
//   `_shim/conversation/reset {conversation_id}`.

// TODO(v1.1): Optional `caller_id` argument on a2a_send + per-server
//   caller_identity config to partition ConversationMap by
//   (caller_id, conversation_id) — see ADR 0004.

// TODO(v1.2): Per-conversation ACP Agent isolation mode (one subprocess
//   per Conversation).

// TODO(v1.2): Permission passthrough strategy — translate ACP
//   `session/request_permission` into A2A `input-required`.

// TODO(v1.2): Elicitation bridging once ACP `elicitation/create` stabilizes.

// TODO(v1.1, opt-in): Mesh discussions — inject `a2a_send` into ACP
//   session/new mcpServers (see ADR 0002). Requires Conversation
//   propagation semantics for nested outbound calls.
```

### 2.14 One-Line Summary

> The `Serve Shim` is an ACP-to-A2A bidirectional translator: A2A
> `Task`s map 1:1 to ACP `Turn`s; multiple `Task`s sharing a
> `conversation_id` reuse a single ACP session for memory continuity,
> while different ids isolate independent discussions. `completed` is
> terminal and not revivable. `ACP Agent` crash takes the `Serve Shim`
> with it; an external supervisor restarts.

---

## 3. Client Mode

### 3.1 Definition

> The `Client Shim` runs as a stdio MCP server spawned by a `Host`. It
> exposes a single tool, `a2a_send`, which the `Host`'s `LLM` uses to
> reach remote A2A endpoints. Internally each call uses A2A
> `message/stream` (SSE) for connection health, but exposes a blocking
> synchronous result to MCP (decision G1-SSE; see Appendix A).

Responsibilities:

1. Be spawned by the `Host` via stdio MCP configuration.
2. Implement MCP `initialize`, `tools/list`, `tools/call`, and
   `notifications/cancelled`.
3. Emit periodic `notifications/progress` heartbeats during in-flight
   calls (ADR 0003).
4. On `a2a_send`, perform an outbound A2A `message/stream` against
   `http://localhost:{port}/`, consume the SSE stream, return the final
   result synchronously to MCP.

### 3.2 Startup: Spawned by the `Host`

The `Client Shim` is not a daemon. Example `Host` (Claude Code) MCP
config:

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

**Configuration philosophy.** The `Client Shim` reads no TOML file; all
settings come from CLI flags or environment variables (`A2A_SHIM_*`).
The `Host`'s MCP configuration is the single source of truth for the
shim's lifecycle.

**CLI options:**

```
a2a-shim client [OPTIONS]
    --connect-timeout-secs <N>   default 120
    --stream-idle-secs <N>       default 600
    --hard-ceiling-secs <N>      default 86400
    --heartbeat-secs <N>         default 30 (see ADR 0003)
    --log-file <PATH>            write logs to file (strongly recommended)
    --log-level <LEVEL>          default info
```

### 3.3 Stdio Discipline

| Stream | Permitted content | Violation consequence |
|---|---|---|
| `stdin` | MCP JSON-RPC messages from `Host` | Parse error → graceful failure |
| `stdout` | MCP JSON-RPC messages to `Host` **only** | `Host` MCP parser corrupted; session dies |
| `stderr` | Logs (default) | `Host` typically tees or ignores |
| File via `--log-file` | All log output when set | Recommended for production |

A CI unit test MUST assert no log line ever reaches stdout.

### 3.4 MCP Server Implementation

**Crate selection.** Phase 0 will validate whether
`agent-client-protocol::mcp_server` can be reused standalone for the
MCP server role. If not, fall back to the Anthropic Rust MCP SDK
(`rmcp`). This is implementation detail and does not block the spec.

**`initialize` response:**

```json
{
  "protocolVersion": "2024-11-05",
  "capabilities": { "tools": { "listChanged": false } },
  "serverInfo": { "name": "a2a-shim-client", "version": "0.1.0" }
}
```

Only the `tools` capability is advertised. No `resources`, `prompts`,
`sampling`, or `roots` — the `Client Shim` is a tool surface, not an
LLM/resource proxy. This is the symmetric counterpart of ADR 0001's
principle on the `Serve Shim` side: neither shim acts as an
LLM/filesystem intermediary.

**`tools/list` response:**

```json
{
  "tools": [
    {
      "name": "a2a_send",
      "description": "Send a message to a remote A2A agent on localhost. Use this when you need to consult, ask, or collaborate with another agent. The remote agent's identity is fully determined by its localhost port.\n\nWORKSPACE ISOLATION: The remote agent has its own working directory; files on the caller's machine are NOT visible to it. Include any relevant code, file contents, or context inline in the `message` argument; do not pass file paths.\n\nEach call is INDEPENDENT — the remote agent does not remember previous calls UNLESS you reuse the same `conversation` value across calls. Memory continuity is best-effort (the remote may forget across restarts or after long idle); do not rely on it for correctness.\n\nReturns the remote agent's complete response synchronously.",
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
            "description": "The message to send. Plain text or markdown. Include any code/data/context inline — file paths on your machine are meaningless to the remote agent."
          },
          "conversation": {
            "type": "string",
            "description": "Optional conversation thread id. Same value across calls = same memory thread on the remote agent. The remote shim does NOT partition by caller, so use a globally unique id. Good examples: \"alice/review-2026-06-03\", \"ci-job-9831\", a UUID. Bad examples: \"review\", \"chat\", \"work\" — these will collide with other callers. Omit for a fully isolated single call."
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
   - If `conversation` is provided → use it.
   - If `conversation` is omitted → generate a fresh UUID for this
     call (per CONTEXT.md `Conversation` defaulting rule).
3. Capture `_meta.progressToken` from the tools/call request if present.
4. Start outbound A2A request (see 3.6) and start the
   heartbeat timer (per ADR 0003) if `progressToken` was captured.
5. Consume the SSE stream until a final event arrives or a timeout
   triggers.
6. Stop the heartbeat timer.
7. Serialize the final task into an MCP tool result (see 3.7).
8. Return.
```

There is no SessionMap in `Client Shim` — every call is stateless (γ1).
The `Host`'s `LLM` is responsible for re-supplying conversation context
in `message` when continuity matters; the `conversation` argument
enables the *remote* `Serve Shim`'s memory via `ConversationMap`.

### 3.6 Outbound A2A Call (G1-SSE)

**Protocol choice: always `message/stream`** even though the MCP call is
synchronous. Rationale:

- Keeps the HTTP connection alive via SSE traffic, defeating idle
  timeouts in port-forwarding layers.
- Lets the `Client Shim` apply the stream-idle timeout (10 min default) to
  distinguish "agent is working" from "agent stuck".

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

**Heartbeat (ADR 0003).** If the `Host` supplied a `progressToken` on
`tools/call`, the `Client Shim` emits `notifications/progress` every 30
seconds with `progress: 0`, `total: null`, and `message: "Waiting for
remote agent (elapsed Xs)"`. The first heartbeat fires at T+30 s, not
T+0. The heartbeat stops when the outbound call terminates (success,
error, or cancellation). If no `progressToken` was supplied, no
heartbeats are emitted (MCP spec requires the token).

**Timeout model:**

| State | Timeout | On trigger |
|---|---|---|
| Between HTTP request sent and first SSE event | `connect_timeout_secs` (120 s) | `a2a_send` fails: `error.kind = "remote_timeout"` |
| Between successive SSE events after the first | `stream_idle_secs` (600 s) | Same |
| Total call duration | `hard_ceiling_secs` (24 h) | Same |

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
        // SSE comment lines (": keepalive") auto-skipped by parser.
    }
}
construct_mcp_tool_result(last_status, buffer_artifacts)
```

### 3.7 MCP Tool Result Serialization

| Remote terminal state | MCP tool result |
|---|---|
| `completed` with artifacts | `content: [{type:"text", text: <artifacts rendered as markdown>}]` |
| `completed` without artifacts | `content: [{type:"text", text: <last agent message>}]` |
| `input-required` | `content: [{type:"text", text: "[Remote is asking for more input]\n\n" + <last message>}]`; `isError` not set |
| `failed` | `content: [{type:"text", text: <error description>}]`, `isError: true` |
| `canceled` | `content: [{type:"text", text: "[Remote task was canceled]"}]`, `isError: true` |
| Network or protocol error | `content: [{type:"text", text: <serialized error JSON>}]`, `isError: true` |

**Multimodal degradation (MVP).** Non-text artifact parts reduced to
placeholder text like:

```
[image: image/png, 12345 bytes — omitted in MVP. v1.1 will surface inline.]
```

Metadata preserved, binary payload not. v1.1 translates to native MCP
content types.

### 3.8 Error Normalization

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

`error.kind`: `network_error` / `remote_timeout` / `remote_failed` /
`remote_canceled` / `protocol_error` / `invalid_request` /
`concurrent_call_not_supported`.

### 3.9 Resource & Cancellation

**Memory.** No SessionMap (γ1). Per-call buffer ≈ accumulated artifact
size. No artifact size cap in MVP; v1.1 adds `--max-artifact-bytes`.

**MCP-level cancellation.** When the `Host` sends
`notifications/cancelled` matching an in-flight `a2a_send`:

1. Abort the outbound HTTP/SSE connection.
2. Stop the heartbeat timer.
3. Issue an A2A `tasks/cancel` to the remote (best-effort, fire and
   forget).
4. Clear the in-flight marker.
5. Do not return a tool result (the `Host` already cancelled).

**`Host` death.** Stdin EOF → MCP server loop exits → for each
in-flight `a2a_send`, best-effort `tasks/cancel` → process exit.

### 3.10 Non-Functional Requirements

| Requirement | Target |
|---|---|
| Cold start (responding to `initialize`) | ≤ 200 ms |
| Steady-state RSS, idle | ≤ 10 MB |
| Concurrency (MVP) | H1: per-(MCP-session, port) serial guard; MCP tool calls are serial by convention |
| Heartbeat cadence | 30 s, only when `progressToken` was provided (ADR 0003) |
| Log destination | Default stderr; `--log-file` redirects |
| Binary size | ≤ 15 MB (release, strip, panic=abort) |

### 3.11 v1.1+ TODO Anchors (Client Mode)

```rust
// TODO(v1.1, G2): Real streaming a2a_send — forward remote progress
//   events as MCP tool_call progress notifications (extends the ADR 0003
//   heartbeat mechanism with real content).

// TODO(v1.1, H2): Allow concurrent a2a_send within one MCP session.

// TODO(v1.1): Multimodal pass-through — propagate image/file/data
//   artifacts as MCP content blocks instead of placeholder text.

// TODO(v1.1): --max-artifact-bytes flag to bound per-call buffer.

// TODO(v1.1): Optional --allowlist <ports> for defense-in-depth, even
//   though the port-mapping layer is authoritative.
```

### 3.12 One-Line Summary

> The `Client Shim` is a stdio MCP server spawned by the `Host`. It
> exposes only `a2a_send`. Each call is independent; the optional
> `conversation` argument triggers memory continuity on the remote side.
> Internally each call uses A2A `message/stream` (for SSE health and
> idle-timeout distinction) but appears synchronous to MCP, with a
> 30-second `notifications/progress` heartbeat keeping the `Host` UI
> alive. No configuration files, no allowlists.

---

## 4. A2A Wire Protocol — Shared Layer

Unchanged from the prior draft except for terminology alignment with
CONTEXT.md. The substantive content (envelope codec, method schemas,
SSE event format, error code table, AgentCard schema, exported
`a2a-shim-core` API surface) all remain valid.

### 4.1 Scope

| In scope | Out of scope |
|---|---|
| A2A JSON-RPC envelope codec | `Task` state machine (Section 2.5) |
| A2A method schemas (params + result) | MCP protocol handling (Section 3) |
| A2A error model + normalization | ACP protocol handling (Section 2) |
| SSE event format and parsing | Business routing (Sections 2 & 3) |
| `CONVERSATION_METADATA_KEY` constant | — |

### 4.2 A2A Method Coverage (MVP)

| Method | `Serve Shim` | `Client Shim` |
|---|---|---|
| `message/send` | ✅ | ❌ (always uses stream internally) |
| `message/stream` | ✅ | ✅ |
| `tasks/get` | ✅ | ❌ |
| `tasks/cancel` | ✅ | ✅ (triggered by MCP cancel) |
| AgentCard at `/.well-known/agent.json` | ✅ | ❌ |

Deferred to v1.1: `tasks/pushNotificationConfig/*`, `tasks/resubscribe`,
any auth-related extension.

### 4.3 JSON-RPC Envelope

```rust
struct JsonRpcRequest<P>  { jsonrpc: &'static str, id: Value, method: String, params: P }
struct JsonRpcResponse<R> { jsonrpc: &'static str, id: Value,
                            #[serde(flatten)] result_or_error: ResultOrError<R> }
enum ResultOrError<R>     { Result(R), Error(JsonRpcError) }
struct JsonRpcError       { code: i32, message: String, data: Option<Value> }
```

Shim writes ~150 lines of envelope code rather than pulling a
third-party JSON-RPC crate, to keep A2A-specific error semantics and
metadata handling under direct control.

### 4.4 Method Schemas

```rust
struct SendMessageParams {
    id: Option<TaskId>,              // continuation only
    message: Message,
    configuration: Option<Value>,    // accepted but ignored in MVP
}

struct Message {
    role: MessageRole,               // User | Agent
    parts: Vec<Part>,
    metadata: Option<MessageMetadata>,
}

enum Part {
    Text { text: String },
    File { name: Option<String>, mime_type: Option<String>,
           bytes: Option<String>, uri: Option<String> },
    Data { data: Value },
}

struct MessageMetadata {
    #[serde(rename = "x-a2a-shim/conversation")]
    conversation: Option<String>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}
```

**Multimodal handling (MVP).** On both ends, non-text `Part`s degrade
to placeholder text preserving Part type and metadata.

**`Task` object** mirrors A2A schema with `context_id` populated to
match `conversation_id` for standard-A2A interop:

```rust
struct Task {
    id: TaskId,
    context_id: Option<String>,      // = conversation_id
    status: TaskStatus,
    history: Vec<Message>,
    artifacts: Vec<Artifact>,
    metadata: Option<Value>,
}
struct TaskStatus {
    state: TaskState,                // Submitted | Working | InputRequired
                                     //   | Completed | Failed | Canceled
    message: Option<Message>,
    timestamp: Option<String>,
}
```

**Conversation ID dual-encoding.** `Serve Shim` populates both
`task.context_id` and `message.metadata["x-a2a-shim/conversation"]`.
When the A2A spec standardizes conversation/thread semantics, the `Serve Shim`
will migrate to the standard key while keeping the `x-a2a-shim` alias
(v1.2 task).

### 4.5 SSE Event Format

```rust
enum SseEvent {
    StatusUpdate { task_id: TaskId, status: TaskStatus, final_: bool },
    ArtifactUpdate { task_id: TaskId, artifact: Artifact, append: bool },
}
```

Wire encoding:
```
data: {"kind":"status-update","taskId":"t-x","status":{"state":"working"},"final":false}\n\n
: keepalive\n\n
data: {"kind":"artifact-update","taskId":"t-x","artifact":{...},"append":false}\n\n
data: {"kind":"status-update","taskId":"t-x","status":{"state":"completed"},"final":true}\n\n
```

Final event MUST carry `final: true`.

### 4.6 Error Code Table

| Code | Name | Trigger |
|---|---|---|
| `-32700` | Parse error | Body not valid JSON |
| `-32600` | Invalid Request | Not valid JSON-RPC 2.0 |
| `-32601` | Method not found | Unknown method |
| `-32602` | Invalid params | Schema validation failed |
| `-32603` | Internal error | Sidecar internal exception |
| `-32001` | TaskNotFoundError | `taskId` does not exist |
| `-32002` | TaskNotCancelableError | Cancel on terminal `Task` or continuation in wrong state |
| `-32010` | **ConversationBusy** (shim) | In-flight prompt exists for same `Conversation` |
| `-32011` | **ConversationLimitReached** (shim) | `max_active` reached |

### 4.7 AgentCard Schema

See 2.10. Type structure omitted here for brevity; types live in
`a2a-shim-core::wire::card`.

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
                    CONVERSATION_BUSY, CONVERSATION_LIMIT_REACHED};
    pub use normalize::{NormalizedError, ErrorKind, normalize_outbound};
}
pub mod timeout {
    pub use idle::IdleGuard;
    pub use ceiling::HardCeiling;
}
pub mod constants {
    pub const CONVERSATION_METADATA_KEY: &str = "x-a2a-shim/conversation";
    pub const SSE_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);
    pub const MCP_PROGRESS_HEARTBEAT_INTERVAL: Duration =
        Duration::from_secs(30);
    pub const PROTOCOL_VERSION: &str = "0.1";
}
```

### 4.9 v1.1 TODO Anchors

```rust
// TODO(v1.1): Multimodal pass-through — preserve File/Data Parts end to
//   end.
// TODO(v1.1): tasks/pushNotificationConfig/* + tasks/resubscribe.
// TODO(v1.1): Auth scheme on AgentCard + enforcement in A2A server.
// TODO(v1.2): Standardize on A2A spec's conversation key once defined.
```

---

## 5. Configuration, CLI, Logging, Observability

### 5.1 Top-Level CLI

```
a2a-shim 0.1.0
USAGE: a2a-shim <SUBCOMMAND>

SUBCOMMANDS:
    serve     Run as Serve Shim
    client    Run as Client Shim
    help
    version
```

Top-level flags applicable to either subcommand:
```
-v, --verbose...          Increase log level
-q, --quiet               Only warn and above
    --log-format <fmt>    compact | json | pretty (default compact)
```

### 5.2 `serve` Subcommand

CLI form, TOML schema, lookup order: see 2.3. Precedence: CLI > env >
TOML > defaults.

### 5.3 `client` Subcommand

See 3.2. No TOML.

### 5.4 Logging

`tracing` + `tracing-subscriber`. Formats: `compact` (default),
`pretty`, `json`.

**Mandatory span fields:**

| Span | Fields |
|---|---|
| `task` (serve) | `task_id`, `conversation_id`, `acp_session_id` |
| `prompt` (serve) | `task_id`, `prompt_turn_n` |
| `outbound` (client) | `mcp_session`, `port`, `conversation`, `outbound_id` |
| `acp_agent_subprocess` | `pid` |
| `heartbeat` (client) | `outbound_id`, `progress_token`, `elapsed_secs` |

**Level conventions:**

| Level | Use |
|---|---|
| ERROR | `Task` failed; `ACP Agent` crash; config load failure; ACP initialize failure |
| WARN | Non-loopback listen; timeout fired; `deny_tool_kinds` matched |
| INFO | Startup ready; `Conversation` create/sweep; `Task` state transitions; outbound start/end |
| DEBUG | Per-SSE-event; per-ACP-message; idle-timer resets; heartbeat emission |
| TRACE | Full wire dumps |

**Sensitive content rules.** Message text content never logged below
DEBUG (only length/hash). Env var values never logged. Full payloads
appear only at TRACE.

### 5.5 Observability (MVP)

**`Serve Shim` health check:** `GET /health`:

```json
{
  "status": "ok",
  "uptime_secs": 12345,
  "acp_agent": {
    "pid": 9876,
    "command": "claude-agent-acp",
    "initialized_at": "2026-06-03T10:00:00Z"
  },
  "conversations": { "active": 3, "max_active": 64 }
}
```

`Client Shim` has no health endpoint.

**No metrics endpoint in MVP.** All required signals as structured
logs. v1.1 adds Prometheus `/metrics`.

### 5.6 Error Messaging

User-visible errors satisfy three rules: what / why / how to fix. Use
`miette` for diagnostics.

### 5.7 Signal Handling

| Signal | `Serve Shim` | `Client Shim` |
|---|---|---|
| SIGTERM / SIGINT | Graceful shutdown (2.4) | Close stdio, best-effort cancel, exit |
| SIGHUP | Ignored in MVP | ≡ SIGTERM |
| Windows Ctrl+Break | ≡ SIGTERM | ≡ SIGTERM |

### 5.8 Binary Build

```toml
[profile.release]
opt-level = 3
lto = "thin"
codegen-units = 1
strip = true
panic = "abort"
```

Target ≤ 15 MB. CI matrix: Linux x86_64/aarch64 (musl), macOS
x86_64/aarch64, Windows x86_64.

### 5.9 v1.1 TODO (Operations)

```rust
// TODO(v1.1): Prometheus /metrics.
// TODO(v1.1): Config hot reload via SIGHUP (timeouts/log level only).
// TODO(v1.1): Structured audit log — separate stream for permission
//   decisions, conversation lifecycle events, outbound A2A calls.
// TODO(v1.1): OpenTelemetry tracing export.
```

---

## 6. Testing Strategy, MVP Milestones, v1.1 Roadmap

### 6.1 Testing Pyramid

```
┌──────────────────────────┐
│  E2E (small set)         │  Real ACP Agent + real LLM
├──────────────────────────┤
│  Integration (~20)       │  Mock ACP Agent / mock A2A peer
├──────────────────────────┤
│  Unit (~100+)            │  Wire codec, state machines, config
└──────────────────────────┘
```

### 6.2 Unit Tests

Per-crate target ≥ 80% line coverage. Highlights remain as in the prior
draft, with these terminology-aware adjustments:

- `a2a-shim-serve` tests reference the `Conversation`, `Task`, `Turn`
  terms from CONTEXT.md when naming test cases.
- `a2a-shim-client` tests include a CI assertion that the `Client Shim`
  emits no log line to stdout (stdio discipline).
- A new test asserts the `Client Shim` emits the first
  `notifications/progress` heartbeat at T+30 s, not earlier, and stops
  on completion / cancellation (ADR 0003).

### 6.3 Integration Tests

**`Serve Shim` — mock `ACP Agent`.** A scriptable Rust mock implementing
the ACP `Agent` trait. Verifies:

- Full `Task` lifecycle (sync and streaming)
- Cancel behavior in each state
- `Conversation` reuse and isolation (same `conversation_id` →
  reused ACP session; different ids → distinct sessions; absent id →
  fresh UUID per call)
- ConversationBusy triggering
- Permission strategies in action
- `clientCapabilities.fs.* = false, terminal = false` declared in
  `initialize` (ADR 0001)
- `session/new` carries `mcpServers = []` (ADR 0002)
- `ACP Agent` crash → all in-flight `Task`s fail → `Serve Shim` exits
- `elicitation/create` returns method-not-implemented

**`Client Shim` — mock A2A server.** A minimal axum mock. Verifies:

- Successful `a2a_send` path
- Each error branch
- Multimodal degradation
- MCP `notifications/cancelled` triggers outbound abort + remote
  `tasks/cancel`
- Heartbeat fires every 30 s when `progressToken` supplied; absent when
  not (ADR 0003)
- `Host` death triggers remote cancel + exit

**Dual-end loopback.** Run `Serve Shim` (mock `ACP Agent`) and
`Client Shim` together; client calls serve. Catches most bilateral
contract bugs.

### 6.4 E2E Tests (Real Stack)

CI optional job; requires `ANTHROPIC_API_KEY`.

- **E2E #1 (Smoke).** Real `claude-agent-acp`; test driver asks
  "What is 1+1?" via `a2a_send`; assert answer contains "2".
- **E2E #2 (Conversation isolation).** Two `conversation` values;
  assert second has no memory of first.
- **E2E #3 (Manual, multi-agent discussion).** Two `Serve Shim`s + one
  `Client Shim` + real Claude Code; human-prompted discussion.
  Documented as a release acceptance step, not in CI.

### 6.5 Testing Infrastructure

| Item | Choice |
|---|---|
| Runner | `cargo test` (unit) + `cargo nextest` (integration) |
| Mocks | Hand-rolled |
| Fixtures | JSON under `tests/fixtures/` |
| Time control | `tokio::time::pause()` |
| Coverage | `cargo llvm-cov`; CI warns at <80% |
| CI OS matrix | Linux x86_64 + macOS aarch64 + Windows x86_64 |

### 6.6 MVP Milestones

**Phase 0 — Dependency reality check.**

- [ ] Spawn `claude-agent-acp` via `agent-client-protocol` 0.13 in a
      throwaway crate; round-trip `initialize` + `session/new` +
      `session/prompt` + one `end_turn`.
- [ ] Validate the `mcp_server` module can serve standalone for
      `Client Shim`. If not, evaluate `rmcp` fallback.
- [ ] **Validate `clientCapabilities.fs.* = false, terminal = false`
      does not break `claude-agent-acp` operation (ADR 0001).**
- [ ] **Validate Claude Code sends `_meta.progressToken` on
      `tools/call` (ADR 0003 prerequisite).**
- [ ] **Validate Claude Code surfaces incoming
      `notifications/progress` in its UI as "alive" rather than
      "stuck" (ADR 0003).**
- [ ] Empirically observe `session/request_permission` frequency.
- [ ] Empirically observe whether `elicitation/create` is currently
      emitted by `claude-agent-acp`.
- [ ] **Validate `ACP Agent` accepts a new `session/prompt` after
      receiving `session/cancel` on the same session.**

**Deliverable:** Phase 0 report confirming assumptions or listing
required spec/ADR amendments.

**Phase 1 — Wire layer (`a2a-shim-core`).** Same as prior draft.

**Phase 2 — `Serve Shim`.** Same scope, with:

- [ ] `clientCapabilities` correctly declared per ADR 0001
- [ ] `mcpServers: []` correctly passed per ADR 0002

**Phase 3 — `Client Shim`.** Same scope, with:

- [ ] 30 s `notifications/progress` heartbeat (ADR 0003), capturing
      `progressToken`, T+30 s first emission, stops on terminal
- [ ] `conversation` default behavior: omitted → fresh UUID

**Phase 4 — Dual-end + cross-platform CI.** Same as prior draft.

**Phase 5 — E2E and real-world validation.** Same as prior draft.

### 6.7 v1.1 Roadmap (consolidated)

**v1.1.0**
1. Multi-modal full support (File/Data Parts end to end)
2. G2 streaming `a2a_send` (real remote progress → MCP
   `tool_call_update`, extending the ADR 0003 heartbeat mechanism)
3. `Conversation` persistence + ACP `session/resume` (`Serve Shim`
   restart-safe; satisfies CONTEXT.md best-effort hardening)
4. Optional `caller_id` argument on `a2a_send` + `Serve Shim`
   `[server.caller_identity]` config to enable
   `(caller_id, conversation_id)` partitioning (ADR 0004)
5. `conversation_mode` argument on `a2a_send` (`new` | `continue` |
   `auto`) with explicit `ConversationLost` signal when `continue`
   misses

**v1.1.1**
6. Push notifications (`tasks/pushNotificationConfig/*`)
7. Prometheus `/metrics`
8. Explicit conversation reset (A2A custom method)

**v1.2.0**
9. Elicitation bridging (once ACP `elicitation/create` stable)
10. Permission passthrough (ACP permission → A2A input-required)
11. H2 concurrent `a2a_send`
12. Per-conversation `ACP Agent` isolation mode (opt-in)
13. **Mesh discussions** — opt-in injection of `a2a_send` into the
    `ACP Agent`'s `session/new` `mcpServers` (ADR 0002 F2 deferral)

**Out of v1.x:** in-shim multi-agent orchestration, auth schemes, HA.

### 6.8 One-Line Summary

> Mocks first, E2E last; MVP in five phases, Phase 0 a critical
> reality check on the ACP crate, `claude-agent-acp` behavior, and the
> ADR 0001 / 0003 assumptions; v1.1 prioritizes multimodal, streaming,
> persistence, and `(caller_id, conversation_id)` partitioning.

---

## Appendix A — Decision Trail

| ID | Question | Decision | Rationale |
|---|---|---|---|
| Q1 | Scope | B: Heterogeneous agent interoperability | Best fits project intent |
| Q2 | Protocol stance | A: Align with Google A2A | Project name and ecosystem maturity |
| Q3 | Deployment shape | B: Sidecar | Language-neutral, fault-isolated |
| Q4 | Local transport (revised) | A → revised to ACP after Q-D | See incident B.2 |
| Q5 | MVP scope | A2A core 8 + multi-turn input-required (later revised by A3) | Operator baseline |
| Q6 | Task state ownership | A: `Serve Shim` owns; `ACP Agent` stateless | Aligns with LLM call pattern |
| Q7a | Timeouts (revised) | Sync idle 2 m / stream idle 10 m / hard 24 h / input-required 24 h | Stream idle prevents middle-layer disconnects |
| Q7b | Cancel semantics | Best-effort; cancel notification on working `Task`s | A2A spec |
| Q7c | Crash handling | Fail-fast on `ACP Agent` death; `Serve Shim` exits | 12-factor |
| Q7d | Outbound error shape | HTTP 200 + structured `{ok, error.kind}` | Friendly to all language clients |
| Q-A | input-required strategy | A3: ACP `end_turn` → A2A `completed`; no simulation | ACP elicitation not stable; `Caller`'s `LLM` drives continuation |
| Q-B | `ACP Agent` lifecycle | B1: Long-lived, single instance per `Serve Shim` | Honors ACP design intent |
| Q-C | `Task` ↔ session mapping (revised) | ❸: Per-`Conversation` ACP session via metadata | Multi-discussion parallelism with memory continuity |
| Q-D | Outbound mechanism | D2: MCP tool bridge | Zero-modification across all ACP agents |
| Q-E | Outbound visibility | E1: Visible via tool_call notification | Observability for multi-agent discussions |
| Q-F | Remote addressing | localhost ports; no allowlist | Port-forwarding owns auth |
| Q-G | Outbound call mode (revised) | G1-SSE: Synchronous to MCP; SSE internally | Keepalive + accurate health monitoring |
| Q-H | Concurrent calls | H1: ConversationBusy error on overlap | No silent queuing; H2 → v1.1 |
| Q-α | Binary structure | α1: Single binary, two subcommands | Easy distribution |
| Q-β | MVP build order | β1: `Serve Shim` first, then `Client Shim` | Server testable via curl before client exists |
| Q-γ | `Client Shim` state (final) | γ1: Stateless per call | A3 already routes continuity via new `Task`s |
| Permission | Permission policy | P4 with default P1 (`auto_approve`) + `deny_tool_kinds` | Operator owns isolation |
| Network | Default bind address | 127.0.0.1; non-loopback warns; `advertised_endpoint` override | Defense by default |
| Agent runtime | How to spawn `ACP Agent` | Whatever the operator configures (e.g., `claude-agent-acp` via npm) | Shim is agent-neutral |

Grilling-session additions:

| ID | Question | Decision | Reference |
|---|---|---|---|
| G-Q1 | What is `Conversation`? | Caller-declared series with isolation-by-default | CONTEXT.md `Conversation` |
| G-Q2 | `Task` vs `Turn` | Two distinct terms; `Task` for A2A protocol object, `Turn` for business cycle | CONTEXT.md `Task`, `Turn` |
| G-Q3 | "agent" overload | 4 precise terms + lowercase informal usage rule | CONTEXT.md `Caller`, `Host`, `ACP Agent`, `LLM` |
| G-Q4 | `Workspace` isolation | Explicit boundary term | CONTEXT.md `Workspace` |
| G-Q5 | `Serve Shim` ACP capabilities | None declared | ADR 0001 |
| G-Q6 | `Serve Shim` MCP injection | None | ADR 0002 |
| G-Q7 | `Client Shim` MCP capabilities | Tools only; no resources/sampling/roots | Symmetry with ADR 0001 |
| G-Q8 | `Client Shim` liveness | 30 s `notifications/progress` heartbeat | ADR 0003 |
| G-Q9 | `Conversation` continuity guarantees | Best-effort; documented loss modes | CONTEXT.md `Conversation` |
| G-Q10 | `conversation_id` namespace | Flat unpartitioned + naming convention | ADR 0004 |
| G-Q11 | `Workspace` warning placement | Embedded in `a2a_send` tool description | Section 3.4 |

## Appendix B — Incident Memos

### B.1 `claude --acp` does not exist (2026-06-03)

Early drafts assumed the `Serve Shim` could spawn `claude --acp`. **This was
incorrect.** Claude Code does not provide an `--acp` entry point. The
ACP ecosystem's "Claude" agent is the independent npm package
`@agentclientprotocol/claude-agent-acp`, maintained by Anthropic.

Lesson: when introducing support for any specific agent, verify the
actual ACP entry point (binary name, arguments, runtime requirements)
before committing examples or code paths.

Resolution: documented examples now use `claude-agent-acp` (globally
installed). Neither `Client Shim` nor `Serve Shim` depends on any specific agent.

### B.2 Local transport revised mid-design (2026-06-03)

The initial design used a custom local HTTP+SSE channel between the
sidecar and the user's agent. This precluded zero-modification
integration with CLI-style agents such as Claude Code, which do not
listen on network ports. The operator pointed out this gap; the design
pivoted to ACP (stdio JSON-RPC), simplifying the design net-net by
reusing the official `agent-client-protocol` crate.

### B.3 "shim" was ambiguous in early grilling (2026-06-03)

During the grilling session, the bare word "shim" was used to refer to
both `Client Shim` and `Serve Shim`, sometimes in the same sentence.
The operator flagged this; CONTEXT.md was extended with the
`Client Shim` / `Serve Shim` / `ACP Client role` / `A2A HTTP client` /
`A2A HTTP server` terms, and "shim" alone is no longer permitted in
design documents.

Lesson: in projects with structural duality, the operative term must
distinguish the duals from day one. Naming hygiene is a load-bearing
design decision.

## Appendix C — Anti-Patterns Accepted in MVP

| Anti-pattern | Why accepted | Upgrade path |
|---|---|---|
| Using `agent_message_chunk` + `end_turn` to simulate elicitation | ACP `elicitation/create` is RFD-stage; current ecosystem norm | v1.2: bidirectional bridging |
| `completed` not revivable; continuation requires a new `Task` with caller-supplied context | Aligns with A3 + γ1; `Caller`'s `LLM` naturally carries discussion context | None planned |
| `auto_approve` default permission strategy | Operator's deployment assumes sandboxed agent | v1.2: `passthrough` strategy enables human-in-the-loop |
| Multimodal `Part`s degraded to text placeholders | Non-trivial mapping work; placeholder retains metadata | v1.1: full bidirectional Part / ContentBlock translation |
| No state persistence | In-memory simplicity; supervisor handles restart | v1.1: SQLite + `session/resume` |
| Internal A2A SSE consumption produces synchronous MCP tool result | G1-SSE compromise | v1.1 G2: forward SSE events as `tool_call_update` |
| `conversation_id` flat namespace (no caller partitioning) | No reliable `Caller` identity; auth not owned by either shim mode | v1.1: optional `caller_id` partitioning (ADR 0004) |
| `Conversation` continuity best-effort (lost on restart/sweep/crash) | In-memory storage trade-off | v1.1: persistence + `conversation_mode` argument |
| `Host`-mediated cross-expert relays grow `Host` context as O(participants × Turns) | Single-direction tree is sufficient for primary use case | v1.2 mesh discussions (ADR 0002 F2) |

---

*End of specification.*
