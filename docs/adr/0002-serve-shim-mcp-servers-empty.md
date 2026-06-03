# ADR 0002 — `Serve Shim` injects no MCP servers in `session/new`

- **Status:** Accepted
- **Date:** 2026-06-03
- **Scope:** `Serve Shim` (does not apply to `Client Shim`)

## Context

When the `Serve Shim` creates an ACP session via `session/new`, the ACP
specification allows it to pass an `mcpServers` list. Any MCP servers
named there become tools the `ACP Agent` can invoke during a `Turn`. The
`Serve Shim` is therefore in the position to decide what extra
capabilities the `ACP Agent` is offered beyond the agent's own built-in
tools.

The shape and breadth of that decision matters because:

- It determines whether the `ACP Agent` can itself make outbound `Turn`s
  to other A2A endpoints (by being given the same `a2a_send` tool that
  the `Client Shim` provides to a `Host`).
- It is a protocol contract: once an `ACP Agent` has been told a tool
  exists in a session, its LLM will use it, and removing the tool later
  changes observable agent behavior.

Four shapes were considered (F1–F4) and evaluated in the design session.

## Decision

In MVP, the `Serve Shim`'s `session/new` request carries:

```json
{ "mcpServers": [] }
```

The `ACP Agent` is offered **no extra MCP tools** beyond whatever it
ships with internally. In particular, an `ACP Agent` cannot itself
initiate an A2A outbound call through this shim.

The semantic structure for future opt-in of `a2a_send` from the
`ACP Agent` side (F2) is preserved in the v1.1 TODO list. When
introduced, it will:

- Be opt-in per `Serve Shim` deployment via configuration.
- Reuse the existing `Client Shim` binary as the MCP server (the
  `ACP Agent` will spawn `a2a-shim client` itself, exactly as a `Host`
  does).
- Require a deliberate semantic definition of how a `Conversation`
  propagates across nested outbound calls (whether an inner outbound
  call inherits the outer `conversation_id`, generates a child id, or is
  isolated entirely).

## Consequences

### Positive

- **`MVP topology is a single-direction tree`.** Discussions originate
  from a `Caller` (typically a `Host` via a `Client Shim`) and flow into
  one or more `ACP Agent`s. The "captain A delegates to expert B"
  pattern, which was the originating motivation for the project, is
  fully supported. Mesh topologies (B asks C while answering A) are
  deferred to v1.1.
- **No recursive outbound problem.** No cycles to detect, no `Conversation`
  inheritance semantics to define, no depth limits to enforce in MVP.
- **Zero extra implementation work.** `session/new` payload is the empty
  default; no MCP server configuration plumbing, no nested-call
  observability, no quota handling.
- **Cleaner authority model for multi-agent discussions.** "Who is in
  charge of the discussion" is unambiguously the originating `Caller` —
  the `ACP Agent` is a consultant, not a peer that can fan out further.

### Negative

- **`ACP Agent` cannot consult other experts on its own.** A peer-to-peer
  or mesh discussion model is unreachable in MVP. If an `ACP Agent`
  thinks it would benefit from a third-party opinion, it can only
  surface that need in its `Turn` output text and rely on the originating
  `Caller` to act on it.
- **Asymmetry with `Client Shim`.** The `Client Shim` gives a `Host` the
  `a2a_send` tool; the `Serve Shim` does not give the `ACP Agent` an
  equivalent. Readers of the codebase may find this surprising; this ADR
  is the explanation.

### Phase 0 verification requirement

No specific Phase 0 verification is required for this ADR — `mcpServers
= []` is the natural default and the `ACP Agent` is expected to operate
correctly without injected tools.

## Alternatives considered

### F2 — Inject `a2a_send` so the `ACP Agent` can also call out

The `Serve Shim` would include in `mcpServers`:

```json
{ "name": "a2a", "command": "a2a-shim", "args": ["client"] }
```

— effectively giving every `ACP Agent` the same outbound capability a
`Host` has. Rejected for MVP because:

- It enables A → B → C → ... cycles. The shim does not detect cycles
  (and arguably should not, leaving the decision to the agents'
  LLMs), but the operational risk of runaway recursion is real.
- `Conversation` semantics across nested outbound calls must be defined
  — inheritance, child-conversation generation, or hard isolation —
  and that choice has user-visible consequences for memory continuity
  in nested discussions.
- The original goal ("`Host` consults specialist `ACP Agent`s") is
  fully served by the single-direction tree; F2 is a separate
  capability and should be introduced as a separate, deliberate
  feature, not as an MVP byproduct.

Retained as a v1.1 task; the semantic骨架 (opt-in flag, reuse of
`Client Shim` binary, conversation propagation rule) is recorded above
so the future work has a clear starting shape.

### F3 — Let the `Caller` pass `mcpServers` via A2A metadata

Rejected outright. Allowing a remote `Caller` to dictate which MCP
servers an `ACP Agent` loads is equivalent to remote code execution: the
`Caller` could direct the `ACP Agent` to spawn arbitrary commands as MCP
servers. This directly contradicts the project's non-ownership of
authentication and isolation (the port-forwarding layer cannot
meaningfully audit an MCP server command embedded in an A2A request).

### F4 — Operator-configured fixed list of MCP servers in serve TOML

Rejected because it turns the `Serve Shim` into a general MCP server
proxy, expanding its responsibility well beyond "translate A2A ↔ ACP".
If an operator needs the `ACP Agent` to have specific MCP tools, that
configuration belongs in the `ACP Agent`'s own configuration (e.g.,
`claude-agent-acp` has its own MCP settings), not in the shim's.

### F5 — Defer to Phase 0

Rejected; the default behavior (empty list) is a well-defined fallback
that requires no Phase 0 input to commit to.

## Notes

The asymmetry between `Client Shim` (offers `a2a_send`) and `Serve
Shim` (offers nothing) is intentional and explicit. It mirrors the
asymmetry of the originating use case: one party drives the
conversation, the other answers. Symmetry is preserved as a future
option (F2), not as an MVP default.
