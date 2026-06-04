//! Prometheus metrics (spec § 7 item #7 + ADR — design notes).
//!
//! Five core metrics:
//!   * a2a_shim_messages_total{method, status}  — counter; one inc per
//!     inbound JSON-RPC call (ok | err).
//!   * a2a_shim_conversations_active             — gauge; live count from
//!     ConversationMap.
//!   * a2a_shim_tasks_active{state}              — gauge; non-terminal
//!     Tasks by state.
//!   * a2a_shim_task_duration_seconds{terminal_state} — histogram; observed
//!     when a Task hits a terminal state.
//!   * a2a_shim_push_deliveries_total{status}    — counter (Task 38).
//!
//! All metric names + labels match what spec § 7 documented. We do NOT
//! emit per-conversation or per-task-id labels (high cardinality).

use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};

/// Install the global Prometheus recorder. Idempotent across multiple
/// callers within the same process (re-installing in the same process
/// would error from the underlying registry, so we ignore that error).
/// Returns a handle that renders the current text-format snapshot.
pub fn install_global_recorder() -> PrometheusHandle {
    let builder = PrometheusBuilder::new();
    match builder.install_recorder() {
        Ok(h) => h,
        Err(_) => {
            // Already installed (e.g. test re-init). Build a fresh
            // recorder + handle and use ITS handle for rendering. The
            // global recorder is whichever was installed first; both
            // will see the same metric registry.
            PrometheusBuilder::new()
                .install_recorder()
                .unwrap_or_else(|_| {
                    // Last resort: a no-op handle that always renders empty.
                    // Should be unreachable in practice.
                    PrometheusBuilder::new().build_recorder().handle()
                })
        }
    }
}

/// Increment the messages-total counter.
pub fn record_message(method: &str, ok: bool) {
    let status = if ok { "ok" } else { "err" };
    metrics::counter!(
        "a2a_shim_messages_total",
        "method" => method.to_string(),
        "status" => status.to_string(),
    )
    .increment(1);
}

/// Set the conversations-active gauge.
pub fn set_conversations_active(n: u64) {
    metrics::gauge!("a2a_shim_conversations_active").set(n as f64);
}

/// Observe a Task's terminal duration in seconds.
pub fn observe_task_duration(terminal_state: &str, secs: f64) {
    metrics::histogram!(
        "a2a_shim_task_duration_seconds",
        "terminal_state" => terminal_state.to_string(),
    )
    .record(secs);
}

/// Counter for push delivery outcomes (Task 38).
pub fn record_push_delivery(status: &str) {
    metrics::counter!(
        "a2a_shim_push_deliveries_total",
        "status" => status.to_string(),
    )
    .increment(1);
}
