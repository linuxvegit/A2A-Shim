//! Restart-recovery: on Serve Shim startup, batch session/load every
//! persisted conversation against the (fresh) ACP agent. Rows whose
//! load is rejected are deleted from the DB. Concurrent up to 8.
//!
//! Per ADR 0007 + Spike A. Returns the count of successfully restored
//! conversations (and the count of dropped/failed).

use crate::acp_client::AcpClient;
use crate::conversation::ConversationMap;
use crate::persistence::Persistence;
use agent_client_protocol::schema::SessionId;
use futures::stream::{FuturesUnordered, StreamExt};
use std::path::PathBuf;

/// Concurrency limit. ADR 0007 caps batch session_load at 8 to avoid
/// hammering the agent on a large restore.
const BOOTSTRAP_CONCURRENCY: usize = 8;

#[derive(Debug, Default, Clone, Copy)]
pub struct RecoveryReport {
    pub restored: usize,
    pub dropped: usize,
}

/// Batch-load every persisted conversation. Successful loads land in
/// `conv_map` via `insert_loaded`; rejected loads are deleted from `persistence`.
pub async fn bootstrap(
    persistence: &Persistence,
    conv_map: &ConversationMap,
    acp_client: &AcpClient,
) -> RecoveryReport {
    let rows = match persistence.list_conversations().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "recovery: list_conversations failed; skipping");
            return RecoveryReport::default();
        }
    };
    if rows.is_empty() {
        tracing::debug!("recovery: no persisted conversations");
        return RecoveryReport::default();
    }
    tracing::info!(
        count = rows.len(),
        "recovery: loading persisted conversations"
    );

    let mut report = RecoveryReport::default();
    let mut tasks: FuturesUnordered<_> = FuturesUnordered::new();
    let mut row_iter = rows.into_iter();

    // Seed the in-flight set up to BOOTSTRAP_CONCURRENCY.
    for _ in 0..BOOTSTRAP_CONCURRENCY {
        if let Some(row) = row_iter.next() {
            tasks.push(load_one(row, acp_client));
        }
    }
    while let Some((row_id, row_session, cwd, result)) = tasks.next().await {
        match result {
            Ok(()) => {
                let ok = conv_map.insert_loaded(&row_id, row_session.clone()).await;
                if ok {
                    report.restored += 1;
                    tracing::debug!(conv = %row_id, "recovery: restored");
                } else {
                    tracing::warn!(conv = %row_id, "recovery: conv_map full or race; skipping");
                }
            }
            Err(e) => {
                tracing::warn!(
                    conv = %row_id,
                    error = %e,
                    "recovery: session/load rejected; deleting persisted row"
                );
                if let Err(e2) = persistence.delete_conversation(&row_id).await {
                    tracing::warn!(conv = %row_id, error = %e2, "recovery: delete failed too");
                }
                report.dropped += 1;
            }
        }
        // Pull the next row to keep the pool saturated.
        if let Some(row) = row_iter.next() {
            tasks.push(load_one(row, acp_client));
        }
        // Silence unused warning on cwd — we passed it through load_one.
        let _ = cwd;
    }
    tracing::info!(
        restored = report.restored,
        dropped = report.dropped,
        "recovery: done"
    );
    report
}

async fn load_one(
    row: crate::persistence::ConversationRow,
    acp_client: &AcpClient,
) -> (
    String,
    String,
    PathBuf,
    Result<(), crate::acp_client::AcpError>,
) {
    let sid = SessionId::from(row.acp_session_id.clone());
    let cwd = PathBuf::from(&row.cwd);
    let result = acp_client.session_load(&sid, cwd.clone()).await;
    (row.conversation_id, row.acp_session_id, cwd, result)
}
