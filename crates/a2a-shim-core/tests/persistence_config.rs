//! v1.1 Task 17: [server.persistence] config block (ADR 0007).

use a2a_shim_core::config::serve_toml::ServeConfig;

#[test]
fn persistence_defaults_to_enabled_and_default_path() {
    let cfg = ServeConfig::from_toml_str(
        r#"
[agent]
command = "claude-agent-acp"
cwd = "/tmp"
"#,
    )
    .unwrap();
    assert!(cfg.server.persistence.enabled);
    assert_eq!(
        cfg.server.persistence.path.as_deref(),
        Some(std::path::Path::new("./a2a-shim.db"))
    );
}

#[test]
fn persistence_can_be_disabled() {
    let cfg = ServeConfig::from_toml_str(
        r#"
[server.persistence]
enabled = false

[agent]
command = "claude-agent-acp"
cwd = "/tmp"
"#,
    )
    .unwrap();
    assert!(!cfg.server.persistence.enabled);
}

#[test]
fn persistence_path_override_parses() {
    let cfg = ServeConfig::from_toml_str(
        r#"
[server.persistence]
path = "/var/lib/a2a-shim/state.db"

[agent]
command = "x"
cwd = "/x"
"#,
    )
    .unwrap();
    assert!(cfg.server.persistence.enabled);
    assert_eq!(
        cfg.server.persistence.path.as_deref(),
        Some(std::path::Path::new("/var/lib/a2a-shim/state.db"))
    );
}

#[test]
fn persistence_disabled_with_no_path_is_valid() {
    let cfg = ServeConfig::from_toml_str(
        r#"
[server.persistence]
enabled = false
# no path

[agent]
command = "x"
cwd = "/x"
"#,
    )
    .unwrap();
    assert!(!cfg.server.persistence.enabled);
}
