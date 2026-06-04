use a2a_shim_core::wire::envelope::{JsonRpcError, JsonRpcRequest, JsonRpcResponse, ResultOrError};
use serde_json::{json, Value};

#[test]
fn request_roundtrip() {
    let req: JsonRpcRequest<Value> = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: json!("req-1"),
        method: "message/send".into(),
        params: json!({"foo": 42}),
    };
    let back: JsonRpcRequest<Value> =
        serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
    assert_eq!(back.id, json!("req-1"));
    assert_eq!(back.method, "message/send");
    assert_eq!(back.params, json!({"foo": 42}));
}

#[test]
fn response_result_serializes_with_result_key() {
    let resp: JsonRpcResponse<Value> = JsonRpcResponse {
        jsonrpc: "2.0".into(),
        id: json!(7),
        result_or_error: ResultOrError::from_result(json!({"ok": true})),
    };
    let s = serde_json::to_string(&resp).unwrap();
    assert!(s.contains(r#""result":{"ok":true}"#), "got: {s}");
}

#[test]
fn response_error_roundtrips() {
    let resp: JsonRpcResponse<Value> = JsonRpcResponse {
        jsonrpc: "2.0".into(),
        id: json!(7),
        result_or_error: ResultOrError::from_error(JsonRpcError {
            code: -32001,
            message: "Task not found".into(),
            data: None,
        }),
    };
    let back: JsonRpcResponse<Value> =
        serde_json::from_str(&serde_json::to_string(&resp).unwrap()).unwrap();
    match back.result_or_error {
        ResultOrError::Error { error } => assert_eq!(error.code, -32001),
        ResultOrError::Result { .. } => panic!("expected error"),
    }
}
