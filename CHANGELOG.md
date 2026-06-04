# Changelog

All notable changes to A2A-Shim are recorded here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), versioning
follows [SemVer](https://semver.org/spec/v2.0.0.html).

## [1.1.0] — 2026-06-04

Second release. Hard cutover from A2A v0.x to v1.0.1 wire, multi-modal
content, server-side streaming surfaced as MCP progress, durable conversation
state across restart, multi-tenant `caller_id` partitioning, explicit
`conversation_mode` (`new` / `continue` / `auto`), webhook push notifications,
a Prometheus `/metrics` endpoint, and an out-of-band `_shim/conversation/reset`
escape hatch. v1.1 spec lives in
[`docs/superpowers/specs/2026-06-04-a2a-shim-v1.1.md`](docs/superpowers/specs/2026-06-04-a2a-shim-v1.1.md).

### Breaking — A2A v1.0.1 wire (ADR 0005)

- `Part` is now untagged member-presence: `{text}`, `{data, …}`,
  `{file: {raw|url, mediaType, filename?}}`. The legacy `{"type":"text",…}`
  shape no longer parses (`v0_legacy_type_tagged_form_no_longer_parses_as_text`).
- JSON-RPC method names are PascalCase: `SendMessage`, `SendStreamingMessage`,
  `GetTask`, `CancelTask`, `SubscribeToTask`, `ListTasks`,
  `{Create,Get,List,Delete}TaskPushNotificationConfig`.
- SSE frames wrap state in `{statusUpdate: {…}}` / `{artifactUpdate: {…}}`
  envelopes (`wire::sse::SseEvent::{status, artifact}`).
- No legacy compat flag — operators on v0.x must upgrade both ends in lockstep.

### Added — Multi-modal (ADR 0006)

- `a2a-shim-serve::translate`: bidirectional `Part` ↔ ACP `ContentBlock`
  mapping with `PartCaps {image, audio, embedded_context}` capability gating
  on inbound; unknown variants drop with `tracing::warn`. Outbound mapping
  emits text / image / audio / resource-link / embedded-resource.
- New `--max-part-bytes` cap (default 10 MiB) enforced before translate
  via `translate::validate_parts`; oversized parts return `INVALID_PARAMS`.
- mock_acp_agent gains `multimodal`, `streamy`, `resumable`, `echo` scripts.

### Added — G2 streaming (Tasks 15+16)

- `Heartbeat::append_text` plus a 4 KiB UTF-8-safe accumulated buffer; tick
  precedence is explicit summary > tail of accumulated > null. Agent text
  chunks now surface as MCP `notifications/progress` messages on the calling
  Host without a parallel side channel.

### Added — Persistence (ADR 0007)

- `a2a-shim-serve::persistence`: rusqlite + `parking_lot::Mutex` +
  `spawn_blocking`. v1 schema = `conversations` + `tasks` +
  `push_notification_configs` with FK cascades and a `_schema_version`
  migration ladder.
- `[server.persistence]` block; on by default at `./a2a-shim.db`.
  Failures degrade to `tracing::warn` — in-memory remains the source of truth.
- `ConversationMap` + `TaskRegistry` write-through on create / transition /
  cancel / sweep_idle.
- `persistence::recovery::bootstrap` replays surviving rows on startup;
  batches `session/load` (concurrency 8) against the configured agent.
  Agents (e.g. `claude-agent-acp`) replay prior turns as session/update
  notifications, so we do not persist `Task.history`/`artifacts`.

### Added — Multi-tenant (Tasks 26+27)

- `[server.caller_identity]` block: `enabled`, `default_caller_id`,
  `trust_header`. Resolution order: trusted `x-a2a-shim/caller_id` header >
  `_shim_caller_id` metadata > configured default.
- Conversation keys become `caller_id\x1Fconv_id` (US separator) so a single
  `ConversationMap` partitions cleanly without generic plumbing.
- Client Shim exposes a `caller_id` MCP tool arg that injects into outbound
  message metadata.

### Added — Conversation lifecycle (Task 29)

- Client Shim exposes a `conversation_mode` MCP tool arg
  (`new` | `continue` | `auto`, default `auto`).
- Server-side dispatch parses `_shim_conversation_mode` from params:
  `new` on an existing key → `CONVERSATION_EXISTS` (-32012);
  `continue` on a missing key → `CONVERSATION_LOST` (-32013).
- `_shim/conversation/reset` cancels every non-terminal task on the key,
  cancels the ACP session, deletes the persisted row, evicts the map.

### Added — Push notifications (ADR 0008)

- `[server.push_notifications]` block. Four JSON-RPC methods:
  `{Create,Get,List,Delete}TaskPushNotificationConfig` with persistence-backed
  registry.
- 8-worker `tokio::mpsc` delivery pool. Retry policy: 3 attempts, 1s/3s/9s
  exponential backoff. `Authentication` and `token` pass through verbatim.
- v1.1 fires deliveries on terminal transitions only; richer cadences land in
  a future release.

### Added — Observability (Tasks 36-38)

- `[server.metrics]` block. `/metrics` route exposes Prometheus exposition
  via `metrics-exporter-prometheus`.
- `a2a_shim_messages_total{method,outcome}`,
  `a2a_shim_conversations_active`,
  `a2a_shim_task_duration_seconds{terminal_state}` histogram,
  `a2a_shim_push_delivery_total{outcome}`.

### Changed

- 4 new error codes wired through `error::codes` and `ErrorKind`:
  `CONVERSATION_EXISTS` (-32012), `CONVERSATION_LOST` (-32013),
  `PUSH_NOTIFICATIONS_NOT_SUPPORTED` (-32030),
  `INVALID_PUSH_NOTIFICATION_CONFIG` (-32031).
- `SseSink::subscribe` softened to `Option<Receiver>` so `SubscribeToTask`
  can re-attach late.
- `TaskRegistry` gains pagination: `list(after, limit) -> (Vec<Task>, Option<TaskId>)`.
- `Heartbeat` removes the historical `chunk_count` field; cadence and threshold
  acceleration deferred to v1.2.

### Deferred to v1.2

- Outbound capability gating (today's `PartCaps::default()` is all-off for
  inbound only).
- Heartbeat cadence acceleration (30s → 1s under stream pressure) and
  threshold-based emission (50 chars).
- `axum::extract::RequestBodyLimit` and per-part outbound size check on the
  Client Shim (per-part inbound validation already enforces the spirit).
- Push deliveries on richer transitions (today: terminal only).
- `ConversationLost` surfaces as a `ProtocolError` on the Client Shim instead
  of a typed MCP error result.

### Stats

- 47 v1.1 tasks across 7 phases; ~50 commits on master since the `v0.1.0` tag.
- 164+ tests across 55 test binaries green; `cargo fmt --all --check` and
  `cargo clippy --workspace --all-targets -- -D warnings` clean.
- New ADRs: 0005 (wire cutover), 0006 (multi-modal mapping),
  0007 (persistence), 0008 (push notifications).

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
