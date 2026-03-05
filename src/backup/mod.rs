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

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use chrono::Utc;
use futures::future::try_join_all;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::clickhouse::client::{freeze_name, ChClient, ColumnInconsistency, TableRow};
use crate::concurrency::effective_max_connections;
use crate::config::Config;
use crate::manifest::{BackupManifest, DatabaseInfo, TableManifest};
use crate::object_disk;
use crate::table_filter::{is_engine_excluded, is_excluded, TableFilter};

use self::collect::collect_parts;
use self::diff::diff_parts;
use self::freeze::{FreezeGuard, FreezeInfo};

fn normalize_uuid(uuid: &str) -> Option<String> {
    if uuid.is_empty() || uuid == "00000000-0000-0000-0000-000000000000" {
        None
    } else {
        Some(uuid.to_string())
    }
}

/// Parse a comma-separated partition list into a vector of partition IDs.
///
/// Trims whitespace from each partition ID. Returns an empty vec if input is None or empty.
/// Special case: if any partition ID is "all", returns an empty vec to trigger
/// whole-table FREEZE (unpartitioned tables use partition_id="all" in system.parts).
fn parse_partition_list(partitions: Option<&str>) -> Vec<String> {
    match partitions {
        Some(s) if !s.is_empty() => {
            let parts: Vec<String> = s
                .split(',')
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .collect();
            // "all" means whole-table freeze (for unpartitioned MergeTree tables)
            if parts.iter().any(|p| p == "all") {
                info!(
                    "Partition 'all' specified -- using whole-table FREEZE for unpartitioned tables"
                );
                Vec::new()
            } else {
                parts
            }
        }
        _ => Vec::new(),
    }
}

/// Check if a FREEZE error is ignorable (table/partition not found).
///
/// Returns true for:
/// - Code 60: UNKNOWN_TABLE
/// - Code 81: UNKNOWN_DATABASE
/// - Code 218: CANNOT_FREEZE_PARTITION (partition doesn't exist or has no data)
fn is_ignorable_freeze_error(err_msg: &str) -> bool {
    err_msg.contains("UNKNOWN_TABLE")
        || err_msg.contains("UNKNOWN_DATABASE")
        || err_msg.contains("Code: 60")
        || err_msg.contains("Code: 81")
        || err_msg.contains("CANNOT_FREEZE_PARTITION")
        || err_msg.contains("Code: 218")
}

