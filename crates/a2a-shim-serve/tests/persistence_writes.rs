//! v1.1 Tasks 18+19: persistence::schema + write API.
//!
//! Uses in-memory SQLite (rusqlite ":memory:") so tests need no on-disk
//! fixtures and run in microseconds.

use a2a_shim_serve::persistence::{Persistence, TaskRow};
use rusqlite::Connection;

fn fresh_persistence() -> Persistence {
    let conn = Connection::open_in_memory().expect("open in-memory db");
    a2a_shim_serve::persistence::schema::ensure_current(&conn).expect("ensure schema");
    Persistence::from_connection(conn)
}

#[tokio::test]
async fn ensure_current_creates_all_tables() {
    let conn = Connection::open_in_memory().unwrap();
    a2a_shim_serve::persistence::schema::ensure_current(&conn).unwrap();
    // _schema_version row exists with version 1
    let version: i64 = conn
        .query_row("SELECT version FROM _schema_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 1);
    // All 3 tables exist.
    for table in ["conversations", "tasks", "push_notification_configs"] {
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?",
                [table],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "missing table: {table}");
    }
}

#[tokio::test]
async fn ensure_current_is_idempotent() {
    let conn = Connection::open_in_memory().unwrap();
    a2a_shim_serve::persistence::schema::ensure_current(&conn).unwrap();
    a2a_shim_serve::persistence::schema::ensure_current(&conn).unwrap();
    let version: i64 = conn
        .query_row("SELECT version FROM _schema_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 1);
}

#[tokio::test]
async fn insert_conversation_persists_row() {
    let p = fresh_persistence();
    p.insert_conversation("alice/x", "sess-1", "/tmp", "anonymous")
        .await
        .unwrap();
    let convs = p.list_conversations().await.unwrap();
    assert_eq!(convs.len(), 1);
    assert_eq!(convs[0].conversation_id, "alice/x");
    assert_eq!(convs[0].acp_session_id, "sess-1");
    assert_eq!(convs[0].caller_id, "anonymous");
}

#[tokio::test]
async fn touch_conversation_updates_last_used() {
    let p = fresh_persistence();
    p.insert_conversation("c", "s", "/tmp", "anon").await.unwrap();
    let before = p.list_conversations().await.unwrap()[0].last_used_at;
    // sleep a millisecond so the timestamp changes
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    p.touch_conversation("c").await.unwrap();
    let after = p.list_conversations().await.unwrap()[0].last_used_at;
    assert!(after > before, "expected last_used_at to advance");
}

#[tokio::test]
async fn delete_conversation_cascades_to_tasks() {
    let p = fresh_persistence();
    p.insert_conversation("c", "s", "/tmp", "anon").await.unwrap();
    p.record_task("t-1", "c", "submitted").await.unwrap();
    p.record_task("t-2", "c", "submitted").await.unwrap();
    assert_eq!(p.list_tasks_for_conversation("c").await.unwrap().len(), 2);

    p.delete_conversation("c").await.unwrap();
    assert_eq!(p.list_conversations().await.unwrap().len(), 0);
    assert_eq!(p.list_tasks_for_conversation("c").await.unwrap().len(), 0);
}

#[tokio::test]
async fn record_task_updates_state() {
    let p = fresh_persistence();
    p.insert_conversation("c", "s", "/tmp", "anon").await.unwrap();
    p.record_task("t-1", "c", "submitted").await.unwrap();
    p.record_task("t-1", "c", "working").await.unwrap();
    p.record_task("t-1", "c", "completed").await.unwrap();
    let rows: Vec<TaskRow> = p.list_tasks_for_conversation("c").await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].state, "completed");
    assert!(
        rows[0].terminal_at.is_some(),
        "terminal_at should populate on terminal transition"
    );
}

#[tokio::test]
async fn record_task_non_terminal_leaves_terminal_at_null() {
    let p = fresh_persistence();
    p.insert_conversation("c", "s", "/tmp", "anon").await.unwrap();
    p.record_task("t-1", "c", "working").await.unwrap();
    let rows = p.list_tasks_for_conversation("c").await.unwrap();
    assert!(rows[0].terminal_at.is_none());
}
