# ADR 0008 — Push Notifications: Delivery Model

**Date:** 2026-06-04
**Status:** Accepted
**Relates to:** v1.1 item #6, ADR 0007 (persistence).
**Builds on:** A2A v1.0.1 §3.1.7-3.1.10 + §3.5.3 + §4.3 (wire shapes
captured in [`verify-v1.1/REPORT.md`](../../verify-v1.1/REPORT.md)
Spike C).

## Context

A2A v1.0.1 includes push notifications as a way for clients to receive
asynchronous task updates via HTTP POST webhooks instead of holding an
SSE connection open. The spec pins the four JSON-RPC methods
(`{Create,Get,List,Delete}TaskPushNotificationConfig`), the
`PushNotificationConfig` shape (`url`, `token`, `authentication`,
`tenant`, etc.), and the delivery semantics (HTTP POST with
`StreamResponse` payload, exponential backoff, MAY stop after N
failures).

v0.1.0 advertised `pushNotifications: false` in its AgentCard and did
not implement any of this. v1.1 ships the full surface.

The grilled decisions (see [`verify-v1.1/REPORT.md`](../../verify-v1.1/REPORT.md)
"Item 6" grilling):

- **Storage:** persist configs in SQLite alongside conversations/tasks
  (ADR 0007). Configs MUST survive Serve restart so a pending Task
  webhook can still fire after deploy.
- **Delivery architecture:** one global worker pool (8 workers).
- **Authentication:** pass through `AuthenticationInfo.scheme +
  credentials` verbatim as `Authorization: <scheme> <credentials>`.
- **AgentCard:** advertise `pushNotifications: true` by default.

This ADR pins the delivery worker, retry policy, and failure handling
that the grilling left under-specified.

## Decision

### Delivery worker pool

A single global `tokio::sync::mpsc::Sender<DeliveryJob>` feeds 8 worker
tasks spawned at Serve Shim start. Each worker:

```rust
while let Some(job) = rx.recv().await {
    deliver(&http_client, &job, retry_policy).await;
}
```

`http_client` is a shared `reqwest::Client` configured with:
- `connect_timeout: 5s`
- `timeout: 10s` (per A2A spec recommended 10-30s)
- `redirect: limited(3)`
- HTTPS verified against system roots; HTTP allowed but
  `tracing::warn!` once per config-id-first-use.

The fixed pool size (8) bounds outbound concurrency and CPU; tunable
in v1.2 if real load shows a need. mpsc channel is unbounded so
producers (the bridge, on terminal Task transitions) never block.

### Trigger points

A `DeliveryJob` is enqueued whenever:

1. `TaskRegistry::transition` lands a `Task` in a terminal state
   (`Completed`, `Failed`, `Canceled`).
2. The corresponding `tasks/get` snapshot has at least one
   `PushNotificationConfig` in the registry.

The job payload is a `StreamResponse`-shaped value matching what the
SSE branch would emit, wrapped per A2A v1.0:

```json
{
  "statusUpdate": {
    "taskId": "t-…",
    "contextId": "<conv id>",
    "status": { "state": "completed", "timestamp": "..." }
  }
}
```

Artifact updates do NOT trigger push deliveries in v1.1 — only terminal
state transitions. Spec § 3.5.3 leaves intermediate updates optional;
pushing every artifact-update would spike webhook volume without
operational value for the v1.1 target use case (long-running tasks that
finish out-of-band). v1.2 may add an opt-in `[server.push_notifications].deliver_artifact_updates = true`.

### Retry policy

```rust
struct RetryPolicy {
    max_attempts: usize,      // 3
    backoff_base_secs: u64,   // 1
    backoff_factor: u64,      // 3
}
```

