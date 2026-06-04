//! SQLite-backed persistence for conversations + tasks + push configs
//! (ADR 0007 / spec § 4 item #3).
//!
//! Synchronous rusqlite under a parking_lot::Mutex. All public methods
//! are async and dispatch the actual SQL through `spawn_blocking` so the
//! tokio runtime stays free. Cheap on a hot path: an in-memory SQLite
//! INSERT under contention is microseconds.
//!
//! Schema migrations live in `schema::ensure_current`, run once at
//! startup. v1: 3 tables + _schema_version (see ADR 0007).

pub mod recovery;
pub mod schema;

use parking_lot::Mutex;
use rusqlite::{params, Connection};
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PersistenceError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("driver task gone")]
    DriverGone,
}

/// Unix epoch ms timestamp.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Cloneable handle. Inner connection is shared through Arc so all
/// clones write to the same db.
#[derive(Clone)]
pub struct Persistence {
    conn: Arc<Mutex<Connection>>,
}

#[derive(Debug, Clone)]
pub struct ConversationRow {
    pub conversation_id: String,
    pub acp_session_id: String,
    pub cwd: String,
    pub caller_id: String,
    pub created_at: i64,
    pub last_used_at: i64,
}

#[derive(Debug, Clone)]
pub struct TaskRow {
    pub task_id: String,
    pub conversation_id: String,
    pub state: String,
    pub created_at: i64,
    pub terminal_at: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct PushConfigRow {
    pub config_id: String,
    pub task_id: String,
    pub url: String,
    pub token: Option<String>,
    pub auth_scheme: Option<String>,
    pub auth_credentials: Option<String>,
    pub tenant: Option<String>,
    pub created_at: i64,
}

impl Persistence {
    /// Wrap an already-open connection. Caller is responsible for
    /// having called `schema::ensure_current` first.
    pub fn from_connection(conn: Connection) -> Self {
        // Enable foreign keys so ON DELETE CASCADE actually triggers.
        let _ = conn.execute("PRAGMA foreign_keys = ON", []);
        Self {
            conn: Arc::new(Mutex::new(conn)),
        }
    }

    /// Open or create the SQLite file at `path`, run migrations, return
    /// a ready Persistence handle. For in-memory testing pass ":memory:".
    pub fn open<P: AsRef<std::path::Path>>(path: P) -> Result<Self, PersistenceError> {
        let conn = Connection::open(path)?;
        schema::ensure_current(&conn)?;
        Ok(Self::from_connection(conn))
    }

    // ──────────────── conversations ────────────────

