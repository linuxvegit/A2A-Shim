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

## Limits (v0.1.0)

- **One in-flight prompt per `conversation_id`.** Overlapping calls on the same conversation return JSON-RPC `-32010` (`CONVERSATION_BUSY`).
- **`auto_approve` is the default** permission strategy. The Serve Shim approves every `session/request_permission` the wrapped agent sends. ADR 0001 disables `fs` and `terminal` client capabilities so the agent has very little it *can* request — but if you re-enable them, audit the strategy first.
- **Loopback-only by default.** `listen = "127.0.0.1:7001"`. Binding non-loopback works and logs a `WARN`, but the shim does NOT own auth or TLS — front it with a port-forward, SSH tunnel, or HTTPS terminator (see [`docs/operating-notes.md`](docs/operating-notes.md)).
- **No `passthrough` permission strategy.** Reserved for v1.2.
- **No idle ACP session cancellation.** When the idle reaper evicts a conversation, the underlying ACP session keeps running until the agent process is restarted. v1.2 will track session ids per conversation so the reaper can issue `session/cancel`.
- **Windows shutdown signal: Ctrl-C only.** No portable SIGTERM equivalent.

## References

- **Spec:** [`docs/superpowers/specs/2026-06-03-a2a-shim-design.md`](docs/superpowers/specs/2026-06-03-a2a-shim-design.md)
- **Glossary:** [`CONTEXT.md`](CONTEXT.md)
- **ADRs:** [`docs/adr/`](docs/adr/)
- **Operating notes:** [`docs/operating-notes.md`](docs/operating-notes.md)
- **Phase 0 reality check:** [`verify/REPORT.md`](verify/REPORT.md)

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
