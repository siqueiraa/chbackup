//! Backup creation: FREEZE + shadow walk + hardlink + CRC64 + UNFREEZE + manifest.
//!
//! Implements the `create` command which:
//! 1. Lists tables matching the filter pattern
//! 2. Checks for pending mutations (design 3.1)
//! 3. Syncs replicas for Replicated tables (design 3.2)
//! 4. FREEZEs each table (parallel, bounded by max_connections)
//! 5. Walks shadow directories and hardlinks parts to backup staging
//! 6. Computes CRC64 checksums of each part's checksums.txt
//! 7. UNFREEZEs all tables
//! 8. Writes BackupManifest to metadata.json

pub mod checksum;
pub mod collect;
pub mod diff;
pub mod freeze;
pub mod mutations;
pub mod sync_replica;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use chrono::Utc;
use futures::future::try_join_all;
use tokio::sync::Semaphore;
use tracing::{info, warn};

use crate::clickhouse::client::{freeze_name, ChClient, TableRow};
use crate::concurrency::effective_max_connections;
use crate::config::Config;
use crate::manifest::{BackupManifest, DatabaseInfo, TableManifest};
use crate::table_filter::{is_engine_excluded, is_excluded, TableFilter};

use self::collect::collect_parts;
use self::diff::diff_parts;
use self::freeze::{FreezeGuard, FreezeInfo};