    pub async fn insert_conversation(
        &self,
        conversation_id: &str,
        acp_session_id: &str,
        cwd: &str,
        caller_id: &str,
    ) -> Result<(), PersistenceError> {
        let conn = Arc::clone(&self.conn);
        let cid = conversation_id.to_owned();
        let sid = acp_session_id.to_owned();
        let cwd = cwd.to_owned();
        let caller = caller_id.to_owned();
        tokio::task::spawn_blocking(move || -> Result<(), PersistenceError> {
            let now = now_ms();
            conn.lock().execute(
                "INSERT OR REPLACE INTO conversations
                 (conversation_id, acp_session_id, cwd, caller_id, created_at, last_used_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                params![cid, sid, cwd, caller, now],
            )?;
            Ok(())
        })
        .await
        .map_err(|_| PersistenceError::DriverGone)??;
        Ok(())
    }

    pub async fn touch_conversation(&self, conversation_id: &str) -> Result<(), PersistenceError> {
        let conn = Arc::clone(&self.conn);
        let cid = conversation_id.to_owned();
        tokio::task::spawn_blocking(move || -> Result<(), PersistenceError> {
            let now = now_ms();
            conn.lock().execute(
                "UPDATE conversations SET last_used_at = ?1 WHERE conversation_id = ?2",
                params![now, cid],
            )?;
            Ok(())
        })
        .await
        .map_err(|_| PersistenceError::DriverGone)??;
        Ok(())
    }

    pub async fn delete_conversation(&self, conversation_id: &str) -> Result<(), PersistenceError> {
        let conn = Arc::clone(&self.conn);
        let cid = conversation_id.to_owned();
        tokio::task::spawn_blocking(move || -> Result<(), PersistenceError> {
            conn.lock().execute(
                "DELETE FROM conversations WHERE conversation_id = ?1",
                params![cid],
            )?;
            Ok(())
        })
        .await
        .map_err(|_| PersistenceError::DriverGone)??;
        Ok(())
    }

    pub async fn list_conversations(&self) -> Result<Vec<ConversationRow>, PersistenceError> {
        let conn = Arc::clone(&self.conn);
        let rows = tokio::task::spawn_blocking(move || -> Result<Vec<ConversationRow>, PersistenceError> {
            let c = conn.lock();
            let mut stmt = c.prepare(
                "SELECT conversation_id, acp_session_id, cwd, caller_id, created_at, last_used_at
                 FROM conversations ORDER BY created_at ASC",
            )?;
            let mapped = stmt
                .query_map([], |r| {
                    Ok(ConversationRow {
                        conversation_id: r.get(0)?,
                        acp_session_id: r.get(1)?,
                        cwd: r.get(2)?,
                        caller_id: r.get(3)?,
                        created_at: r.get(4)?,
                        last_used_at: r.get(5)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(mapped)
        })
        .await
        .map_err(|_| PersistenceError::DriverGone)??;
        Ok(rows)
    }

    // ──────────────── tasks ────────────────

    /// INSERT-OR-UPDATE a task row by id. Sets `terminal_at` to now() if
    /// `state` is one of the terminal values (`completed`/`failed`/
    /// `canceled`); else leaves it NULL (or unchanged on subsequent
    /// transitions through non-terminal states).
    pub async fn record_task(
        &self,
        task_id: &str,
        conversation_id: &str,
        state: &str,
    ) -> Result<(), PersistenceError> {
        let conn = Arc::clone(&self.conn);
        let tid = task_id.to_owned();
        let cid = conversation_id.to_owned();
        let state = state.to_owned();
        tokio::task::spawn_blocking(move || -> Result<(), PersistenceError> {
            let now = now_ms();
            let terminal_at: Option<i64> = match state.as_str() {
                "completed" | "failed" | "canceled" => Some(now),
                _ => None,
            };
            conn.lock().execute(
                "INSERT INTO tasks (task_id, conversation_id, state, created_at, terminal_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(task_id) DO UPDATE SET
                     state = excluded.state,
                     terminal_at = excluded.terminal_at",
                params![tid, cid, state, now, terminal_at],
            )?;
            Ok(())
        })
        .await
        .map_err(|_| PersistenceError::DriverGone)??;
        Ok(())
    }

    pub async fn list_tasks_for_conversation(
        &self,
        conversation_id: &str,
    ) -> Result<Vec<TaskRow>, PersistenceError> {
        let conn = Arc::clone(&self.conn);
        let cid = conversation_id.to_owned();
        let rows = tokio::task::spawn_blocking(move || -> Result<Vec<TaskRow>, PersistenceError> {
            let c = conn.lock();
            let mut stmt = c.prepare(
                "SELECT task_id, conversation_id, state, created_at, terminal_at
                 FROM tasks WHERE conversation_id = ?1 ORDER BY created_at ASC",
            )?;
            let mapped = stmt
                .query_map(params![cid], |r| {
                    Ok(TaskRow {
                        task_id: r.get(0)?,
                        conversation_id: r.get(1)?,
                        state: r.get(2)?,
                        created_at: r.get(3)?,
                        terminal_at: r.get(4)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(mapped)
        })
        .await
        .map_err(|_| PersistenceError::DriverGone)??;
        Ok(rows)
    }

    // ──────────────── push notification configs ────────────────

    pub async fn insert_push_config(
        &self,
        row: PushConfigRow,
    ) -> Result<(), PersistenceError> {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || -> Result<(), PersistenceError> {
            conn.lock().execute(
                "INSERT OR REPLACE INTO push_notification_configs
                 (config_id, task_id, url, token, auth_scheme, auth_credentials, tenant, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    row.config_id,
                    row.task_id,
                    row.url,
                    row.token,
                    row.auth_scheme,
                    row.auth_credentials,
                    row.tenant,
                    row.created_at,
                ],
            )?;
            Ok(())
        })
        .await
        .map_err(|_| PersistenceError::DriverGone)??;
        Ok(())
    }

    pub async fn delete_push_config(&self, config_id: &str) -> Result<(), PersistenceError> {
        let conn = Arc::clone(&self.conn);
        let cid = config_id.to_owned();
        tokio::task::spawn_blocking(move || -> Result<(), PersistenceError> {
            conn.lock().execute(
                "DELETE FROM push_notification_configs WHERE config_id = ?1",
                params![cid],
            )?;
            Ok(())
        })
        .await
        .map_err(|_| PersistenceError::DriverGone)??;
        Ok(())
    }

    pub async fn list_push_configs_for_task(
        &self,
        task_id: &str,
    ) -> Result<Vec<PushConfigRow>, PersistenceError> {
        let conn = Arc::clone(&self.conn);
        let tid = task_id.to_owned();
        let rows = tokio::task::spawn_blocking(move || -> Result<Vec<PushConfigRow>, PersistenceError> {
            let c = conn.lock();
            let mut stmt = c.prepare(
                "SELECT config_id, task_id, url, token, auth_scheme, auth_credentials, tenant, created_at
                 FROM push_notification_configs WHERE task_id = ?1 ORDER BY created_at ASC",
            )?;
            let mapped = stmt
                .query_map(params![tid], |r| {
                    Ok(PushConfigRow {
                        config_id: r.get(0)?,
                        task_id: r.get(1)?,
                        url: r.get(2)?,
                        token: r.get(3)?,
                        auth_scheme: r.get(4)?,
                        auth_credentials: r.get(5)?,
                        tenant: r.get(6)?,
                        created_at: r.get(7)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(mapped)
        })
        .await
        .map_err(|_| PersistenceError::DriverGone)??;
        Ok(rows)
    }

    pub async fn get_push_config(
        &self,
        config_id: &str,
    ) -> Result<Option<PushConfigRow>, PersistenceError> {
        let conn = Arc::clone(&self.conn);
        let cid = config_id.to_owned();
        let row = tokio::task::spawn_blocking(move || -> Result<Option<PushConfigRow>, PersistenceError> {
            let c = conn.lock();
            let mut stmt = c.prepare(
                "SELECT config_id, task_id, url, token, auth_scheme, auth_credentials, tenant, created_at
                 FROM push_notification_configs WHERE config_id = ?1",
            )?;
            let mut iter = stmt.query_map(params![cid], |r| {
                Ok(PushConfigRow {
                    config_id: r.get(0)?,
                    task_id: r.get(1)?,
                    url: r.get(2)?,
                    token: r.get(3)?,
                    auth_scheme: r.get(4)?,
                    auth_credentials: r.get(5)?,
                    tenant: r.get(6)?,
                    created_at: r.get(7)?,
                })
            })?;
            Ok(iter.next().transpose()?)
        })
        .await
        .map_err(|_| PersistenceError::DriverGone)??;
        Ok(row)
    }
}
