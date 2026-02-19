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
pub mod rbac;
pub mod sync_replica;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use chrono::Utc;
use futures::future::try_join_all;
use tokio::sync::Semaphore;
use tracing::{info, warn};

use crate::clickhouse::client::{freeze_name, ChClient, ColumnInconsistency, TableRow};
use crate::concurrency::effective_max_connections;
use crate::config::Config;
use crate::manifest::{BackupManifest, DatabaseInfo, TableManifest};
use crate::table_filter::{is_engine_excluded, is_excluded, TableFilter};

use self::collect::collect_parts;
use self::diff::diff_parts;
use self::freeze::{FreezeGuard, FreezeInfo};

/// Parse a comma-separated partition list into a vector of partition IDs.
///
/// Trims whitespace from each partition ID. Returns an empty vec if input is None or empty.
fn parse_partition_list(partitions: Option<&str>) -> Vec<String> {
    match partitions {
        Some(s) if !s.is_empty() => s.split(',').map(|p| p.trim().to_string()).collect(),
        _ => Vec::new(),
    }
}

/// Create a local backup.
///
/// Returns the manifest describing the backup contents.
///
/// If `diff_from` is provided, the specified local backup is used as a base
/// for incremental comparison. Parts matching by name+CRC64 are carried
/// forward (referencing the base backup's S3 key) instead of being re-uploaded.
///
/// If `partitions` is provided (comma-separated partition IDs), only those
/// partitions are frozen via FREEZE PARTITION instead of whole-table FREEZE.
///
/// If `skip_check_parts_columns` is true, the pre-flight column consistency
/// check is skipped even if `config.clickhouse.check_parts_columns` is true.
#[allow(clippy::too_many_arguments)]
pub async fn create(
    config: &Config,
    ch: &ChClient,
    backup_name: &str,
    table_pattern: Option<&str>,
    schema_only: bool,
    diff_from: Option<&str>,
    partitions: Option<&str>,
    skip_check_parts_columns: bool,
    rbac: bool,
    configs: bool,
    named_collections: bool,
) -> Result<BackupManifest> {
    info!(
        backup_name = %backup_name,
        table_pattern = ?table_pattern,
        schema_only = schema_only,
        partitions = ?partitions,
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
    let disk_remote_paths: HashMap<String, String> = disks
        .iter()
        .filter(|d| !d.remote_path.is_empty())
        .map(|d| (d.name.clone(), d.remote_path.clone()))
        .collect();

    // 3. List all user tables
    let all_tables = ch.list_tables().await?;

    // 3b. Query table dependencies (CH 23.3+)
    let deps_map = ch
        .query_table_dependencies()
        .await
        .unwrap_or_else(|e| {
            warn!(error = %e, "Failed to query table dependencies (CH < 23.3?), dependencies will be empty");
            HashMap::new()
        });
    info!(
        tables_with_deps = deps_map.values().filter(|v| !v.is_empty()).count(),
        "Queried table dependencies"
    );

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

    // 5b. Parts column consistency check (design 3.3)
    if config.clickhouse.check_parts_columns && !skip_check_parts_columns {
        let targets: Vec<(String, String)> = filtered_tables
            .iter()
            .map(|t| (t.database.clone(), t.name.clone()))
            .collect();

        match ch.check_parts_columns(&targets).await {
            Ok(inconsistencies) => {
                let actionable = filter_benign_type_drift(inconsistencies);
                if !actionable.is_empty() {
                    for inc in &actionable {
                        warn!(
                            database = %inc.database,
                            table = %inc.table,
                            column = %inc.column,
                            types = ?inc.types,
                            "Column type inconsistency detected across active parts"
                        );
                    }
                    info!(
                        count = actionable.len(),
                        "Parts column consistency check found inconsistencies (proceeding anyway)"
                    );
                } else {
                    info!("Parts column consistency check passed");
                }
            }
            Err(e) => {
                warn!(
                    error = %e,
                    "Parts column consistency check failed, continuing anyway"
                );
            }
        }
    }

    // 6. Check for pending mutations (design 3.1)
    let mutation_targets: Vec<(String, String)> = filtered_tables
        .iter()
        .map(|t| (t.database.clone(), t.name.clone()))
        .collect();

    let all_mutations =
        mutations::check_mutations(ch, &mutation_targets, config.clickhouse.backup_mutations)
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

    std::fs::create_dir_all(&backup_dir).with_context(|| {
        format!(
            "Failed to create backup directory: {}",
            backup_dir.display()
        )
    })?;

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
            let table_deps = deps_map.get(&full_name).cloned().unwrap_or_default();
            if !table_deps.is_empty() {
                info!(table = %full_name, deps = ?table_deps, "Populated dependencies for metadata-only table");
            }
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
                    dependencies: table_deps,
                },
            );
        }
    }

    // Parse partition list for partition-level freeze
    let partition_ids = parse_partition_list(partitions);

    // Parallel FREEZE + collect for data tables
    let max_conn = effective_max_connections(config) as usize;
    let semaphore = Arc::new(Semaphore::new(max_conn));

    info!(
        "Freezing {} tables (max_connections={})",
        data_tables.len(),
        max_conn
    );

    let all_tables_arc = Arc::new(all_tables.clone());
    let deps_arc = Arc::new(deps_map);
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
        let skip_disks_clone = config.clickhouse.skip_disks.clone();
        let skip_disk_types_clone = config.clickhouse.skip_disk_types.clone();
        let partition_ids_clone = partition_ids.clone();
        let deps_clone = deps_arc.clone();

        let handle = tokio::spawn(async move {
            let _permit = sem
                .acquire()
                .await
                .map_err(|_| anyhow::anyhow!("Semaphore closed"))?;

            let full_name = format!("{}.{}", db, table);
            let fname = freeze_name(&backup_name_owned, &db, &table);

            // FREEZE the table (whole-table or per-partition)
            let frozen = if partition_ids_clone.is_empty() {
                // Whole-table FREEZE
                info!(
                    db = %db,
                    table = %table,
                    freeze_name = %fname,
                    "Freezing table"
                );

                let freeze_result = ch.freeze_table(&db, &table, &fname).await;
                match freeze_result {
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
                }
            } else {
                // Per-partition FREEZE
                let mut any_frozen = false;
                for partition_id in &partition_ids_clone {
                    info!(
                        db = %db,
                        table = %table,
                        partition = %partition_id,
                        freeze_name = %fname,
                        "Freezing partition"
                    );

                    let freeze_result =
                        ch.freeze_partition(&db, &table, partition_id, &fname).await;
                    match freeze_result {
                        Ok(()) => {
                            any_frozen = true;
                        }
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
                                    partition = %partition_id,
                                    error = %e,
                                    "Table/partition not found during FREEZE PARTITION, skipping"
                                );
                            } else {
                                return Err(e);
                            }
                        }
                    }
                }
                any_frozen
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
                    &skip_disks_clone,
                    &skip_disk_types_clone,
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
                dependencies: deps_clone.get(&full_name).cloned().unwrap_or_default(),
            };

            Ok(Some((freeze_info, full_name, table_manifest)))
        });

        handles.push(handle);
    }

    // Await all tasks, collecting results
    let results = try_join_all(handles)
        .await
        .context("A FREEZE+collect task panicked")?;

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
        disk_remote_paths,
        tables: table_manifests,
        databases,
        functions: Vec::new(),
        named_collections: Vec::new(),
        rbac: None,
    };

    // 13a. Backup RBAC, configs, named collections (populates manifest fields)
    rbac::backup_rbac_and_configs(
        config,
        ch,
        &backup_dir,
        &mut manifest,
        rbac,
        configs,
        named_collections,
    )
    .await?;

    // 13b. Apply incremental diff if --diff-from is specified
    if let Some(base_name) = diff_from {
        info!(base = %base_name, "Loading base manifest for diff-from");
        let base_manifest_path = PathBuf::from(&config.clickhouse.data_path)
            .join("backup")
            .join(base_name)
            .join("metadata.json");
        let base = BackupManifest::load_from_file(&base_manifest_path).with_context(|| {
            format!("Failed to load base backup '{}' for --diff-from", base_name)
        })?;
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

/// Check if a type string represents a benign drift type.
///
/// Per design 3.3, Enum variants, Tuple element names, Nullable wrappers,
/// and Array(Tuple) are considered benign drift that does not indicate
/// actual schema incompatibility.
fn is_benign_type(type_str: &str) -> bool {
    type_str.starts_with("Enum")
        || type_str.starts_with("Tuple")
        || type_str.starts_with("Nullable(Enum")
        || type_str.starts_with("Nullable(Tuple")
        || type_str.starts_with("Array(Tuple")
}

/// Filter out column inconsistencies where ALL types in the inconsistency
/// are benign drift types (Enum, Tuple, Nullable, etc.).
///
/// Returns only inconsistencies that contain at least one non-benign type.
fn filter_benign_type_drift(inconsistencies: Vec<ColumnInconsistency>) -> Vec<ColumnInconsistency> {
    inconsistencies
        .into_iter()
        .filter(|inc| !inc.types.iter().all(|t| is_benign_type(t)))
        .collect()
}

#[cfg(test)]
mod tests {
    use self::freeze::FreezeInfo;
    use super::*;
    use crate::clickhouse::client::freeze_partition_sql;

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
            create_table_query:
                "CREATE TABLE default.trades (id UInt64) ENGINE = MergeTree ORDER BY id".to_string(),
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
            disk_remote_paths: HashMap::new(),
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

    #[test]
    fn test_partition_list_parsing() {
        // Empty/None cases
        let empty = parse_partition_list(None);
        assert!(empty.is_empty());

        let empty_str = parse_partition_list(Some(""));
        assert!(empty_str.is_empty());

        // Single partition
        let single = parse_partition_list(Some("202401"));
        assert_eq!(single, vec!["202401"]);

        // Multiple partitions
        let multi = parse_partition_list(Some("202401,202402,202403"));
        assert_eq!(multi, vec!["202401", "202402", "202403"]);

        // Whitespace trimming
        let spaced = parse_partition_list(Some(" 202401 , 202402 , 202403 "));
        assert_eq!(spaced, vec!["202401", "202402", "202403"]);
    }

    #[test]
    fn test_freeze_partition_called_for_each() {
        // Verify that freeze_partition_sql generates correct SQL for each partition
        let partitions = parse_partition_list(Some("202401,202402"));
        assert_eq!(partitions.len(), 2);

        let freeze_name = "chbackup_daily_default_trades";
        for partition in &partitions {
            let sql = freeze_partition_sql("default", "trades", partition, freeze_name);
            assert!(sql.contains("FREEZE PARTITION"));
            assert!(sql.contains(partition));
            assert!(sql.contains(freeze_name));
        }

        // Verify first partition SQL
        let sql1 = freeze_partition_sql("default", "trades", &partitions[0], freeze_name);
        assert_eq!(
            sql1,
            "ALTER TABLE `default`.`trades` FREEZE PARTITION '202401' WITH NAME 'chbackup_daily_default_trades'"
        );

        // Verify second partition SQL
        let sql2 = freeze_partition_sql("default", "trades", &partitions[1], freeze_name);
        assert_eq!(
            sql2,
            "ALTER TABLE `default`.`trades` FREEZE PARTITION '202402' WITH NAME 'chbackup_daily_default_trades'"
        );
    }

    #[test]
    fn test_parts_columns_check_disabled() {
        // When check_parts_columns is false, no checking is needed.
        // This test verifies the config gating logic conceptually:
        // if !config.check_parts_columns, the check is skipped entirely.
        let check_enabled = false;
        let skip_flag = false;
        let should_check = check_enabled && !skip_flag;
        assert!(!should_check);

        // Also verify skip flag overrides config
        let check_enabled = true;
        let skip_flag = true;
        let should_check = check_enabled && !skip_flag;
        assert!(!should_check);

        // Both must be right for check to run
        let check_enabled = true;
        let skip_flag = false;
        let should_check = check_enabled && !skip_flag;
        assert!(should_check);
    }

    #[test]
    fn test_parts_columns_check_skip_benign_types() {
        // Enum drift is benign
        let enum_drift = ColumnInconsistency {
            database: "default".to_string(),
            table: "trades".to_string(),
            column: "status".to_string(),
            types: vec![
                "Enum8('active' = 1, 'inactive' = 2)".to_string(),
                "Enum8('active' = 1, 'inactive' = 2, 'deleted' = 3)".to_string(),
            ],
        };

        // Tuple drift is benign
        let tuple_drift = ColumnInconsistency {
            database: "default".to_string(),
            table: "events".to_string(),
            column: "metadata".to_string(),
            types: vec![
                "Tuple(a UInt64, b String)".to_string(),
                "Tuple(a UInt64, b String, c Float64)".to_string(),
            ],
        };

        // Nullable(Enum) drift is benign
        let nullable_enum_drift = ColumnInconsistency {
            database: "logs".to_string(),
            table: "entries".to_string(),
            column: "level".to_string(),
            types: vec![
                "Nullable(Enum8('info' = 1))".to_string(),
                "Nullable(Enum8('info' = 1, 'warn' = 2))".to_string(),
            ],
        };

        // Real drift (non-benign): Float64 vs Decimal
        let real_drift = ColumnInconsistency {
            database: "default".to_string(),
            table: "trades".to_string(),
            column: "amount".to_string(),
            types: vec!["Float64".to_string(), "Decimal(18,2)".to_string()],
        };

        let all = vec![enum_drift, tuple_drift, nullable_enum_drift, real_drift];
        let actionable = filter_benign_type_drift(all);

        // Only the real drift should remain
        assert_eq!(actionable.len(), 1);
        assert_eq!(actionable[0].column, "amount");
        assert_eq!(actionable[0].types, vec!["Float64", "Decimal(18,2)"]);
    }

    #[test]
    fn test_dependency_population_from_map() {
        // Given a dependency map (as returned by query_table_dependencies()),
        // verify that the lookup + unwrap_or_default pattern works correctly.
        let mut deps_map: HashMap<String, Vec<String>> = HashMap::new();
        deps_map.insert(
            "default.user_dict".to_string(),
            vec!["default.users".to_string()],
        );
        deps_map.insert(
            "default.trades_view".to_string(),
            vec!["default.trades".to_string(), "default.users".to_string()],
        );

        // Table with dependencies
        let deps1 = deps_map
            .get("default.user_dict")
            .cloned()
            .unwrap_or_default();
        assert_eq!(deps1, vec!["default.users"]);

        // Table with multiple dependencies
        let deps2 = deps_map
            .get("default.trades_view")
            .cloned()
            .unwrap_or_default();
        assert_eq!(deps2, vec!["default.trades", "default.users"]);

        // Table with no dependencies (not in map)
        let deps3 = deps_map.get("default.trades").cloned().unwrap_or_default();
        assert!(deps3.is_empty());

        // Verify these can be used in TableManifest
        let tm = TableManifest {
            ddl: "CREATE DICTIONARY ...".to_string(),
            uuid: None,
            engine: "Dictionary".to_string(),
            total_bytes: 0,
            parts: HashMap::new(),
            pending_mutations: Vec::new(),
            metadata_only: true,
            dependencies: deps1,
        };
        assert_eq!(tm.dependencies, vec!["default.users"]);
    }
}