/// Create a local backup.
///
/// Returns the manifest describing the backup contents.
///
/// If `diff_from` is provided, the specified local backup is used as a base
/// for incremental comparison. Parts matching by name+CRC64 are carried
/// forward (referencing the base backup's S3 key) instead of being re-uploaded.
pub async fn create(
    config: &Config,
    ch: &ChClient,
    backup_name: &str,
    table_pattern: Option<&str>,
    schema_only: bool,
    diff_from: Option<&str>,
) -> Result<BackupManifest> {
    info!(
        backup_name = %backup_name,
        table_pattern = ?table_pattern,
        schema_only = schema_only,
        "Starting backup creation"
    );

    // 1. Get ClickHouse version
    let ch_version = match ch.get_version().await {
        Ok(v) => {
            info!(version = %v, "ClickHouse version");
            v
        }
        Err(e) => {
            warn!(error = %e, "Failed to get ClickHouse version, using empty string");
            String::new()
        }
    };

    // 2. Get disk information
    let disks = match ch.get_disks().await {
        Ok(d) => d,
        Err(e) => {
            warn!(error = %e, "Failed to get disk info, using defaults");
            Vec::new()
        }
    };

    let disk_map: HashMap<String, String> = disks
        .iter()
        .map(|d| (d.name.clone(), d.path.clone()))
        .collect();
    let disk_type_map: HashMap<String, String> = disks
        .iter()
        .map(|d| (d.name.clone(), d.disk_type.clone()))
        .collect();

    // 3. List all user tables
    let all_tables = ch.list_tables().await?;

    // 4. Filter tables by pattern, skip_tables, skip_table_engines
    let pattern = table_pattern.unwrap_or(&config.backup.tables);
    let filter = TableFilter::new(pattern);

    let filtered_tables: Vec<&TableRow> = all_tables
        .iter()
        .filter(|t| filter.matches(&t.database, &t.name))
        .filter(|t| !is_excluded(&t.database, &t.name, &config.clickhouse.skip_tables))
        .filter(|t| !is_engine_excluded(&t.engine, &config.clickhouse.skip_table_engines))
        .collect();

    info!(
        total = all_tables.len(),
        filtered = filtered_tables.len(),
        "Tables selected for backup"
    );

    // 5. Check allow_empty_backups
    if filtered_tables.is_empty() && !config.backup.allow_empty_backups {
        bail!(
            "No tables matched pattern '{}'. Set backup.allow_empty_backups=true to allow empty backups",
            pattern
        );
    }

    // 6. Check for pending mutations (design 3.1)
    let mutation_targets: Vec<(String, String)> = filtered_tables
        .iter()
        .map(|t| (t.database.clone(), t.name.clone()))
        .collect();

    let all_mutations = mutations::check_mutations(
        ch,
        &mutation_targets,
        config.clickhouse.backup_mutations,
    )
    .await?;

    // 7. Sync replicas (design 3.2)
    if config.clickhouse.sync_replicated_tables {
        let tables_vec: Vec<TableRow> = filtered_tables.iter().map(|t| (*t).clone()).collect();
        sync_replica::sync_replicas(ch, &tables_vec).await?;
    }

    // 8. Create backup directory
    let backup_dir = PathBuf::from(&config.clickhouse.data_path)
        .join("backup")
        .join(backup_name);

    std::fs::create_dir_all(&backup_dir)
        .with_context(|| format!("Failed to create backup directory: {}", backup_dir.display()))?;

    // 9. FREEZE tables and collect parts (parallel, bounded by max_connections)
    let mut table_manifests: HashMap<String, TableManifest> = HashMap::new();
    let mut databases_seen: HashMap<String, String> = HashMap::new();

    // Separate metadata-only / schema-only tables from data tables
    let mut data_tables: Vec<TableRow> = Vec::new();

    for table_row in &filtered_tables {
        let db = &table_row.database;
        let full_name = format!("{}.{}", db, table_row.name);

        // Track unique databases for DDL
        if !databases_seen.contains_key(db) {
            let ddl = format!("CREATE DATABASE IF NOT EXISTS `{}` ENGINE = Atomic", db);
            databases_seen.insert(db.clone(), ddl);
        }

        let is_metadata_only = is_metadata_only_engine(&table_row.engine);

        if !schema_only && !is_metadata_only {
            data_tables.push((*table_row).clone());
        } else {
            // Schema-only or metadata-only table -- no FREEZE needed
            table_manifests.insert(
                full_name,
                TableManifest {
                    ddl: table_row.create_table_query.clone(),
                    uuid: if table_row.uuid.is_empty()
                        || table_row.uuid == "00000000-0000-0000-0000-000000000000"
                    {
                        None
                    } else {
                        Some(table_row.uuid.clone())
                    },
                    engine: table_row.engine.clone(),
                    total_bytes: 0,
                    parts: HashMap::new(),
                    pending_mutations: Vec::new(),
                    metadata_only: is_metadata_only,
                    dependencies: Vec::new(),
                },
            );
        }
    }

    // Parallel FREEZE + collect for data tables
    let max_conn = effective_max_connections(config) as usize;
    let semaphore = Arc::new(Semaphore::new(max_conn));

    info!(
        "Freezing {} tables (max_connections={})",
        data_tables.len(),
        max_conn
    );

    let all_tables_arc = Arc::new(all_tables.clone());
    let mut handles = Vec::with_capacity(data_tables.len());

    for table_row in &data_tables {
        let sem = semaphore.clone();
        let ch = ch.clone();
        let backup_name_owned = backup_name.to_string();
        let db = table_row.database.clone();
        let table = table_row.name.clone();
        let data_path = config.clickhouse.data_path.clone();
        let backup_dir_clone = backup_dir.clone();
        let tables_for_collect = all_tables_arc.clone();
        let ignore_not_exists = config.clickhouse.ignore_not_exists_error_during_freeze;
        let all_mutations_clone = all_mutations.clone();
        let table_row_clone = table_row.clone();
        let disk_type_map_clone = disk_type_map.clone();
        let disk_map_clone = disk_map.clone();

        let handle = tokio::spawn(async move {
            let _permit = sem
                .acquire()
                .await
                .map_err(|_| anyhow::anyhow!("Semaphore closed"))?;

            let full_name = format!("{}.{}", db, table);
            let fname = freeze_name(&backup_name_owned, &db, &table);

            // FREEZE the table
            info!(
                db = %db,
                table = %table,
                freeze_name = %fname,
                "Freezing table"
            );

            let freeze_result = ch.freeze_table(&db, &table, &fname).await;
            let frozen = match freeze_result {
                Ok(()) => true,
                Err(e) => {
                    let err_msg = format!("{e:#}");
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
                        false
                    } else {
                        return Err(e);
                    }
                }
            };

            if !frozen {
                info!(
                    db = %db,
                    table = %table,
                    "Table skipped (not found during FREEZE)"
                );
                return Ok(None);
            }

            let freeze_info = FreezeInfo {
                database: db.clone(),
                table: table.clone(),
                freeze_name: fname.clone(),
            };

            // Collect parts from shadow using spawn_blocking for filesystem I/O
            let fname_for_collect = fname;
            let parts_map = tokio::task::spawn_blocking(move || {
                collect_parts(
                    &data_path,
                    &fname_for_collect,
                    &backup_dir_clone,
                    &tables_for_collect,
                    &disk_type_map_clone,
                    &disk_map_clone,
                )
            })
            .await
            .context("spawn_blocking panicked during collect_parts")??;

            // Build TableManifest: group collected parts by actual disk name
            let collected = parts_map.get(&full_name).cloned().unwrap_or_default();
            let total_bytes: u64 = collected.iter().map(|cp| cp.part_info.size).sum();

            let mut parts_by_disk: HashMap<String, Vec<_>> = HashMap::new();
            for cp in collected {
                parts_by_disk
                    .entry(cp.disk_name)
                    .or_default()
                    .push(cp.part_info);
            }

            let table_manifest = TableManifest {
                ddl: table_row_clone.create_table_query.clone(),
                uuid: if table_row_clone.uuid.is_empty()
                    || table_row_clone.uuid == "00000000-0000-0000-0000-000000000000"
                {
                    None
                } else {
                    Some(table_row_clone.uuid.clone())
                },
                engine: table_row_clone.engine.clone(),
                total_bytes,
                parts: parts_by_disk,
                pending_mutations: all_mutations_clone,
                metadata_only: false,
                dependencies: Vec::new(),
            };

            Ok(Some((freeze_info, full_name, table_manifest)))
        });

        handles.push(handle);
    }

    // Await all tasks, collecting results
    let results = try_join_all(handles).await.context(
        "A FREEZE+collect task panicked",
    )?;

    // Build FreezeGuard from successful results for cleanup, and aggregate table manifests
    let mut freeze_guard = FreezeGuard::new();
    let mut had_error = false;
    let mut first_error: Option<anyhow::Error> = None;

    for result in results {
        match result {
            Ok(Some((freeze_info, full_name, table_manifest))) => {
                freeze_guard.add(freeze_info);
                table_manifests.insert(full_name, table_manifest);
            }
            Ok(None) => {
                // Table was skipped (not found during FREEZE)
            }
            Err(e) => {
                if !had_error {
                    had_error = true;
                    first_error = Some(e);
                }
            }
        }
    }

    // 10. UNFREEZE all tables (even on error, clean up frozen tables)
    info!("Unfreezing all tables");
    freeze_guard.unfreeze_all(ch).await?;
    // Clear the guard so Drop doesn't warn
    let _ = std::mem::take(&mut freeze_guard);

    // Propagate the first error if any task failed
    if let Some(e) = first_error {
        return Err(e);
    }

    // 11. Build database list
    let databases: Vec<DatabaseInfo> = databases_seen
        .into_iter()
        .map(|(name, ddl)| DatabaseInfo { name, ddl })
        .collect();

    // 12. Save per-table metadata files
    let metadata_dir = backup_dir.join("metadata");
    for (full_name, table_manifest) in &table_manifests {
        if let Some((db, table)) = full_name.split_once('.') {
            let table_metadata_dir = metadata_dir.join(db);
            std::fs::create_dir_all(&table_metadata_dir)?;
            let table_json = serde_json::to_string_pretty(table_manifest)?;
            std::fs::write(
                table_metadata_dir.join(format!("{}.json", table)),
                &table_json,
            )?;
        }
    }

    // 13. Build manifest
    let mut manifest = BackupManifest {
        manifest_version: 1,
        name: backup_name.to_string(),
        timestamp: Utc::now(),
        clickhouse_version: ch_version,
        chbackup_version: env!("CARGO_PKG_VERSION").to_string(),
        data_format: config.backup.compression.clone(),
        compressed_size: 0, // Set during upload
        metadata_size: 0,
        disks: disk_map,
        disk_types: disk_type_map,
        tables: table_manifests,
        databases,
        functions: Vec::new(),
        named_collections: Vec::new(),
        rbac: None,
    };

    // 13b. Apply incremental diff if --diff-from is specified
    if let Some(base_name) = diff_from {
        info!(base = %base_name, "Loading base manifest for diff-from");
        let base_manifest_path = PathBuf::from(&config.clickhouse.data_path)
            .join("backup")
            .join(base_name)
            .join("metadata.json");
        let base = BackupManifest::load_from_file(&base_manifest_path)
            .with_context(|| format!("Failed to load base backup '{}' for --diff-from", base_name))?;
        let result = diff_parts(&mut manifest, &base);
        info!(
            carried = result.carried,
            uploaded = result.uploaded,
            crc_mismatches = result.crc_mismatches,
            "Incremental diff applied to manifest"
        );
    }

    // 14. Save manifest
    let manifest_path = backup_dir.join("metadata.json");
    manifest.save_to_file(&manifest_path)?;

    // Calculate metadata_size
    let metadata_size = std::fs::metadata(&manifest_path)
        .map(|m| m.len())
        .unwrap_or(0);

    // Update manifest with metadata_size and re-save
    manifest.metadata_size = metadata_size;
    manifest.save_to_file(&manifest_path)?;

    let table_count = manifest.tables.len();
    let part_count: usize = manifest
        .tables
        .values()
        .flat_map(|t| t.parts.values())
        .map(|parts| parts.len())
        .sum();

    info!(
        backup_name = %backup_name,
        tables = table_count,
        parts = part_count,
        "Backup created successfully"
    );

    Ok(manifest)
}

