# ADR 0003 — `Client Shim` emits periodic MCP `notifications/progress` during long `a2a_send`

- **Status:** Accepted
- **Date:** 2026-06-03
- **Scope:** `Client Shim` (does not apply to `Serve Shim`)

## Context

A single `a2a_send` call can run for minutes — for example, when the
remote `ACP Agent` is doing a substantial code review or generation. The
G1-SSE outbound design (see spec section 3.6) keeps the underlying
HTTP/SSE connection alive against middle-layer idle timeouts, but
**that protects only the `Client Shim` → remote `Serve Shim` leg**. It
says nothing about the leg between the `Host` and the `Client Shim`.

On that inner leg, the `Host` issued an MCP `tools/call` and is blocked
waiting for a response. The MCP specification provides
`notifications/progress` for exactly this case: the MCP server (here,
the `Client Shim`) can send progress updates so that the `Host`
remains aware the call is alive.

If the `Client Shim` sends nothing, the `Host`'s behavior depends
entirely on its MCP runtime:

- It may apply a per-tool-call timeout that is shorter than a typical
  long remote `Turn` and abort the call.
- It may show no UI affordance that the call is still running, leading
  the user to believe the system is hung.
- It may keep waiting indefinitely — but we cannot rely on this.

## Decision

The `Client Shim` emits a periodic MCP `notifications/progress`
notification, every **30 seconds** (matching the SSE keepalive interval
defined in spec sections 2.12 and 4.5), for the entire duration of any
in-flight `a2a_send` call. The notification carries a minimal payload:

```json
{
  "method": "notifications/progress",
  "params": {
    "progressToken": "<token from tools/call>",
    "progress": 0,
    "total": null,
    "message": "Waiting for remote agent (elapsed Xs)"
  }
}
```

- `progressToken` is the `_meta.progressToken` value the `Host` supplied
  on the original `tools/call`. If the `Host` did not supply one,
  **no progress notifications are sent** (the MCP spec requires a token
  for progress).
- `progress` stays `0` and `total` stays `null` in MVP — these are
  placeholders. The notification's purpose in MVP is liveness, not
  measured progress.
- The 30 s cadence is the same constant as SSE keepalive
  (`SSE_KEEPALIVE_INTERVAL`), so the two layers stay in lockstep and the
  configuration is single-sourced.

The first progress notification is sent at T + 30 s, not immediately at
T + 0, to avoid noise for short calls that finish in under 30 s.

## Consequences

### Positive

- **Defends against `Host` MCP runtime tool-call timeouts** without
  requiring the operator to know or tune those timeouts.
- **`Host` UI gets a meaningful liveness signal**, so end users see
  ongoing activity rather than a frozen tool spinner.
- **Mechanism is forward-compatible with v1.1 G2 (real streaming).**
  G2 replaces the placeholder payload with actual remote-progress
  events; the wiring, scheduling, capability negotiation, and
  `progressToken` handling are all reused unchanged.
- **No new configuration surface in MVP.** The interval is the existing
  SSE constant.

### Negative

- **`Host`s that do not send a `progressToken` get no liveness signal.**
  This is unavoidable per MCP spec semantics. Phase 0 must verify that
  the reference `Host` (Claude Code) sends `progressToken` for tool
  calls; if it does not, this ADR's protection is ineffective in
  practice and a fallback (e.g., G1-MVP-C log notifications) must be
  reconsidered.
- **Adds a small amount of code (~50 LoC + tests)** for the heartbeat
  timer and the notification emitter.
- **`Host` UI may render the heartbeat awkwardly** if it interprets
  progress=0/total=null as "no progress at all" rather than "alive".
  Phase 0 will surface this and inform the wording of the `message`
  field if needed.

### Phase 0 verification requirements

Two items added to the Phase 0 dependency reality check:

1. Verify Claude Code (and any other reference `Host`) sends
   `_meta.progressToken` on `tools/call`.
2. Verify Claude Code surfaces incoming `notifications/progress` in its
   UI in a way that conveys "alive" rather than "stuck".

## Alternatives considered

### G1-MVP-A — Send nothing

Rejected. Relying on the `Host` MCP runtime to never time out an
in-flight long tool call is an assumption that breaks silently when
violated, and the failure mode (a remote `Turn` killed mid-flight while
the `ACP Agent` is mid-compute) is expensive and confusing for users.

### G1-MVP-C — Use `notifications/message` (logging) instead

Rejected on semantic grounds. Logging notifications are intended for
diagnostic or user-relevant messages, not for liveness signaling. Using
them as heartbeats would pollute the `Host`'s log surface with noise on
every long call, and the `Host` UI is not designed to extract liveness
information from log streams.

Logging notifications remain available as a v1.1 fallback if Phase 0
shows that the reference `Host` does not support `progressToken` /
`notifications/progress` at all — but only as a degraded mode.

### G1-MVP-D — Defer to Phase 0

Rejected for MVP commitment, accepted as the verification stance: this
ADR commits to G1-MVP-B as the design, with Phase 0 acting as the
falsification gate. If Phase 0 invalidates the approach, this ADR will
be superseded.

## Notes

This ADR is intentionally narrow: it concerns only `Client Shim`
liveness signaling during long outbound `a2a_send` calls. It does not
change the MVP commitment that the MCP tool call is **synchronous** from
the `Host`'s perspective (G1). Real streaming of remote progress
content to the `Host` (G2) remains a v1.1 task.

When G2 is implemented, this ADR continues to apply unchanged — only
the `params.progress`, `params.total`, and `params.message` payloads
change.
