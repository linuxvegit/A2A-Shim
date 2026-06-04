# A2A-Shim — Operating Notes

Production-flavor advice for running `a2a-shim` outside a developer laptop. The
[README](../README.md) covers the happy path; this document is the where-it-bites
companion.

## Authentication & TLS

A2A-Shim does NOT own authentication or TLS — by design. The Serve Shim assumes
its HTTP surface is only reachable by trusted callers, and the Client Shim does
not present a TLS client certificate. You must front the Serve Shim with one of:

- **Loopback only** (the default `127.0.0.1:7001`). No network exposure; the
  Client Shim runs on the same host and talks over the loopback.
- **An SSH tunnel**, e.g. `ssh -L 7001:127.0.0.1:7001 user@bastion`. The Serve
  Shim still binds loopback on the remote host; the tunnel does the auth.
- **A port-forward / sidecar proxy** with its own TLS terminator and auth (e.g.
  Caddy with mTLS, Cloudflare Tunnel, or an internal API gateway).
- **A WireGuard / Tailscale mesh**, where the network layer authenticates peers.

Binding `0.0.0.0` directly works and logs a `WARN` line on start, but please don't
in production. The Phase 0 reality check confirmed `claude-agent-acp` does not
sandbox file system access by itself; if a hostile caller reaches the Serve Shim
they get whatever the wrapped agent can do.

## Stdout discipline (Client Shim)

The Client Shim's stdout IS the MCP transport. The codebase enforces this at the
type level: `LoggingOptions::destination` has only `Stderr` and `File` variants
— there is no `Stdout`. If you see Host-side JSON parse errors, look for:

- A subprocess wrapper that conflates stderr into stdout (e.g. `2>&1` redirects).
- Any custom shim around the Client Shim binary that writes to stdout itself.
- The end-to-end `client_run_smoke` test in this repo asserts every stdout line
  parses as JSON; run it as a regression after deploying a wrapper.

## Logging

Default destination is stderr at `info`. Override per subcommand:

```
a2a-shim serve  --log-file /var/log/a2a-shim-serve.log  --log-format json
a2a-shim client --log-file /var/log/a2a-shim-client.log --log-level debug
```

Or via env:

```
A2A_SHIM_LOG_FILE=/var/log/a2a-shim-client.log
A2A_SHIM_LOG_LEVEL=debug
```

`--log-format json` emits one JSON object per line — ready for `vector`, `loki`,
or any structured log pipeline. Correlation IDs are emitted as `task_id` and
`conversation_id` fields under `tracing` spans.

For grepping by `task_id` across both shims, the Client Shim records the
returned `_meta.a2aTask.id` (`t-<uuid>`) as `task_id` in its
`render_completed`/`render_failed` events; the Serve Shim emits the same id in
its bridge spans.

## Shutdown

- **Unix:** SIGINT (Ctrl-C) and SIGTERM both trigger graceful shutdown via
  `tokio::signal::ctrl_c`. The serve loop stops accepting new connections,
  drains in-flight bridges, and closes the ACP subprocess. Default
  `shutdown_grace_secs = 5` then forces exit.
- **Windows:** Ctrl-C only. tokio does not expose a portable SIGTERM
  equivalent. Service managers like NSSM should be configured to send Ctrl-C
  (not Ctrl-Break, which is interpreted differently). Killing the parent
  process tree via `taskkill /T /F` is fine for forced shutdown but skips
  graceful drain.

The Client Shim has no signal handler — it exits when its stdin EOFs, which is
what the Host does on tool-server unload.

## Idle conversation eviction

The Serve Shim sweeps `ConversationMap` on a timer (default `idle_secs / 4`,
clamped to 30s..3600s). Conversations whose `last_used_at` is older than
`idle_secs` are evicted from the map. **The underlying ACP session is not
explicitly cancelled in v0.1.0** — that information was dropped with the entry,
so the agent process accumulates dead sessions until restart. Two mitigations
for production:

1. Set `idle_secs` long enough that operators restart the Serve Shim before
   significant accumulation. 24h (the default) usually works.
2. Run the Serve Shim under a process supervisor that restarts daily.

v1.2 will track session ids per evicted conversation and issue `session/cancel`
from the reaper. This is in the spec's R6 follow-up.

## Failure modes & retry

A2A-Shim does NOT retry. Every outbound call is single-shot:

