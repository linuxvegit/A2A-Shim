//! Outbound: ACP ContentBlock → A2A v1.0 Part (ADR 0006).

use a2a_shim_core::wire::message::Part;
use a2a_shim_serve::translate::acp_to_a2a;
use agent_client_protocol::schema::{
    AudioContent, ContentBlock, ImageContent, ResourceLink, TextContent,
};

#[test]
fn text_block_to_text_part() {
    let blocks = vec![ContentBlock::Text(TextContent::new("hi"))];
    let parts = acp_to_a2a(&blocks);
    assert_eq!(parts.len(), 1);
    assert!(matches!(&parts[0], Part::Text { text } if text == "hi"));
}

#[test]
fn image_block_to_file_part_with_raw_and_image_media_type() {
    let blocks = vec![ContentBlock::Image(ImageContent::new("AAAA", "image/png"))];
    let parts = acp_to_a2a(&blocks);
    assert_eq!(parts.len(), 1);
    match &parts[0] {
        Part::File {
            raw,
            url,
            media_type,
            filename,
        } => {
            assert_eq!(raw.as_deref(), Some("AAAA"));
            assert!(url.is_none());
            assert_eq!(media_type, "image/png");
            assert!(filename.is_none());
        }
        other => panic!("expected File, got {other:?}"),
    }
}

#[test]
fn audio_block_to_file_part_with_audio_media_type() {
    let blocks = vec![ContentBlock::Audio(AudioContent::new("BBBB", "audio/wav"))];
    let parts = acp_to_a2a(&blocks);
    assert_eq!(parts.len(), 1);
    assert!(matches!(
        &parts[0],
        Part::File { raw: Some(r), media_type, .. }
            if r == "BBBB" && media_type == "audio/wav"
    ));
}

#[test]
fn resource_link_block_to_file_part_with_url() {
    let blocks = vec![ContentBlock::ResourceLink(
        ResourceLink::new("doc.pdf", "https://example.com/x.pdf").mime_type("application/pdf"),
    )];
    let parts = acp_to_a2a(&blocks);
    assert_eq!(parts.len(), 1);
    match &parts[0] {
        Part::File {
            raw,
            url,
            media_type,
            ..
        } => {
            assert!(raw.is_none());
            assert_eq!(url.as_deref(), Some("https://example.com/x.pdf"));
            assert_eq!(media_type, "application/pdf");
        }
        other => panic!("got {other:?}"),
    }
}

#[test]
fn resource_link_without_mime_type_defaults_to_octet_stream() {
    let blocks = vec![ContentBlock::ResourceLink(ResourceLink::new(
        "raw",
        "https://example.com/raw",
    ))];
    let parts = acp_to_a2a(&blocks);
    assert!(matches!(
        &parts[0],
        Part::File { media_type, .. } if media_type == "application/octet-stream"
    ));
}

#[test]
fn mixed_blocks_translate_each_independently() {
    let blocks = vec![
        ContentBlock::Text(TextContent::new("answer:")),
        ContentBlock::Image(ImageContent::new("Z", "image/jpeg")),
    ];
    let parts = acp_to_a2a(&blocks);
    assert_eq!(parts.len(), 2);
    assert!(matches!(&parts[0], Part::Text { .. }));
    assert!(matches!(&parts[1], Part::File { .. }));
}
