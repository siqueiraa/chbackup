//! Backup creation: FREEZE + shadow walk + hardlink + CRC64 + UNFREEZE + manifest.
//!
//! Implements the `create` command which:
//! 1. Lists tables matching the filter pattern
//! 2. Checks for pending mutations (design 3.1)
//! 3. Syncs replicas for Replicated tables (design 3.2)
//! 4. FREEZEs each table
//! 5. Walks shadow directories and hardlinks parts to backup staging
//! 6. Computes CRC64 checksums of each part's checksums.txt
//! 7. UNFREEZEs all tables
//! 8. Writes BackupManifest to metadata.json

pub mod checksum;
pub mod collect;
pub mod freeze;
pub mod mutations;
pub mod sync_replica;

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use chrono::Utc;
use tracing::{info, warn};

use crate::clickhouse::client::{freeze_name, ChClient, TableRow};
use crate::config::Config;
use crate::manifest::{BackupManifest, DatabaseInfo, TableManifest};
use crate::table_filter::{is_engine_excluded, is_excluded, TableFilter};

use self::collect::collect_parts;
use self::freeze::{freeze_table, FreezeGuard};

/// Create a local backup.
///
/// Returns the manifest describing the backup contents.
pub async fn create(
    config: &Config,
    ch: &ChClient,
    backup_name: &str,
    table_pattern: Option<&str>,
    schema_only: bool,
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

    // 9. FREEZE tables and collect parts
    let mut freeze_guard = FreezeGuard::new();
    let mut table_manifests: HashMap<String, TableManifest> = HashMap::new();
    let mut databases_seen: HashMap<String, String> = HashMap::new();

    for table_row in &filtered_tables {
        let db = &table_row.database;
        let table = &table_row.name;
        let full_name = format!("{}.{}", db, table);

        // Track unique databases for DDL
        if !databases_seen.contains_key(db) {
            // We use a simple CREATE DATABASE IF NOT EXISTS
            let ddl = format!("CREATE DATABASE IF NOT EXISTS `{}` ENGINE = Atomic", db);
            databases_seen.insert(db.clone(), ddl);
        }

        // Determine if this is a metadata-only table (views, dictionaries, etc.)
        let is_metadata_only = is_metadata_only_engine(&table_row.engine);

        if !schema_only && !is_metadata_only {
            // Generate freeze name
            let fname = freeze_name(backup_name, db, table);

            // FREEZE the table
            let frozen = freeze_table(
                ch,
                &mut freeze_guard,
                db,
                table,
                &fname,
                config.clickhouse.ignore_not_exists_error_during_freeze,
            )
            .await?;

            if !frozen {
                info!(
                    db = %db,
                    table = %table,
                    "Table skipped (not found during FREEZE)"
                );
                continue;
            }

            // Collect parts from shadow using spawn_blocking for filesystem I/O
            let data_path = config.clickhouse.data_path.clone();
            let fname_clone = fname.clone();
            let backup_dir_clone = backup_dir.clone();
            let tables_for_collect: Vec<TableRow> = all_tables.clone();

            let parts_map = tokio::task::spawn_blocking(move || {
                collect_parts(
                    &data_path,
                    &fname_clone,
                    &backup_dir_clone,
                    &tables_for_collect,
                )
            })
            .await
            .context("spawn_blocking panicked during collect_parts")??;

            // Get mutations for this specific table
            // Phase 1: mutation info doesn't carry db/table, include all
            let table_mutations: Vec<_> = all_mutations.clone();

            // Build TableManifest
            let parts_for_table = parts_map.get(&full_name).cloned().unwrap_or_default();
            let total_bytes = parts_for_table.iter().map(|p| p.size).sum();

            let mut parts_by_disk: HashMap<String, Vec<_>> = HashMap::new();
            if !parts_for_table.is_empty() {
                // Phase 1: all parts go to "default" disk
                parts_by_disk.insert("default".to_string(), parts_for_table);
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
                    total_bytes,
                    parts: parts_by_disk,
                    pending_mutations: table_mutations,
                    metadata_only: false,
                    dependencies: Vec::new(),
                },
            );
        } else {
            // Schema-only or metadata-only table
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

    // 10. UNFREEZE all tables
    info!("Unfreezing all tables");
    freeze_guard.unfreeze_all(ch).await?;
    // Clear the guard so Drop doesn't warn
    let _ = std::mem::take(&mut freeze_guard);

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
    let manifest = BackupManifest {
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

    // 14. Save manifest
    let manifest_path = backup_dir.join("metadata.json");
    manifest.save_to_file(&manifest_path)?;

    // Calculate metadata_size
    let metadata_size = std::fs::metadata(&manifest_path)
        .map(|m| m.len())
        .unwrap_or(0);

    // Update manifest with metadata_size and re-save
    let mut manifest = manifest;
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

    // DEBUG_MARKER:F006 - verify backup creation produces valid manifest
    info!(
        target: "debug",
        "DEBUG_VERIFY:F006 tables={} parts={} backup_name={}",
        table_count, part_count, backup_name
    );
    // END_DEBUG_MARKER:F006

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