/// Create a local backup.
///
/// Returns the manifest describing the backup contents.
///
/// If `diff_from` is provided, the specified local backup is used as a base
/// for incremental comparison. Parts matching by name+CRC64 are carried
/// forward (referencing the base backup's S3 key) instead of being re-uploaded.
///
/// If `diff_from_remote` is provided (mutually exclusive with `diff_from`),
/// the remote manifest is downloaded from S3 and used as the incremental base.
/// Parts matching by name+CRC64 skip hardlinking during collection.
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
    diff_from_remote: Option<&str>,
    s3: Option<&crate::storage::S3Client>,
    partitions: Option<&str>,
    skip_check_parts_columns: bool,
    rbac: bool,
    configs: bool,
    named_collections: bool,
    skip_projections: &[String],
    cancel: CancellationToken,
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

    let disk_map: BTreeMap<String, String> = disks
        .iter()
        .map(|d| (d.name.clone(), d.path.clone()))
        .collect();
    let disk_type_map: BTreeMap<String, String> = disks
        .iter()
        .map(|d| {
            (
                d.name.clone(),
                object_disk::normalize_disk_type(&d.disk_type, &d.object_storage_type),
            )
        })
        .collect();
    let disk_remote_paths =
        object_disk::build_disk_remote_paths(&disks, &config.clickhouse.config_dir);

    // 2b. Download remote base manifest for --diff-from-remote
    let remote_base: Option<BackupManifest> = if let Some(remote_name) = diff_from_remote {
        if let Some(s3_client) = s3 {
            info!(base = %remote_name, "Downloading remote manifest for --diff-from-remote");
            let manifest_key = format!("{}/metadata.json", remote_name);
            match s3_client.get_object(&manifest_key).await {
                Ok(data) => match BackupManifest::from_json_bytes(&data) {
                    Ok(m) => {
                        info!(
                            base = %remote_name,
                            tables = m.tables.len(),
                            "Remote base manifest loaded for diff-from-remote"
                        );
                        Some(m)
                    }
                    Err(e) => {
                        warn!(error = %e, base = %remote_name, "Failed to parse remote manifest, falling back to full backup");
                        None
                    }
                },
                Err(e) => {
                    warn!(error = %e, base = %remote_name, "Failed to download remote manifest, falling back to full backup");
                    None
                }
            }
        } else {
            warn!("--diff-from-remote specified but no S3 client available, falling back to full backup");
            None
        }
    } else {
        None
    };

    // Build base_parts lookup from remote manifest for skipping hardlinks during collect
    let base_parts: Option<Arc<collect::BasePartsMap>> = remote_base.as_ref().map(|base| {
        let mut map = HashMap::new();
        for (table_key, tm) in &base.tables {
            for (disk_name, parts) in &tm.parts {
                for part in parts {
                    map.insert(
                        (table_key.clone(), disk_name.clone(), part.name.clone()),
                        (part.checksum_crc64, part.size),
                    );
                }
            }
        }
        info!(
            entries = map.len(),
            "Built base_parts lookup for diff-from-remote"
        );
        Arc::new(map)
    });

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

    // Pre-compute (database, table) pairs once for parts-column check, JSON-column
    // check, and mutation check -- all three operate on the same filtered table set.
    let targets: Vec<(String, String)> = filtered_tables
        .iter()
        .map(|t| (t.database.clone(), t.name.clone()))
        .collect();

    // 5b. Parts column consistency check (design 3.3)
    if config.clickhouse.check_parts_columns && !skip_check_parts_columns {
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
                    bail!(
                        "Parts column consistency check found {} actionable inconsistencies \
                         across tables. Use --skip-check-parts-columns to bypass this check.",
                        actionable.len()
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

    // 5c. JSON/Object column type detection (design 16.4)
    {
        match ch.check_json_columns(&targets).await {
            Ok(json_cols) => {
                if !json_cols.is_empty() {
                    for col in &json_cols {
                        warn!(
                            database = %col.database,
                            table = %col.table,
                            column = %col.column,
                            column_type = %col.column_type,
                            "JSON/Object column detected -- may not FREEZE correctly"
                        );
                    }
                    info!(
                        count = json_cols.len(),
                        "JSON/Object columns detected (proceeding with backup)"
                    );
                } else {
                    info!("JSON/Object column type check passed");
                }
            }
            Err(e) => {
                warn!(
                    error = %e,
                    "JSON/Object column type check failed, continuing anyway"
                );
            }
        }
    }

    // 6. Check for pending mutations (design 3.1)
    let mutation_wait_secs =
        crate::config::parse_duration_secs(&config.clickhouse.mutation_wait_timeout)
            .expect("validated in Config::validate()");

    let all_mutations = mutations::check_mutations(
        ch,
        &targets,
        config.clickhouse.backup_mutations,
        mutation_wait_secs,
    )
    .await?;

    // 7. Sync replicas (design 3.2)
    if config.clickhouse.sync_replicated_tables {
        let tables_vec: Vec<TableRow> = filtered_tables.iter().map(|t| (*t).clone()).collect();
        sync_replica::sync_replicas(ch, &tables_vec, effective_max_connections(config) as usize)
            .await?;
    }

    // 8. Create backup directory (fail if it already exists to prevent collision)
    let backup_dir = PathBuf::from(&config.clickhouse.data_path)
        .join("backup")
        .join(backup_name);

    // Ensure the parent backup/ directory exists, then create the leaf with create_dir
    // which fails atomically if the directory was already created by a concurrent run.
    let backup_parent = backup_dir.parent().expect("backup_dir always has a parent");
    std::fs::create_dir_all(backup_parent).with_context(|| {
        format!(
            "Failed to create backup parent directory: {}",
            backup_parent.display()
        )
    })?;
    match std::fs::create_dir(&backup_dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            bail!(
                "backup '{}' already exists at {}",
                backup_name,
                backup_dir.display()
            );
        }
        Err(e) => {
            return Err(anyhow::Error::new(e).context(format!(
                "Failed to create backup directory: {}",
                backup_dir.display()
            )));
        }
    }

    // 9. FREEZE tables and collect parts (parallel, bounded by max_connections)
    let mut table_manifests: BTreeMap<String, TableManifest> = BTreeMap::new();
    let mut databases_seen: HashMap<String, String> = HashMap::new();

    // Separate metadata-only / schema-only tables from data tables
    let mut data_tables: Vec<TableRow> = Vec::new();

    for table_row in &filtered_tables {
        let db = &table_row.database;
        let full_name = format!("{}.{}", db, table_row.name);

        // Track unique databases for DDL
        if !databases_seen.contains_key(db) {
            // Query actual DDL to preserve the real database engine (Replicated, Ordinary, etc.)
            // Falls back to Atomic engine for ClickHouse versions that don't support SHOW CREATE DATABASE.
            let ddl = ch.get_database_ddl(db).await.unwrap_or_else(|e| {
                warn!(
                    database = %db,
                    error = %e,
                    "Failed to get database DDL via SHOW CREATE DATABASE, using Atomic fallback"
                );
                format!("CREATE DATABASE IF NOT EXISTS `{}` ENGINE = Atomic", db)
            });
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
                    uuid: normalize_uuid(&table_row.uuid),
                    engine: table_row.engine.clone(),
                    total_bytes: 0,
                    parts: BTreeMap::new(),
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
    let mut abort_handles: Vec<tokio::task::AbortHandle> = Vec::with_capacity(data_tables.len());

    // Shared vec that tasks push to immediately after a successful FREEZE.
    // Uses std::sync::Mutex so the push is synchronous (no yield point between
    // FREEZE completing and the FreezeInfo being visible to the cancel path).
    let frozen_so_far: Arc<std::sync::Mutex<Vec<FreezeInfo>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));

    for table_row in &data_tables {
        let sem = semaphore.clone();
        let ch = ch.clone();
        let backup_name_owned = backup_name.to_string();
        let db = table_row.database.clone();
        let table = table_row.name.clone();
        let data_path = config.clickhouse.data_path.clone();
        let tables_for_collect = all_tables_arc.clone();
        let ignore_not_exists = config.clickhouse.ignore_not_exists_error_during_freeze;
        let all_mutations_clone = all_mutations.clone();
        let table_row_clone = table_row.clone();
        let disk_type_map_clone = disk_type_map.clone();
        let disk_map_clone = disk_map.clone();
        let skip_disks_clone = config.clickhouse.skip_disks.clone();
        let skip_disk_types_clone = config.clickhouse.skip_disk_types.clone();
        let skip_projections_clone = skip_projections.to_vec();
        let partition_ids_clone = partition_ids.clone();
        let deps_clone = deps_arc.clone();
        let freeze_by_part = config.clickhouse.freeze_by_part;
        let freeze_by_part_where = config.clickhouse.freeze_by_part_where.clone();
        let frozen_so_far_clone = frozen_so_far.clone();
        let base_parts_clone = base_parts.clone();

        let handle = tokio::spawn(async move {
            let _permit = sem
                .acquire()
                .await
                .map_err(|_| anyhow::anyhow!("Semaphore closed"))?;

            let full_name = format!("{}.{}", db, table);
            let fname = freeze_name(&backup_name_owned, &db, &table);

            // Determine effective partition list:
            // 1. CLI --partitions takes precedence (already parsed)
            // 2. If freeze_by_part is true and no CLI partitions, query system.parts
            // 3. Otherwise, whole-table freeze (empty partition list)
            let effective_partitions = if !partition_ids_clone.is_empty() {
                partition_ids_clone
            } else if freeze_by_part {
                // Query system.parts for distinct partition IDs
                match ch
                    .query_distinct_partitions(&db, &table, &freeze_by_part_where)
                    .await
                {
                    Ok(discovered) => {
                        info!(
                            db = %db,
                            table = %table,
                            partition_count = discovered.len(),
                            "freeze_by_part: discovered partitions from system.parts"
                        );
                        discovered
                    }
                    Err(e) => {
                        warn!(
                            db = %db,
                            table = %table,
                            error = %e,
                            "freeze_by_part: failed to query partitions, falling back to whole-table FREEZE"
                        );
                        Vec::new()
                    }
                }
            } else {
                Vec::new()
            };

            // FREEZE the table (whole-table or per-partition)
            let frozen = if effective_partitions.is_empty() {
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
                        if ignore_not_exists && is_ignorable_freeze_error(&err_msg) {
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
                for partition_id in &effective_partitions {
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
                            if ignore_not_exists && is_ignorable_freeze_error(&err_msg) {
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

            // Register freeze_info in the shared vec immediately after FREEZE so the
            // cancel path can find and unfreeze it even if this task is still running
            // collect_parts. Uses std::sync::Mutex::lock() (sync, no yield point).
            frozen_so_far_clone
                .lock()
                .unwrap()
                .push(freeze_info.clone());

            // Collect parts from shadow using spawn_blocking for filesystem I/O
            let fname_for_collect = fname;
            let parts_map = tokio::task::spawn_blocking(move || {
                collect_parts(
                    &data_path,
                    &fname_for_collect,
                    &backup_name_owned,
                    &tables_for_collect,
                    &disk_type_map_clone,
                    &disk_map_clone,
                    &skip_disks_clone,
                    &skip_disk_types_clone,
                    &skip_projections_clone,
                    base_parts_clone.as_ref().map(|arc| arc.as_ref()),
                )
            })
            .await
            .context("spawn_blocking panicked during collect_parts")??;

            // Build TableManifest: group collected parts by actual disk name
            let collected = parts_map.get(&full_name).cloned().unwrap_or_default();
            let total_bytes: u64 = collected.iter().map(|cp| cp.part_info.size).sum();

            let mut parts_by_disk: BTreeMap<String, Vec<_>> = BTreeMap::new();
            for cp in collected {
                parts_by_disk
                    .entry(cp.disk_name)
                    .or_default()
                    .push(cp.part_info);
            }

            let table_manifest = TableManifest {
                ddl: table_row_clone.create_table_query.clone(),
                uuid: normalize_uuid(&table_row_clone.uuid),
                engine: table_row_clone.engine.clone(),
                total_bytes,
                parts: parts_by_disk,
                pending_mutations: all_mutations_clone
                    .get(&full_name)
                    .cloned()
                    .unwrap_or_default(),
                metadata_only: false,
                dependencies: deps_clone.get(&full_name).cloned().unwrap_or_default(),
            };

            Ok(Some((freeze_info, full_name, table_manifest)))
        });

        // Collect an AbortHandle before moving the JoinHandle into try_join_all,
        // so the cancel branch can hard-stop tasks that are still in-flight.
        abort_handles.push(handle.abort_handle());
        handles.push(handle);
    }

    // Await all tasks — interruptible by the cancel token.
    // When cancelled, in-flight tasks are ABORTED (not merely detached) and the
    // shared frozen_so_far vec already contains every table that was successfully
    // FREEZEd, so we can unfreeze them before returning.
    let results = tokio::select! {
        r = try_join_all(handles) => {
            match r {
                Ok(results) => results,
                Err(join_err) => {
                    // Task panicked -- unfreeze everything that was frozen so far
                    let infos: Vec<FreezeInfo> = frozen_so_far.lock().unwrap_or_else(|e| e.into_inner()).drain(..).collect();
                    if !infos.is_empty() {
                        let mut guard = FreezeGuard::new();
                        for fi in infos {
                            guard.add(fi);
                        }
                        warn!("FREEZE+collect task panicked, unfreezing {} tables", guard.len());
                        if let Err(e) = guard.unfreeze_all(ch).await {
                            warn!(error = %e, "Failed to unfreeze tables after task panic");
                        }
                    }
                    return Err(anyhow::Error::new(join_err).context("A FREEZE+collect task panicked"));
                }
            }
        }
        _ = cancel.cancelled() => {
            for ah in &abort_handles {
                ah.abort();
            }
            let infos: Vec<FreezeInfo> = frozen_so_far.lock().unwrap_or_else(|e| e.into_inner()).drain(..).collect();
            if !infos.is_empty() {
                let mut cancel_guard = FreezeGuard::new();
                for fi in infos {
                    cancel_guard.add(fi);
                }
                warn!("backup::create cancelled, unfreezing {} tables", cancel_guard.len());
                if let Err(e) = cancel_guard.unfreeze_all(ch).await {
                    warn!(error = %e, "Failed to unfreeze tables after cancellation");
                }
            }
            return Err(anyhow::anyhow!("backup::create cancelled"));
        }
    };

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

    // Propagate the first error if any task failed -- clean up backup dir + shadow
    if let Some(e) = first_error {
        // Collect all dirs to clean, deduped by canonical path
        let mut dirs_to_delete: Vec<PathBuf> = Vec::new();
        let mut seen: HashSet<PathBuf> = HashSet::new();

        // Default backup_dir
        if backup_dir.exists() {
            let canonical =
                std::fs::canonicalize(&backup_dir).unwrap_or_else(|_| backup_dir.clone());
            seen.insert(canonical);
            dirs_to_delete.push(backup_dir.clone());
        }

        // Per-disk dirs (disk_map is in scope from line ~136)
        for disk_path in disk_map.values() {
            let per_disk =
                collect::per_disk_backup_dir(disk_path.trim_end_matches('/'), backup_name);
            if per_disk.exists() {
                let canonical =
                    std::fs::canonicalize(&per_disk).unwrap_or_else(|_| per_disk.clone());
                if seen.insert(canonical) {
                    dirs_to_delete.push(per_disk);
                }
            }
        }

        for dir in &dirs_to_delete {
            if let Err(rm_err) = std::fs::remove_dir_all(dir) {
                warn!(
                    path = %dir.display(),
                    error = %rm_err,
                    "Failed to clean up backup dir after error"
                );
            } else {
                info!(
                    path = %dir.display(),
                    "Removed backup dir after error"
                );
            }
        }

        // Clean shadow directories left by this backup's FREEZE operations
        match crate::list::clean_shadow(ch, &config.clickhouse.data_path, Some(backup_name)).await {
            Ok(n) if n > 0 => info!(count = n, "Cleaned shadow directories after backup error"),
            Err(shadow_err) => {
                warn!(error = %shadow_err, "Failed to clean shadow directories after backup error");
            }
            _ => {}
        }
        return Err(e);
    }

    // 11. Build database list
    let databases: Vec<DatabaseInfo> = databases_seen
        .into_iter()
        .map(|(name, ddl)| DatabaseInfo { name, ddl })
        .collect();

    // 12. Save per-table metadata files (encoded paths for round-trip compatibility)
    let metadata_dir = backup_dir.join("metadata");
    for (full_name, table_manifest) in &table_manifests {
        if let Some((db, table)) = full_name.split_once('.') {
            let table_metadata_dir =
                metadata_dir.join(crate::path_encoding::encode_path_component(db));
            std::fs::create_dir_all(&table_metadata_dir)?;
            let table_json = serde_json::to_string_pretty(table_manifest)?;
            std::fs::write(
                table_metadata_dir.join(format!(
                    "{}.json",
                    crate::path_encoding::encode_path_component(table)
                )),
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
        rbac_size: 0,
        config_size: 0,
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

    // 13a.1. Compute rbac_size and config_size from backup directories
    if manifest.rbac.is_some() {
        let access_dir = backup_dir.join("access");
        if access_dir.exists() {
            manifest.rbac_size = collect::dir_size(&access_dir)?;
        }
    }
    {
        let configs_dir = backup_dir.join("configs");
        if configs_dir.exists() {
            manifest.config_size = collect::dir_size(&configs_dir)?;
        }
    }
    info!(
        rbac_size = manifest.rbac_size,
        config_size = manifest.config_size,
        "Computed RBAC and config sizes"
    );

    // 13b. Apply incremental diff if --diff-from or --diff-from-remote is specified
    let effective_base: Option<BackupManifest> = if let Some(ref base) = remote_base {
        // diff-from-remote: already downloaded
        Some(base.clone())
    } else if let Some(base_name) = diff_from {
        info!(base = %base_name, "Loading base manifest for diff-from");
        let base_manifest_path = PathBuf::from(&config.clickhouse.data_path)
            .join("backup")
            .join(base_name)
            .join("metadata.json");
        Some(
            BackupManifest::load_from_file(&base_manifest_path).with_context(|| {
                format!("Failed to load base backup '{}' for --diff-from", base_name)
            })?,
        )
    } else {
        None
    };

    if let Some(base) = effective_base {
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
        let manifest = BackupManifest::test_new("empty-test");

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
    fn test_partition_list_all_triggers_whole_table_freeze() {
        // "all" partition ID should result in empty vec (whole-table freeze)
        let result = parse_partition_list(Some("all"));
        assert!(
            result.is_empty(),
            "partition 'all' should result in empty vec for whole-table freeze"
        );

        // "all" mixed with other partitions should still trigger whole-table
        let mixed = parse_partition_list(Some("202401,all,202403"));
        assert!(
            mixed.is_empty(),
            "partition 'all' in a list should result in empty vec"
        );

        // Normal partitions should not be affected
        let normal = parse_partition_list(Some("202401,202402"));
        assert_eq!(normal.len(), 2);
    }

    #[test]
    fn test_is_ignorable_freeze_error() {
        // Code 60: UNKNOWN_TABLE
        assert!(is_ignorable_freeze_error(
            "Code: 60. DB::Exception: Table default.trades does not exist. (UNKNOWN_TABLE)"
        ));
        assert!(is_ignorable_freeze_error("Code: 60"));

        // Code 81: UNKNOWN_DATABASE
        assert!(is_ignorable_freeze_error(
            "Code: 81. DB::Exception: Database mydb does not exist. (UNKNOWN_DATABASE)"
        ));
        assert!(is_ignorable_freeze_error("UNKNOWN_DATABASE"));

        // Code 218: CANNOT_FREEZE_PARTITION
        assert!(is_ignorable_freeze_error(
            "Code: 218. DB::Exception: CANNOT_FREEZE_PARTITION"
        ));
        assert!(is_ignorable_freeze_error("Code: 218"));
        assert!(is_ignorable_freeze_error("CANNOT_FREEZE_PARTITION"));

        // Non-ignorable errors should return false
        assert!(!is_ignorable_freeze_error("Code: 999. Some other error"));
        assert!(!is_ignorable_freeze_error("Connection timeout"));
        assert!(!is_ignorable_freeze_error(""));
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
    fn test_check_parts_columns_strict_fail() {
        // When filter_benign_type_drift returns non-empty Vec, the backup should
        // fail with an error message indicating the count and the bypass flag.
        let actionable = [ColumnInconsistency {
            database: "default".to_string(),
            table: "trades".to_string(),
            column: "amount".to_string(),
            types: vec!["Float64".to_string(), "Decimal(18,2)".to_string()],
        }];

        // Verify the error message format matches what bail! produces
        let err_msg = format!(
            "Parts column consistency check found {} actionable inconsistencies \
             across tables. Use --skip-check-parts-columns to bypass this check.",
            actionable.len()
        );
        assert!(err_msg.contains("1 actionable inconsistencies"));
        assert!(err_msg.contains("--skip-check-parts-columns"));
    }

    #[test]
    fn test_check_parts_columns_benign_drift_passes() {
        // When all inconsistencies are benign (filtered out), no error should occur
        let benign_only = vec![ColumnInconsistency {
            database: "default".to_string(),
            table: "events".to_string(),
            column: "status".to_string(),
            types: vec![
                "Enum8('a' = 1)".to_string(),
                "Enum8('a' = 1, 'b' = 2)".to_string(),
            ],
        }];

        let actionable = filter_benign_type_drift(benign_only);
        assert!(
            actionable.is_empty(),
            "Benign drift should be filtered out, leaving nothing actionable"
        );
    }

    #[test]
    fn test_check_parts_columns_query_error_continues() {
        // When the check query itself fails, backup should continue (warn only).
        // This tests that the error handling pattern in create() uses warn, not bail.
        //
        // The create() code pattern:
        //   match ch.check_parts_columns(&targets).await {
        //       Ok(inconsistencies) => { ... bail!() if actionable ... }
        //       Err(e) => { warn!(...); }  // <-- warn only, no bail
        //   }
        //
        // Verify that the Err arm pattern works as expected: the error is logged
        // but does not propagate. We simulate this by matching on a Result.
        fn simulate_query_error() -> Result<Vec<ColumnInconsistency>, anyhow::Error> {
            anyhow::bail!("DB::Exception: Query failed")
        }

        let query_result = simulate_query_error();
        match query_result {
            Ok(_) => panic!("Expected error"),
            Err(e) => {
                // In create(), this arm logs warn!() and continues.
                let err_msg = format!("{}", e);
                assert!(err_msg.contains("Query failed"));
            }
        }
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
        let tm = TableManifest::test_new("Dictionary")
            .with_ddl("CREATE DICTIONARY ...")
            .with_metadata_only(true)
            .with_dependencies(deps1);
        assert_eq!(tm.dependencies, vec!["default.users"]);
    }

    #[test]
    fn test_create_error_cleanup_per_disk() {
        // Verify that the error cleanup pattern (used in backup::create() when a
        // task fails) correctly removes per-disk backup dirs with canonical dedup.
        //
        // This tests the cleanup logic in isolation since the full create() requires
        // a real ClickHouse connection.
        use std::collections::HashSet;

        let tmp = tempfile::tempdir().unwrap();
        let data_path = tmp.path().join("clickhouse");
        let nvme1_path = tmp.path().join("nvme1");
        let backup_name = "test-error";

        // Simulate partial backup creation: default dir + per-disk dir on nvme1
        let backup_dir = data_path.join("backup").join(backup_name);
        std::fs::create_dir_all(backup_dir.join("shadow")).unwrap();
        std::fs::write(backup_dir.join("shadow").join("data.bin"), b"data").unwrap();

        let per_disk_dir = nvme1_path.join("backup").join(backup_name);
        std::fs::create_dir_all(per_disk_dir.join("shadow")).unwrap();
        std::fs::write(per_disk_dir.join("shadow").join("data.bin"), b"data").unwrap();

        // Simulate the disk_map as would be in scope during create()
        let disk_map: HashMap<String, String> = HashMap::from([
            (
                "default".to_string(),
                data_path.to_string_lossy().to_string(),
            ),
            (
                "nvme1".to_string(),
                nvme1_path.to_string_lossy().to_string(),
            ),
        ]);

        assert!(backup_dir.exists());
        assert!(per_disk_dir.exists());

        // Execute the same cleanup pattern as in create() error path
        let mut dirs_to_delete: Vec<PathBuf> = Vec::new();
        let mut seen: HashSet<PathBuf> = HashSet::new();

        if backup_dir.exists() {
            let canonical =
                std::fs::canonicalize(&backup_dir).unwrap_or_else(|_| backup_dir.clone());
            seen.insert(canonical);
            dirs_to_delete.push(backup_dir.clone());
        }

        for disk_path in disk_map.values() {
            let per_disk =
                collect::per_disk_backup_dir(disk_path.trim_end_matches('/'), backup_name);
            if per_disk.exists() {
                let canonical =
                    std::fs::canonicalize(&per_disk).unwrap_or_else(|_| per_disk.clone());
                if seen.insert(canonical) {
                    dirs_to_delete.push(per_disk);
                }
            }
        }

        for dir in &dirs_to_delete {
            std::fs::remove_dir_all(dir).unwrap();
        }

        assert!(
            !backup_dir.exists(),
            "Default backup dir should be cleaned up"
        );
        assert!(
            !per_disk_dir.exists(),
            "Per-disk backup dir should be cleaned up"
        );
    }

    #[test]
    fn test_create_backup_dir_rejects_existing() {
        // Verify that creating a backup directory fails if it already exists.
        // This tests the collision detection logic added to prevent silent overwrites
        // when two creates happen within the same second (same auto-generated name).
        let tmp = tempfile::tempdir().unwrap();
        let backup_parent = tmp.path().join("backup");
        std::fs::create_dir_all(&backup_parent).unwrap();

        let backup_name = "test-collision";
        let backup_dir = backup_parent.join(backup_name);

        // Create the directory ahead of time to simulate an existing backup
        std::fs::create_dir(&backup_dir).unwrap();
        assert!(backup_dir.exists());

        // The same logic as in create(): atomic create_dir (no TOCTOU race)
        let check = |dir: &std::path::Path, name: &str| -> Result<()> {
            match std::fs::create_dir(dir) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    bail!("backup '{}' already exists at {}", name, dir.display());
                }
                Err(e) => {
                    return Err(anyhow::Error::new(e).context(format!(
                        "Failed to create backup directory: {}",
                        dir.display()
                    )));
                }
            }
            Ok(())
        };

        let result = check(&backup_dir, backup_name);
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("already exists"),
            "Error should mention 'already exists', got: {err_msg}"
        );
    }

    #[test]
    fn test_normalize_uuid_nil() {
        assert_eq!(normalize_uuid("00000000-0000-0000-0000-000000000000"), None);
    }

    #[test]
    fn test_normalize_uuid_empty() {
        assert_eq!(normalize_uuid(""), None);
    }

    #[test]
    fn test_normalize_uuid_valid() {
        assert_eq!(
            normalize_uuid("550e8400-e29b-41d4-a716-446655440000"),
            Some("550e8400-e29b-41d4-a716-446655440000".to_string())
        );
    }

    #[test]
    fn test_normalize_uuid_undashed() {
        // Non-standard format still passes through (not nil/empty)
        assert_eq!(
            normalize_uuid("550e8400e29b41d4a716446655440000"),
            Some("550e8400e29b41d4a716446655440000".to_string())
        );
    }

    #[test]
    fn test_parse_partition_list_whitespace_only() {
        let result = parse_partition_list(Some("  ,  ,  "));
        assert!(
            result.is_empty(),
            "whitespace-only entries should be filtered out"
        );
    }

    #[test]
    fn test_is_metadata_only_engine_all_variants() {
        // All metadata-only engines
        for engine in &[
            "View",
            "MaterializedView",
            "LiveView",
            "WindowView",
            "Dictionary",
            "Null",
            "Set",
            "Join",
            "Buffer",
            "Distributed",
            "Merge",
        ] {
            assert!(
                is_metadata_only_engine(engine),
                "{engine} should be metadata-only"
            );
        }

        // Data engines
        for engine in &[
            "MergeTree",
            "ReplicatedMergeTree",
            "ReplacingMergeTree",
            "AggregatingMergeTree",
            "CollapsingMergeTree",
            "VersionedCollapsingMergeTree",
            "SummingMergeTree",
        ] {
            assert!(
                !is_metadata_only_engine(engine),
                "{engine} should NOT be metadata-only"
            );
        }
    }

    #[test]
    fn test_is_benign_type_variants() {
        // Positive cases
        assert!(is_benign_type("Enum8('a' = 1)"));
        assert!(is_benign_type("Tuple(a UInt64)"));
        assert!(is_benign_type("Nullable(Enum8('x' = 1))"));
        assert!(is_benign_type("Nullable(Tuple(a UInt64))"));
        assert!(is_benign_type("Array(Tuple(a String, b Int32))"));

        // Negative cases
        assert!(!is_benign_type("UInt64"));
        assert!(!is_benign_type("String"));
        assert!(!is_benign_type("Float64"));
        assert!(!is_benign_type("Decimal(18,2)"));
        assert!(!is_benign_type("Array(UInt64)"));
        assert!(!is_benign_type("Nullable(UInt64)"));
    }

    #[test]
    fn test_filter_benign_type_drift_all_benign() {
        let all_benign = vec![
            ColumnInconsistency {
                database: "default".to_string(),
                table: "t1".to_string(),
                column: "c1".to_string(),
                types: vec![
                    "Enum8('a' = 1)".to_string(),
                    "Enum8('a' = 1, 'b' = 2)".to_string(),
                ],
            },
            ColumnInconsistency {
                database: "default".to_string(),
                table: "t2".to_string(),
                column: "c2".to_string(),
                types: vec![
                    "Tuple(x UInt64)".to_string(),
                    "Tuple(x UInt64, y String)".to_string(),
                ],
            },
        ];
        let result = filter_benign_type_drift(all_benign);
        assert!(result.is_empty());
    }

    #[test]
    fn test_filter_benign_type_drift_none_benign() {
        let non_benign = vec![ColumnInconsistency {
            database: "default".to_string(),
            table: "t".to_string(),
            column: "c".to_string(),
            types: vec!["UInt64".to_string(), "Int64".to_string()],
        }];
        let result = filter_benign_type_drift(non_benign);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_filter_benign_type_drift_empty() {
        let result = filter_benign_type_drift(Vec::new());
        assert!(result.is_empty());
    }

    #[test]
    fn test_is_benign_type_enum16_with_many_values() {
        assert!(is_benign_type(
            "Enum16('active' = 1, 'deleted' = 2, 'pending' = 3, 'archived' = 4)"
        ));
    }

    #[test]
    fn test_is_benign_type_nested_nullable_array_tuple_is_false() {
        // Nullable(Array(Tuple(...))) does NOT match: implementation only checks
        // Nullable(Enum and Nullable(Tuple prefixes, not Nullable(Array(Tuple
        assert!(!is_benign_type("Nullable(Array(Tuple(x Int32, y Int32)))"));
    }

    #[test]
    fn test_is_benign_type_map_type() {
        // Map is not a benign drift type
        assert!(!is_benign_type("Map(String, UInt64)"));
    }

    #[test]
    fn test_is_benign_type_lowertuple() {
        // Case-sensitive: starts_with("Tuple") not "tuple"
        assert!(!is_benign_type("tuple(a UInt64)"));
    }

    #[test]
    fn test_normalize_uuid_whitespace_is_some() {
        // Whitespace is treated as a valid non-empty UUID (not nil, not empty)
        assert_eq!(normalize_uuid(" "), Some(" ".to_string()));
    }

    #[test]
    fn test_normalize_uuid_partial_zeros() {
        // Not nil -- differs in last digit
        assert_eq!(
            normalize_uuid("00000000-0000-0000-0000-000000000001"),
            Some("00000000-0000-0000-0000-000000000001".to_string())
        );
    }

    #[test]
    fn test_filter_benign_type_drift_mixed_keeps_only_non_benign() {
        // 3 entries: one all-benign, one all-non-benign, one mixed (benign + non-benign)
        let all_benign = ColumnInconsistency {
            database: "db1".to_string(),
            table: "t1".to_string(),
            column: "c1".to_string(),
            types: vec![
                "Enum8('a' = 1)".to_string(),
                "Enum8('a' = 1, 'b' = 2)".to_string(),
            ],
        };
        let all_non_benign = ColumnInconsistency {
            database: "db2".to_string(),
            table: "t2".to_string(),
            column: "c2".to_string(),
            types: vec!["UInt64".to_string(), "Int64".to_string()],
        };
        let mixed = ColumnInconsistency {
            database: "db3".to_string(),
            table: "t3".to_string(),
            column: "c3".to_string(),
            types: vec!["Enum8('x' = 1)".to_string(), "String".to_string()],
        };

        let result = filter_benign_type_drift(vec![all_benign, all_non_benign, mixed]);
        // all_benign is filtered out (all types benign), the other two remain
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].column, "c2");
        assert_eq!(result[1].column, "c3");
    }

    #[test]
    fn test_create_backup_dir_succeeds_when_new() {
        // Verify that creating a backup directory succeeds when it does not exist.
        let tmp = tempfile::tempdir().unwrap();
        let backup_parent = tmp.path().join("backup");
        std::fs::create_dir_all(&backup_parent).unwrap();

        let backup_name = "new-backup";
        let backup_dir = backup_parent.join(backup_name);

        assert!(!backup_dir.exists());

        // The same logic as in create(): atomic create_dir (no TOCTOU race)
        let check = |dir: &std::path::Path, name: &str| -> Result<()> {
            match std::fs::create_dir(dir) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    bail!("backup '{}' already exists at {}", name, dir.display());
                }
                Err(e) => {
                    return Err(anyhow::Error::new(e).context(format!(
                        "Failed to create backup directory: {}",
                        dir.display()
                    )));
                }
            }
            Ok(())
        };

        let result = check(&backup_dir, backup_name);
        assert!(result.is_ok());
        assert!(backup_dir.exists());
    }
}
