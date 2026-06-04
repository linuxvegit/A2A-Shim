//! Bidirectional translation between A2A v1.0 `Part` and ACP
//! `ContentBlock` (ADR 0006).
//!
//! Pure functions; no I/O. Lives in `a2a-shim-serve` (not core) because
//! it depends on `agent-client-protocol` and the Client Shim never sees
//! ContentBlock directly.
//!
//! Unknown variants in either direction get `tracing::warn`'d and dropped
//! — the enclosing message continues with its remaining parts.
//!
//! Capability gating (inbound only): if the wrapped agent did not
//! advertise `image` / `audio` / `embeddedContext` prompt capability,
//! gated variants are dropped at translation time so the agent never
//! sees a payload it cannot consume.

use a2a_shim_core::wire::message::Part;
use agent_client_protocol::schema::{
    AudioContent, BlobResourceContents, ContentBlock, EmbeddedResource,
    EmbeddedResourceResource, ImageContent, ResourceLink, TextContent, TextResourceContents,
};

/// Subset of ACP `PromptCapabilities` relevant to part translation. The
/// Serve Shim caches this on `initialize` so every translate call has a
/// cheap reference.
#[derive(Debug, Clone, Copy, Default)]
pub struct PartCaps {
    pub image: bool,
    pub audio: bool,
    pub embedded_context: bool,
}

impl PartCaps {
    /// All capabilities OFF. Conservative default used by tests and as
    /// a fallback if the agent's `initialize` response is missing.
    pub fn none() -> Self {
        Self::default()
    }

    /// All capabilities ON. Convenience for tests.
    pub fn all() -> Self {
        Self {
            image: true,
            audio: true,
            embedded_context: true,
        }
    }
}

/// Inbound: A2A `Part` → ACP `ContentBlock`. Unknown / gated parts are
/// dropped with a tracing warning; the surviving translations are returned
/// in input order.
pub fn a2a_to_acp(parts: &[Part], caps: &PartCaps) -> Vec<ContentBlock> {
    let mut out = Vec::with_capacity(parts.len());
    for p in parts {
        if let Some(block) = translate_one_inbound(p, caps) {
            out.push(block);
        }
    }
    out
}

fn translate_one_inbound(p: &Part, caps: &PartCaps) -> Option<ContentBlock> {
    match p {
        Part::Text { text } => Some(ContentBlock::Text(TextContent::new(text.clone()))),
        Part::File {
            raw: Some(raw),
            url: _,
            media_type,
            filename: _,
        } => {
            if media_type.starts_with("image/") {
                if !caps.image {
                    tracing::warn!(
                        media_type,
                        "dropping inbound Image Part: agent lacks image capability"
                    );
                    return None;
                }
                Some(ContentBlock::Image(ImageContent::new(
                    raw.clone(),
                    media_type.clone(),
                )))
            } else if media_type.starts_with("audio/") {
                if !caps.audio {
                    tracing::warn!(
                        media_type,
                        "dropping inbound Audio Part: agent lacks audio capability"
                    );
                    return None;
                }
                Some(ContentBlock::Audio(AudioContent::new(
                    raw.clone(),
                    media_type.clone(),
                )))
            } else {
                // Generic binary blob → EmbeddedResource (BlobResourceContents).
                if !caps.embedded_context {
                    tracing::warn!(
                        media_type,
                        "dropping inbound embedded blob Part: agent lacks embeddedContext capability"
                    );
                    return None;
                }
                // ACP requires a `uri`; we synthesize a data: URI so the
                // agent has something stable to reference.
                let uri = format!("data:{media_type};base64,{raw}");
                let mut blob = BlobResourceContents::new(raw.clone(), uri);
                blob.mime_type = Some(media_type.clone());
                Some(ContentBlock::Resource(EmbeddedResource::new(
                    EmbeddedResourceResource::BlobResourceContents(blob),
                )))
            }
        }
        Part::File {
            raw: None,
            url: Some(url),
            media_type,
            ..
        } => {
            // ResourceLink is always available (per ADR 0006) — no cap gating.
            let link = ResourceLink::new(
                derive_link_name(url),
                url.clone(),
            )
            .mime_type(media_type.clone());
            Some(ContentBlock::ResourceLink(link))
        }
        Part::File {
            raw: None,
            url: None,
            ..
        } => {
            tracing::warn!("dropping inbound File Part: neither raw nor url present");
            None
        }
        Part::Data { data, media_type } => {
            if !caps.embedded_context {
                tracing::warn!(
                    media_type,
                    "dropping inbound Data Part: agent lacks embeddedContext capability"
                );
                return None;
            }
            // Serialize the JSON value as text; agent receives it as
            // a TextResourceContents with the supplied mediaType.
            let text = data.to_string();
            let uri = format!("data:{media_type};utf8,{text}");
            let mut tr = TextResourceContents::new(text, uri);
            tr.mime_type = Some(media_type.clone());
            Some(ContentBlock::Resource(EmbeddedResource::new(
                EmbeddedResourceResource::TextResourceContents(tr),
            )))
        }
    }
}

/// Outbound: ACP `ContentBlock` → A2A `Part`. Variants the SDK adds in
/// future (non_exhaustive) drop with a warning.
pub fn acp_to_a2a(blocks: &[ContentBlock]) -> Vec<Part> {
    let mut out = Vec::with_capacity(blocks.len());
    for b in blocks {
        if let Some(part) = translate_one_outbound(b) {
            out.push(part);
        }
    }
    out
}

fn translate_one_outbound(b: &ContentBlock) -> Option<Part> {
    match b {
        ContentBlock::Text(t) => Some(Part::Text {
            text: t.text.clone(),
        }),
        ContentBlock::Image(img) => Some(Part::File {
            raw: Some(img.data.clone()),
            url: None,
            media_type: img.mime_type.clone(),
            filename: None,
        }),
        ContentBlock::Audio(a) => Some(Part::File {
            raw: Some(a.data.clone()),
            url: None,
            media_type: a.mime_type.clone(),
            filename: None,
        }),
        ContentBlock::ResourceLink(rl) => Some(Part::File {
            raw: None,
            url: Some(rl.uri.clone()),
            media_type: rl
                .mime_type
                .clone()
                .unwrap_or_else(|| "application/octet-stream".into()),
            filename: Some(rl.name.clone()),
        }),
        ContentBlock::Resource(r) => match &r.resource {
            EmbeddedResourceResource::BlobResourceContents(blob) => Some(Part::File {
                raw: Some(blob.blob.clone()),
                url: None,
                media_type: blob
                    .mime_type
                    .clone()
                    .unwrap_or_else(|| "application/octet-stream".into()),
                filename: None,
            }),
            EmbeddedResourceResource::TextResourceContents(tr) => {
                let media_type = tr.mime_type.clone().unwrap_or_else(|| "text/plain".into());
                // If the text parses as JSON and media is application/json,
                // preserve as Data with parsed value; else wrap as Data
                // with the raw text under a JSON string.
                let data: serde_json::Value = serde_json::from_str(&tr.text)
                    .unwrap_or_else(|_| serde_json::Value::String(tr.text.clone()));
                Some(Part::Data { data, media_type })
            }
            _ => {
                tracing::warn!("dropping outbound EmbeddedResource: unknown EmbeddedResourceResource variant");
                None
            }
        },
        other => {
            tracing::warn!(?other, "dropping outbound ContentBlock: unknown variant");
            None
        }
    }
}

fn derive_link_name(url: &str) -> String {
    url.rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(url)
        .to_string()
}
