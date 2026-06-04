//! v1.1 Tasks 21+22: ConversationMap and TaskRegistry write through
//! to Persistence.

use a2a_shim_core::wire::task::TaskState;
use a2a_shim_serve::conversation::ConversationMap;
use a2a_shim_serve::persistence::Persistence;
use a2a_shim_serve::task_registry::TaskRegistry;
use std::time::Duration;

fn fresh_persistence() -> Persistence {
    let conn = rusqlite::Connection::open_in_memory().expect("open db");
    a2a_shim_serve::persistence::schema::ensure_current(&conn).unwrap();
    Persistence::from_connection(conn)
}

#[tokio::test]
async fn conversation_map_persists_on_create() {
    let p = fresh_persistence();
    let map = ConversationMap::with_persistence(8, Duration::from_secs(3600), Some(p.clone()));
    let (conv, created) = map
        .get_or_create_with_meta("alice/x", "/tmp", "anonymous", || async {
            Ok::<_, ()>("sess-1".into())
        })
        .await
        .unwrap();
    assert!(created);
    assert_eq!(conv.acp_session_id, "sess-1");

    let rows = p.list_conversations().await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].conversation_id, "alice/x");
    assert_eq!(rows[0].acp_session_id, "sess-1");
    assert_eq!(rows[0].caller_id, "anonymous");
}

#[tokio::test]
async fn conversation_map_persists_delete_on_sweep() {
    let p = fresh_persistence();
    let map = ConversationMap::with_persistence(8, Duration::from_secs(0), Some(p.clone()));
    map.get_or_create_with_meta("c", "/tmp", "anon", || async { Ok::<_, ()>("s".into()) })
        .await
        .unwrap();
    // Wait so last_used_at < now - window (window is 0).
    tokio::time::sleep(Duration::from_millis(10)).await;
    let dropped = map.sweep_idle().await;
    assert_eq!(dropped, vec!["c".to_string()]);
    let rows = p.list_conversations().await.unwrap();
    assert!(
        rows.is_empty(),
        "expected DB row removed on sweep, got: {rows:?}"
    );
}

#[tokio::test]
async fn task_registry_persists_create_and_transitions() {
    let p = fresh_persistence();
    p.insert_conversation("c", "s", "/tmp", "anon")
        .await
        .unwrap();
    let reg = TaskRegistry::with_persistence(Some(p.clone()));
    let task_id = reg.create("c", "s").await;
    reg.transition(&task_id, TaskState::Working).await.unwrap();
    reg.transition(&task_id, TaskState::Completed)
        .await
        .unwrap();

    let rows = p.list_tasks_for_conversation("c").await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].task_id, task_id.as_str());
    assert_eq!(rows[0].state, "completed");
    assert!(rows[0].terminal_at.is_some());
}

#[tokio::test]
async fn task_registry_persists_cancel() {
    let p = fresh_persistence();
    p.insert_conversation("c", "s", "/tmp", "anon")
        .await
        .unwrap();
    let reg = TaskRegistry::with_persistence(Some(p.clone()));
    let task_id = reg.create("c", "s").await;
    reg.transition(&task_id, TaskState::Working).await.unwrap();
    reg.cancel(&task_id).await.unwrap();
    let rows = p.list_tasks_for_conversation("c").await.unwrap();
    assert_eq!(rows[0].state, "canceled");
    assert!(rows[0].terminal_at.is_some());
}

#[tokio::test]
async fn registries_without_persistence_still_work() {
    // Backward compat: no Persistence handle, all writes are pure
    // in-memory (v0.1.0 behavior).
    let map = ConversationMap::with_persistence(8, Duration::from_secs(3600), None);
    let (_, created) = map
        .get_or_create_with_meta("c", "/tmp", "anon", || async { Ok::<_, ()>("s".into()) })
        .await
        .unwrap();
    assert!(created);

    let reg = TaskRegistry::with_persistence(None);
    let _ = reg.create("c", "s").await;
}
