//! Push notification delivery (ADR 0008).
//!
//! Three pieces working together:
//!   * `PushConfigRegistry` — in-memory mirror of push_notification_configs
//!     rows, keyed by TaskId. Mutations are write-through to Persistence.
//!   * `DeliveryJob` — one payload to be POSTed to a webhook with retry.
//!   * Worker pool (8 by default) — drains an mpsc of jobs and applies
//!     the retry policy.
//!
//! Trigger: `TaskRegistry::transition`/`cancel` (Task 35) consults the
//! registry on terminal transitions and enqueues a job per registered
//! config.

use crate::persistence::{Persistence, PushConfigRow};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct PushNotificationConfig {
    pub config_id: String,
    pub task_id: String,
    pub url: String,
    pub token: Option<String>,
    pub auth_scheme: Option<String>,
    pub auth_credentials: Option<String>,
    pub tenant: Option<String>,
}

impl From<PushConfigRow> for PushNotificationConfig {
    fn from(r: PushConfigRow) -> Self {
        Self {
            config_id: r.config_id,
            task_id: r.task_id,
            url: r.url,
            token: r.token,
            auth_scheme: r.auth_scheme,
            auth_credentials: r.auth_credentials,
            tenant: r.tenant,
        }
    }
}

/// Registry of push notification configs. In-memory mirror of the SQLite
/// rows; mutations write through.
#[derive(Clone)]
pub struct PushConfigRegistry {
    inner: Arc<Mutex<HashMap<String, Vec<PushNotificationConfig>>>>,
    persistence: Option<Persistence>,
    /// Per-config failure counters (live in memory; reset on restart).
    failure_counts: Arc<Mutex<HashMap<String, u32>>>,
    permanent_failure_threshold: u32,
}

impl PushConfigRegistry {
    pub fn new(persistence: Option<Persistence>, permanent_failure_threshold: u32) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            persistence,
            failure_counts: Arc::new(Mutex::new(HashMap::new())),
            permanent_failure_threshold,
        }
    }

    /// Insert (or replace) a push config. Writes through to Persistence.
    pub async fn insert(&self, cfg: PushNotificationConfig) -> Result<(), String> {
        let row = PushConfigRow {
            config_id: cfg.config_id.clone(),
            task_id: cfg.task_id.clone(),
            url: cfg.url.clone(),
            token: cfg.token.clone(),
            auth_scheme: cfg.auth_scheme.clone(),
            auth_credentials: cfg.auth_credentials.clone(),
            tenant: cfg.tenant.clone(),
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0),
        };
        if let Some(p) = self.persistence.as_ref() {
            p.insert_push_config(row).await.map_err(|e| e.to_string())?;
        }
        let mut map = self.inner.lock();
        let list = map.entry(cfg.task_id.clone()).or_default();
        list.retain(|c| c.config_id != cfg.config_id);
        list.push(cfg);
        Ok(())
    }

    /// Get a single config by id. Falls back to Persistence if not in
    /// memory (covers post-restart with no recovery).
    pub async fn get(&self, config_id: &str) -> Result<Option<PushNotificationConfig>, String> {
        {
            let map = self.inner.lock();
            for list in map.values() {
                if let Some(c) = list.iter().find(|c| c.config_id == config_id) {
                    return Ok(Some(c.clone()));
                }
            }
        }
        if let Some(p) = self.persistence.as_ref() {
            return Ok(p
                .get_push_config(config_id)
                .await
                .map_err(|e| e.to_string())?
                .map(Into::into));
        }
        Ok(None)
    }

    pub async fn delete(&self, config_id: &str) -> Result<(), String> {
        if let Some(p) = self.persistence.as_ref() {
            p.delete_push_config(config_id)
                .await
                .map_err(|e| e.to_string())?;
        }
        let mut map = self.inner.lock();
        for list in map.values_mut() {
            list.retain(|c| c.config_id != config_id);
        }
        self.failure_counts.lock().remove(config_id);
        Ok(())
    }

    pub fn list_for_task(&self, task_id: &str) -> Vec<PushNotificationConfig> {
        self.inner
            .lock()
            .get(task_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Increment the failure counter for `config_id`. Returns true if
    /// the new count has reached the permanent-failure threshold (caller
    /// should delete the config).
    pub fn record_permanent_failure(&self, config_id: &str) -> bool {
        let mut counts = self.failure_counts.lock();
        let n = counts.entry(config_id.to_string()).or_insert(0);
        *n += 1;
        *n >= self.permanent_failure_threshold
    }

    pub fn reset_failure_count(&self, config_id: &str) {
        self.failure_counts.lock().remove(config_id);
    }
}

/// One delivery job. Built by `TaskRegistry::transition` (Task 35) on
/// terminal state and sent through the mpsc to the worker pool.
#[derive(Debug, Clone)]
pub struct DeliveryJob {
    pub config: PushNotificationConfig,
    pub payload: Value,
}

/// Retry policy (ADR 0008).
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub max_attempts: usize,
    pub backoff_base_secs: u64,
    pub backoff_factor: u64,
}

#[derive(Debug, Clone, Copy)]
pub enum DeliveryOutcome {
    /// Webhook accepted (2xx/3xx).
    Success,
    /// 4xx other than 408/429 — give up immediately.
    PermanentFailure,
    /// Exhausted retries (transient errors / 5xx / timeouts).
    Dropped,
}

