//! Bidirectional Part <-> ContentBlock translation tests (ADR 0006).
//! Inbound = A2A v1.0 Part → ACP ContentBlock.

use a2a_shim_core::wire::message::Part;
use a2a_shim_serve::translate::{a2a_to_acp, PartCaps};
use agent_client_protocol::schema::ContentBlock;
use serde_json::json;

fn all_caps() -> PartCaps {
    PartCaps {
        image: true,
        audio: true,
        embedded_context: true,
    }
}

fn no_media_caps() -> PartCaps {
    PartCaps {
        image: false,
        audio: false,
        embedded_context: false,
    }
}

#[test]
fn text_part_to_text_contentblock() {
    let parts = vec![Part::Text { text: "hello".into() }];
    let blocks = a2a_to_acp(&parts, &all_caps());
    assert_eq!(blocks.len(), 1);
    assert!(matches!(&blocks[0], ContentBlock::Text(t) if t.text == "hello"));
}

#[test]
fn image_file_part_to_image_contentblock() {
    let parts = vec![Part::File {
        raw: Some("AAAA".into()),
        url: None,
        media_type: "image/png".into(),
        filename: Some("x.png".into()),
    }];
    let blocks = a2a_to_acp(&parts, &all_caps());
    assert_eq!(blocks.len(), 1);
    assert!(matches!(&blocks[0], ContentBlock::Image(img) if img.data == "AAAA" && img.mime_type == "image/png"));
}

#[test]
fn audio_file_part_to_audio_contentblock() {
    let parts = vec![Part::File {
        raw: Some("BBBB".into()),
        url: None,
        media_type: "audio/wav".into(),
        filename: None,
    }];
    let blocks = a2a_to_acp(&parts, &all_caps());
    assert_eq!(blocks.len(), 1);
    assert!(matches!(&blocks[0], ContentBlock::Audio(a) if a.data == "BBBB"));
}

#[test]
fn other_binary_file_part_to_embedded_resource() {
    let parts = vec![Part::File {
        raw: Some("CCCC".into()),
        url: None,
        media_type: "application/pdf".into(),
        filename: Some("doc.pdf".into()),
    }];
    let blocks = a2a_to_acp(&parts, &all_caps());
    assert_eq!(blocks.len(), 1);
    assert!(
        matches!(&blocks[0], ContentBlock::Resource(_)),
        "expected Resource (embedded), got {:?}",
        blocks[0]
    );
}

#[test]
fn url_file_part_to_resource_link() {
    let parts = vec![Part::File {
        raw: None,
        url: Some("https://example.com/x.png".into()),
        media_type: "image/png".into(),
        filename: None,
    }];
    let blocks = a2a_to_acp(&parts, &all_caps());
    assert_eq!(blocks.len(), 1);
    assert!(
        matches!(&blocks[0], ContentBlock::ResourceLink(rl) if rl.uri == "https://example.com/x.png"),
        "got {:?}",
        blocks[0]
    );
}

#[test]
fn data_part_to_embedded_resource() {
    let parts = vec![Part::Data {
        data: json!({"k": 1}),
        media_type: "application/json".into(),
    }];
    let blocks = a2a_to_acp(&parts, &all_caps());
    assert_eq!(blocks.len(), 1);
    assert!(matches!(&blocks[0], ContentBlock::Resource(_)));
}

#[test]
fn image_dropped_when_capability_missing() {
    let parts = vec![Part::File {
        raw: Some("AAAA".into()),
        url: None,
        media_type: "image/png".into(),
        filename: None,
    }];
    let blocks = a2a_to_acp(&parts, &no_media_caps());
    assert!(blocks.is_empty(), "expected drop, got {blocks:?}");
}

#[test]
fn audio_dropped_when_capability_missing() {
    let parts = vec![Part::File {
        raw: Some("BBBB".into()),
        url: None,
        media_type: "audio/wav".into(),
        filename: None,
    }];
    let blocks = a2a_to_acp(&parts, &no_media_caps());
    assert!(blocks.is_empty());
}

#[test]
fn embedded_resource_dropped_when_capability_missing() {
    let parts = vec![Part::Data {
        data: json!({"k":1}),
        media_type: "application/json".into(),
    }];
    let blocks = a2a_to_acp(&parts, &no_media_caps());
    assert!(blocks.is_empty(), "expected drop, got {blocks:?}");
}

#[test]
fn resource_link_passes_through_without_embedded_context_cap() {
    // ResourceLink doesn't require embedded_context.
    let parts = vec![Part::File {
        raw: None,
        url: Some("https://example.com/x.png".into()),
        media_type: "image/png".into(),
        filename: None,
    }];
    let blocks = a2a_to_acp(&parts, &no_media_caps());
    assert_eq!(blocks.len(), 1);
    assert!(matches!(&blocks[0], ContentBlock::ResourceLink(_)));
}

#[test]
fn text_unaffected_by_caps() {
    let parts = vec![Part::Text { text: "always works".into() }];
    let blocks = a2a_to_acp(&parts, &no_media_caps());
    assert_eq!(blocks.len(), 1);
}

#[test]
fn mixed_parts_drop_only_gated_variants() {
    let parts = vec![
        Part::Text { text: "ok".into() },
        Part::File {
            raw: Some("AAAA".into()),
            url: None,
            media_type: "image/png".into(),
            filename: None,
        },
        Part::Text { text: "also ok".into() },
    ];
    let blocks = a2a_to_acp(&parts, &no_media_caps());
    assert_eq!(blocks.len(), 2);
    assert!(matches!(&blocks[0], ContentBlock::Text(t) if t.text == "ok"));
    assert!(matches!(&blocks[1], ContentBlock::Text(t) if t.text == "also ok"));
}
