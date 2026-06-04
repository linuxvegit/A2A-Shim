//! Per-conversation routing table (spec § 2.6).
//!
//! Binds an opaque `conversation_id` (ADR 0004) to one long-lived ACP
//! `session_id`. The Serve Shim consults this map on every inbound
//! `message/send` and `message/stream`:
//!
//!   1. `get_or_create(id, || spawn_session())` either returns the cached
//!      `Conversation` or invokes the spawn closure once under a write
//!      lock. Capacity-limited by `max_active` (spec § 2.6, default 64).
//!   2. `acquire_in_flight(id)` enforces invariant H1: at most one prompt
//!      in flight per conversation. Returns `Busy` if another guard is
//!      held, `NotFound` if the conversation is gone. Caller releases by
//!      dropping the returned `InFlightGuard`.
//!   3. `sweep_idle()` evicts entries whose `last_used_at` is older than
//!      the configured idle window. The Phase 2 idle reaper (Task 27)
//!      calls this on a timer and then issues `session/cancel` against
//!      the evicted ACP sessions.
//!
//! All locks are `tokio::sync::*` because `acquire_in_flight` holds the
//! per-conversation guard across `.await` (the whole prompt turn).

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard, RwLock};
use tokio::time::Instant;

pub type SessionId = String;

#[derive(Debug)]
pub struct Conversation {
    pub id: String,
    pub acp_session_id: SessionId,
    pub created_at: Instant,
    /// Last activity timestamp; updated on every cache hit and on
    /// successful `acquire_in_flight`. Read-mostly under
    /// `parking_lot::Mutex` because the critical section is one write
    /// and no .await.
    pub last_used_at: Mutex<Instant>,
    /// H1 guard. Held across the entire prompt turn (.await boundaries)
    /// so it must be `tokio::sync::Mutex`, not `parking_lot::Mutex`.
    pub in_flight: Arc<AsyncMutex<()>>,
}

#[derive(Debug, thiserror::Error)]
pub enum NewError<E> {
    #[error("max active conversations reached")]
    LimitReached,
    #[error("session spawn failed: {0}")]
    Spawn(E),
}

#[derive(Debug, thiserror::Error)]
pub enum AcquireError {
    #[error("conversation busy")]
    Busy,
    #[error("conversation not found")]
    NotFound,
}

/// Owned guard returned by `acquire_in_flight`; releasing it (drop) frees
/// the conversation's in-flight slot so the next prompt can proceed.
#[derive(Debug)]
pub struct InFlightGuard {
    // Held only to keep the lock acquired; the value itself is never read.
    _inner: OwnedMutexGuard<()>,
}

#[derive(Clone)]
pub struct ConversationMap {
    inner: Arc<RwLock<HashMap<String, Arc<Conversation>>>>,
    max_active: u32,
    idle_window: Duration,
}

impl ConversationMap {
    pub fn new(max_active: u32, idle_window: Duration) -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
            max_active,
            idle_window,
        }
    }

    /// Returns `(conversation, created)` where `created = true` means the
    /// spawn closure was invoked and the entry is fresh.
    pub async fn get_or_create<F, Fut, E>(
        &self,
        id: &str,
        spawn: F,
    ) -> Result<(Arc<Conversation>, bool), NewError<E>>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<SessionId, E>>,
    {
        // Fast path: read lock.
        {
            let r = self.inner.read().await;
            if let Some(conv) = r.get(id) {
                *conv.last_used_at.lock() = Instant::now();
                return Ok((conv.clone(), false));
            }
        }

        // Slow path: write lock. Re-check under the write lock to absorb
        // any race where two callers raced through the read-lock arm.
        //
        // Note: the spawn future runs WHILE THE WRITE LOCK IS HELD. That
        // serializes new-session creation across conversations, which is
        // exactly what we want — it bounds the concurrent subprocess /
        // session_new pressure on the ACP Agent. If this ever becomes a
        // bottleneck we can insert a pending-id placeholder map keyed on
        // id, release the write lock for the spawn, then re-acquire to
        // commit. Not needed in MVP.
        let mut w = self.inner.write().await;
        if let Some(conv) = w.get(id) {
            *conv.last_used_at.lock() = Instant::now();
            return Ok((conv.clone(), false));
        }
        if w.len() as u32 >= self.max_active {
            return Err(NewError::LimitReached);
        }

        let session_id = spawn().await.map_err(NewError::Spawn)?;
        let now = Instant::now();
        let conv = Arc::new(Conversation {
            id: id.to_string(),
            acp_session_id: session_id,
            created_at: now,
            last_used_at: Mutex::new(now),
            in_flight: Arc::new(AsyncMutex::new(())),
        });
        w.insert(id.to_string(), conv.clone());
        Ok((conv, true))
    }

    /// Try to acquire the H1 single-in-flight permit. Synchronous (no
    /// awaiting on the lock) so overlap is reported as `Busy` rather than
    /// silently serialized.
    pub async fn acquire_in_flight(&self, id: &str) -> Result<InFlightGuard, AcquireError> {
        let conv = {
            let r = self.inner.read().await;
            r.get(id).cloned().ok_or(AcquireError::NotFound)?
        };
        match conv.in_flight.clone().try_lock_owned() {
            Ok(guard) => Ok(InFlightGuard { _inner: guard }),
            Err(_) => Err(AcquireError::Busy),
        }
    }

    /// Evict entries whose last_used_at is older than the idle window.
    /// Returns the evicted ids so the caller can issue session/cancel
    /// against the underlying ACP sessions.
    pub async fn sweep_idle(&self) -> Vec<String> {
        let now = Instant::now();
        let mut to_drop = Vec::new();
        {
            let r = self.inner.read().await;
            for (k, c) in r.iter() {
                let last = *c.last_used_at.lock();
                if now.saturating_duration_since(last) >= self.idle_window {
                    to_drop.push(k.clone());
                }
            }
        }
        if !to_drop.is_empty() {
            let mut w = self.inner.write().await;
            for k in &to_drop {
                w.remove(k);
            }
        }
        to_drop
    }

    pub async fn get(&self, id: &str) -> Option<Arc<Conversation>> {
        self.inner.read().await.get(id).cloned()
    }
}
