//! Integration test: boot the axum router on a random loopback port and
//! GET /.well-known/agent.json. Validates Task 24 wiring without yet
//! requiring an ACP Agent to be alive.

use a2a_shim_core::config::serve_toml::ServeConfig;
use a2a_shim_core::wire::card::AgentCard;
use a2a_shim_serve::http;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::time::timeout;

fn cfg() -> ServeConfig {
    ServeConfig::from_toml_str(
        r#"
[agent]
command = "claude-agent-acp"
cwd = "/tmp"
"#,
    )
    .unwrap()
}

#[tokio::test]
async fn agent_card_endpoint_returns_card() {
    let cfg = Arc::new(cfg());
    let state = http::ServeState::new_for_test(cfg.clone());
    let app = http::router(state);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    // Tiny breath so the listener is in accept().
    tokio::time::sleep(Duration::from_millis(50)).await;

    let url = format!("http://{addr}/.well-known/agent.json");
    let resp = timeout(Duration::from_secs(2), reqwest::get(&url))
        .await
        .expect("request timed out")
        .expect("reqwest ok");
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("application/json")
    );
    let card: AgentCard = resp.json().await.expect("parse AgentCard");
    assert!(card.capabilities.streaming);
    assert!(
        card.capabilities.push_notifications,
        "v1.1: push notifications default ON"
    );
    // URL falls back to http://<bound>/ (no advertised_endpoint in cfg).
    assert!(card.url.starts_with("http://127.0.0.1:"));
}
