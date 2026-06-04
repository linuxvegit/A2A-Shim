# Phase 0 Reality Check — A2A-Shim

**Date:** 2026-06-03
**Probe binary:** `verify/src/main.rs` (run via `cargo run --manifest-path verify/Cargo.toml`)
**Full transcript:** `verify/run.log` (committed)

| Component | Resolved version |
|---|---|
| `agent-client-protocol` (Rust crate) | **0.13.1** (crates.io latest as of 2026-06-03) |
| `agent-client-protocol-schema` (transitive) | 0.13.5 |
| `@agentclientprotocol/claude-agent-acp` (npm) | **0.40.0** |
| Node.js | v24.16.0 |
| rustc | 1.95.0 |
| Host OS | Windows 11 Enterprise (10.0.26200) |

The probe ran the canonical happy-path flow against the real `claude-agent-acp` binary:

```
initialize(V1) → session/new(cwd) → session/prompt("2+2") → cancel → session/prompt("3+3")
```

The agent answered `"4"` and `"6"` respectively, both with `stop_reason = EndTurn`.

---

## V1 — Basic ACP flow works against `claude-agent-acp`

**Verdict: PASS**

Evidence (`run.log` lines 8–24):

```
[init] OK — agent_info = Some(Implementation { name: "@agentclientprotocol/claude-agent-acp",
                                                title: Some("Claude Agent"), version: "0.40.0", ... })
[session/new] OK — session_id = SessionId("0d90af5b-2475-4e91-a155-b9204317ea06")
[prompt #1] "What is 2+2? Reply with just the number, nothing else."
[notification] AgentMessageChunk(ContentChunk { content: Text(TextContent { text: "4", ... }), ... })
[prompt #1] stop_reason = EndTurn
```

The 0.13.1 → 0.40.0 cross-implementation handshake succeeded with no protocol-level failures. Streaming
`AgentMessageChunk` notifications were delivered to our callback handler and produced the expected text.

---

## V2 — Usable MCP server crate for Client Shim stdio loop

**Verdict: REVISED on closer reading — hand-roll the stdio MCP loop**

Initially I read `agent_client_protocol::mcp_server` as a stdio MCP server
facility. On revisit during Phase 3 planning, the module docs make clear it
is the **MCP-over-ACP** transport: infrastructure for letting an ACP Agent
invoke MCP tools that the ACP *client* hosts. That is the wrong protocol
direction for the Client Shim, which must serve MCP **to a Host** (Claude
Code) over stdio JSON-RPC.

Re-investigated candidates:

1. `rmcp = "1.7"` — official Rust MCP SDK. Works but pulls a large
   transitive tree and adds attack surface for a Client Shim whose hard
   rule is "stdout = MCP transport, nothing else".
2. **Hand-roll NDJSON over `tokio::io::BufReader<stdin>` / `stdout`.**
   The wire surface we need is small (`initialize`, `tools/list`,
   `tools/call`, `notifications/progress`, `notifications/cancelled`).
   We already have `JsonRpcRequest`/`JsonRpcResponse`/`JsonRpcError` in
   `a2a-shim-core::wire::envelope` so 80% of the codec is reused.

**Decision (revised):** hand-roll. Smaller dep blast radius, full
control over the writer side (which is critical for the
"no log line ever lands on stdout" guarantee), and we already own the
JSON-RPC envelope types. If MCP gains required complex types later
(elicitation, sampling, resources) we can revisit and adopt `rmcp`
behind a feature flag.

---

## V3 — Default `ClientCapabilities` (fs disabled, terminal=false) accepted (ADR 0001)

**Verdict: PASS**

The probe sent `InitializeRequest::new(ProtocolVersion::V1)`, which uses `ClientCapabilities::default()`.
By inspection of the 0.13.1 schema, that default sets `fs.read_text_file = false`, `fs.write_text_file = false`,
and `terminal = false` — exactly what ADR 0001 mandates. The agent accepted the handshake and proceeded through
two complete prompt turns. **ADR 0001 stands as written.**

Implication: in Phase 2 the `AcpClient::initialize` impl (Task 17) can rely on `ClientCapabilities::default()`
and document the dependency on the default in code comments. If a future ACP crate version flips a default to
`true`, the test in Task 17 will catch it.

---

## V4 — Claude Code sends `_meta.progressToken` on `tools/call` (ADR 0003)

**Verdict: DEFERRED**

Not testable from this probe — V4 is a property of the MCP **Host** (Claude Code), not of the ACP agent.
Verification venue: end-to-end integration in Phase 3 (Task 32 heartbeat test) and Phase 4 (Task 34
self-loopback). The Client Shim heartbeat module (Task 31) is already designed to no-op when
`progress_token = None`, so a negative outcome simply means no progress UI in the Host, not a feature
failure.

