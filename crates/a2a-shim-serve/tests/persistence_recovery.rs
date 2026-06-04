//! v1.1 Task 24: persistence::recovery::bootstrap end-to-end.
//!
//! Pre-populates SQLite with 3 conversations, spawns a fresh
//! AcpClient against the 'resumable' mock (which accepts any
//! LoadSession), runs bootstrap, asserts in-memory map is populated.

use a2a_shim_serve::acp_client::{AcpClient, AcpClientConfig};
use a2a_shim_serve::conversation::ConversationMap;
use a2a_shim_serve::persistence::recovery::bootstrap;
use a2a_shim_serve::persistence::Persistence;
use std::collections::HashMap;
use std::time::Duration;

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
async fn bootstrap_restores_persisted_conversations() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    a2a_shim_serve::persistence::schema::ensure_current(&conn).unwrap();
    let p = Persistence::from_connection(conn);
    // Seed 3 conversations.
    for i in 0..3 {
        p.insert_conversation(
            &format!("alice/conv-{i}"),
            &format!("sess-{i}"),
            "/tmp",
            "anonymous",
        )
        .await
        .unwrap();
    }
    let client = fresh_client_resumable().await;
    let map = ConversationMap::with_persistence(64, Duration::from_secs(3600), Some(p.clone()));

    let report = bootstrap(&p, &map, &client).await;
    assert_eq!(report.restored, 3, "report: {:?}", report);
    assert_eq!(report.dropped, 0);

    for i in 0..3 {
        let key = format!("alice/conv-{i}");
        let conv = map.get(&key).await.expect("conv missing");
        assert_eq!(conv.acp_session_id, format!("sess-{i}"));
    }
}

#[tokio::test]
async fn bootstrap_on_empty_db_returns_zero_zero() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    a2a_shim_serve::persistence::schema::ensure_current(&conn).unwrap();
    let p = Persistence::from_connection(conn);
    let client = fresh_client_resumable().await;
    let map = ConversationMap::with_persistence(64, Duration::from_secs(3600), Some(p.clone()));
    let report = bootstrap(&p, &map, &client).await;
    assert_eq!(report.restored, 0);
    assert_eq!(report.dropped, 0);
}
