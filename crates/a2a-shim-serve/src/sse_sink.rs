//! Per-Task SSE broadcast sink (spec § 2.12, § 4.5).
//!
//! One `SseSink` lives inside each `TaskBinding`. HTTP handlers serving
//! `message/stream` call `subscribe()` to obtain a `broadcast::Receiver`
//! and pipe its frames to the response body.
//!
//! Lifecycle:
//!   * `publish_event(ev)` — fan out an in-progress event.
//!   * `publish_final(ev)` — fan out the terminal event, then *drop* the
//!     sender so subscribers observe `RecvError::Closed`. Subsequent
//!     publishes are no-ops.
//!   * `start_keepalive(interval)` — spawn a background task that emits
//!     `Keepalive` frames every `interval` until the sender is gone. The
//!     HTTP handler renders these as `: keepalive\n\n` comment lines.

use a2a_shim_core::wire::sse::SseEvent;
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;

/// One frame on the per-Task SSE channel.
#[derive(Debug, Clone)]
pub enum SseFrame {
    Event(SseEvent),
    Keepalive,
}

/// Cloneable handle to a per-Task broadcast channel.
///
/// The inner sender is wrapped in `Option` so `publish_final` can drop it
/// deterministically, which is what causes downstream `recv()` calls to
/// return `RecvError::Closed`. We use `parking_lot::Mutex` (not `tokio`'s)
/// because the guard is never held across `.await`.
#[derive(Clone)]
pub struct SseSink {
    tx: Arc<Mutex<Option<broadcast::Sender<SseFrame>>>>,
}

impl SseSink {
    /// `capacity` bounds the per-subscriber lag. A subscriber that falls
    /// further behind will see `RecvError::Lagged`; the HTTP handler should
    /// translate that into ending the SSE stream so the client reconnects.
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self {
            tx: Arc::new(Mutex::new(Some(tx))),
        }
    }

    /// Subscribe to all *future* frames. Existing buffered frames within
    /// the capacity window are also delivered, per `tokio::sync::broadcast`
    /// semantics.
    ///
    /// Panics if called after `publish_final` has dropped the sender; that
    /// is a programming error — handlers must subscribe before the Task
    /// reaches a terminal state.
    pub fn subscribe(&self) -> broadcast::Receiver<SseFrame> {
        self.tx
            .lock()
            .as_ref()
            .expect("SseSink::subscribe called after publish_final")
            .subscribe()
    }

    fn publish(&self, frame: SseFrame) {
        if let Some(tx) = self.tx.lock().as_ref() {
            // Returns Err if no subscribers; that's not an error for us.
            let _ = tx.send(frame);
        }
    }

    pub fn publish_event(&self, event: SseEvent) {
        self.publish(SseFrame::Event(event));
    }

    /// Publish the terminal event, then close the channel so subscribers
    /// observe `RecvError::Closed`. Subsequent publishes are silently
    /// dropped.
    pub fn publish_final(&self, event: SseEvent) {
        let mut g = self.tx.lock();
        if let Some(tx) = g.as_ref() {
            let _ = tx.send(SseFrame::Event(event));
        }
        // Drop the sender so all receivers see Closed on the next recv.
        *g = None;
    }

    /// Spawn a tokio task that emits `Keepalive` frames every `interval`
    /// until the sender is dropped (typically by `publish_final`).
    pub fn start_keepalive(&self, interval: Duration) {
        let sink = self.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // The first tick fires immediately; skip it so the first
            // keepalive arrives after one full interval.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                // Stop once the channel is closed.
                if sink.tx.lock().is_none() {
                    break;
                }
                sink.publish(SseFrame::Keepalive);
            }
        });
    }
}
