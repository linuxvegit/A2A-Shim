//! Schema definition + migrations for SQLite persistence (ADR 0007).
//!
//! Hand-rolled migration ladder: `_schema_version` table tracks the
//! applied version (1 today). Each upgrade fn runs in a transaction and
//! bumps the version on success. New versions append themselves to
//! `MIGRATIONS` and write their DDL in their own function — no external
//! migration crate dependency.

use rusqlite::Connection;

/// Highest schema version known to this build.
pub const CURRENT_VERSION: i64 = 1;

/// Bring `conn` up to `CURRENT_VERSION`. Idempotent: re-running against
/// an already-current db is a no-op.
pub fn ensure_current(conn: &Connection) -> rusqlite::Result<()> {
    // The version table itself is the first thing we create — if it
    // doesn't exist we treat the db as fresh (version 0) and apply all
    // migrations in order.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS _schema_version (
            version INTEGER NOT NULL PRIMARY KEY
        )",
        [],
    )?;
    let current: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM _schema_version",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);

    if current < 1 {
        apply_v1(conn)?;
    }

    Ok(())
}

fn apply_v1(conn: &Connection) -> rusqlite::Result<()> {
    let tx_sql = r#"
        CREATE TABLE conversations (
            conversation_id   TEXT NOT NULL PRIMARY KEY,
            acp_session_id    TEXT NOT NULL,
            cwd               TEXT NOT NULL,
            caller_id         TEXT NOT NULL DEFAULT 'anonymous',
            created_at        INTEGER NOT NULL,
            last_used_at      INTEGER NOT NULL
        );

        CREATE TABLE tasks (
            task_id           TEXT NOT NULL PRIMARY KEY,
            conversation_id   TEXT NOT NULL REFERENCES conversations(conversation_id) ON DELETE CASCADE,
            state             TEXT NOT NULL,
            created_at        INTEGER NOT NULL,
            terminal_at       INTEGER
        );

        CREATE TABLE push_notification_configs (
            config_id         TEXT NOT NULL PRIMARY KEY,
            task_id           TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE CASCADE,
            url               TEXT NOT NULL,
            token             TEXT,
            auth_scheme       TEXT,
            auth_credentials  TEXT,
            tenant            TEXT,
            created_at        INTEGER NOT NULL
        );

        CREATE INDEX idx_tasks_conversation ON tasks(conversation_id);
        CREATE INDEX idx_push_configs_task  ON push_notification_configs(task_id);
    "#;
    conn.execute_batch(tx_sql)?;
    conn.execute("INSERT INTO _schema_version (version) VALUES (1)", [])?;
    Ok(())
}
