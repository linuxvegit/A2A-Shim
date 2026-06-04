# A2A-Shim

A bidirectional shim between [Agent Client Protocol](https://agentclientprotocol.com) (ACP) agents and [Google's A2A](https://github.com/google-deepmind/a2a) protocol. Lets a Host like Claude Code consult specialist ACP agents over HTTP as MCP tools, with one wrapped ACP subprocess per Serve Shim.

Full design is in [`docs/superpowers/specs/2026-06-03-a2a-shim-design.md`](docs/superpowers/specs/2026-06-03-a2a-shim-design.md). This README is the operator quickstart.

---

## What it gives you

Two subcommands of one binary:

- **`a2a-shim serve`** — wraps a spawned ACP agent and exposes it as an A2A HTTP endpoint. One per agent.
- **`a2a-shim client`** — runs as a stdio MCP server that the Host registers as a tool. Exposes a single `a2a_send` tool that calls remote A2A endpoints.

Wire it like this:

```
┌────────────────┐  stdio MCP   ┌─────────────────┐   HTTP/SSE    ┌──────────────┐  stdio ACP   ┌─────────────┐
│ Host (Claude)  │ ───────────► │ a2a-shim client │ ────────────► │ a2a-shim     │ ───────────► │ ACP agent   │
│  + MCP server  │              │ (a2a_send tool) │               │ serve        │              │ (e.g.       │
│                │              │                 │               │ + AgentCard  │              │  claude-    │
│                │              │                 │               │              │              │  agent-acp) │
└────────────────┘              └─────────────────┘               └──────────────┘              └─────────────┘
```

You can stand up many Serve Shims (one per specialist agent) and point one Client Shim at all of them — every `a2a_send` call carries the `endpoint` URL.

## Install

```bash
cargo build --release
```

Produces `target/release/a2a-shim` (or `a2a-shim.exe` on Windows).

## Serve quickstart

1. Install an ACP agent. Pick one:

   ```bash
   # The reference agent the project is validated against:
   npm install -g @agentclientprotocol/claude-agent-acp
   # Or the Zed alternative:
   npm install -g @zed-industries/claude-code-acp
   ```

2. Copy and edit the sample config:

   ```bash
   cp sample-config.toml my-serve.toml
   # Edit [agent].command and [agent].cwd to taste.
   ```

3. Run:

   ```bash
   a2a-shim serve --config my-serve.toml
   ```

4. Verify the AgentCard:

   ```bash
   curl http://127.0.0.1:7001/.well-known/agent.json | jq .capabilities
   # {
   #   "streaming": true,
   #   "pushNotifications": false,
   #   "stateTransitionHistory": true
   # }
   ```

## Client quickstart

Register the Client Shim as an MCP server in your Host. For Claude Code, add to `~/.claude.json` (or the project-local equivalent):

```jsonc
{
  "mcpServers": {
    "a2a": {
      "type": "stdio",
      "command": "/abs/path/to/a2a-shim",
      "args": ["client", "--log-file", "/tmp/a2a-shim-client.log"]
    }
  }
}
```

The `a2a_send` tool now appears in the Host's tool catalog. Example call:

```json
{
  "name": "a2a_send",
  "arguments": {
    "endpoint": "http://127.0.0.1:7001",
    "conversation_id": "alice/code-review",
    "message": "Review the most recent diff and flag any concurrency bugs."
  }
}
```

Reuse the same `conversation_id` across calls to keep the remote agent's session warm.

## What's in v1.1

- **A2A protocol v1.0 wire** (PascalCase methods, member-presence `Part`,
  wrapped SSE events). v0.x clients incompatible — pin v0.1.x for legacy.
- **Multi-modal Parts** end-to-end via `translate::a2a_to_acp` /
  `acp_to_a2a` (text, image, audio, embedded resource, link).
- **G2 streaming** — `notifications/progress` carries accumulated agent
  text so Host UIs see live output, not just liveness ticks.
- **SQLite persistence** (default on, `./a2a-shim.db`). Conversations
  survive Serve Shim restart via ACP `session/load` batch recovery (8
  concurrent).
- **caller_id partitioning** (opt-in). Three sources: `X-A2A-Caller-Id`
  header, message metadata, config default. Trust model documented.
- **conversation_mode** arg (`new` / `continue` / `auto`) with
  `CONVERSATION_EXISTS` / `CONVERSATION_LOST` error codes.
- **Push notifications** — 4 JSON-RPC methods + 8-worker pool with
  3-attempt exponential-backoff retry. AgentCard advertises
  `pushNotifications: true`.
- **Prometheus `/metrics`** route mounted on the existing axum router.
- **`_shim/conversation/reset`** extension method.
- **`SubscribeToTask`** + **`ListTasks`** (new A2A v1.0 methods).

Full v1.1 details: [`docs/operating-notes.md` § v1.1 deltas](docs/operating-notes.md).

## Limits

- **One in-flight prompt per `(caller_id, conversation_id)`.** Overlapping calls return JSON-RPC `-32010` (`CONVERSATION_BUSY`).
- **`auto_approve` is the default** permission strategy. The Serve Shim approves every `session/request_permission` the wrapped agent sends. ADR 0001 disables `fs` and `terminal` client capabilities so the agent has very little it *can* request — but if you re-enable them, audit the strategy first.
- **Loopback-only by default.** `listen = "127.0.0.1:7001"`. Binding non-loopback works and logs a `WARN`, but the shim does NOT own auth or TLS — front it with a port-forward, SSH tunnel, or HTTPS terminator (see [`docs/operating-notes.md`](docs/operating-notes.md)).
- **No `passthrough` permission strategy.** Reserved for v1.2.
- **No idle ACP session cancellation.** When the idle reaper evicts a conversation, the underlying ACP session keeps running until the agent process is restarted. v1.2 will track session ids per conversation so the reaper can issue `session/cancel`.
- **Windows shutdown signal: Ctrl-C only.** No portable SIGTERM equivalent.
- **v1.1: PartCaps default off.** Inbound Image/Audio/EmbeddedResource Parts are dropped unless `[server] max_part_bytes` and the agent's cap-cache wiring (v1.2) allow them through. ResourceLink and Text always pass.
- **v1.1: ConversationLost surfaces as ProtocolError on Client Shim.** Serve correctly returns the typed JSON-RPC error; Client outbound doesn't translate it. v1.2 fix.
- **v1.1: TaskRegistry write amplification.** `last_used_at` writes through on every cache hit. v1.1.1 will add 1-in-N sampling.

## References

- **v0.1.0 spec:** [`docs/superpowers/specs/2026-06-03-a2a-shim-design.md`](docs/superpowers/specs/2026-06-03-a2a-shim-design.md)
- **v1.1 spec:** [`docs/superpowers/specs/2026-06-04-a2a-shim-v1.1.md`](docs/superpowers/specs/2026-06-04-a2a-shim-v1.1.md)
- **Glossary:** [`CONTEXT.md`](CONTEXT.md)
- **ADRs:** [`docs/adr/`](docs/adr/) (v0.1.0: 0001-0004; v1.1: 0005-0008)
- **Operating notes:** [`docs/operating-notes.md`](docs/operating-notes.md)
- **Phase 0 reality checks:** [`verify/REPORT.md`](verify/REPORT.md) (v0.1.0), [`verify-v1.1/REPORT.md`](verify-v1.1/REPORT.md) (v1.1)

## Development

```bash
# Full test suite (~30s on a fast machine; spawns subprocesses for e2e).
cargo test --workspace

# Real claude-agent-acp e2e (opt-in; requires ANTHROPIC_API_KEY).
cargo test -p a2a-shim --test e2e_claude_agent_acp -- --ignored

# Lint clean check.
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

The plan-of-record for the v0.1.0 build is
[`docs/superpowers/plans/2026-06-03-a2a-shim-implementation.md`](docs/superpowers/plans/2026-06-03-a2a-shim-implementation.md). Every commit references back to it.

## License

Apache-2.0.
