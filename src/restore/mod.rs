//! Restore: read manifest, CREATE DB/TABLE, hardlink to detached, ATTACH PART.
//!
//! Implements Mode B (non-destructive) restore flow from design doc section 5:
//! 1. Read manifest from `{backup_dir}/metadata.json`
//! 2. CREATE databases from manifest.databases DDL
//! 3. For each table in manifest (filtered by table_pattern), parallel by max_connections:
//!    - If table does not exist: CREATE TABLE from DDL
//!    - Sort parts by (partition, min_block)
//!    - Hardlink parts to detached/ directory
//!    - ALTER TABLE ATTACH PART for each part (engine-aware routing)
//! 4. Log summary

pub mod attach;
pub mod schema;
pub mod sort;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use futures::future::try_join_all;
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};

use crate::clickhouse::client::ChClient;
use crate::concurrency::{effective_max_connections, effective_object_disk_server_side_copy_concurrency};
use crate::config::Config;
use crate::manifest::BackupManifest;
use crate::object_disk::is_s3_disk;
use crate::storage::S3Client;
use crate::table_filter::TableFilter;

use attach::{attach_parts_owned, detect_clickhouse_ownership, get_table_data_path, OwnedAttachParams};
use schema::{create_databases, create_tables};

/// Restore a backup to ClickHouse.
///
/// Implements Mode B (non-destructive): creates databases and tables if they
/// don't exist, then attaches data parts via detached/ directory.
/// Tables are restored in parallel, bounded by max_connections semaphore.
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

    // Check if any tables have S3 disk parts -- if so, build S3Client
    let has_s3_disks = manifest.disk_types.values().any(|dt| is_s3_disk(dt));
    let s3_client = if has_s3_disks {
        match S3Client::new(&config.s3).await {
            Ok(client) => Some(client),
            Err(e) => {
                warn!(error = %e, "Failed to create S3Client for S3 disk restore, S3 disk parts will fail");
                None
            }
        }
    } else {
        None
    };

    // Build disk remote paths from live disks (for S3 CopyObject source)
    let disk_remote_paths: HashMap<String, String> = if has_s3_disks {
        match ch.get_disks().await {
            Ok(disks) => disks
                .into_iter()
                .filter(|d| !d.remote_path.is_empty())
                .map(|d| (d.name, d.remote_path))
                .collect(),
            Err(e) => {
                warn!(error = %e, "Failed to get disk info for S3 restore");
                HashMap::new()
            }
        }
    } else {
        HashMap::new()
    };

    let object_disk_concurrency =
        effective_object_disk_server_side_copy_concurrency(config) as usize;
    let allow_streaming = config.s3.allow_object_disk_streaming;

    // Collect tables that need data restore
    let mut restore_items: Vec<(String, OwnedAttachParams)> = Vec::new();

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

        // Find the table's UUID from live tables (needed for S3 restore path derivation)
        let table_uuid = find_table_uuid(&live_tables, db, table)
            .or_else(|| table_manifest.uuid.clone());

        info!(
            table = %table_key,
            parts = all_parts.len(),
            data_path = %table_data_path.display(),
            "Restoring table data"
        );

        restore_items.push((
            table_key.clone(),
            OwnedAttachParams {
                ch: ch.clone(),
                db: db.to_string(),
                table: table.to_string(),
                parts: all_parts,
                backup_dir: backup_dir.clone(),
                table_data_path,
                clickhouse_uid: ch_uid,
                clickhouse_gid: ch_gid,
                engine: table_manifest.engine.clone(),
                s3_client: s3_client.clone(),
                disk_type_map: manifest.disk_types.clone(),
                object_disk_server_side_copy_concurrency: object_disk_concurrency,
                allow_object_disk_streaming: allow_streaming,
                disk_remote_paths: disk_remote_paths.clone(),
                table_uuid,
                parts_by_disk: table_manifest.parts.clone(),
            },
        ));
    }

    let max_conn = effective_max_connections(config) as usize;
    let table_count = restore_items.len();

    info!(
        "Restoring {} tables (max_connections={})",
        table_count, max_conn
    );

    // 6. Parallel table restore with semaphore
    let semaphore = Arc::new(Semaphore::new(max_conn));

    let mut handles = Vec::with_capacity(table_count);

    for (table_key, params) in restore_items {
        let sem = semaphore.clone();

        let handle = tokio::spawn(async move {
            let _permit = sem
                .acquire()
                .await
                .map_err(|_| anyhow::anyhow!("Semaphore closed"))?;

            let attached = attach_parts_owned(params)
                .await
                .with_context(|| format!("Failed to attach parts for table {}", table_key))?;

            Ok::<(String, u64), anyhow::Error>((table_key, attached))
        });

        handles.push(handle);
    }

    // Await all restore tasks
    let results: Vec<(String, u64)> = try_join_all(handles)
        .await
        .context("A restore task panicked")?
        .into_iter()
        .collect::<Result<Vec<_>>>()?;

    // 7. Tally totals
    let mut total_attached = 0u64;
    let tables_restored = results.len() as u64;
    for (_table_key, attached) in &results {
        total_attached += attached;
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

/// Find the UUID for a table from the live table list.
///
/// The UUID is used for S3 disk restore to derive UUID-isolated object paths.
/// Prefers the live table UUID (current destination) over the manifest UUID.
fn find_table_uuid(
    live_tables: &[crate::clickhouse::client::TableRow],
    db: &str,
    table: &str,
) -> Option<String> {
    for row in live_tables {
        if row.database == db && row.name == table && !row.uuid.is_empty() {
            return Some(row.uuid.clone());
        }
    }
    None
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