Sequence: attempt #1 immediately, on failure wait 1s, attempt #2, on
failure wait 3s, attempt #3, then drop with `tracing::warn!(taskId,
configId, error = …, "push delivery dropped after exhausted retries")`.

A "failure" is any of:
- `reqwest::Error` (connect / timeout / TLS).
- HTTP 5xx response.
- HTTP 408 (Request Timeout) or 429 (Too Many Requests).

A 2xx or 3xx response is success. A 4xx other than 408/429 is a
permanent failure (the webhook said "no" — retrying won't help): drop
immediately with `tracing::error!(status, body_preview, "push
delivery rejected by webhook")`.

After 3 consecutive successful 2xx responses to a given config, no
state change. After M (default 10) consecutive permanent-failure
responses to the same config, the worker DELETEs the
`PushNotificationConfig` from the registry + DB and `tracing::warn`s.
Operators get a clear breadcrumb that the webhook stopped working
without us hammering it forever.

### Authentication

`PushNotificationConfig.authentication = { scheme, credentials }` is
passed through to the outbound request as:

```
Authorization: <scheme> <credentials>
```

No interpretation: `Bearer xyz`, `Basic dXNlcjpwYXNz`, `OAuth2 …`
all serialize the same way. The webhook is responsible for validating.

The deprecated `PushNotificationConfig.token` field (which spec § 3.1.7
keeps for compatibility) is mapped equivalently when present:
`Authorization: Bearer <token>` (Bearer implied per spec example
payload).

If both `authentication` and `token` are set, `authentication` wins
and `token` is logged at `debug!` as ignored.

### Storage lifecycle

Configs live in `push_notification_configs` (ADR 0007 schema). The
lifecycle binding to Tasks:

- INSERT on `CreateTaskPushNotificationConfig`.
- DELETE on `DeleteTaskPushNotificationConfig` OR when the
  parent Task row is deleted (CASCADE).
- DELETE on M consecutive permanent-failure deliveries (above).
- Survives Serve restart: on startup, the bootstrap reads
  `push_notification_configs` rows for any non-terminal Task and
  re-installs them in the in-memory registry. Configs for
  already-terminal Tasks were either delivered (and not deleted —
  spec § 3.1.7 says configs "MUST persist until task completion or
  explicit deletion", so we keep them for GET/LIST visibility) or
  will be re-tried on the next terminal transition (which won't
  happen for a terminal task; effectively they hang on for GET/LIST
  inspection until DELETE).

### AgentCard

```json
"capabilities": {
  "streaming": true,
  "pushNotifications": true,
  "stateTransitionHistory": true
}
```

Operators who want pushNotifications off (e.g. air-gapped deploys
that can't reach external webhooks) set
`[server.push_notifications].enabled = false`. When disabled, the four
methods return spec § 3.1.7's `PushNotificationNotSupportedError`
(JSON-RPC code TBD in our error-codes.rs as
`PUSH_NOTIFICATIONS_NOT_SUPPORTED = -32030`).

## Consequences

- One new workspace dep: nothing new — `reqwest` is already pulled.
- The mpsc channel is unbounded so the SQLite write rate is the
  effective backpressure: if push delivery jobs queue faster than the
  pool can drain, memory grows. In practice the rate is bounded by
  Task terminal transitions (slow). We log a `WARN` at 1000 queued
  jobs as a tripwire.
- HTTP webhook is the only outbound network call the Serve Shim makes
  on its own (Client Shim has its own outbound for `a2a_send`). New
  egress surface — operators with strict outbound firewall need to
  allowlist webhook destinations. Documented in operating-notes.md.
- A2A v1.0 push notification payloads are `StreamResponse`-shaped, so
  the same encoder we use for SSE `message/stream` is reused — no
  divergent serialization paths.

## Alternatives Considered

- **In-memory only.** Rejected: a Task that terminates while the
  webhook target is briefly unreachable would lose its delivery
  forever on Serve restart. Item #3's persistence already provides
  the storage; co-locating push configs there is cheap.
- **Per-Task tokio task (lazy spawn).** Rejected because terminal
  transitions are bursty (many Tasks finishing around the same time
  is common in agent workflows) and per-Task task creation
  outweighs the pool dispatch cost. 8 workers handle bursts gracefully.
- **Allow operator-tuned worker count.** Rejected for v1.1 — 8 is fine
  for any deploy we can foresee; add `[server.push_notifications].workers`
  in v1.2 if real load demands it.
- **Deliver artifact-update events too.** Rejected as default-ON;
  spec says intermediate updates are optional; pushing on every chunk
  for a streaming Task would generate hundreds of webhook hits per
  Task. v1.2 may add an opt-in.

## Implementation Notes

- Trigger inside `TaskRegistry::transition`: when the new state is
  terminal, look up registered configs, build `DeliveryJob` per
  config, send through the mpsc. Holds parking_lot::Mutex during DB
  lookup but does NOT await — pure lookups.
- Worker errors `tracing::warn` with full context (`taskId`, `configId`,
  `url`, `attempt`, `status_code` if available, `error_chain`).
- Deletion-after-M-failures requires a counter per `configId`. Store
  in memory (HashMap) — survives the process but not Serve restart;
  after restart the counter resets. Acceptable since restart implies
  the webhook target also probably changed.
- Idempotency: each delivery includes a `Idempotency-Key` header set
  to `task-<taskId>-state-<state>` so a receiver getting the same
  notification twice (across our retry attempts or after restart) can
  dedupe.
