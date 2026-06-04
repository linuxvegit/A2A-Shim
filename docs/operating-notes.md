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
