//! Concurrency helper functions for resolving effective parallelism levels.
//!
//! Each command pipeline uses a different concurrency knob:
//! - **upload/download**: `backup.upload_concurrency` / `backup.download_concurrency`
//!   with fallback to `general.*` when the backup-level value is 0.
//! - **create/restore**: `clickhouse.max_connections` (no fallback).

use crate::config::Config;

/// Resolve the effective upload concurrency.
///
/// Uses `backup.upload_concurrency` when > 0, otherwise falls back to
/// `general.upload_concurrency`. Config validation ensures at least one
/// is > 0.
pub fn effective_upload_concurrency(config: &Config) -> u32 {
    if config.backup.upload_concurrency > 0 {
        config.backup.upload_concurrency
    } else {
        config.general.upload_concurrency
    }
}

/// Resolve the effective download concurrency.
///
/// Uses `backup.download_concurrency` when > 0, otherwise falls back to
/// `general.download_concurrency`. Config validation ensures at least one
/// is > 0.
pub fn effective_download_concurrency(config: &Config) -> u32 {
    if config.backup.download_concurrency > 0 {
        config.backup.download_concurrency
    } else {
        config.general.download_concurrency
    }
}

/// Resolve the effective max connections for table-level parallelism.
///
/// Returns `clickhouse.max_connections` directly (no fallback).
pub fn effective_max_connections(config: &Config) -> u32 {
    config.clickhouse.max_connections
}

/// Resolve the effective object disk copy concurrency for backup/upload.
///
/// Returns `backup.object_disk_copy_concurrency` directly (default 8).
/// Used to bound CopyObject operations during upload (Task 6).
/// Conservative default since backup runs alongside FREEZE.
pub fn effective_object_disk_copy_concurrency(config: &Config) -> u32 {
    config.backup.object_disk_copy_concurrency
}

/// Resolve the effective object disk server-side copy concurrency for restore.
///
/// Returns `general.object_disk_server_side_copy_concurrency` directly (default 32).
/// Used to bound CopyObject operations during restore (Task 8).
/// More aggressive default since restore typically has the cluster to itself.
pub fn effective_object_disk_server_side_copy_concurrency(config: &Config) -> u32 {
    config.general.object_disk_server_side_copy_concurrency
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn test_effective_upload_concurrency() {
        // When backup.upload_concurrency > 0, it takes priority
        let mut config = Config::default();
        config.backup.upload_concurrency = 8;
        config.general.upload_concurrency = 2;
        assert_eq!(effective_upload_concurrency(&config), 8);

        // When backup.upload_concurrency is 0, fall back to general
        config.backup.upload_concurrency = 0;
        config.general.upload_concurrency = 3;
        assert_eq!(effective_upload_concurrency(&config), 3);
    }

    #[test]
    fn test_effective_download_concurrency() {
        // When backup.download_concurrency > 0, it takes priority
        let mut config = Config::default();
        config.backup.download_concurrency = 16;
        config.general.download_concurrency = 4;
        assert_eq!(effective_download_concurrency(&config), 16);

        // When backup.download_concurrency is 0, fall back to general
        config.backup.download_concurrency = 0;
        config.general.download_concurrency = 5;
        assert_eq!(effective_download_concurrency(&config), 5);
    }

    #[test]
    fn test_effective_max_connections() {
        let mut config = Config::default();
        config.clickhouse.max_connections = 10;
        assert_eq!(effective_max_connections(&config), 10);

        // Default value is 1 (conservative sequential default per design doc §12)
        let config = Config::default();
        assert_eq!(effective_max_connections(&config), 1);
    }

    #[test]
    fn test_effective_object_disk_copy_concurrency() {
        // Default is 8
        let config = Config::default();
        assert_eq!(effective_object_disk_copy_concurrency(&config), 8);

        // Custom value
        let mut config = Config::default();
        config.backup.object_disk_copy_concurrency = 16;
        assert_eq!(effective_object_disk_copy_concurrency(&config), 16);
    }

    #[test]
    fn test_effective_object_disk_server_side_copy_concurrency() {
        // Default is 32
        let config = Config::default();
        assert_eq!(
            effective_object_disk_server_side_copy_concurrency(&config),
            32
        );

        // Custom value
        let mut config = Config::default();
        config.general.object_disk_server_side_copy_concurrency = 64;
        assert_eq!(
            effective_object_disk_server_side_copy_concurrency(&config),
            64
        );
    }
}
