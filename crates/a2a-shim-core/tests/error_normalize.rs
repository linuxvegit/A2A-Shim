use a2a_shim_core::error::codes;
use a2a_shim_core::error::normalize::{ErrorKind, NormalizedError, NormalizedErrorEnvelope};

#[test]
fn codes_match_spec_4_6() {
    assert_eq!(codes::PARSE_ERROR, -32700);
    assert_eq!(codes::INVALID_REQUEST, -32600);
    assert_eq!(codes::METHOD_NOT_FOUND, -32601);
    assert_eq!(codes::INVALID_PARAMS, -32602);
    assert_eq!(codes::INTERNAL_ERROR, -32603);
    assert_eq!(codes::TASK_NOT_FOUND, -32001);
    assert_eq!(codes::TASK_NOT_CANCELABLE, -32002);
    assert_eq!(codes::CONVERSATION_BUSY, -32010);
    assert_eq!(codes::CONVERSATION_LIMIT_REACHED, -32011);
    // v1.1 additions
    assert_eq!(codes::CONVERSATION_EXISTS, -32012);
    assert_eq!(codes::CONVERSATION_LOST, -32013);
    assert_eq!(codes::PUSH_NOTIFICATIONS_NOT_SUPPORTED, -32030);
    assert_eq!(codes::INVALID_PUSH_NOTIFICATION_CONFIG, -32031);
}

#[test]
fn envelope_wraps_under_error_key() {
    let env = NormalizedErrorEnvelope(NormalizedError {
        kind: ErrorKind::RemoteTimeout,
        message: "Remote did not respond within 120 seconds".into(),
        remote_task_id: None,
    });
    let v = serde_json::to_value(&env).unwrap();
    assert_eq!(v["error"]["kind"], "remote_timeout");
    assert!(v["error"]["message"]
        .as_str()
        .unwrap()
        .contains("did not respond"));
    assert!(v["error"]["remote_task_id"].is_null());
}

#[test]
fn all_kinds_roundtrip() {
    for kind in [
        ErrorKind::NetworkError,
        ErrorKind::RemoteTimeout,
        ErrorKind::RemoteFailed,
        ErrorKind::RemoteCanceled,
        ErrorKind::ProtocolError,
        ErrorKind::InvalidRequest,
        ErrorKind::ConcurrentCallNotSupported,
        // v1.1 additions
        ErrorKind::ConversationLost,
        ErrorKind::ConversationExists,
        ErrorKind::PushNotificationsNotSupported,
        ErrorKind::InvalidPushNotificationConfig,
    ] {
        let env = NormalizedErrorEnvelope(NormalizedError {
            kind,
            message: "x".into(),
            remote_task_id: None,
        });
        let back: NormalizedErrorEnvelope =
            serde_json::from_str(&serde_json::to_string(&env).unwrap()).unwrap();
        assert_eq!(back.0.kind, kind);
    }
}
