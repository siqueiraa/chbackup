//! FreezeGuard pattern for safe FREEZE/UNFREEZE lifecycle.
//!
//! The FreezeGuard holds the metadata needed to UNFREEZE a table. Callers
//! MUST call `unfreeze()` explicitly since Drop is synchronous and cannot
//! await async operations.

use anyhow::Result;
use tracing::{debug, warn};

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
        Self { frozen: Vec::new() }
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
    pub async fn unfreeze_all(&mut self, ch: &ChClient) -> Result<()> {
        for info in &self.frozen {
            debug!(
                db = %info.database,
                table = %info.table,
                freeze_name = %info.freeze_name,
                "Unfreezing table"
            );

            if let Err(e) = ch
                .unfreeze_table(&info.database, &info.table, &info.freeze_name)
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

        self.frozen.clear();

        Ok(())
    }
}

impl Drop for FreezeGuard {
    fn drop(&mut self) {
        if !self.frozen.is_empty() {
            warn!(
                count = self.frozen.len(),
                "FreezeGuard dropped with unfrozen tables -- shadow data may remain. \
                 Run `chbackup clean` to remove leftover shadow directories"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_freeze_guard_new_is_empty() {
        let guard = FreezeGuard::new();
        assert!(guard.is_empty());
        assert_eq!(guard.len(), 0);
        assert!(guard.frozen_tables().is_empty());
    }

    #[test]
    fn test_freeze_guard_default_is_empty() {
        let guard = FreezeGuard::default();
        assert!(guard.is_empty());
    }

    #[test]
    fn test_freeze_guard_add_and_len() {
        let mut guard = FreezeGuard::new();
        guard.add(FreezeInfo {
            database: "default".to_string(),
            table: "trades".to_string(),
            freeze_name: "chbackup_test_default_trades".to_string(),
        });
        assert!(!guard.is_empty());
        assert_eq!(guard.len(), 1);

        guard.add(FreezeInfo {
            database: "default".to_string(),
            table: "users".to_string(),
            freeze_name: "chbackup_test_default_users".to_string(),
        });
        assert_eq!(guard.len(), 2);
    }

    #[test]
    fn test_freeze_guard_frozen_tables() {
        let mut guard = FreezeGuard::new();
        guard.add(FreezeInfo {
            database: "db1".to_string(),
            table: "t1".to_string(),
            freeze_name: "fn1".to_string(),
        });
        guard.add(FreezeInfo {
            database: "db2".to_string(),
            table: "t2".to_string(),
            freeze_name: "fn2".to_string(),
        });

        let tables = guard.frozen_tables();
        assert_eq!(tables.len(), 2);
        assert_eq!(tables[0].database, "db1");
        assert_eq!(tables[0].table, "t1");
        assert_eq!(tables[1].database, "db2");
        assert_eq!(tables[1].table, "t2");
    }

    #[test]
    fn test_freeze_guard_drop_when_empty_does_not_panic() {
        let guard = FreezeGuard::new();
        drop(guard);
        // No panic means success
    }

    #[test]
    fn test_freeze_guard_drop_when_non_empty_does_not_panic() {
        let mut guard = FreezeGuard::new();
        guard.add(FreezeInfo {
            database: "default".to_string(),
            table: "trades".to_string(),
            freeze_name: "test".to_string(),
        });
        // Drop should log a warning but not panic
        drop(guard);
    }

    #[test]
    fn test_freeze_info_clone() {
        let info = FreezeInfo {
            database: "mydb".to_string(),
            table: "mytable".to_string(),
            freeze_name: "myfreeze".to_string(),
        };
        let cloned = info.clone();
        assert_eq!(cloned.database, "mydb");
        assert_eq!(cloned.table, "mytable");
        assert_eq!(cloned.freeze_name, "myfreeze");
    }
}