/// Check if an engine is metadata-only (views, dictionaries, etc.).
fn is_metadata_only_engine(engine: &str) -> bool {
    matches!(
        engine,
        "View"
            | "MaterializedView"
            | "LiveView"
            | "WindowView"
            | "Dictionary"
            | "Null"
            | "Set"
            | "Join"
            | "Buffer"
            | "Distributed"
            | "Merge"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use self::freeze::FreezeInfo;

    #[test]
    fn test_freeze_info_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<FreezeInfo>();
    }

    #[test]
    fn test_table_row_clone() {
        let row = TableRow {
            database: "default".to_string(),
            name: "trades".to_string(),
            engine: "MergeTree".to_string(),
            create_table_query: "CREATE TABLE default.trades (id UInt64) ENGINE = MergeTree ORDER BY id".to_string(),
            uuid: "abc-123".to_string(),
            data_paths: vec!["/var/lib/clickhouse/store/abc/abc123/".to_string()],
            total_bytes: Some(1000),
        };

        let cloned = row.clone();
        assert_eq!(cloned.database, "default");
        assert_eq!(cloned.name, "trades");
        assert_eq!(cloned.engine, "MergeTree");
    }

    #[test]
    fn test_is_metadata_only_engine() {
        assert!(is_metadata_only_engine("View"));
        assert!(is_metadata_only_engine("MaterializedView"));
        assert!(is_metadata_only_engine("Dictionary"));
        assert!(is_metadata_only_engine("Distributed"));
        assert!(!is_metadata_only_engine("MergeTree"));
        assert!(!is_metadata_only_engine("ReplicatedMergeTree"));
        assert!(!is_metadata_only_engine("ReplacingMergeTree"));
    }

    #[test]
    fn test_allow_empty_backup() {
        // Verify an empty manifest is valid with zero tables
        let manifest = BackupManifest {
            manifest_version: 1,
            name: "empty-test".to_string(),
            timestamp: Utc::now(),
            clickhouse_version: "24.1.3".to_string(),
            chbackup_version: "0.1.0".to_string(),
            data_format: "lz4".to_string(),
            compressed_size: 0,
            metadata_size: 0,
            disks: HashMap::new(),
            disk_types: HashMap::new(),
            tables: HashMap::new(),
            databases: Vec::new(),
            functions: Vec::new(),
            named_collections: Vec::new(),
            rbac: None,
        };

        let json = serde_json::to_string(&manifest).unwrap();
        let parsed: BackupManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.tables.len(), 0);
        assert_eq!(parsed.name, "empty-test");
    }
}
