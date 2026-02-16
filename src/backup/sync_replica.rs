//! Replica synchronization before backup.
//!
//! Per design 3.2, for tables with Replicated engine, run SYSTEM SYNC REPLICA
//! before FREEZE to ensure all pending replication queue entries are applied.

use anyhow::Result;
use tracing::{debug, info, warn};

use crate::clickhouse::client::{ChClient, TableRow};

/// Sync replicas for all Replicated* engine tables in the list.
///
/// Non-replicated tables are silently skipped.
pub async fn sync_replicas(ch: &ChClient, tables: &[TableRow]) -> Result<()> {
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
        "Syncing replicated tables before FREEZE"
    );

    for table in &replicated {
        debug!(
            db = %table.database,
            table = %table.name,
            engine = %table.engine,
            "Syncing replica"
        );

        if let Err(e) = ch.sync_replica(&table.database, &table.name).await {
            warn!(
                db = %table.database,
                table = %table.name,
                error = %e,
                "SYNC REPLICA failed (proceeding with backup anyway)"
            );
        }
    }

    Ok(())
}
