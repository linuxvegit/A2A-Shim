use a2a_shim_core::config::serve_toml::{PermissionStrategy, ServeConfig};

#[test]
fn defaults_with_only_required() {
    let cfg = ServeConfig::from_toml_str(
        r#"
[agent]
command = "claude-agent-acp"
cwd = "/tmp/work"
"#,
    )
    .unwrap();
    assert_eq!(cfg.server.listen, "127.0.0.1:7001");
    assert_eq!(cfg.server.conversations.idle_secs, 86400);
    assert_eq!(cfg.server.conversations.max_active, 64);
    assert_eq!(
        cfg.agent.permissions.strategy,
        PermissionStrategy::AutoApprove
    );
    assert!(cfg.agent.permissions.deny_tool_kinds.is_empty());
    assert_eq!(cfg.timeouts.agent_sync_idle_secs, 120);
    assert_eq!(cfg.timeouts.agent_stream_idle_secs, 600);
}

#[test]
fn full_override_parses() {
    let cfg = ServeConfig::from_toml_str(
        r#"
[server]
listen = "0.0.0.0:9000"
advertised_endpoint = "https://x.example.com"

[server.conversations]
idle_secs = 3600
max_active = 8

[agent]
command = "/usr/local/bin/codex-acp"
args = ["--flag"]
cwd = "/srv/agent"

[agent.permissions]
strategy = "auto_reject"
deny_tool_kinds = ["delete", "execute"]

[timeouts]
agent_sync_idle_secs = 60
agent_stream_idle_secs = 300
shutdown_grace_secs = 10
"#,
    )
    .unwrap();
    assert_eq!(cfg.server.listen, "0.0.0.0:9000");
    assert_eq!(
        cfg.server.advertised_endpoint.as_deref(),
        Some("https://x.example.com")
    );
    assert_eq!(cfg.server.conversations.max_active, 8);
    assert_eq!(cfg.agent.args, vec!["--flag".to_string()]);
    assert_eq!(
        cfg.agent.permissions.strategy,
        PermissionStrategy::AutoReject
    );
    assert_eq!(
        cfg.agent.permissions.deny_tool_kinds,
        vec!["delete".to_string(), "execute".to_string()]
    );
    assert_eq!(cfg.timeouts.shutdown_grace_secs, 10);
}

#[test]
fn passthrough_rejected_with_v1_2_note() {
    let err = ServeConfig::from_toml_str(
        r#"
[agent]
command = "x"
cwd = "/x"

[agent.permissions]
strategy = "passthrough"
"#,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("passthrough"), "got: {err}");
    assert!(err.contains("v1.2"), "got: {err}");
}

#[test]
fn missing_agent_is_error() {
    let err = ServeConfig::from_toml_str("[server]\nlisten=\"127.0.0.1:7001\"\n")
        .unwrap_err()
        .to_string()
        .to_lowercase();
    assert!(err.contains("agent"), "got: {err}");
}