/// Run one delivery with retries. The client is cloned cheaply by reqwest.
pub async fn deliver(
    client: &reqwest::Client,
    job: &DeliveryJob,
    policy: RetryPolicy,
) -> DeliveryOutcome {
    let mut attempt: usize = 0;
    loop {
        attempt += 1;
        match try_once(client, job).await {
            Ok(()) => return DeliveryOutcome::Success,
            Err(DeliveryError::PermanentFailure { status, .. }) => {
                tracing::warn!(
                    config = %job.config.config_id,
                    task = %job.config.task_id,
                    status,
                    "push delivery permanently rejected"
                );
                return DeliveryOutcome::PermanentFailure;
            }
            Err(DeliveryError::Transient { reason }) => {
                if attempt >= policy.max_attempts {
                    tracing::warn!(
                        config = %job.config.config_id,
                        task = %job.config.task_id,
                        attempts = attempt,
                        last_error = %reason,
                        "push delivery dropped after exhausted retries"
                    );
                    return DeliveryOutcome::Dropped;
                }
                let wait = Duration::from_secs(
                    policy.backoff_base_secs
                        * policy.backoff_factor.pow((attempt - 1) as u32),
                );
                tracing::debug!(
                    config = %job.config.config_id,
                    attempt,
                    wait_secs = wait.as_secs(),
                    "push delivery retrying after transient failure"
                );
                tokio::time::sleep(wait).await;
            }
        }
    }
}

#[derive(Debug)]
enum DeliveryError {
    PermanentFailure { status: u16, _body: String },
    Transient { reason: String },
}

async fn try_once(client: &reqwest::Client, job: &DeliveryJob) -> Result<(), DeliveryError> {
    let mut req = client
        .post(&job.config.url)
        .header("content-type", "application/a2a+json")
        .header(
            "idempotency-key",
            format!(
                "task-{}-config-{}",
                job.config.task_id, job.config.config_id
            ),
        );
    // Authorization: prefer authentication.{scheme,credentials}; else fall
    // back to the deprecated `token` field as Bearer.
    if let (Some(scheme), Some(creds)) = (
        job.config.auth_scheme.as_deref(),
        job.config.auth_credentials.as_deref(),
    ) {
        req = req.header("authorization", format!("{scheme} {creds}"));
    } else if let Some(tok) = job.config.token.as_deref() {
        req = req.header("authorization", format!("Bearer {tok}"));
    }
    let resp = match req.json(&job.payload).send().await {
        Ok(r) => r,
        Err(e) => {
            return Err(DeliveryError::Transient {
                reason: format!("reqwest: {e}"),
            })
        }
    };
    let status = resp.status();
    if status.is_success() || status.is_redirection() {
        Ok(())
    } else if status.is_server_error() || status.as_u16() == 408 || status.as_u16() == 429 {
        Err(DeliveryError::Transient {
            reason: format!("HTTP {}", status.as_u16()),
        })
    } else {
        // 4xx other than 408/429 — webhook said no.
        let body = resp.text().await.unwrap_or_default();
        let preview = body.chars().take(200).collect::<String>();
        Err(DeliveryError::PermanentFailure {
            status: status.as_u16(),
            _body: preview,
        })
    }
}

/// Spawn a fixed-size pool of worker tasks that drain `rx`. Returns
/// the unbounded sender callers push jobs into.
pub fn start_worker_pool(
    workers: usize,
    http: reqwest::Client,
    registry: PushConfigRegistry,
    policy: RetryPolicy,
) -> tokio::sync::mpsc::UnboundedSender<DeliveryJob> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<DeliveryJob>();
    let rx = Arc::new(tokio::sync::Mutex::new(rx));
    for _ in 0..workers {
        let rx = Arc::clone(&rx);
        let http = http.clone();
        let registry = registry.clone();
        tokio::spawn(async move {
            loop {
                let job = {
                    let mut guard = rx.lock().await;
                    match guard.recv().await {
                        Some(j) => j,
                        None => break,
                    }
                };
                match deliver(&http, &job, policy).await {
                    DeliveryOutcome::Success => {
                        registry.reset_failure_count(&job.config.config_id);
                    }
                    DeliveryOutcome::PermanentFailure => {
                        let should_delete =
                            registry.record_permanent_failure(&job.config.config_id);
                        if should_delete {
                            if let Err(e) = registry.delete(&job.config.config_id).await {
                                tracing::warn!(
                                    config = %job.config.config_id,
                                    error = %e,
                                    "failed to delete config after permanent-failure threshold"
                                );
                            } else {
                                tracing::warn!(
                                    config = %job.config.config_id,
                                    "deleted push config: hit permanent failure threshold"
                                );
                            }
                        }
                    }
                    DeliveryOutcome::Dropped => {} // already logged inside deliver
                }
            }
        });
    }
    tx
}

/// Build the StreamResponse-shaped payload for a terminal Task transition
/// (ADR 0008 + A2A v1.0.1 § 3.5.3). Caller passes the wire-form status
/// payload — we wrap it.
pub fn build_status_update_payload(
    task_id: &str,
    state: &str,
    timestamp_ms: i64,
) -> Value {
    json!({
        "statusUpdate": {
            "taskId": task_id,
            "status": {
                "state": state,
                "timestamp": format_iso8601(timestamp_ms),
            },
            "final": true
        }
    })
}

fn format_iso8601(ms: i64) -> String {
    // Minimal ISO-8601 — secs precision, UTC. Not chrono to keep deps small.
    let secs = ms / 1000;
    let days = secs / 86400;
    let secs_in_day = secs % 86400;
    // Compute date from days since unix epoch using a simple civil-from-days.
    let (y, m, d) = civil_from_days(days);
    let h = secs_in_day / 3600;
    let mi = (secs_in_day % 3600) / 60;
    let s = secs_in_day % 60;
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// Howard Hinnant's date algorithm: days since unix epoch (1970-01-01)
/// -> (year, month, day).
fn civil_from_days(z: i64) -> (i64, u8, u8) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let m = if mp < 10 { (mp + 3) as u8 } else { (mp - 9) as u8 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}
