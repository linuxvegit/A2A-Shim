//! A2A v1.0 § 9.4.4 ListTasks with cursor pagination.

use a2a_shim_core::config::serve_toml::ServeConfig;
use a2a_shim_serve::acp_client::{AcpClient, AcpClientConfig};
use a2a_shim_serve::http;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;

fn mock_bin() -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("current_exe");
    let target_dir = exe
        .parent()
        .and_then(|p| p.parent())
        .expect("two parents up");
    let mut bin = target_dir.join("mock_acp_agent");
    if cfg!(windows) {
        bin.set_extension("exe");
    }
    assert!(bin.exists(), "mock_acp_agent missing");
    bin
}

async fn fresh_client() -> AcpClient {
    let cfg = AcpClientConfig {
        command: mock_bin().to_string_lossy().into_owned(),
        args: vec!["--script".into(), "happy".into()],
        cwd: std::env::temp_dir(),
        env: HashMap::new(),
    };
    let c = AcpClient::spawn(cfg).await.expect("spawn");
    c.initialize().await.expect("init");
    c
}

async fn start_server() -> SocketAddr {
    let cfg = Arc::new(
        ServeConfig::from_toml_str(
            r#"
[agent]
command = "claude-agent-acp"
cwd = "/tmp"
"#,
        )
        .unwrap(),
    );
    let state = http::ServeState::with_client(cfg, fresh_client().await);
    let app = http::router(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    addr
}

async fn rpc(addr: SocketAddr, method: &str, params: Value) -> Value {
    reqwest::Client::new()
        .post(format!("http://{addr}/"))
        .json(&json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}))
        .send()
        .await
        .expect("send")
        .json()
        .await
        .expect("json")
}

async fn send_one(addr: SocketAddr, conv: &str) -> String {
    let r = rpc(
        addr,
        "SendMessage",
        json!({
            "message": {
                "role": "user",
                "parts": [{"text":"go"}],
                "metadata": {"x-a2a-shim/conversation": conv}
            }
        }),
    )
    .await;
    r["result"]["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn list_tasks_empty_returns_empty_array() {
    let addr = start_server().await;
    let r = rpc(addr, "ListTasks", json!({})).await;
    let tasks = r["result"]["tasks"].as_array().expect("tasks array");
    assert!(tasks.is_empty(), "got: {r}");
    assert!(r["result"]["nextPageToken"].is_null());
}

#[tokio::test]
async fn list_tasks_after_five_sends_returns_five() {
    let addr = start_server().await;
    for i in 0..5 {
        send_one(addr, &format!("alice/list-{i}")).await;
    }
    let r = rpc(addr, "ListTasks", json!({})).await;
    let tasks = r["result"]["tasks"].as_array().expect("tasks");
    assert_eq!(tasks.len(), 5, "got: {r}");
    assert!(r["result"]["nextPageToken"].is_null());
}

#[tokio::test]
async fn list_tasks_pagination_works() {
    let addr = start_server().await;
    let mut created = Vec::new();
    for i in 0..5 {
        created.push(send_one(addr, &format!("alice/page-{i}")).await);
    }

    let page1 = rpc(addr, "ListTasks", json!({"pageSize": 2})).await;
    let tasks1 = page1["result"]["tasks"].as_array().expect("tasks");
    assert_eq!(tasks1.len(), 2);
    let token = page1["result"]["nextPageToken"].as_str().expect("token");

    let page2 = rpc(
        addr,
        "ListTasks",
        json!({"pageSize": 2, "pageToken": token}),
    )
    .await;
    let tasks2 = page2["result"]["tasks"].as_array().expect("tasks");
    assert_eq!(tasks2.len(), 2);
    let token2 = page2["result"]["nextPageToken"].as_str().expect("token");

    let page3 = rpc(
        addr,
        "ListTasks",
        json!({"pageSize": 2, "pageToken": token2}),
    )
    .await;
    let tasks3 = page3["result"]["tasks"].as_array().expect("tasks");
    assert_eq!(tasks3.len(), 1, "final page should have 1 task");
    assert!(
        page3["result"]["nextPageToken"].is_null(),
        "final page should have no token"
    );

    // Union of ids should equal the set of created ids.
    let mut seen = std::collections::HashSet::new();
    for arr in [tasks1, tasks2, tasks3] {
        for t in arr {
            seen.insert(t["id"].as_str().unwrap().to_string());
        }
    }
    for c in &created {
        assert!(seen.contains(c), "missing created task {c}");
    }
}
