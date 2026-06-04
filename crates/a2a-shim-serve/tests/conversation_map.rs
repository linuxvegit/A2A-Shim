use a2a_shim_serve::conversation::{AcquireError, ConversationMap, NewError};
use std::time::Duration;

#[tokio::test]
async fn first_sight_creates_returns_session() {
    let map = ConversationMap::new(8, Duration::from_secs(3600));
    let (conv, created) = map
        .get_or_create("alice/review", || async { Ok::<_, ()>("sess-1".into()) })
        .await
        .unwrap();
    assert!(created);
    assert_eq!(conv.acp_session_id, "sess-1");
    assert_eq!(conv.id, "alice/review");
}

#[tokio::test]
async fn second_sight_reuses_without_calling_spawn() {
    let map = ConversationMap::new(8, Duration::from_secs(3600));
    map.get_or_create("c", || async { Ok::<_, ()>("s1".into()) })
        .await
        .unwrap();

    let mut spawned = false;
    let (conv, created) = map
        .get_or_create("c", || async {
            spawned = true;
            Ok::<_, ()>("s2".into())
        })
        .await
        .unwrap();

    assert!(!created, "should not have re-created");
    assert!(!spawned, "spawn closure must not be invoked on cache hit");
    assert_eq!(conv.acp_session_id, "s1");
}

#[tokio::test]
async fn max_active_rejects_third() {
    let map = ConversationMap::new(2, Duration::from_secs(3600));
    map.get_or_create("a", || async { Ok::<_, ()>("s".into()) })
        .await
        .unwrap();
    map.get_or_create("b", || async { Ok::<_, ()>("s".into()) })
        .await
        .unwrap();
    let err = map
        .get_or_create("c", || async { Ok::<_, ()>("s".into()) })
        .await
        .unwrap_err();
    assert!(matches!(err, NewError::LimitReached));
}

#[tokio::test]
async fn busy_guard_rejects_overlap_then_releases() {
    let map = ConversationMap::new(8, Duration::from_secs(3600));
    map.get_or_create("c", || async { Ok::<_, ()>("s1".into()) })
        .await
        .unwrap();

    let permit = map.acquire_in_flight("c").await.expect("first acquire");
    let busy_err = map.acquire_in_flight("c").await.unwrap_err();
    assert!(matches!(busy_err, AcquireError::Busy), "got {busy_err:?}");
    drop(permit);

    assert!(map.acquire_in_flight("c").await.is_ok());
}

#[tokio::test]
async fn acquire_unknown_id_is_not_found() {
    let map = ConversationMap::new(8, Duration::from_secs(3600));
    let err = map.acquire_in_flight("nope").await.unwrap_err();
    assert!(matches!(err, AcquireError::NotFound), "got {err:?}");
}

#[tokio::test(start_paused = true)]
async fn idle_sweep_drops_old_entries() {
    let map = ConversationMap::new(8, Duration::from_secs(2));
    map.get_or_create("c", || async { Ok::<_, ()>("s".into()) })
        .await
        .unwrap();
    tokio::time::advance(Duration::from_secs(5)).await;

    let dropped = map.sweep_idle().await;
    assert_eq!(dropped, vec!["c".to_string()]);
    assert!(map.get("c").await.is_none());
}

#[tokio::test(start_paused = true)]
async fn idle_sweep_keeps_fresh_entries() {
    let map = ConversationMap::new(8, Duration::from_secs(2));
    map.get_or_create("fresh", || async { Ok::<_, ()>("s".into()) })
        .await
        .unwrap();
    tokio::time::advance(Duration::from_millis(500)).await;

    let dropped = map.sweep_idle().await;
    assert!(dropped.is_empty(), "got {dropped:?}");
    assert!(map.get("fresh").await.is_some());
}
