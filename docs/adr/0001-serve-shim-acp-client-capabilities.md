# ADR 0001 — `Serve Shim` declares no ACP filesystem or terminal capabilities

- **Status:** Accepted
- **Date:** 2026-06-03
- **Scope:** `Serve Shim` (does not apply to `Client Shim`, which does not
  participate in ACP)

## Context

The `Serve Shim` plays the `ACP Client role` opposite the spawned `ACP
Agent` subprocess. During ACP `initialize`, the `ACP Client role` MUST
declare which reverse-RPC capabilities it supports, via the
`clientCapabilities` field. The capabilities at issue are:

- `fs.readTextFile` / `fs.writeTextFile` — lets the `ACP Agent` ask the
  `Serve Shim` to read or write files on its behalf.
- `terminal` — lets the `ACP Agent` ask the `Serve Shim` to spawn and
  manage shell sessions on its behalf.

Declaring a capability is a binding protocol contract: the `ACP Agent`
will use it. Not declaring it forces the `ACP Agent` to either skip the
operation or perform it through its own process — which it can already
do, because it runs inside the same `Workspace` (filesystem, env, host
machine) and has direct OS-level access.

The `session/request_permission` method is a baseline ACP method, not a
capability, and is handled separately by the permission strategy
decision (`P4`, default `P1`).

## Decision

The `Serve Shim` declares **none** of these capabilities. Specifically,
the ACP `initialize` request from the `Serve Shim` carries:

```json
{
  "clientCapabilities": {
    "fs": { "readTextFile": false, "writeTextFile": false },
    "terminal": false
  }
}
```

The `ACP Agent` is expected to perform any filesystem or shell work
directly within its own process, against its own `Workspace`.

## Consequences

### Positive

- **Smallest possible attack surface.** The `Serve Shim` exposes no
  arbitrary filesystem read/write or command execution endpoint. The
  port-forwarding layer remains the sole external authentication
  boundary, consistent with the project's stated non-goal of handling
  auth or TLS.
- **No security-audit responsibility on the `Serve Shim`.** It need not
  implement path-traversal protection, command sanitization, or terminal
  resource limits — all of which would otherwise be required and would
  contradict the "shim does not own auth/isolation" principle.
- **Implementation is zero-cost.** No code to write, test, or maintain
  for `fs/*` and `terminal/*` handlers.
- **Behavior remains agent-agnostic.** The `Serve Shim` makes no
  assumptions about how any specific `ACP Agent` handles files or shells.

### Negative

- **`Caller` loses visibility into the `ACP Agent`'s filesystem and
  terminal activity** that would otherwise have been observable through
  reverse RPC and re-emitted as A2A events. The `Caller` sees only the
  final `Task` result and whatever the `ACP Agent` chooses to narrate in
  its messages.
- **Some `ACP Agent` implementations may degrade** in features that
  depend on the host providing fs/terminal — for example, an agent that
  uses the host's terminal capability to surface a live `cargo test`
  pane to the user. This is acceptable because the `Serve Shim` is not
  acting as an interactive IDE.

### Phase 0 verification requirement

Before this decision is considered final, Phase 0 must confirm that
`claude-agent-acp` (the reference `ACP Agent` for MVP) operates
acceptably when both capabilities are declared `false`. If the agent
refuses to initialize, throws hard errors, or loses essential
functionality (e.g., cannot read project files at all), this ADR must be
revisited.

## Alternatives considered

### E2 — Declare and implement both

The `Serve Shim` would expose real `fs/*` and `terminal/*` handlers
that perform OS operations on the agent's behalf. This was rejected
because:

- It makes the `Serve Shim` a remotely-reachable command execution
  endpoint accessible via the A2A port. Even with port-forwarding-layer
  auth in front, this expands the attack surface significantly.
- The `Serve Shim` would have to implement and maintain path-traversal
  protection, shell argument quoting, terminal lifecycle limits, and
  related security controls — squarely contradicting the project's
  intentional non-ownership of authentication and isolation concerns.
- The capability adds no functional power: the `ACP Agent` already has
  the same OS access; routing it through the `Serve Shim` only changes
  observability, not what is possible.

### E3 — Declare and transparently forward

A middle ground where the `Serve Shim` declares the capabilities and
forwards each request to the OS, while logging and emitting A2A events
for observability. Rejected because it inherits E2's expanded attack
surface without providing meaningful added safety — a transparent
forward without sanitization is no safer than E2 implementation, while
sanitization would push toward E2's full responsibility.

### E4 — Mixed declaration (e.g., fs yes, terminal no)

Rejected because the underlying argument is uniform across both
capabilities: in both cases the `ACP Agent` can already perform the
operation directly, and in both cases declaring it transfers
security-audit responsibility to the `Serve Shim`. There is no
principled reason to draw the line between `fs/*` and `terminal/*`.

### E5 — Defer to Phase 0

Rejected as the default decision but adopted as a verification gate:
this ADR encodes E1 now so that Phase 0 has a clear hypothesis to
falsify, rather than leaving the choice open.

## Notes

This decision applies only to the MVP and is revisitable. If a future
deployment scenario (e.g., a managed `Serve Shim` distribution intended
to surface agent activity to end users) genuinely requires `Serve Shim`
mediation of fs/terminal calls, a follow-on ADR can supersede this one,
ideally with a per-deployment configuration flag rather than a default
behavior change.
