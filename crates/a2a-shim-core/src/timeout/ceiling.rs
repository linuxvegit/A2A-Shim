//! `HardCeiling` — absolute wall-clock budget. Once `limit` has elapsed
//! since construction, `exceeded()` returns true and stays true.

use std::time::Duration;
use tokio::time::Instant;

#[derive(Debug, Clone)]
pub struct HardCeiling {
    started: Instant,
    limit: Duration,
}

impl HardCeiling {
    pub fn new(limit: Duration) -> Self {
        Self {
            started: Instant::now(),
            limit,
        }
    }

    pub fn exceeded(&self) -> bool {
        self.started.elapsed() >= self.limit
    }

    /// Time left before `exceeded()` flips, or `None` if it already has.
    pub fn remaining(&self) -> Option<Duration> {
        let elapsed = self.started.elapsed();
        (elapsed < self.limit).then(|| self.limit - elapsed)
    }
}