---

## V5 — Claude Code renders `notifications/progress` as "alive"

**Verdict: DEFERRED** (same reason as V4)

Will be validated by visual inspection in Phase 4 once the Client Shim is wired to a real Claude Code Host.

---

## V6 — `session/request_permission` frequency under `claude-agent-acp`

**Observation: zero permission requests for low-risk LLM-only turns.**

Across two arithmetic prompts (no tool invocations) the probe's
`on_receive_request::<RequestPermissionRequest>` callback was **never invoked**. The agent only requests
permission when it actually wants to do something the client should authorize (file I/O, command execution,
etc.) — and our `ClientCapabilities` advertise neither.

**Implication for the spec § 2.7 default permission strategy:** `AutoApprove` is safe as the MVP default *because*
the only thing it can approve is what the agent asks for, and with `fs/terminal` capabilities disabled the agent
mostly has nothing to ask. If/when the Serve Shim is later configured to enable file or terminal capabilities,
the operator should be steered toward `AutoReject` + an allow-list.

The plan already records this nuance in `docs/adr/0001` and `docs/superpowers/specs/...§ 2.7`. No spec change
needed.

---

## V7 — Does `claude-agent-acp` emit `elicitation/create`?

**Verdict: NOT OBSERVED**

Zero `elicitation/create` requests across both turns. The spec §2.8 design — return `-32601` Method Not Found
and transition the Task to `Failed` — remains the right MVP behavior. If a future Phase 0 re-run under a more
complex prompt does see `elicitation/create`, revisit before Phase 2 Task 21.

---

## V8 — ACP Agent accepts a new `session/prompt` after `session/cancel`

**Verdict: PASS** (this is the most important finding for the Serve Shim Task lifecycle)

Evidence (`run.log` lines 27–32):

```
[cancel] sending SessionCancelNotification (V8 probe setup)
[cancel] notification dispatched
[prompt #2] (V8) "And what is 3+3? Reply with just the number."
[notification] AgentMessageChunk(ContentChunk { content: Text(TextContent { text: "6", ... }), ... })
[prompt #2] V8 PASS — agent accepted reprompt; stop_reason = EndTurn
```

The same `SessionId` accepted a fresh prompt immediately after a cancel notification. This means the **A2A
`conversation_id` → ACP `session_id` 1:1 binding can survive `tasks/cancel`** without spawning a new ACP
session, validating spec §2.6 H2 (session reuse across Tasks).

---

## Unexpected finding — `usage_update` notification variant unknown to schema 0.13.5

**Severity: NOISE, not blocking.** Must be addressed in Phase 2 Task 17.

The agent (`claude-agent-acp@0.40.0`) emits `sessionUpdate: "usage_update"` notifications carrying token-usage
and cost metadata. The Rust schema crate at the version we depend on (`agent-client-protocol-schema = 0.13.5`)
does not include this variant in `SessionUpdate`, so the SDK's incoming-message actor logs:

```
WARN agent_client_protocol::jsonrpc::incoming_actor: Handler errored, reporting back to remote
  method="session/update" err=Error { code: -32602: Invalid params,
  data: { "error": "unknown variant `usage_update`, expected one of `user_message_chunk`, … " ... } }
```

Three of these warnings appeared per prompt turn. **They did not stop the prompt from completing.** The agent
ignored the `-32602` responses and continued streaming `AgentMessageChunk` events through to `EndTurn`.

**Action for Phase 2 Task 17 (AcpClient):**
- Treat `unknown variant` errors on `session/update` deserialization as **soft warnings**, not Task failures.
- Log them at `tracing::debug!` level (not WARN — they are protocol-version skew, not bugs).
- Do **not** transition the Task to `Failed` because of them.
- Add an integration test that injects a `usage_update` notification (via the mock-acp-agent, Task 22) and
  confirms the bridge keeps the Task running.

**Action for Phase 4 housekeeping (Task 36 lint pass):**
- Watch `crates.io` for an `agent-client-protocol = 0.14+` release that adds `usage_update`; bump and remove
  the soft-warning compatibility shim then.

---

## Action items before Phase 1

None blocking. All three load-bearing checks (V1, V3, V8) pass. V2 chose the built-in path. V4/V5 are deferred
to Phase 3/4 as planned. V6/V7 confirm spec §2.7/§2.8 defaults remain sound.

**Decision: proceed to Phase 1 — workspace scaffolding (Task 2).**

One small note carried forward: in Phase 2 Task 17, **add explicit handling for forward-compat
`SessionUpdate` variants** (treat unknown variants as benign, not fatal). Add this to the plan's Risk Register
as R6 and amend Task 17's "Implementation notes" with one bullet.
