//! `IdleGuard` — single-shot idle detector keyed off `tokio::time::Instant`
//! so paused-time tests work without spawning real timers.

use std::time::Duration;
use tokio::time::Instant;

#[derive(Debug)]
pub struct IdleGuard {
    window: Duration,
    last_activity: Instant,
}

impl IdleGuard {
    pub fn new(window: Duration) -> Self {
        Self {
            window,
            last_activity: Instant::now(),
        }
    }

    /// Reset the activity clock — call when fresh data arrives on the
    /// monitored stream/socket.
    pub fn reset(&mut self) {
        self.last_activity = Instant::now();
    }

    /// Returns `Some(elapsed)` if the idle window has elapsed since the
    /// last activity reset, `None` otherwise.
    pub fn would_trip_now(&self) -> Option<Duration> {
        let elapsed = self.last_activity.elapsed();
        (elapsed >= self.window).then_some(elapsed)
    }
}
