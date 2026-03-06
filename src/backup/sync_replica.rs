//! Replica synchronization before backup.
//!
//! Per design 3.2, for tables with Replicated engine, run SYSTEM SYNC REPLICA
//! before FREEZE to ensure all pending replication queue entries are applied.
//!
//! Sync operations run in parallel bounded by a semaphore (max_connections).
//! Sync failures are logged as warnings but do not abort the backup
//! (matching Go clickhouse-backup behavior).

use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};

use crate::clickhouse::client::{ChClient, TableRow};

/// Sync replicas for all Replicated* engine tables in the list.
///
/// Non-replicated tables are silently skipped. Sync operations are performed
/// in parallel, bounded by `max_connections` via a semaphore.
///
/// Sync failures are logged as warnings and do not abort the backup.
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

    let sync_start = Instant::now();

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

    let mut error_count = 0usize;

    for handle in handles {
        match handle.await {
            Ok(Ok(())) => {}
            Ok(Err((db, table, e))) => {
                warn!(
                    db = %db,
                    table = %table,
                    error = format_args!("{e:#}"),
                    "SYNC REPLICA failed, proceeding with backup"
                );
                error_count += 1;
            }
            Err(join_err) => {
                warn!(error = %join_err, "SYNC REPLICA task panicked or was cancelled");
                error_count += 1;
            }
        }
    }

    let elapsed = sync_start.elapsed();
    info!(
        count = replicated.len(),
        elapsed_secs = elapsed.as_secs_f64(),
        errors = error_count,
        "Replica sync completed"
    );

    Ok(())
}