- Client Shim `a2a_send` → on `NetworkError`/`RemoteTimeout`/`RemoteFailed`,
  the tool returns `isError: true` with `_meta.error.kind` set. The Host
  decides whether to retry; we do not.
- Serve Shim `message/send` → on bridge error, returns the JSON-RPC envelope
  with the spec § 4.6 code. Client Shim turns that into an `isError` tool
  result.

If you need retries, do them at the Host level — that's where the policy
should live (idempotency, backoff, dead-letter routing all depend on the
caller's intent).

## Observability checklist

- `tracing` spans carry `conversation_id` and `task_id`. Either field grep
  joins all Serve+Client log lines for one user-visible interaction.
- The AgentCard's `metadata.x-a2a-shim/conversations` block carries `maxActive`
  and `idleSecs`; scrape it as a sanity check that the running config matches
  what you deployed.
- Process metrics: PID 1 of the Serve Shim wraps one ACP subprocess. Watch its
  RSS — large prompts streamed through `bridge` accumulate the canonical answer
  artifact in memory until the Task terminates.
- The Client Shim's stdout is the MCP transport, so prometheus-style metrics
  endpoints are out. If you need them, embed an `/metrics` axum route in the
  Serve Shim (under your loopback-or-tunnel boundary, not exposed) — not in
  the Client Shim.

## v1.1 deltas (Phase 1-6 of v1.1)

### A2A v1.0 wire (ADR 0005)

v1.1 ships a hard cutover to A2A protocol v1.0.1. JSON-RPC method names
are PascalCase (`SendMessage`, `SendStreamingMessage`, `GetTask`,
`CancelTask`, `ListTasks`, `SubscribeToTask`, `CreateTaskPushNotificationConfig`,
etc.). `Part` discrimination is member-presence (no `type` tag).
SSE events use wrapped form (`{"statusUpdate": {...}}`). v0.x-conformant
clients no longer interoperate; pin to v0.1.x if you need legacy.

### Persistence (ADR 0007)

`[server.persistence]` is on by default and writes a SQLite file at
`./a2a-shim.db` (override via `path`). On startup the shim batch-loads
every persisted conversation via ACP `session/load` (8 concurrent);
sessions the agent no longer recognizes are deleted from the DB.

Operational notes:
- The DB file is per-Serve-Shim. Two shims pointing at the same path
  would corrupt each other.
- Backup is `cp a2a-shim.db a2a-shim.db.bak` while the shim is stopped
  (online backup not yet supported).
- Schema migrations are versioned via `_schema_version`; today only
  v1 exists. Future versions will upgrade in-place.
- To start fresh: stop shim, `rm a2a-shim.db`, restart. All
  conversations and tasks are lost; agent sessions on the ACP side
  become orphaned until the agent process is restarted.
- v1.1 limitation: write amplification on hot conversations is
  unmitigated. If you see high `last_used_at` write rates in iostat,
  set `[server.persistence].enabled = false` until v1.1.1.

### Push notifications (ADR 0008)

`[server.push_notifications]` is on by default. AgentCard now
advertises `pushNotifications: true`. Four JSON-RPC methods are
supported:

- `CreateTaskPushNotificationConfig` — register a webhook for a Task.
  Body shape: `{taskId, pushNotificationConfig: {url, token?, authentication?, tenant?}}`.
- `GetTaskPushNotificationConfig {configId}` — fetch one.
- `ListTaskPushNotificationConfigs {taskId}` — list for a Task.
- `DeleteTaskPushNotificationConfig {configId}` — remove one.

Delivery: when a Task reaches a terminal state (`Completed | Failed |
Canceled`), the shim POSTs a `{statusUpdate: {...}}` payload to each
registered config. Content-Type is `application/a2a+json`. Each request
includes an `Idempotency-Key: task-<taskId>-config-<configId>` header
so receivers can dedupe.

Retry policy: 3 attempts with 1s/3s/9s exponential backoff for 5xx /
408 / 429 / connection failures. 4xx other than 408/429 is a permanent
failure: drop immediately. After 10 consecutive permanent failures the
config is auto-deleted (DELETE row + warn log).

Auth: pass `authentication: {scheme, credentials}` in the config. The
webhook receives `Authorization: <scheme> <credentials>`. The deprecated
`token` field is mapped to `Bearer <token>`.

Egress firewall: the Serve Shim now makes outbound HTTP calls to
webhook URLs. Allowlist those destinations explicitly when the shim
runs behind a strict egress filter.

### caller_id partitioning (spec § 5)

`[server.caller_identity].enabled = false` by default (preserves v0.1.0
behavior). When enabled, conversations are partitioned by
`(caller_id, conversation_id)`. Three sources, priority order:
1. `X-A2A-Caller-Id` request header (when `trust_header = true`).
2. `x-a2a-shim/caller_id` metadata key in `SendMessage.params.message.metadata`.
3. `[server.caller_identity].default_caller_id` (default `"anonymous"`).

**Trust model:** when `trust_header = true`, your reverse proxy MUST
authenticate the caller and set the header. The shim does NOT validate
the header against any identity. Set `trust_header = false` if the shim
is exposed directly to untrusted callers.

### conversation_mode (spec item #5)

The Client Shim's `a2a_send` tool gains a `conversation_mode` argument:
- `auto` (default): silently create or reuse — same as v0.1.0.
- `new`: reject if the conversation_id already exists
  (`CONVERSATION_EXISTS` = -32012).
- `continue`: reject if it doesn't exist (`CONVERSATION_LOST` = -32013).

v1.1 limitation: when the Serve Shim returns these errors, the Client
Shim's outbound layer sees them as a JSON-RPC envelope on what it
expected to be an SSE response, surfacing them as `ProtocolError`
rather than the matching `ErrorKind`. v1.2 will translate explicitly.

### Multi-modal Parts (ADR 0006)

Both directions now translate non-text content between A2A `Part` and
ACP `ContentBlock`. Mapping table is in ADR 0006. Unknown variants
are warn-and-drop (the rest of the message continues normally).

Capability gating: inbound Image/Audio/EmbeddedResource Parts are
dropped when the agent's `initialize` response did NOT advertise the
matching capability. v1.1's `PartCaps::default()` is all-off pending
the cap-cache wiring; v1.2 will pull caps from the live initialize
response.

### `--max-part-bytes` (10 MiB default, ADR 0006)

Cap on individual A2A `Part` payload size, enforced before
`a2a_to_acp` translation. Oversize returns `INVALID_PARAMS` with a
message naming the offending index. Configure via
`[server].max_part_bytes`.

### Prometheus `/metrics` (spec § 7)

`[server.metrics].enabled = true` by default. `GET /metrics` returns
text/plain `version=0.0.4` Prometheus format. Five metrics today:
- `a2a_shim_messages_total{method, status}` — counter.
- `a2a_shim_conversations_active` — gauge (wiring deferred to v1.1.1).
- `a2a_shim_tasks_active{state}` — gauge (wiring deferred).
- `a2a_shim_task_duration_seconds{terminal_state}` — histogram
  (recorder available; observation hook deferred).
- `a2a_shim_push_deliveries_total{status}` — counter.

`record_message` fires on every JSON-RPC call. `record_push_delivery`
fires per worker outcome. Cardinality is bounded — no
per-conversation or per-task-id labels.

### `_shim/conversation/reset`

Custom JSON-RPC method (note `_shim/` prefix, flagging non-standard
origin). Params: `{conversation_id, caller_id?}`. Returns
`{cleared: bool, cancelled_task_ids: [...]}`. Side effects:
1. Cancels every non-terminal Task whose `context_id` matches the
   resolved partition key.
2. Sends ACP `session/cancel` (best-effort, logs on failure).
3. DELETEs the conversation row from persistence (CASCADE removes
   tasks + push configs).
4. Removes the entry from in-memory ConversationMap.

Idempotent: reset of an unknown id returns `cleared = false` with no
side effects.

## Known v0.1.0 limitations

| Area | Limit | Spec follow-up |
|------|-------|----------------|
| Permission strategy | `passthrough` reserved | v1.2 |
| Idle session cancel | Orphans ACP sessions | v1.2 R6 |
| `elicitation/create` | Returns `-32601` Method Not Found | v1.2 |
| Retries | None — Host owns retry policy | n/a |
| Windows SIGTERM | Ctrl-C only | tokio limit |
| Multiple ACP agents per Serve | One per Serve Shim | v1.2 |
| Auth/TLS | Delegated to external layer | by design |
