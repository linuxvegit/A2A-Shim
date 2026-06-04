use a2a_shim_core::timeout::ceiling::HardCeiling;
use a2a_shim_core::timeout::idle::IdleGuard;
use std::time::Duration;

#[tokio::test(start_paused = true)]
async fn idle_fires_after_window() {
    let g = IdleGuard::new(Duration::from_secs(2));
    tokio::time::advance(Duration::from_millis(1900)).await;
    assert!(g.would_trip_now().is_none());
    tokio::time::advance(Duration::from_millis(200)).await;
    assert!(g.would_trip_now().is_some());
}

#[tokio::test(start_paused = true)]
async fn idle_reset_extends_window() {
    let mut g = IdleGuard::new(Duration::from_secs(2));
    tokio::time::advance(Duration::from_millis(1500)).await;
    g.reset();
    tokio::time::advance(Duration::from_millis(1500)).await;
    assert!(g.would_trip_now().is_none());
}

#[tokio::test(start_paused = true)]
async fn ceiling_fires_after_window() {
    let c = HardCeiling::new(Duration::from_secs(5));
    tokio::time::advance(Duration::from_secs(4)).await;
    assert!(!c.exceeded());
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(c.exceeded());
}

#[tokio::test(start_paused = true)]
async fn ceiling_remaining_decreases() {
    let c = HardCeiling::new(Duration::from_secs(10));
    let r0 = c.remaining().unwrap();
    tokio::time::advance(Duration::from_secs(4)).await;
    let r1 = c.remaining().unwrap();
    assert!(r1 < r0);
    tokio::time::advance(Duration::from_secs(10)).await;
    assert!(c.remaining().is_none());
}
