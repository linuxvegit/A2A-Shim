use a2a_shim_client::tool_schema::tool_definition;
use serde_json::Value;

#[test]
fn tool_definition_has_required_fields_and_a2a_send_name() {
    let def = tool_definition();
    let v: Value = serde_json::to_value(&def).expect("serialize");
    assert_eq!(v["name"], "a2a_send");
    let desc = v["description"].as_str().expect("description string");
    assert!(desc.to_lowercase().contains("conversation"), "got: {desc}");
    assert!(
        desc.to_lowercase().contains("stream") || desc.to_lowercase().contains("a2a"),
        "got: {desc}"
    );
}

#[test]
fn input_schema_required_fields_are_endpoint_conversation_message() {
    let v = serde_json::to_value(tool_definition()).unwrap();
    let req = v["inputSchema"]["required"]
        .as_array()
        .expect("required array");
    let names: Vec<&str> = req.iter().filter_map(|x| x.as_str()).collect();
    assert_eq!(
        names,
        vec!["endpoint", "conversation_id", "message"],
        "got: {names:?}"
    );
}

#[test]
fn input_schema_endpoint_format_is_uri_and_additional_props_false() {
    let v = serde_json::to_value(tool_definition()).unwrap();
    assert_eq!(v["inputSchema"]["properties"]["endpoint"]["format"], "uri");
    assert_eq!(v["inputSchema"]["additionalProperties"], false);
}

#[test]
fn input_schema_includes_optional_task_id_timeout_metadata() {
    let v = serde_json::to_value(tool_definition()).unwrap();
    let props = &v["inputSchema"]["properties"];
    assert!(props.get("task_id").is_some(), "missing task_id property");
    assert!(props.get("timeout_secs").is_some(), "missing timeout_secs");
    assert!(props.get("metadata").is_some(), "missing metadata");
}

#[test]
fn tool_definition_roundtrips_via_serde() {
    let def = tool_definition();
    let raw = serde_json::to_string(&def).unwrap();
    let back = serde_json::from_str::<Value>(&raw).unwrap();
    let again = serde_json::to_value(&def).unwrap();
    assert_eq!(back, again);
}

#[test]
fn input_schema_includes_caller_id_and_conversation_mode() {
    // v1.1 items #4 + #5.
    let v = serde_json::to_value(tool_definition()).unwrap();
    let props = &v["inputSchema"]["properties"];
    assert!(props.get("caller_id").is_some(), "missing caller_id property");
    assert_eq!(props["caller_id"]["type"], "string");

    let mode = &props["conversation_mode"];
    assert!(!mode.is_null(), "missing conversation_mode property");
    assert_eq!(mode["type"], "string");
    let enums = mode["enum"].as_array().expect("enum array");
    let names: Vec<&str> = enums.iter().filter_map(|x| x.as_str()).collect();
    assert_eq!(names, vec!["new", "continue", "auto"]);
    assert_eq!(mode["default"], "auto");
}
