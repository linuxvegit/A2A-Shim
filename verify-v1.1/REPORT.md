# v1.1 Phase 0 Reality Check — A2A-Shim

**Date:** 2026-06-04
**Probes:** `verify-v1.1/src/spike_a_resume.rs` (built; transcript at
`verify-v1.1/spike_a.log`).
**Outcome:** All planned spikes resolved. **One scope-changing finding**
(Spike C) — must be reflected in spec/plan before v1.1 implementation
begins.

| Component | Resolved version |
|---|---|
| `agent-client-protocol` (Rust crate) | 0.13.1 |
| `agent-client-protocol-schema` | 0.13.5 |
| `@agentclientprotocol/claude-agent-acp` (npm) | 0.40.0 |
| A2A protocol spec | v1.0.1 (released 2025; see https://a2a-protocol.org/dev/specification/) |

---

## Spike A — ACP `session/load` + `session/resume` (drives item #3 persistence)

**Verdict: FULL PASS — including a bonus that simplifies persistence.**

### Static (schema-level) findings

Reading `agent-client-protocol-schema = 0.13.5`'s source directly:
- `LoadSessionRequest` / `LoadSessionResponse` are defined (v1 + v2).
  Constructor: `LoadSessionRequest::new(session_id, cwd)`.
- `ResumeSessionRequest` / `ResumeSessionResponse` are defined similarly.
- `AgentCapabilities.load_session: bool` gates `session/load`.
- `AgentCapabilities.session_capabilities.resume: Option<SessionResumeCapabilities>`
  gates `session/resume`.
- Method strings: `SESSION_LOAD_METHOD_NAME = "session/load"`,
  `SESSION_RESUME_METHOD_NAME = "session/resume"`.

### Runtime (live) findings against `claude-agent-acp@0.40.0`

Captured by `spike_a_resume`:

```
[caps] load_session advertised   = YES
[caps] session.resume advertised = YES
[session/load]   OK — response = LoadSessionResponse { modes: ... }
[session/resume] OK — response = ResumeSessionResponse { modes: ... }
```

### Bonus finding (simplifies the persistence story)

After `session/load`, the agent **replays prior turns as
`session/update` notifications immediately** (`UserMessageChunk` then
`AgentMessageChunk` for the seed prompt). This means:

- SQLite persistence does NOT need to store `Task.history` /
  `Task.artifacts` — the agent will re-emit them on load.
- SQLite needs only:
  - `conversations(conversation_id PK, acp_session_id, cwd, created_at, last_used_at)`
  - `tasks(task_id PK, conversation_id FK, last_known_state, created_at)`
    — for forensic `tasks/get` against a Task whose original ConvMap entry
    was swept.

### Action items for v1.1 plan

- Item #3 (persistence) is GO with the full restart-resume story.
- Use `LoadSessionRequest::new(session_id, cwd)` after re-spawning the
  agent on Serve Shim restart. Bridge re-attaches and receives the
  replay automatically.
- Persist only the minimal schema above. History/artifacts are
  reconstructed from the replay.

---

## Spike B — Claude Code `notifications/progress.message` rendering

**Verdict: DEFERRED (per user direction).**

Item #2 (G2 streaming) extends ADR 0003 by stuffing accumulated agent
text into `notifications/progress.params.message`. Whether Claude Code
actually renders that field — vs ignoring it and showing only liveness
ticks — is a property of the Host, not testable from a Rust probe.

User chose to skip live verification and take the MCP specification at
its word that `message` is rendered as inline progress text. Risk
docketed:

- Implementation will include a bootstrap test that loads a real Claude
  Code session, fires an `a2a_send`, and confirms streamed text reaches
  the UI. If the field is ignored, fallback to "ticks only" — same
  degraded mode ADR 0003 already documents.

### Action items for v1.1 plan

- Treat MCP `notifications/progress.message` as the authoritative
  streaming surface.
- Add an opt-in bootstrap test (`v11_progress_render_check`, marked
  `#[ignore]`) for an operator to flip on once integrated with their
  Host.

---

## Spike C — A2A `pushNotificationConfig` wire schema (drives item #6)

**Verdict: PASS — and the static lookup uncovered a major v0.1.0
conformance gap that v1.1 MUST close.**

### Push notification schema (confirmed against A2A spec v1.0.1)

JSON-RPC methods (PascalCase, per spec §9.4.7):
- `CreateTaskPushNotificationConfig`
- `GetTaskPushNotificationConfig`
- `ListTaskPushNotificationConfigs`
- `DeleteTaskPushNotificationConfig`

Wire shape:

```
PushNotificationConfig {
  tenant: string?               # opaque routing id; must match AgentInterface.tenant if set
  id: string?                   # config id (UUID); server-assigned if absent
  taskId: string?               # associated task
  url: string                   # REQUIRED — webhook destination
  token: string?                # per-task/session token surfacing as Authorization header
  authentication: AuthenticationInfo?
}

AuthenticationInfo {
  scheme: string                # "Bearer" | "Basic" | "OAuth2" | …
  credentials: string           # raw credential value
}
```

Webhook delivery (per spec §3.5.3 + §4.3):
- HTTP POST with `Content-Type: application/a2a+json`.
- Auth via request headers per `PushNotificationConfig.authentication`.
- Agents SHOULD: 10-30s timeouts, exponential backoff, MAY stop after N
  consecutive failures.
- HTTPS RECOMMENDED for webhook URLs.

### Action items for v1.1 plan (item #6)

- Implement the 4 PascalCase methods with the wire shape above.
- In-memory storage for configs (NOT crossed with item #3 SQLite
  persistence — different lifecycle: configs live with the Task).
- Delivery worker: tokio task per Task that POSTs `StreamResponse`-shaped
  payloads on terminal transition. Default policy: 3 attempts, 1/3/9s
  backoff, then drop and log.

### BLOCKING DISCOVERY — v0.1.0 implements A2A v0.3.x/v0.4.x legacy wire

Reading the A2A v1.0.1 spec end-to-end (cached as artifact://235), I
verified three places where our v0.1.0 implementation does NOT match
the current upstream specification:

| Surface | v0.1.0 (legacy v0.3.x form) | A2A v1.0.1 (current form) |
|---|---|---|
| JSON-RPC method names | `message/send`, `message/stream`, `tasks/get`, `tasks/cancel` | `SendMessage`, `SendStreamingMessage`, `GetTask`, `CancelTask`, etc. |
| `Part` discriminator | `{ "type": "text", "text": "…" }` (tagged enum) | `{ "text": "…" }` (member-presence discriminator); same for File/Data |
| SSE event shape | `{ "kind": "status-update", "taskId": …, "status": … }` | `{ "statusUpdate": { "taskId": …, "status": … } }`; same for artifactUpdate |

A2A v1.0's Appendix A.2 records these as "Breaking Change: Kind
Discriminator Removed" and lists legacy/current name pairs. Servers MAY
accept both forms during an overlap period, but emitting current form
in responses is recommended.

### Action items for v1.1 plan (NEW item #0)

User direction: **hard cutover**. v1.1 wire upgrade ships before any
new item that depends on the wire shape (items #1, #6 in particular).

- New item #0 (sequenced before #1, #6): replace v0.x method names,
  Part discriminator, and SSE event wrappers with A2A v1.0 forms. No
  legacy compatibility flag.
- Update `a2a-shim-core::wire::message::Part` to drop `#[serde(tag =
  "type")]`, use `#[serde(untagged)]` with member-presence dispatch.
- Update `a2a-shim-core::wire::sse::SseEvent` to wrap inner objects.
- Update `a2a-shim-serve::http::dispatch` method match arms.
- Update `a2a-shim-client::outbound::build_request_body` to emit
  PascalCase method name.
- Round-trip tests across `a2a-shim-core` need re-baselining; the e2e
  self-loopback already validates end-to-end so it'll re-verify the
  new wire works as a pair.

---

## Summary

| Spike | Outcome | Drives | Action |
|---|---|---|---|
| A — session/load + resume | PASS + simpler schema than expected | item #3 (persistence) | Persist conversations + task index only; agent replays history on load |
| B — Claude Code progress rendering | DEFERRED | item #2 (G2 streaming) | Take MCP spec at its word; add opt-in bootstrap test |
| C — pushNotificationConfig schema | PASS | item #6 (push notifications) | Implement 4 PascalCase methods with the documented wire shape |
| C bonus — A2A v1.0 conformance gap | **NEW item #0 added** | items #0, #1, #6 | Hard cutover; legacy v0.x wire dropped entirely |

**Decision: proceed to grilling phase**, with the v1.1 item count
adjusted from 8 → 9 (item #0 prepended), and the persistence schema
narrowed per Spike A's replay-on-load finding.
