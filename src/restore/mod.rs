//! Restore: read manifest, CREATE DB/TABLE, hardlink to detached, ATTACH PART.
//!
//! Implements Mode B (non-destructive) restore flow from design doc section 5:
//! 1. Read manifest from `{backup_dir}/metadata.json`
//! 2. CREATE databases from manifest.databases DDL
//! 3. For each table in manifest (filtered by table_pattern):
//!    - If table does not exist: CREATE TABLE from DDL
//!    - Sort parts by (partition, min_block)
//!    - Hardlink parts to detached/ directory
//!    - ALTER TABLE ATTACH PART for each part
//! 4. Log summary

pub mod attach;
pub mod schema;
pub mod sort;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

use crate::clickhouse::client::ChClient;
use crate::config::Config;
use crate::manifest::BackupManifest;
use crate::table_filter::TableFilter;

use attach::{attach_parts, detect_clickhouse_ownership, get_table_data_path, AttachParams};
use schema::{create_databases, create_tables};

/// Restore a backup to ClickHouse.
///
/// Implements Mode B (non-destructive): creates databases and tables if they
/// don't exist, then attaches data parts via detached/ directory.
///
/// # Arguments
///
/// * `config` - Application configuration
/// * `ch` - ClickHouse client for DDL and ATTACH PART queries
/// * `backup_name` - Name of the backup to restore
/// * `table_pattern` - Optional table filter pattern (glob)
/// * `schema_only` - If true, only restore schema (no data)
/// * `data_only` - If true, only restore data (no schema creation)
pub async fn restore(
    config: &Config,
    ch: &ChClient,
    backup_name: &str,
    table_pattern: Option<&str>,
    schema_only: bool,
    data_only: bool,
) -> Result<()> {
    let data_path = &config.clickhouse.data_path;
    let backup_dir = PathBuf::from(data_path).join("backup").join(backup_name);

    info!(
        backup_name = %backup_name,
        backup_dir = %backup_dir.display(),
        schema_only = schema_only,
        data_only = data_only,
        "Starting restore"
    );

    // 1. Read manifest
    let manifest_path = backup_dir.join("metadata.json");
    let manifest = BackupManifest::load_from_file(&manifest_path)
        .with_context(|| {
            format!(
                "Failed to load manifest for backup '{}' at {}",
                backup_name,
                manifest_path.display()
            )
        })?;

    info!(
        backup_name = %manifest.name,
        tables = manifest.tables.len(),
        databases = manifest.databases.len(),
        "Loaded manifest"
    );

    // 2. Filter tables by pattern
    let table_filter = match table_pattern {
        Some(pattern) => TableFilter::new(pattern),
        None => TableFilter::new("*.*"),
    };

    let table_keys: Vec<String> = manifest
        .tables
        .keys()
        .filter(|key| {
            let (db, table) = key.split_once('.').unwrap_or(("default", key));
            table_filter.matches(db, table)
        })
        .cloned()
        .collect();

    if table_keys.is_empty() {
        warn!("No tables match the filter pattern");
        return Ok(());
    }

    info!(
        matched_tables = table_keys.len(),
        total_tables = manifest.tables.len(),
        "Tables matched filter"
    );

    // 3. CREATE databases (Phase 1: create databases from manifest)
    if !data_only {
        create_databases(ch, &manifest).await?;
    }

    // 4. CREATE tables
    create_tables(ch, &manifest, &table_keys, data_only).await?;

    // 5. Attach data parts (skip if schema_only)
    if schema_only {
        info!("Schema-only mode, skipping data restore");
        return Ok(());
    }

    // Detect ClickHouse ownership for chown
    let (ch_uid, ch_gid) = detect_clickhouse_ownership(Path::new(data_path))
        .unwrap_or_else(|e| {
            warn!(error = %e, "Failed to detect ClickHouse ownership");
            (None, None)
        });

    // Get current table information from ClickHouse for data paths
    let live_tables = ch.list_tables().await.unwrap_or_else(|e| {
        warn!(error = %e, "Failed to list live tables, using fallback paths");
        Vec::new()
    });

    let mut total_attached = 0u64;
    let mut tables_restored = 0u64;

    for table_key in &table_keys {
        let table_manifest = match manifest.tables.get(table_key) {
            Some(tm) => tm,
            None => continue,
        };

        // Skip metadata-only tables for data restore
        if table_manifest.metadata_only {
            debug!(table = %table_key, "Metadata-only table, skipping data restore");
            continue;
        }

        let (db, table) = table_key.split_once('.').unwrap_or(("default", table_key));

        // Collect all parts from all disks into a flat list
        let all_parts: Vec<_> = table_manifest
            .parts
            .values()
            .flat_map(|parts| parts.iter().cloned())
            .collect();

        if all_parts.is_empty() {
            debug!(table = %table_key, "No data parts, skipping");
            continue;
        }

        // Find the table's data path from live table info
        let table_data_path = find_table_data_path(
            &live_tables,
            db,
            table,
            data_path,
        );

        info!(
            table = %table_key,
            parts = all_parts.len(),
            data_path = %table_data_path.display(),
            "Restoring table data"
        );

        let attached = attach_parts(&AttachParams {
            ch,
            db,
            table,
            parts: &all_parts,
            backup_dir: &backup_dir,
            table_data_path: &table_data_path,
            clickhouse_uid: ch_uid,
            clickhouse_gid: ch_gid,
        })
        .await
        .with_context(|| format!("Failed to attach parts for table {}", table_key))?;

        total_attached += attached;
        tables_restored += 1;
    }

    info!(
        backup_name = %backup_name,
        tables = tables_restored,
        parts = total_attached,
        "Restore complete"
    );

    Ok(())
}

/// Find the data path for a table from the live table list.
fn find_table_data_path(
    live_tables: &[crate::clickhouse::client::TableRow],
    db: &str,
    table: &str,
    config_data_path: &str,
) -> PathBuf {
    for row in live_tables {
        if row.database == db && row.name == table {
            return get_table_data_path(&row.data_paths, config_data_path, db, table);
        }
    }

    // Table not found in live tables -- use default path construction
    get_table_data_path(&[], config_data_path, db, table)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clickhouse::client::TableRow;

    #[test]
    fn test_find_table_data_path_from_live_tables() {
        let live_tables = vec![TableRow {
            database: "default".to_string(),
            name: "trades".to_string(),
            engine: "MergeTree".to_string(),
            create_table_query: String::new(),
            uuid: "abc-123".to_string(),
            data_paths: vec!["/var/lib/clickhouse/store/abc/abc123/".to_string()],
            total_bytes: Some(1000),
        }];

        let result = find_table_data_path(
            &live_tables,
            "default",
            "trades",
            "/var/lib/clickhouse",
        );
        assert_eq!(
            result,
            PathBuf::from("/var/lib/clickhouse/store/abc/abc123/")
        );
    }

    #[test]
    fn test_find_table_data_path_not_found() {
        let live_tables: Vec<TableRow> = vec![];
        let result = find_table_data_path(
            &live_tables,
            "default",
            "trades",
            "/var/lib/clickhouse",
        );
        assert_eq!(
            result,
            PathBuf::from("/var/lib/clickhouse/data/default/trades")
        );
    }
}
