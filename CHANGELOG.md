# Changelog

All notable changes to A2A-Shim are recorded here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), versioning
follows [SemVer](https://semver.org/spec/v2.0.0.html).

## [0.1.0] — 2026-06-04

The MVP. Ships the bidirectional shim between ACP agents and Google's A2A
protocol as a single binary with `serve` and `client` subcommands. Full
design lives in
[`docs/superpowers/specs/2026-06-03-a2a-shim-design.md`](docs/superpowers/specs/2026-06-03-a2a-shim-design.md);
operational guidance in [`docs/operating-notes.md`](docs/operating-notes.md).

### Added — Phase 0 (Reality Check)

- `verify/` throwaway crate exercises `agent-client-protocol = 0.13.1`
  against the real `@agentclientprotocol/claude-agent-acp` npm binary
  end-to-end (initialize → session/new → prompt → cancel → re-prompt).
- `verify/REPORT.md` records V1-V8 verdicts and an unexpected forward-compat
  finding (R6: `usage_update` SessionUpdate variants from agent >= 0.40
  that the schema crate does not know).

### Added — Phase 1 (Workspace + wire layer, `a2a-shim-core`)

- Cargo workspace with five members: `a2a-shim` (binary), `a2a-shim-core`,
  `a2a-shim-serve`, `a2a-shim-client`, `tests/mock_acp_agent` (dev-only).
- `wire::envelope`: JSON-RPC 2.0 `JsonRpcRequest<P>`, `JsonRpcResponse<R>`,
  `ResultOrError<R>`, `JsonRpcError`.
- `wire::message`: A2A `Message`, `Part::{Text,File,Data}`, `MessageMetadata`
  with `#[serde(flatten)]` passthrough for unknown keys.
- `wire::task`: `Task`, `TaskStatus`, `TaskState` (kebab-case),
  `TaskState::is_terminal()` source of truth, `Artifact`, `TaskId`.
- `wire::methods`: `SendMessageParams`, `TaskIdParams`.
- `wire::sse`: typed `SseEvent::{StatusUpdate, ArtifactUpdate}` codec.
- `wire::card`: `AgentCard` with `x-a2a-shim/conversations` extension.
- `error::{codes, normalize}`: spec § 4.6 codes (`PARSE_ERROR`,
  `TASK_NOT_FOUND`, `CONVERSATION_BUSY`, etc.) and `NormalizedError`
  envelope.
- `timeout::{idle, ceiling}`: paused-time testable `IdleGuard` and
  `HardCeiling`.
- `config::serve_toml`: full Serve Shim TOML loader with `passthrough`
  rejection.
- `logging`: centralized `tracing_subscriber` init. Type-level guarantee
  that the Client Shim cannot log to stdout (no `Stdout` variant of
  `LogDestination`).
- CLI: `clap`-derive `Cli`/`Command`/`ServeOpts`/`ClientOpts` with env-var
  fallbacks (`A2A_SHIM_*`).
- 35 tests, all green.

### Added — Phase 2 (Serve Shim, `a2a-shim-serve`)

- `SseSink`: per-Task `tokio::sync::broadcast` channel + keepalive emitter.
- `ConversationMap` (spec § 2.6 H1 invariant): per-`conversation_id`
  serial-prompt guard via `try_lock_owned`; capacity-limited; async
  spawn closure for `AcpClient::session_new`; idle sweep returning
  evicted ids.
- `AcpClient`: stable handle over the SDK's `connect_with` driver. A
  background task owns `ConnectionTo<Agent>`; control-plane commands
  flow over `mpsc::UnboundedSender<Command>`. Spawn/initialize/
  session_new/session_prompt/session_cancel. Per-session
  `mpsc<BridgeEvent>` routing for `session/update` notifications, with
  forward-compat handling for unknown variants (R6).
- `TaskRegistry`: state-machine-enforced transitions per spec § 2.5;
  history + artifact accumulators; per-Task `SseSink` accessor.
- `bridge::run_session`: ACP `SessionUpdate` → A2A `Task` state +
  per-Task SSE frames. Accumulates streamed text into a canonical
  `a-answer` artifact.
- `permission::evaluate`: pure decision function for the three strategy
  axes (`auto_approve`, `auto_reject`, plus `deny_tool_kinds`).
- `elicitation::error_message`: canonical reason string for the
  `elicitation/create` → `Failed` path.
- `mock_acp_agent`: scripted stdio mock with `happy` script for tests.
- `agent_card::build_agent_card`: pure function honoring
  `advertised_endpoint`.
- `http`: `axum` router with `GET /.well-known/agent.json`, `POST /`
  JSON-RPC dispatch (`message/send`, `message/stream`, `tasks/get`,
  `tasks/cancel`). All spec § 4.6 error codes mapped.
- `run::run`: top-level orchestration. Config load + CLI override +
  tracing init + non-loopback warn + ACP spawn + listener bind +
  idle reaper + graceful shutdown on Ctrl-C.
- 43 new tests including end-to-end smoke against `mock_acp_agent`.

### Added — Phase 3 (Client Shim, `a2a-shim-client`)

- `tool_schema::tool_definition`: typed `McpToolDefinition` for the
  `a2a_send` tool. Required: `endpoint`, `conversation_id`, `message`.
  Optional: `task_id`, `timeout_secs`, `metadata`.
  `additionalProperties: false`.
- `mcp_server::serve_loop`: hand-rolled stdio MCP JSON-RPC dispatcher.
  Single writer task owns the `AsyncWrite` so heartbeat + tools/call
  result cannot produce interleaved partial frames. Handles
  `initialize`, `tools/list`, `tools/call`, `notifications/*`,
  unknown methods, and malformed JSON.
- `outbound::stream`: HTTP/SSE consumer via `reqwest` +
  `eventsource-stream`. Wraps per-event polls in `IdleGuard` +
  `HardCeiling`; maps every failure mode to `OutboundError`.
- `heartbeat::Heartbeat`: per-call `notifications/progress` emitter at
  configurable cadence. Drop-cancellation via `CancellationToken`.
  No-op when `progress_token` is None (V4/V5 deferred path).
- `cancellation::CancellationRegistry`: per-call token table keyed by
  inbound MCP request id. Wired into `notifications/cancelled` and the
  call handler's `tokio::select!` race.
- `render::{render_completed, render_failed, render_input_required}`:
  pure functions mapping a terminal A2A Task into the three MCP
  `tools/call` result shapes from spec § 3.5.
- `call_handler::call_a2a_send`: orchestrator. Parses args, registers
  cancellation, starts heartbeat, opens outbound stream, pumps events
  updating heartbeat summary, renders terminal result. Sentinel-based
  INVALID_PARAMS signalling so the MCP layer wraps it correctly.
- `run::run`: top-level orchestration. Tracing init with
  stderr-or-file destination (Stdout is unreachable by type
  construction); `serve_loop` driven over real stdin/stdout.
- 23 new tests.

### Added — Phase 4 (Integration & Release)

- `e2e_self_loopback`: spawns real `a2a-shim serve` (wrapping
  `mock_acp_agent`) plus real `a2a-shim client`, drives the client
  via stdin and reads its stdout. Validates the full chain end-to-end
  in under one second.
- `e2e_claude_agent_acp`: optional opt-in test against the real
  `claude-agent-acp` npm binary. Marked `#[ignore]` so default
  `cargo test` skips it; runtime early-return when
  `ANTHROPIC_API_KEY` is unset or the npm package is missing.
- `tests/common::kill_tree`: cross-test helper that uses Windows
  `taskkill /T /F /PID` to wipe subprocess trees so `mock_acp_agent`
  orphans do not deadlock subsequent `cargo test --workspace` runs.
- README + `sample-config.toml` + `docs/operating-notes.md` package
  the operator-facing surface.

### Documentation

- Spec: [`docs/superpowers/specs/2026-06-03-a2a-shim-design.md`](docs/superpowers/specs/2026-06-03-a2a-shim-design.md).
- Plan: [`docs/superpowers/plans/2026-06-03-a2a-shim-implementation.md`](docs/superpowers/plans/2026-06-03-a2a-shim-implementation.md).
- ADRs: [`docs/adr/0001`](docs/adr/0001-serve-shim-acp-client-capabilities.md),
  [`0002`](docs/adr/0002-serve-shim-mcp-servers-empty.md),
  [`0003`](docs/adr/0003-client-shim-progress-heartbeat.md),
  [`0004`](docs/adr/0004-conversation-id-flat-namespace.md).
- Glossary: [`CONTEXT.md`](CONTEXT.md).

### Quality gates

- `cargo build --workspace`: clean.
- `cargo test --workspace`: **102/102 green** (101 passed + 1 ignored).
- `cargo fmt --all -- --check`: clean.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.

### Notable plan deviations (each justified in the relevant commit)

- Rust toolchain pinned at 1.95 (not the plan's 1.75) — follows the
  local environment with the same MSRV semantics; `rust-version =
  "1.95"` in `[workspace.package]`.
- `JsonRpcRequest.jsonrpc` is owned `String`, not `&'static str` —
  serde cannot deserialize into a borrowed `'static` slice.
- Phase 0 V2 was revised in `5b8023b`: `agent-client-protocol::mcp_server`
  is MCP-over-ACP (wrong direction); the Client Shim hand-rolls the
  small stdio NDJSON dispatcher instead.
- `parking_lot::{Mutex, RwLock}` used wherever the guard does not
  cross `.await` per the project's rs-parking-lot rule.
- `LazyLock` preferred over `OnceLock` per the project's rs-lazylock
  rule; the logging idempotency flag was dropped entirely in favor of
  delegating to `tracing_subscriber::try_init`.
- `BridgeEvent::Update` boxes its `SessionUpdate` payload to avoid
  the 304-byte / 1-byte variant-size disparity (`large_enum_variant`).

### Known limitations (carry over to v0.2+)

- `passthrough` permission strategy reserved for v1.2.
- Idle reaper does not explicitly cancel evicted ACP sessions (R6).
- `elicitation/create` returns `-32601` Method Not Found.
- No built-in retry — the Host owns retry policy.
- Windows shutdown signal: Ctrl-C only (tokio limit).
