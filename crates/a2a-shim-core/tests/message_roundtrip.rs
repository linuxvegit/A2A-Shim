use a2a_shim_core::wire::message::{Message, MessageMetadata, MessageRole, Part};
use serde_json::json;

#[test]
fn text_message_roundtrip() {
    let m = Message {
        role: MessageRole::User,
        parts: vec![Part::Text { text: "hello".into() }],
        metadata: None,
    };
    let back: Message = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
    assert_eq!(back.role, MessageRole::User);
    assert_eq!(back.parts.len(), 1);
}

#[test]
fn metadata_extracts_conversation_and_preserves_unknown_keys() {
    let raw = r#"{
        "role":"user",
        "parts":[{"type":"text","text":"hi"}],
        "metadata":{
            "x-a2a-shim/conversation":"alice/review",
            "x-custom/marker":"keep"
        }
    }"#;
    let m: Message = serde_json::from_str(raw).unwrap();
    let md = m.metadata.as_ref().unwrap();
    assert_eq!(md.conversation.as_deref(), Some("alice/review"));
    assert_eq!(md.extra.get("x-custom/marker"), Some(&json!("keep")));
    // Re-serialize and confirm the unknown key survives the round-trip.
    let s = serde_json::to_string(&m).unwrap();
    assert!(s.contains("x-custom/marker"), "lost passthrough key: {s}");
}

#[test]
fn file_part_with_bytes() {
    let raw = r#"{"type":"file","name":"x.png","mimeType":"image/png","bytes":"AAAA"}"#;
    let Part::File { name, mime_type, bytes, uri } = serde_json::from_str(raw).unwrap()
    else {
        panic!("expected File variant");
    };
    assert_eq!(name.as_deref(), Some("x.png"));
    assert_eq!(mime_type.as_deref(), Some("image/png"));
    assert_eq!(bytes.as_deref(), Some("AAAA"));
    assert!(uri.is_none());
}

#[test]
fn data_part_carries_arbitrary_value() {
    let raw = r#"{"type":"data","data":{"x":1}}"#;
    let Part::Data { data } = serde_json::from_str(raw).unwrap() else {
        panic!("expected Data variant");
    };
    assert_eq!(data, json!({"x":1}));
}

#[test]
fn default_metadata_omits_conversation_key() {
    let s = serde_json::to_string(&MessageMetadata::default()).unwrap();
    assert!(!s.contains("x-a2a-shim/conversation"), "got: {s}");
}
