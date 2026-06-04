use a2a_shim_core::config::serve_toml::ServeConfig;
use a2a_shim_serve::agent_card::build_agent_card;

fn cfg(advertised: Option<&str>) -> ServeConfig {
    let mut s = r#"
[agent]
command = "claude-agent-acp"
cwd = "/tmp"
"#
    .to_string();
    if let Some(url) = advertised {
        s.push_str(&format!("\n[server]\nadvertised_endpoint = \"{url}\"\n"));
    }
    ServeConfig::from_toml_str(&s).unwrap()
}

#[test]
fn url_falls_back_to_bound_address_when_advertised_endpoint_absent() {
    let c = cfg(None);
    let card = build_agent_card(&c, "127.0.0.1:7001");
    assert_eq!(card.url, "http://127.0.0.1:7001/");
}

#[test]
fn url_uses_advertised_endpoint_when_set() {
    let c = cfg(Some("https://x.example.com/"));
    let card = build_agent_card(&c, "127.0.0.1:7001");
    assert_eq!(card.url, "https://x.example.com/");
}

#[test]
fn capabilities_match_spec_2_10() {
    let c = cfg(None);
    let card = build_agent_card(&c, "127.0.0.1:7001");
    assert!(card.capabilities.streaming);
    assert!(
        card.capabilities.push_notifications,
        "v1.1: push notifications default ON"
    );
    assert!(card.capabilities.state_transition_history);
}

#[test]
fn conversations_metadata_mirrors_config() {
    let c = ServeConfig::from_toml_str(
        r#"
[server.conversations]
max_active = 8
idle_secs = 3600
[agent]
command = "x"
cwd = "/x"
"#,
    )
    .unwrap();
    let card = build_agent_card(&c, "127.0.0.1:7001");
    let conv = card
        .metadata
        .as_ref()
        .and_then(|m| m.conversations.as_ref())
        .expect("conversations capability present");
    assert!(conv.supported);
    assert_eq!(conv.metadata_key, "x-a2a-shim/conversation");
    assert!(conv.context_id_alias);
    assert_eq!(conv.max_active, 8);
    assert_eq!(conv.idle_secs, 3600);
}

#[test]
fn name_and_description_come_from_config() {
    let c = ServeConfig::from_toml_str(
        r#"
[agent]
command = "x"
cwd = "/x"

[agent.card]
name = "claude-code-sidecar"
description = "Claude Agent exposed as an A2A endpoint"
version = "0.2.0"
"#,
    )
    .unwrap();
    let card = build_agent_card(&c, "127.0.0.1:7001");
    assert_eq!(card.name, "claude-code-sidecar");
    assert_eq!(card.description, "Claude Agent exposed as an A2A endpoint");
    assert_eq!(card.version, "0.2.0");
}
