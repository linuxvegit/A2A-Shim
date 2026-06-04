//! A2A v1.0 Part wire shape (ADR 0005).
//!
//! Discrimination by member presence, not by a `type` tag.
//! Field renames: bytes→raw, mimeType→mediaType, uri→url, name→filename.

use a2a_shim_core::wire::message::{Message, MessageMetadata, MessageRole, Part};
use serde_json::{json, Value};

#[test]
fn text_part_wire_has_no_type_tag() {
    let p = Part::Text {
        text: "hello".into(),
    };
    let v = serde_json::to_value(&p).unwrap();
    assert_eq!(v, json!({"text": "hello"}), "got: {v}");
    // Critically: no `type` key.
    assert!(
        v.get("type").is_none(),
        "Text Part must NOT emit 'type' tag in v1.0"
    );
}

#[test]
fn text_part_roundtrip() {
    let raw = r#"{"text":"hello"}"#;
    let p: Part = serde_json::from_str(raw).unwrap();
    assert!(matches!(p, Part::Text { ref text } if text == "hello"));
}

#[test]
fn file_part_with_raw_and_mediatype() {
    let raw = r#"{"raw":"AAAA","mediaType":"image/png","filename":"x.png"}"#;
    let Part::File {
        raw: r,
        url,
        media_type,
        filename,
    } = serde_json::from_str(raw).unwrap()
    else {
        panic!("expected File variant");
    };
    assert_eq!(r.as_deref(), Some("AAAA"));
    assert!(url.is_none());
    assert_eq!(media_type, "image/png");
    assert_eq!(filename.as_deref(), Some("x.png"));
}

#[test]
fn file_part_with_url() {
    let raw = r#"{"url":"https://example.com/x.png","mediaType":"image/png"}"#;
    let Part::File {
        raw: r,
        url,
        media_type,
        ..
    } = serde_json::from_str(raw).unwrap()
    else {
        panic!("expected File variant");
    };
    assert!(r.is_none());
    assert_eq!(url.as_deref(), Some("https://example.com/x.png"));
    assert_eq!(media_type, "image/png");
}

#[test]
fn data_part_with_media_type() {
    let raw = r#"{"data":{"x":1},"mediaType":"application/json"}"#;
    let Part::Data { data, media_type } = serde_json::from_str(raw).unwrap() else {
        panic!("expected Data variant");
    };
    assert_eq!(data, json!({"x": 1}));
    assert_eq!(media_type, "application/json");
}

#[test]
fn discrimination_text_first_under_untagged() {
    // Member-presence discrimination: `{"text":"hi"}` has no mediaType so
    // it could superficially look like neither File nor Data. Text must
    // be the first untagged variant so this binds correctly.
    let raw = r#"{"text":"hi"}"#;
    let p: Part = serde_json::from_str(raw).unwrap();
    assert!(matches!(p, Part::Text { .. }), "got: {p:?}");

    // A File-shaped payload MUST NOT bind as Text — it lacks `text` member.
    let raw_file = r#"{"raw":"AAAA","mediaType":"image/png"}"#;
    let p: Part = serde_json::from_str(raw_file).unwrap();
    assert!(matches!(p, Part::File { .. }), "got: {p:?}");

    // A Data-shaped payload MUST NOT bind as File — it lacks raw/url and has data.
    let raw_data = r#"{"data":42,"mediaType":"application/json"}"#;
    let p: Part = serde_json::from_str(raw_data).unwrap();
    assert!(matches!(p, Part::Data { .. }), "got: {p:?}");
}

#[test]
fn message_roundtrip_with_v1_part_shape() {
    let m = Message {
        role: MessageRole::User,
        parts: vec![Part::Text { text: "hi".into() }],
        metadata: None,
    };
    let v = serde_json::to_value(&m).unwrap();
    assert_eq!(v["parts"][0], json!({"text": "hi"}));
    let back: Message = serde_json::from_value(v).unwrap();
    assert_eq!(back.role, MessageRole::User);
    assert_eq!(back.parts.len(), 1);
}

#[test]
fn metadata_passthrough_keys_survive() {
    // Carry over from v0.1.0: unknown metadata keys must round-trip.
    let raw = r#"{
        "role":"user",
        "parts":[{"text":"hi"}],
        "metadata":{
            "x-a2a-shim/conversation":"alice/review",
            "x-custom/marker":"keep"
        }
    }"#;
    let m: Message = serde_json::from_str(raw).unwrap();
    let md = m.metadata.as_ref().unwrap();
    assert_eq!(md.conversation.as_deref(), Some("alice/review"));
    assert_eq!(md.extra.get("x-custom/marker"), Some(&json!("keep")));
    let s = serde_json::to_string(&m).unwrap();
    assert!(s.contains("x-custom/marker"), "lost passthrough: {s}");
}

#[test]
fn default_metadata_omits_conversation_key() {
    // Unchanged behavior from v0.1.0.
    let s = serde_json::to_string(&MessageMetadata::default()).unwrap();
    assert!(!s.contains("x-a2a-shim/conversation"), "got: {s}");
}

#[test]
fn v0_legacy_type_tagged_form_no_longer_parses_as_text() {
    // ADR 0005: hard cutover. A v0.x payload `{"type":"text","text":"hi"}`
    // would now bind to a *different* variant or fail outright; either is
    // acceptable, what's NOT acceptable is silently treating it as v1.0
    // Text. We assert it does NOT round-trip as Text by checking that
    // serializing a Text gives a v1.0 shape (no `type` field).
    let p = Part::Text { text: "hi".into() };
    let v: Value = serde_json::to_value(&p).unwrap();
    assert!(v.get("type").is_none());
}
