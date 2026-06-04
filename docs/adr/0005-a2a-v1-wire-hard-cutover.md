# ADR 0005 — A2A v1.0 Wire Hard Cutover (Drop v0.x Legacy)

**Date:** 2026-06-04
**Status:** Accepted
**Supersedes:** wire-shape decisions in
[`docs/superpowers/specs/2026-06-03-a2a-shim-design.md`](../superpowers/specs/2026-06-03-a2a-shim-design.md)
§ 2.6, § 4.3, § 4.5, § 4.7 as they existed for v0.1.0.
**Relates to:** v1.1 items #0, #1, #6.

## Context

The v0.1.0 implementation followed a draft of the A2A specification that
predates the v1.0.0 release (issued 2025) and the current v1.0.1
revision. v1.1 Phase 0 Spike C (see
[`verify-v1.1/REPORT.md`](../../verify-v1.1/REPORT.md)) cross-referenced
our wire shapes against the live spec at
https://a2a-protocol.org/dev/specification/ (v1.0.1) and found three
incompatible differences:

| Surface | v0.1.0 (legacy v0.3.x form) | A2A v1.0.1 (current form) |
|---|---|---|
| JSON-RPC method names | `message/send`, `message/stream`, `tasks/get`, `tasks/cancel` | `SendMessage`, `SendStreamingMessage`, `GetTask`, `CancelTask`, etc. |
| `Part` discriminator | `{ "type": "text", "text": "…" }` (tagged enum) | `{ "text": "…" }` (member-presence discriminator); same for File/Data |
| SSE event shape | `{ "kind": "status-update", "taskId": …, "status": … }` | `{ "statusUpdate": { "taskId": …, "status": … } }`; same for artifactUpdate |

The A2A v1.0 migration appendix (A.2.1) labels the `Part` and SSE changes
"breaking" and lists legacy/current name pairs. Servers MAY accept both
forms during an overlap period, but emitting the current form is
recommended.

## Decision

v1.1 implements a **hard cutover** to A2A v1.0.1 wire shapes. Three
implications:

1. **JSON-RPC method names.** All inbound and outbound A2A method
   strings use the PascalCase forms documented in A2A v1.0.1 §9.4:
   `SendMessage`, `SendStreamingMessage`, `GetTask`, `ListTasks`,
   `CancelTask`, `SubscribeToTask`, `CreateTaskPushNotificationConfig`,
   `GetTaskPushNotificationConfig`, `ListTaskPushNotificationConfigs`,
   `DeleteTaskPushNotificationConfig`, `GetExtendedAgentCard`. Our
   shim-private extension methods (ADR 0008 push delivery webhooks
   payloads; item #8 conversation reset) live under a leading `_shim/`
   prefix to make their non-standard status visible at first sight.

2. **`Part` discriminator.** `a2a_shim_core::wire::message::Part` drops
   `#[serde(tag = "type")]`. Member presence drives discrimination:

   ```rust
   #[derive(Debug, Clone, Serialize, Deserialize)]
   #[serde(untagged)]
   pub enum Part {
       Text { text: String },
       File {
           #[serde(skip_serializing_if = "Option::is_none")] raw: Option<String>,
           #[serde(skip_serializing_if = "Option::is_none")] url: Option<String>,
           #[serde(rename = "mediaType")] media_type: String,
           #[serde(skip_serializing_if = "Option::is_none")] filename: Option<String>,
       },
       Data { data: Value, #[serde(rename = "mediaType")] media_type: String },
   }
   ```

   Order matters under `#[serde(untagged)]`:
   - **`Text` first** so a `{"text":"..."}` payload binds to `Text` (the
     only variant with a `text` member) and not as a `File` whose
     required `mediaType` is missing.
   - **`Data` before `File`** so a `{"data":...,"mediaType":"..."}`
     payload binds to `Data` and not as a `File` (whose `raw`/`url`
     are both optional, leaving `mediaType` as its only required
     field — which `Data` also has). Discovered during Task 2 impl;
     captured in tests/message_roundtrip.rs::discrimination_text_first_under_untagged.
   - Final ordering: `Text`, `Data`, `File`.

3. **SSE event wrappers.** `a2a_shim_core::wire::sse::SseEvent`
   re-shapes to:

   ```rust
   #[derive(Debug, Clone, Serialize, Deserialize)]
   #[serde(untagged)]
   pub enum SseEvent {
       StatusUpdate { #[serde(rename = "statusUpdate")] inner: TaskStatusEnvelope },
       ArtifactUpdate { #[serde(rename = "artifactUpdate")] inner: TaskArtifactEnvelope },
   }
   ```

   Inner envelopes carry the same `taskId` / `contextId` / `status` /
   `artifact` / `final` / `append` fields they did under the legacy
   `kind`-tagged form.

**No legacy wire compatibility flag.** The plan we considered of a
`[server.wire_compat = "legacy"]` opt-in was rejected (see Q-0 in the
grilling): it doubles the deserialize surface, doubles the test matrix,
and the only known consumers of our v0.1.0 wire are our own e2e tests
which we will update in lockstep with the cutover.

## Consequences

- **Breaking for any external A2A v0.x client** that may have integrated
  against our v0.1.0 wire. We do not know of any in the wild; our
  v0.1.0 e2e suite is the only fixture that exercised the legacy shape.
  Operators relying on v0.1.0 wire must pin to a v0.1.x release.
- The hard cutover lets items #1 (multi-modal) and #6 (push notifications)
  ship straight against the v1.0 surface without paper-over translation
  shims. Both items concretely depend on the v1.0 `Part` shape.
- A2A v1.0's `subscribeToTask` and `ListTasks` (both new) are picked up
  as part of the same upgrade (item #0.2 / #0.3). They are pure
  additions and require no client-side coordination.
- All round-trip tests in `a2a-shim-core` need re-baselining against the
  new payload literals; the e2e self-loopback re-verifies the end-to-end
  pair works.

## Alternatives Considered

- **Dual-mode (accept legacy + current, emit current)** — matches the
  A2A migration appendix's recommendation. Rejected for scope: we have
  no live legacy callers to support, and doubling the deserialize path
  buys nothing concrete.
- **Defer wire upgrade to v1.2** — would let v1.1 ship the new feature
  items on the existing legacy wire. Rejected because items #1 and #6
  need the v1.0 `Part` shape and v1.0 push-notif endpoint shape
  respectively; ramming them into legacy form would create migration
  debt that grows with every release.
- **Wire-compat flag (default v1.0, opt-in legacy)** — adds config
  surface for no concrete operator. Rejected as YAGNI.

## Implementation Notes

- `a2a-shim-core::wire::envelope::JsonRpcRequest.method` stays a String;
  the dispatcher matches on the PascalCase strings directly.
- The migration table from A2A v1.0 Appendix A.2.1 is mirrored in our
  test suite as test-only `legacy::` helpers so anyone investigating a
  trace of a v0.x peer can quickly diff.
- v0.1.0's `wire::message::Part::File` carried `{name, mimeType,
  bytes, uri}`. v1.0's shape is `{raw, url, mediaType, filename}`.
  Field rename is per spec; `bytes` → `raw`, `mimeType` → `mediaType`,
  `uri` → `url`, `name` → `filename`.
