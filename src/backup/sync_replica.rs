//! Replica synchronization before backup.
//!
//! Per design 3.2, for tables with Replicated engine, run SYSTEM SYNC REPLICA
//! before FREEZE to ensure all pending replication queue entries are applied.
//!
//! Sync operations run in parallel bounded by a semaphore (max_connections).

use std::sync::Arc;

use anyhow::Result;
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};

use crate::clickhouse::client::{ChClient, TableRow};

/// Sync replicas for all Replicated* engine tables in the list.
///
/// Non-replicated tables are silently skipped. Sync operations are performed
/// in parallel, bounded by `max_connections` via a semaphore.
pub async fn sync_replicas(
    ch: &ChClient,
    tables: &[TableRow],
    max_connections: usize,
) -> Result<()> {
    let replicated: Vec<&TableRow> = tables
        .iter()
        .filter(|t| t.engine.contains("Replicated"))
        .collect();

    if replicated.is_empty() {
        debug!("No replicated tables to sync");
        return Ok(());
    }

    info!(
        count = replicated.len(),
        max_connections = max_connections,
        "Syncing replicated tables before FREEZE"
    );

    let semaphore = Arc::new(Semaphore::new(max_connections.max(1)));
    let mut handles = Vec::with_capacity(replicated.len());

    for table in &replicated {
        let sem = semaphore.clone();
        let ch = ch.clone();
        let db = table.database.clone();
        let name = table.name.clone();
        let engine = table.engine.clone();

        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.expect("semaphore closed");

            debug!(
                db = %db,
                table = %name,
                engine = %engine,
                "Syncing replica"
            );

            if let Err(e) = ch.sync_replica(&db, &name).await {
                warn!(
                    db = %db,
                    table = %name,
                    error = %e,
                    "SYNC REPLICA failed (proceeding with backup anyway)"
                );
            }
        }));
    }

    for handle in handles {
        if let Err(join_err) = handle.await {
            warn!(error = %join_err, "sync_replica task panicked or was cancelled");
        }
    }

    Ok(())
}
