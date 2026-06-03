# ADR 0004 — `conversation_id` is a flat unpartitioned namespace in MVP

- **Status:** Accepted
- **Date:** 2026-06-03
- **Scope:** `Serve Shim`'s `ConversationMap`; `Client Shim`'s
  `a2a_send` tool schema and description

## Context

The `Caller` chooses a `conversation_id` and passes it on every
`a2a_send`. The `Serve Shim` uses that id as the key into its
`ConversationMap` to decide whether to reuse an existing ACP `sessionId`
or call `session/new`.

In any deployment where more than one independent `Caller` reaches the
same `Serve Shim`, two `Caller`s can pick the same string (`"review"`,
`"chat"`, `"db-schema"`, etc.) without knowing about each other. With a
single flat namespace, the `Serve Shim` cannot tell them apart and they
silently share an ACP session — leaking context across unrelated users
and producing answers that reference each other's prior `Turn`s.

The natural defense — partitioning by `Caller` identity — does not work
in MVP, because the `Serve Shim` deliberately performs no
authentication (spec section 1.8) and inbound HTTP connections all
appear to come from `127.0.0.1` (they arrive via the port-forwarding
layer). There is no `Caller` identity available to the `Serve Shim` to
partition with.

## Decision

In MVP, the `Serve Shim`'s `ConversationMap` is **keyed by the bare
`conversation_id` string only**. There is no `Caller`-side partitioning.
The defenses are:

1. **CONTEXT.md** documents this as a known property of the
   `Conversation` term, names the collision risk explicitly, and gives
   recommended vs. discouraged naming patterns.
2. **The `a2a_send` tool description (MCP `tools/list` payload)
   contains both good and bad examples in the `conversation` argument's
   description field**, so the `Host`'s LLM sees them at the point of
   use. Example wording:

   > `conversation`: Optional conversation thread id. Same value across
   > calls = same memory thread on the remote agent. **The remote shim
   > does not partition by caller, so use a globally unique id (e.g.,
   > `"alice/review-2026-06-03"`, `"ci-job-9831"`, or a UUID). Avoid
   > short generic names like `"review"` or `"chat"` — they will
   > collide with other callers.** Omit for fully isolated calls.

3. **v1.1 introduces an optional `caller_id` parameter** on `a2a_send`,
   along with an optional `[server.caller_identity]` configuration
   block on the `Serve Shim` to enable partitioning by
   `(caller_id, conversation_id)` when desired.

## Consequences

### Positive

- **MVP simplicity preserved.** No new parameter, no new error code,
  no new configuration surface, no `Caller` identity model to design.
- **Aligned with the project's auth non-ownership stance** (spec
  section 1.8 + ADR 0001 + ADR 0002): no auth, therefore no reliable
  identity, therefore no partitioning by identity. Honest and
  consistent.
- **Reasonable for the realistic MVP deployment shape.** The primary
  use case (a single operator's `Host` consulting one or more
  per-`Workspace` `Serve Shim`s, each with one `ACP Agent`) is
  single-`Caller`, so collisions are not a routine event.
- **Forward-compatible.** Adding `caller_id` in v1.1 is a backward-
  compatible extension: existing single-`Caller` deployments continue
  to work unchanged with `caller_id` defaulted to `"anonymous"`.

### Negative

- **Multi-`Caller` deployments are exposed to silent collision** unless
  every `Caller` follows the naming convention. There is no defense in
  depth — an LLM that ignores the tool description's warning will
  produce a colliding id and the collision will not be detected.
- **The CONTEXT.md warning is a soft contract.** `Caller`s are
  expected to read and follow it; the `Serve Shim` cannot enforce.
- **No observability for collisions.** When two `Caller`s collide on
  `"review"`, neither sees an error; both see a remote agent that
  occasionally references content they did not send. Diagnosing this
  from logs requires the operator to correlate `Caller`-side traces
  with `Serve Shim`-side `Conversation` activity.

### Phase 0 verification requirement

Phase 0 already tests with a single `Caller` (the test driver). No new
verification is required for this ADR. The collision scenario is
acknowledged but deliberately not exercised in MVP testing because the
intended deployment shape does not trigger it.

## Alternatives considered

### I1 — Document the risk only, no example pattern in the tool schema

Rejected because operator-side documentation alone has minimal effect
when the actual user of `a2a_send` is the `Host`'s LLM. Embedding the
warning in the MCP tool description gives the LLM the warning at the
exact moment it formulates the argument. The cost is a few extra lines
in the tool description.

### I2 — Partition by HTTP-level `Caller` identity (source IP/port,
client cert, etc.)

Rejected as technically infeasible in MVP. Inbound HTTP traffic
arrives from the port-forwarding layer, which strips or replaces
`Caller` identity by design. The `Serve Shim` has no authenticated
identity to use as a key, and inventing one (e.g., trusting an
`X-Forwarded-For` header without verification) would be worse than
no partitioning at all.

### I3 — Require `caller_id` argument on `a2a_send` in MVP

Rejected for MVP because:

- It adds another decision the `Host`'s LLM must make correctly. LLMs
  do not have a natural notion of "who am I"; the operator would have
  to pin this in the system prompt for every `Host` deployment.
- It can be introduced later (v1.1) without breaking existing
  single-`Caller` deployments, so paying the complexity cost now is
  premature.

Retained as the v1.1 implementation, with semantics defined in this
ADR.

### I5 — Defer to Phase 0

Rejected. The collision shape is well-defined without empirical input;
deferring would not produce new information.

## Notes

This ADR codifies a known soft spot rather than a hard solution. It
exists primarily so that future readers understand the asymmetry — why
the `Serve Shim` does *not* partition by caller when other RPC-style
servers typically do — and so that the v1.1 partitioning work has a
clear starting position.

The decision composes cleanly with the other shim-philosophy ADRs:
ADR 0001 (no fs/terminal capabilities), ADR 0002 (no MCP server
injection), and this ADR all flow from the same root principle — the
shim does not own authentication, identity, or isolation, and is
designed to remain useful even when those concerns are handled
entirely by surrounding infrastructure.
