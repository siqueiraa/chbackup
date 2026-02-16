//! FreezeGuard pattern for safe FREEZE/UNFREEZE lifecycle.
//!
//! The FreezeGuard holds the metadata needed to UNFREEZE a table. Callers
//! MUST call `unfreeze()` explicitly since Drop is synchronous and cannot
//! await async operations.

use anyhow::Result;
use tracing::{info, warn};

use crate::clickhouse::client::ChClient;

/// Metadata for a frozen table. Used to track what needs unfreezing.
#[derive(Debug, Clone)]
pub struct FreezeInfo {
    pub database: String,
    pub table: String,
    pub freeze_name: String,
}

/// Guard holding references to frozen tables. The caller MUST call
/// `unfreeze_all()` to release the freeze. If not called, the frozen
/// data will remain in the shadow directory until manually cleaned.
pub struct FreezeGuard {
    frozen: Vec<FreezeInfo>,
}

impl Default for FreezeGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl FreezeGuard {
    /// Create a new empty FreezeGuard.
    pub fn new() -> Self {
        Self {
            frozen: Vec::new(),
        }
    }

    /// Record that a table has been frozen.
    pub fn add(&mut self, info: FreezeInfo) {
        self.frozen.push(info);
    }

    /// Get the list of frozen tables.
    pub fn frozen_tables(&self) -> &[FreezeInfo] {
        &self.frozen
    }

    /// Number of frozen tables.
    pub fn len(&self) -> usize {
        self.frozen.len()
    }

    /// Whether there are any frozen tables.
    pub fn is_empty(&self) -> bool {
        self.frozen.is_empty()
    }

    /// Unfreeze all tables. Logs warnings on failure but does not fail
    /// the whole operation -- leftover shadow data can be cleaned later.
    pub async fn unfreeze_all(&self, ch: &ChClient) -> Result<()> {
        for info in &self.frozen {
            info!(
                db = %info.database,
                table = %info.table,
                freeze_name = %info.freeze_name,
                "Unfreezing table"
            );

            if let Err(e) =
                ch.unfreeze_table(&info.database, &info.table, &info.freeze_name)
                    .await
            {
                warn!(
                    db = %info.database,
                    table = %info.table,
                    error = %e,
                    "Failed to UNFREEZE table (shadow data may need manual cleanup)"
                );
            }
        }

        Ok(())
    }
}

impl Drop for FreezeGuard {
    fn drop(&mut self) {
        if !self.frozen.is_empty() {
            warn!(
                count = self.frozen.len(),
                "FreezeGuard dropped with unfrozen tables -- call unfreeze_all() explicitly"
            );
        }
    }
}

/// Freeze a single table and add it to the guard.
///
/// If `ignore_not_exists` is true and the table has been dropped during
/// the backup, the error is logged as a warning and the table is skipped.
pub async fn freeze_table(
    ch: &ChClient,
    guard: &mut FreezeGuard,
    db: &str,
    table: &str,
    freeze_name: &str,
    ignore_not_exists: bool,
) -> Result<bool> {
    info!(
        db = %db,
        table = %table,
        freeze_name = %freeze_name,
        "Freezing table"
    );

    match ch.freeze_table(db, table, freeze_name).await {
        Ok(()) => {
            guard.add(FreezeInfo {
                database: db.to_string(),
                table: table.to_string(),
                freeze_name: freeze_name.to_string(),
            });
            Ok(true)
        }
        Err(e) => {
            let err_msg = format!("{e:#}");
            // ClickHouse error codes 60 (UNKNOWN_TABLE) and 81 (UNKNOWN_DATABASE)
            if ignore_not_exists
                && (err_msg.contains("UNKNOWN_TABLE")
                    || err_msg.contains("UNKNOWN_DATABASE")
                    || err_msg.contains("Code: 60")
                    || err_msg.contains("Code: 81"))
            {
                warn!(
                    db = %db,
                    table = %table,
                    error = %e,
                    "Table not found during FREEZE (possibly dropped), skipping"
                );
                Ok(false)
            } else {
                Err(e)
            }
        }
    }
}
