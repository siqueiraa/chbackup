//! Replica synchronization before backup.
//!
//! Per design 3.2, for tables with Replicated engine, run SYSTEM SYNC REPLICA
//! before FREEZE to ensure all pending replication queue entries are applied.
//!
//! Sync operations run in parallel bounded by a semaphore (max_connections).
//! If any sync fails, the entire operation returns an error listing all failures.
//! Users who want lenient behavior should set `sync_replicated_tables: false`.

use std::sync::Arc;

use anyhow::{bail, Result};
use tokio::sync::Semaphore;
use tracing::{debug, info};

use crate::clickhouse::client::{ChClient, TableRow};

/// Sync replicas for all Replicated* engine tables in the list.
///
/// Non-replicated tables are silently skipped. Sync operations are performed
/// in parallel, bounded by `max_connections` via a semaphore.
///
/// Returns an error if any table fails to sync.
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

            match ch.sync_replica(&db, &name).await {
                Ok(()) => Ok(()),
                Err(e) => Err((db, name, e)),
            }
        }));
    }

    let mut errors: Vec<(String, String, String)> = Vec::new();

    for handle in handles {
        match handle.await {
            Ok(Ok(())) => {}
            Ok(Err((db, table, e))) => {
                errors.push((db, table, e.to_string()));
            }
            Err(join_err) => {
                errors.push((
                    "unknown".to_string(),
                    "unknown".to_string(),
                    format!("task panicked or was cancelled: {join_err}"),
                ));
            }
        }
    }

    if !errors.is_empty() {
        let details: Vec<String> = errors
            .iter()
            .map(|(db, table, e)| format!("  {db}.{table}: {e}"))
            .collect();
        bail!(
            "SYNC REPLICA failed for {} table(s) (set sync_replicated_tables: false to skip):\n{}",
            errors.len(),
            details.join("\n")
        );
    }

    Ok(())
}
