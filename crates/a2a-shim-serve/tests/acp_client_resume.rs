//! v1.1 Task 23: AcpClient::session_load + session_resume.

use a2a_shim_serve::acp_client::{AcpClient, AcpClientConfig};
use agent_client_protocol::schema::SessionId;
use std::collections::HashMap;

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
    assert!(bin.exists());
    bin
}

async fn fresh_client_resumable() -> AcpClient {
    let cfg = AcpClientConfig {
        command: mock_bin().to_string_lossy().into_owned(),
        args: vec!["--script".into(), "resumable".into()],
        cwd: std::env::temp_dir(),
        env: HashMap::new(),
    };
    let c = AcpClient::spawn(cfg).await.expect("spawn");
    c.initialize().await.expect("init");
    c
}

#[tokio::test]
async fn session_load_accepted_by_resumable_mock() {
    let client = fresh_client_resumable().await;
    let sid = SessionId::from("any-prior-session-id".to_string());
    client
        .session_load(&sid, std::env::temp_dir())
        .await
        .expect("session_load should succeed against the resumable mock");
}

#[tokio::test]
async fn session_resume_accepted_by_resumable_mock() {
    let client = fresh_client_resumable().await;
    let sid = SessionId::from("any-prior-session-id".to_string());
    client
        .session_resume(&sid, std::env::temp_dir())
        .await
        .expect("session_resume should succeed against the resumable mock");
}
