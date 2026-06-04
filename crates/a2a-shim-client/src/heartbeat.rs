//! MCP `notifications/progress` heartbeat (ADR 0003 + spec § 3.7).
//!
//! The Host (Claude Code) sees the call as "alive" only as long as
//! progress notifications keep arriving. Without them, long-running
//! a2a_send calls trip the Host's idle-tool-call detector and the user
//! sees a hung tool. We emit `notifications/progress` every `interval`
//! while the guard is held; dropping the guard stops the task.
//!
//! `progress_token` is None when the Host did not send `_meta.progressToken`
//! on `tools/call` (V4/V5 DEFERRED outcome from Phase 0). In that case
//! `start` returns a no-op guard that never writes — same surface for
//! the caller, zero output on the wire.
//!
//! `update_summary(text)` mutates the message included in the NEXT tick,
//! so the heartbeat reflects the most recent outbound event (e.g.
//! "streamed chunk 14"). Synchronously safe because the summary lives
//! behind a parking_lot::Mutex; the lock is never held across .await.

use parking_lot::Mutex;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

struct HeartbeatShared {
    progress: AtomicI64,
    /// Replaced wholesale by `update_summary`.
    message: Mutex<Option<String>>,
    /// Append-only buffer accumulated by `append_text`. Capped at
    /// MAX_ACCUM_BYTES; tail is preserved on overflow so the heartbeat
    /// always shows the most recent text.
    accumulated: Mutex<String>,
}

const MAX_ACCUM_BYTES: usize = 4096;
const TAIL_CHARS: usize = 200;

pub struct Heartbeat {
    shared: Arc<HeartbeatShared>,
    cancel: CancellationToken,
}

impl Heartbeat {
    /// Spawn the heartbeat. Returns a guard whose Drop cancels the task.
    /// When `progress_token` is None the guard is a no-op (no task spawned).
    pub fn start(
        writer_tx: mpsc::UnboundedSender<Value>,
        progress_token: Option<Value>,
        interval: Duration,
    ) -> Heartbeat {
        let shared = Arc::new(HeartbeatShared {
            progress: AtomicI64::new(0),
            message: Mutex::new(None),
            accumulated: Mutex::new(String::new()),
        });
        let cancel = CancellationToken::new();

        if let Some(token) = progress_token {
            let shared_for_task = Arc::clone(&shared);
            let cancel_for_task = cancel.clone();
            tokio::spawn(async move {
                // tokio::time::sleep per iteration (instead of the more
                // ergonomic tokio::time::interval) so paused-time tests
                // advance through each tick deterministically: every
                // sleep yields to the scheduler and a single advance()
                // call satisfies multiple sleeps in sequence.
                loop {
                    let sleep = tokio::time::sleep(interval);
                    tokio::pin!(sleep);
                    tokio::select! {
                        _ = cancel_for_task.cancelled() => break,
                        _ = &mut sleep => {}
                    }
                    let progress = shared_for_task.progress.fetch_add(1, Ordering::Relaxed) + 1;
                    // Prefer explicit summary; else last TAIL_CHARS of
                    // accumulated streaming text; else null. ADR 0003 v1.1.
                    let message: Value = {
                        let summary = shared_for_task.message.lock().clone();
                        if let Some(s) = summary {
                            Value::String(s)
                        } else {
                            let accum = shared_for_task.accumulated.lock();
                            if accum.is_empty() {
                                Value::Null
                            } else {
                                Value::String(tail_chars(&accum, TAIL_CHARS))
                            }
                        }
                    };
                    let frame = json!({
                        "jsonrpc": "2.0",
                        "method": "notifications/progress",
                        "params": {
                            "progressToken": token,
                            "progress": progress,
                            "total": Value::Null,
                            "message": message
                        }
                    });
                    if writer_tx.send(frame).is_err() {
                        break;
                    }
                }
            });
        }

        Heartbeat { shared, cancel }
    }

    /// Replace the human-readable summary embedded in the next tick.
    pub fn update_summary(&self, text: String) {
        *self.shared.message.lock() = Some(text);
    }

    /// Append agent-streamed text to the accumulated buffer. When no
    /// explicit summary is set, the next tick's `message` field renders
    /// the last TAIL_CHARS characters of this buffer. ADR 0003 v1.1
    /// extension (spec § 3 item #2 G2 streaming).
    pub fn append_text(&self, s: &str) {
        let mut buf = self.shared.accumulated.lock();
        buf.push_str(s);
        if buf.len() > MAX_ACCUM_BYTES {
            let cutoff = buf.len() - MAX_ACCUM_BYTES;
            let safe_cutoff = (cutoff..buf.len())
                .find(|&i| buf.is_char_boundary(i))
                .unwrap_or(buf.len());
            *buf = buf[safe_cutoff..].to_string();
        }
    }
}

impl Drop for Heartbeat {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// Return the last `max_chars` characters of `s` (not bytes — UTF-8 safe).
fn tail_chars(s: &str, max_chars: usize) -> String {
    let total = s.chars().count();
    if total <= max_chars {
        return s.to_string();
    }
    s.chars().skip(total - max_chars).collect()
}
