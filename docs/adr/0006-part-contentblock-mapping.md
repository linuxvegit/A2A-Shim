# ADR 0006 — Bidirectional `Part` ↔ `ContentBlock` Mapping (Multi-Modal)

**Date:** 2026-06-04
**Status:** Accepted
**Relates to:** v1.1 item #1, ADR 0005.
**Builds on:** ADR 0005 (A2A v1.0 Part shape).

## Context

v0.1.0 only handled text content end to end. Non-text Parts on inbound
(A2A → Serve Shim → ACP) were dropped silently; non-text ACP
ContentBlocks on outbound (ACP → Serve Shim → A2A) became text
placeholders. Spec § 2.13 and § 3.11 listed multi-modal as a v1.1
TODO; Spike A's Phase 0 finding confirmed `claude-agent-acp@0.40.0`
advertises `prompt_capabilities.image: true` and
`embedded_context: true`, so a real agent is ready to consume the
non-text content if we plumb it through.

The A2A v1.0 `Part` enum has three variants (post-ADR-0005):
- `Text { text }`
- `File { raw|url, mediaType, filename? }`
- `Data { data: Value, mediaType }`

ACP's `ContentBlock` has five (`#[non_exhaustive]`):
- `Text(TextContent)`
- `Image(ImageContent)` — base64-embedded image, requires `image` cap
- `Audio(AudioContent)` — base64-embedded audio, requires `audio` cap
- `ResourceLink(ResourceLink)` — URI reference, always supported
- `Resource(EmbeddedResource)` — inline resource bytes or text,
  requires `embeddedContext` cap

The mapping is asymmetric (3 ↔ 5) and lossy in one direction. This
ADR pins the table so the bridge code has one source of truth.

## Decision

**Bidirectional 5-variant mapping with mime-sniff and warn-and-drop on
unknown variants.**

### Inbound — A2A `Part` → ACP `ContentBlock`

| A2A `Part` | mediaType prefix | ACP `ContentBlock` |
|---|---|---|
| `Text { text }` | (n/a) | `Text(TextContent { text, .. })` |
| `File { raw, mediaType }` | `image/*` | `Image(ImageContent { data: raw, mime_type: mediaType })` |
| `File { raw, mediaType }` | `audio/*` | `Audio(AudioContent { data: raw, mime_type: mediaType })` |
| `File { raw, mediaType }` | other binary | `Resource(EmbeddedResource { resource: BlobResourceContents { blob: raw, mime_type: mediaType, .. } })` |
| `File { url, mediaType }` | any | `ResourceLink(ResourceLink { uri: url, mime_type: Some(mediaType), .. })` |
| `Data { data, mediaType: "application/json" }` | (json) | `Resource(EmbeddedResource { resource: TextResourceContents { text: data.to_string(), mime_type: Some("application/json"), .. } })` |
| `Data { data, mediaType: other }` | any | `Resource(EmbeddedResource { resource: TextResourceContents { text: data.to_string(), mime_type: Some(mediaType), .. } })` |

**Capability gating.** If the upstream `claude-agent-acp`-style ACP
agent does not advertise `image` / `audio` / `embedded_context`
capability and we receive a Part that would map to a gated variant,
we tracing::warn and drop that Part (do not fail the whole `SendMessage`).

### Outbound — ACP `ContentBlock` → A2A `Part`

| ACP `ContentBlock` | A2A `Part` |
|---|---|
| `Text(TextContent { text })` | `Text { text }` |
| `Image(ImageContent { data, mime_type })` | `File { raw: Some(data), mediaType: mime_type, url: None, filename: None }` |
| `Audio(AudioContent { data, mime_type })` | `File { raw: Some(data), mediaType: mime_type, url: None, filename: None }` |
| `ResourceLink(ResourceLink { uri, mime_type })` | `File { url: Some(uri), mediaType: mime_type.unwrap_or("application/octet-stream"), raw: None, filename: None }` |
| `Resource(EmbeddedResource { resource: BlobResourceContents { blob, mime_type } })` | `File { raw: Some(blob), mediaType: mime_type, .. }` |
| `Resource(EmbeddedResource { resource: TextResourceContents { text, mime_type, .. } })` | `Data { data: serde_json::from_str(text).unwrap_or(json!(text)), mediaType: mime_type.unwrap_or("text/plain") }` |

### Unknown variants

Both `ContentBlock` and `Part` are `#[non_exhaustive]` (the SDK
explicitly so for ContentBlock; A2A's spec is silent but treats
extensions via the `_meta` field). When a future variant arrives that
neither direction knows how to map:

- **Tracing.** `tracing::warn!(part_type = ?incoming, "dropping unmapped
  multi-modal part")` with a one-line summary.
- **Drop.** The unknown part is removed from the message before
  forwarding. The remaining parts continue through normally.
- **No failure.** The enclosing `SendMessage` / `session/prompt` does
  not fail; the Task transitions normally and finishes on its remaining
  content.

This matches v0.1.0's behavior for non-text Parts and avoids brittleness
across ACP SDK / A2A spec version drift.

## Consequences

- The bridge code in `a2a-shim-serve::bridge` and the analogous code in
  `a2a-shim-client::call_handler` both invoke
  `a2a_shim_core::wire::message::translate::*` (newly added in v1.1)
  for the typed mapping. Tests live alongside the translate module so
  every cell of the table above is covered.
- Capability gating is centralized in
  `a2a_shim_serve::permission::part_allowed(part, &agent_caps)`,
  parallel to the existing tool-permission policy. Test surface is
  small (~6 cases).
- Outbound `Data` for non-JSON text is best-effort: if `text` is valid
  JSON we round-trip the parsed `Value`; if not, it becomes
  `Value::String(text)`. Operators inspecting the wire on a
  `text/markdown` ContentBlock will see a JSON-quoted string; this is
  intentional so the `Data.data` field stays a JSON value as the spec
  requires.

## Alternatives Considered

- **Preserve unknown variants as opaque `Data { mediaType:
  "application/x-a2a-shim-unknown" }`.** Rejected: round-trip
  preservation has no operational value when neither side knows what to
  do with the bytes, and it pollutes downstream traffic with garbage
  types. Drop + warn is honest about what happened.
- **Text + Image only.** Rejected as too restrictive; Spike A confirmed
  the reference agent advertises `embedded_context` so we should
  honor it.

## Implementation Notes

- New module `a2a_shim_core::wire::message::translate` ships:
  - `pub fn a2a_to_acp(parts: &[Part], caps: &PromptCapabilities) -> Vec<ContentBlock>`
  - `pub fn acp_to_a2a(blocks: &[ContentBlock]) -> Vec<Part>`
- Both functions are pure; the bridge calls them inline, no I/O.
- Size cap (`--max-part-bytes`, default 10 MiB) is enforced
  out-of-band of this module in the HTTP layer before deserialization
  even reaches `Part` (via axum `RequestBodyLimit`); enforced for the
  Client Shim in `outbound::stream` before sending and on inbound SSE
  before forwarding to `bridge::run_session`. See ADR 0005's wire shape
  for where size lives in the payload.
- Capability gating lookups: the Serve Shim caches the agent's
  `PromptCapabilities` on `initialize` so every translate call doesn't
  re-derive them.
