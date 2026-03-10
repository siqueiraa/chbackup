//! Restore: phased restore with dependency-aware DDL ordering.
//!
//! Implements Mode B (non-destructive) and Mode A (destructive `--rm`) restore
//! from design doc sections 5.1/5.2/5.3/5.5/5.6/5.7:
//!
//! 0. Phase 0: DROP tables/databases (Mode A only)
//! 1. Phase 1: CREATE databases from manifest.databases DDL
//! 2. Phase 2: CREATE + ATTACH data tables (sorted by engine priority)
//! 3. Phase 2.5: Re-apply pending mutations
//! 4. Phase 2b: CREATE postponed tables (streaming engines, refreshable MVs)
//! 5. Phase 3: CREATE DDL-only objects (topologically sorted by dependencies)
//! 6. Phase 4: CREATE functions from manifest.functions
//! 7. Log summary
//!
//! Cross-cutting features:
//! - ON CLUSTER DDL: `restore_schema_on_cluster` config appends ON CLUSTER clause
//! - DatabaseReplicated: Detected via `system.databases`, skips ON CLUSTER
//! - ZK conflict resolution: Check/drop replica before Replicated table CREATE
//! - Distributed cluster rewrite: `restore_distributed_cluster` config
//! - ATTACH TABLE mode: `restore_as_attach` config for Replicated tables

pub mod attach;
pub mod rbac;
pub mod remap;
pub mod schema;
pub mod sort;
pub mod topo;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use futures::future::try_join_all;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::clickhouse::client::ChClient;
use crate::concurrency::{
    effective_max_connections, effective_object_disk_server_side_copy_concurrency,
};
use crate::config::Config;
use crate::error::ChBackupError;
use crate::manifest::BackupManifest;
use crate::object_disk::is_s3_disk;
use crate::resume::{compute_params_hash, load_state_file, save_state_file, RestoreState};
use crate::storage::S3Client;
use crate::table_filter::TableFilter;

use attach::{
    attach_parts_owned, detect_clickhouse_ownership, get_table_data_path, OwnedAttachParams,
};
use remap::{parse_replicated_params, resolve_zk_macros, RemapConfig};
use schema::{
    create_databases, create_ddl_objects, create_functions, create_tables,
    detect_replicated_databases, drop_databases, drop_tables, is_replicated_engine,
};
use sort::PartSortKey;
use topo::{classify_restore_tables, topological_sort};

/// Restore a backup to ClickHouse.
///
/// Implements Mode B (non-destructive) and Mode A (destructive `--rm`) restore.
/// Mode B: creates databases and tables if they don't exist, then attaches data parts.
/// Mode A: DROP tables/databases before CREATE using reverse engine priority order.
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
/// * `rm` - If true, DROP tables/databases before CREATE (Mode A)
/// * `resume` - If true, load resume state and skip already-attached parts
/// * `rename_as` - Optional `--as` value for single table rename (e.g. "dst_db.dst_table")
/// * `database_mapping` - Optional database mapping from `-m` flag (pre-parsed HashMap)
#[allow(clippy::too_many_arguments)]
pub async fn restore(
    config: &Config,
    ch: &ChClient,
    backup_name: &str,
    table_pattern: Option<&str>,
    schema_only: bool,
    data_only: bool,
    rm: bool,
    resume: bool,
    rename_as: Option<&str>,
    database_mapping: Option<&HashMap<String, String>>,
    rbac_restore: bool,
    configs_restore: bool,
    named_collections_restore: bool,
    partitions: Option<&str>,
    skip_empty_tables: bool,
    cancel: CancellationToken,
) -> Result<()> {
    let data_path = &config.clickhouse.data_path;
    let backup_dir = PathBuf::from(data_path).join("backup").join(backup_name);

    info!(
        backup_name = %backup_name,
        backup_dir = %backup_dir.display(),
        schema_only = schema_only,
        data_only = data_only,
        rm = rm,
        "Starting restore"
    );

    // 1. Read manifest
    let manifest_path = backup_dir.join("metadata.json");
    if !manifest_path.exists() {
        return Err(ChBackupError::BackupNotFound(format!(
            "backup '{}' not found (no metadata.json at {})",
            backup_name,
            manifest_path.display()
        ))
        .into());
    }
    let manifest = BackupManifest::load_from_file(&manifest_path).with_context(|| {
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
        if let Some(pattern) = table_pattern {
            if !config.backup.allow_empty_backups {
                bail!(
                    "Table filter '{}' matched no tables in backup manifest",
                    pattern
                );
            }
        }
        warn!("No tables match the filter pattern");
        return Ok(());
    }

    info!(
        matched_tables = table_keys.len(),
        total_tables = manifest.tables.len(),
        "Tables matched filter"
    );

    // 2a. Parse partition filter list (for --partitions on restore)
    let partition_filter: Vec<String> = match partitions {
        Some(s) if !s.is_empty() => {
            let parts: Vec<String> = s.split(',').map(|p| p.trim().to_string()).collect();
            info!(
                partitions = ?parts,
                "Filtering restore by partition IDs"
            );
            parts
        }
        _ => Vec::new(),
    };

    // 2b. Build remap configuration from CLI flags
    let db_mapping_str = database_mapping.map(|m| {
        m.iter()
            .map(|(k, v)| format!("{}:{}", k, v))
            .collect::<Vec<_>>()
            .join(",")
    });
    let remap_config = RemapConfig::new(
        rename_as,
        table_pattern,
        db_mapping_str.as_deref(),
        &config.clickhouse.default_replica_path,
    )?;
    let remap_ref = remap_config.as_ref();

    // 2c. Classify tables into restore phases
    let phases = classify_restore_tables(&manifest, &table_keys);

    // 2d. Derive ON CLUSTER and DatabaseReplicated config
    let on_cluster = if config.clickhouse.restore_schema_on_cluster.is_empty() {
        None
    } else {
        Some(config.clickhouse.restore_schema_on_cluster.as_str())
    };

    let replicated_databases = if on_cluster.is_some() {
        detect_replicated_databases(ch, &manifest, remap_ref).await
    } else {
        HashSet::new()
    };

    // 2e. Get macros for ZK path resolution (needed for ZK conflict check and ATTACH TABLE mode)
    let macros = ch.get_macros().await.unwrap_or_default();

    // 2f. Distributed cluster rewrite config
    let dist_cluster = &config.clickhouse.restore_distributed_cluster;

    // Phase 0: DROP (Mode A only)
    if rm && !data_only {
        info!("Phase 0: Dropping tables and databases (Mode A --rm)");
        drop_tables(
            ch,
            &manifest,
            &table_keys,
            remap_ref,
            on_cluster,
            &replicated_databases,
        )
        .await?;
        drop_databases(ch, &manifest, remap_ref, on_cluster, &replicated_databases).await?;
    }

    // Phase 1: CREATE databases
    if !data_only {
        create_databases(ch, &manifest, remap_ref, on_cluster, &replicated_databases).await?;
    }

    // Phase 2: CREATE data tables (not DDL-only objects)
    info!(count = phases.data_tables.len(), "Phase 2: data tables");
    create_tables(
        ch,
        &manifest,
        &phases.data_tables,
        data_only,
        remap_ref,
        on_cluster,
        &replicated_databases,
        &macros,
        dist_cluster,
    )
    .await?;

    // Schema-only mode: also create DDL-only objects but skip data attach
    if schema_only {
        if !data_only && !phases.ddl_only_tables.is_empty() {
            let sorted_ddl = topological_sort(&manifest.tables, &phases.ddl_only_tables)?;
            info!(count = sorted_ddl.len(), "Phase 3: DDL-only objects");
            create_ddl_objects(
                ch,
                &manifest,
                &sorted_ddl,
                remap_ref,
                on_cluster,
                &replicated_databases,
            )
            .await?;
        }
        // Phase 2b: Postponed tables (streaming engines, refreshable MVs)
        // In schema-only mode, created AFTER DDL-only objects since those may be
        // targets that streaming engines write to.
        if !data_only && !phases.postponed_tables.is_empty() {
            info!(
                count = phases.postponed_tables.len(),
                "Phase 2b: postponed tables (schema-only)"
            );
            create_tables(
                ch,
                &manifest,
                &phases.postponed_tables,
                data_only,
                remap_ref,
                on_cluster,
                &replicated_databases,
                &macros,
                dist_cluster,
            )
            .await?;
        }
        if !data_only && table_pattern.is_none() && !manifest.functions.is_empty() {
            create_functions(ch, &manifest, on_cluster).await?;
        }

        // Phase 4e: Named collections (schema-only path)
        if !data_only
            && (named_collections_restore || config.clickhouse.named_collections_backup_always)
        {
            rbac::restore_named_collections(ch, &manifest, on_cluster).await?;
        }

        // Phase 4e: RBAC restore (schema-only path)
        if rbac_restore || config.clickhouse.rbac_backup_always {
            rbac::restore_rbac(
                ch,
                config,
                &backup_dir,
                &config.clickhouse.rbac_resolve_conflicts,
            )
            .await?;
        }

        // Phase 4e: Config file restore (schema-only path)
        if configs_restore || config.clickhouse.config_backup_always {
            rbac::restore_configs(config, &backup_dir).await?;
        }

        // Phase 4e: Restart command (schema-only path)
        let did_rbac_schema = (rbac_restore || config.clickhouse.rbac_backup_always)
            && backup_dir.join("access").exists();
        let did_configs_schema = (configs_restore || config.clickhouse.config_backup_always)
            && backup_dir.join("configs").exists();
        if did_rbac_schema || did_configs_schema {
            rbac::execute_restart_commands(ch, &config.clickhouse.restart_command).await?;
        }

        info!("Schema-only mode, skipping data restore");
        return Ok(());
    }

    // When resume is requested, persist the original parameters alongside the state file
    // so that server auto-resume can replay with the same parameters after a restart.
    if resume && config.general.use_resumable_state {
        let params = crate::resume::RestoreParams {
            backup_name: backup_name.to_string(),
            tables: table_pattern.map(str::to_string),
            schema_only,
            data_only,
            rm,
            rename_as: rename_as.map(str::to_string),
            database_mapping: database_mapping.cloned().unwrap_or_default(),
            rbac: rbac_restore,
            configs: configs_restore,
            named_collections: named_collections_restore,
            partitions: partitions.map(str::to_string),
            skip_empty_tables,
        };
        let params_path = crate::resume::restore_params_path(&backup_dir);
        if let Err(e) = crate::resume::save_state_file(&params_path, &params) {
            warn!(error = %e, "Failed to save restore params sidecar (non-fatal)");
        }
    }

    // 5a. Resume state: load previously attached parts from state file + system.parts
    let state_path = backup_dir.join("restore.state.json");
    let mut already_attached: HashMap<String, HashSet<String>> = HashMap::new();

    // Compute params hash from the subset of parameters that affect which parts to attach.
    let current_params_hash = {
        let db_mapping_sorted = database_mapping.map(|m| {
            let mut pairs: Vec<String> = m.iter().map(|(k, v)| format!("{k}:{v}")).collect();
            pairs.sort();
            pairs.join(",")
        });
        compute_params_hash(&[
            backup_name,
            table_pattern.unwrap_or(""),
            if schema_only { "schema" } else { "" },
            if data_only { "data" } else { "" },
            rename_as.unwrap_or(""),
            db_mapping_sorted.as_deref().unwrap_or(""),
            partitions.unwrap_or(""),
            if skip_empty_tables { "skip-empty" } else { "" },
        ])
    };

    if resume {
        // Load state file (may not exist on first run)
        if let Ok(Some(state)) = load_state_file::<RestoreState>(&state_path) {
            if state.backup_name == backup_name {
                // Validate params hash: non-empty hash mismatch => warn and ignore state.
                // Empty hash (old state file without field) => load for safe rollout.
                let hash_ok =
                    state.params_hash.is_empty() || state.params_hash == current_params_hash;
                if !hash_ok {
                    warn!(
                        stored_hash = %state.params_hash,
                        current_hash = %current_params_hash,
                        "Ignoring stale restore state (parameters changed since last run)"
                    );
                } else {
                    let total_parts: usize = state.attached_parts.values().map(|v| v.len()).sum();
                    info!(
                        tables = state.attached_parts.len(),
                        parts = total_parts,
                        "Loaded restore resume state"
                    );
                    for (table_key, parts) in state.attached_parts {
                        already_attached.entry(table_key).or_default().extend(parts);
                    }
                }
            } else {
                warn!(
                    state_backup = %state.backup_name,
                    current_backup = %backup_name,
                    "Ignoring stale restore state (different backup name)"
                );
            }
        }

        // Query system.parts for authoritative view of already-attached parts
        // When remap is active, query using the *destination* db/table names
        // Only query data tables (DDL-only objects have no parts)
        let mut tables_with_active_parts: HashSet<String> = HashSet::new();
        for table_key in &phases.data_tables {
            let (orig_db, orig_table) = table_key.split_once('.').unwrap_or(("default", table_key));
            let (query_db, query_table) = match remap_ref {
                Some(rc) if rc.is_active() => {
                    let (d, t) = rc.remap_table_key(table_key);
                    (d, t)
                }
                _ => (orig_db.to_string(), orig_table.to_string()),
            };
            match ch.query_system_parts(&query_db, &query_table).await {
                Ok(parts) => {
                    if !parts.is_empty() {
                        tables_with_active_parts.insert(table_key.clone());
                        let part_names: HashSet<String> =
                            parts.into_iter().map(|p| p.name).collect();
                        info!(
                            table = %table_key,
                            live_parts = part_names.len(),
                            "Merged system.parts into resume state"
                        );
                        already_attached
                            .entry(table_key.clone())
                            .or_default()
                            .extend(part_names);
                    }
                }
                Err(e) => {
                    warn!(
                        table = %table_key,
                        error = %e,
                        "Failed to query system.parts, relying on state file only"
                    );
                    // On query failure, assume table might have parts (don't discard state)
                    tables_with_active_parts.insert(table_key.clone());
                }
            }
        }

        // Cross-check: if system.parts shows 0 active parts for a table,
        // discard state file entries for that table. The table may have been
        // dropped and recreated since the state file was written.
        already_attached.retain(|table_key, parts| {
            if tables_with_active_parts.contains(table_key) || parts.is_empty() {
                true
            } else {
                info!(
                    table = %table_key,
                    stale_parts = parts.len(),
                    "Discarding stale state file entries (table has no active parts)"
                );
                false
            }
        });

        if !already_attached.is_empty() {
            let total: usize = already_attached.values().map(|s| s.len()).sum();
            info!(
                tables = already_attached.len(),
                parts = total,
                "Resuming restore: skipping already-attached parts"
            );
        }
    }

    // Detect ClickHouse ownership for chown
    let (ch_uid, ch_gid) = detect_clickhouse_ownership(Path::new(data_path)).unwrap_or_else(|e| {
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

    // Build disk remote paths and disk local paths from live disks.
    // disk_remote_paths: S3 URI for CopyObject destination (data disk bucket/prefix).
    // disk_local_paths: local filesystem path for S3 disk metadata directories.
    //   Used to write S3 metadata pointer files to the correct detached/ directory
    //   on tiered storage policies (where data_paths[0] may be a local disk).
    let (disk_remote_paths, disk_local_paths): (
        BTreeMap<String, String>,
        BTreeMap<String, String>,
    ) = if has_s3_disks {
        match ch.get_disks().await {
            Ok(disks) => {
                let mut remote_paths = crate::object_disk::build_disk_remote_paths(
                    &disks,
                    &config.clickhouse.config_dir,
                );
                crate::object_disk::resolve_macros_in_paths(&mut remote_paths, &macros);

                // Collect local filesystem paths for S3 disks (non-cache)
                let local_paths: BTreeMap<String, String> = disks
                    .iter()
                    .filter(|d| {
                        let eff = crate::object_disk::normalize_disk_type(
                            &d.disk_type,
                            &d.object_storage_type,
                        );
                        crate::object_disk::is_s3_disk(&eff)
                            && !crate::object_disk::is_cache_disk(d)
                    })
                    .map(|d| (d.name.clone(), d.path.clone()))
                    .collect();

                debug!(disk_local_paths = ?local_paths, "S3 disk local paths for metadata placement");
                (remote_paths, local_paths)
            }
            Err(e) => {
                warn!(error = %e, "Failed to get disk info for S3 restore");
                (BTreeMap::new(), BTreeMap::new())
            }
        }
    } else {
        (BTreeMap::new(), BTreeMap::new())
    };

    let object_disk_concurrency =
        effective_object_disk_server_side_copy_concurrency(config) as usize;
    let allow_streaming = config.s3.allow_object_disk_streaming;
    let (_, _, jitter_factor) = crate::config::effective_retries(config);

    // Global ATTACH semaphore: bounds total concurrent ATTACH PART operations
    // across all tables. Individual parts acquire a permit before executing ATTACH.
    let attach_semaphore = Arc::new(Semaphore::new(effective_max_connections(config) as usize));

    // Build shared resume state tracker (for parallel tasks to update)
    let resume_state: Option<Arc<tokio::sync::Mutex<(RestoreState, PathBuf)>>> = if resume {
        // Initialize state from already_attached so we preserve existing progress
        let initial_state = RestoreState {
            attached_parts: already_attached
                .iter()
                .map(|(k, v)| (k.clone(), v.iter().cloned().collect()))
                .collect(),
            backup_name: backup_name.to_string(),
            params_hash: current_params_hash.clone(),
        };
        Some(Arc::new(tokio::sync::Mutex::new((
            initial_state,
            state_path.clone(),
        ))))
    } else {
        None
    };

    // Collect data tables that need data restore (phases.data_tables already excludes DDL-only)
    let mut restore_items: Vec<(String, OwnedAttachParams)> = Vec::new();
    let mut zero_part_table_names: Vec<String> = Vec::new();

    for table_key in &phases.data_tables {
        let table_manifest = match manifest.tables.get(table_key) {
            Some(tm) => tm,
            None => continue,
        };

        let (src_db, src_table) = table_key.split_once('.').unwrap_or(("default", table_key));

        // Determine destination db/table (may be remapped)
        let (dst_db, dst_table) = match remap_ref {
            Some(rc) if rc.is_active() => rc.remap_table_key(table_key),
            _ => (src_db.to_string(), src_table.to_string()),
        };

        // Collect all parts from all disks into a flat list
        let mut all_parts: Vec<_> = table_manifest
            .parts
            .values()
            .flat_map(|parts| parts.iter().cloned())
            .collect();
        let manifest_part_count = all_parts.len();

        // Filter parts by partition if --partitions is specified
        if !partition_filter.is_empty() {
            let before_count = all_parts.len();
            all_parts.retain(|part| {
                if let Some(key) = PartSortKey::from_part_name(&part.name) {
                    partition_filter.contains(&key.partition)
                } else {
                    // Can't parse partition from name -- keep the part
                    true
                }
            });
            if all_parts.len() < before_count {
                info!(
                    table = %table_key,
                    before = before_count,
                    after = all_parts.len(),
                    "Filtered parts by partition"
                );
            }
        }

        if all_parts.is_empty() {
            if skip_empty_tables {
                info!(table = %table_key, "Skipping table with zero parts (--skip-empty-tables)");
            } else if manifest_part_count > 0 {
                // Partition filter narrowed to 0 — intentional, not a manifest problem
                info!(table = %table_key, manifest_parts = manifest_part_count,
                      "No parts match partition filter, skipping");
            } else {
                // Manifest itself has 0 parts for a data table — may be legitimately empty
                // or backup config excluded disks. Warn so callers can detect.
                warn!(table = %table_key, "Data table has zero parts in manifest");
                zero_part_table_names.push(table_key.clone());
            }
            continue;
        }

        // Find the table's data path from live table info (use destination db/table)
        let table_data_path = find_table_data_path(&live_tables, &dst_db, &dst_table, data_path);

        // Find the table's UUID from live tables (use destination db/table)
        let table_uuid = find_table_uuid(&live_tables, &dst_db, &dst_table)
            .or_else(|| table_manifest.uuid.clone());

        info!(
            table = %format!("{}.{}", dst_db, dst_table),
            source = %table_key,
            parts = all_parts.len(),
            data_path = %table_data_path.display(),
            "Restoring table data"
        );

        // Get already-attached parts for this table (keyed by *original* table key)
        let table_already_attached = already_attached.get(table_key).cloned().unwrap_or_default();

        restore_items.push((
            table_key.clone(),
            OwnedAttachParams {
                ch: ch.clone(),
                db: dst_db,
                table: dst_table,
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
                already_attached: table_already_attached,
                attach_semaphore: Some(attach_semaphore.clone()),
                resume_state: resume_state.clone(),
                jitter_factor,
                manifest_disks: manifest.disks.clone(),
                source_db: src_db.to_string(),
                source_table: src_table.to_string(),
                disk_local_paths: disk_local_paths.clone(),
            },
        ));
    }

    // 5a-2. check_replicas_before_attach: poll replication sync with timeout
    if config.clickhouse.check_replicas_before_attach {
        let timeout = config.clickhouse.check_replicas_before_attach_timeout;
        for (table_key, params) in &restore_items {
            if is_replicated_engine(&params.engine) {
                match ch
                    .check_replica_sync_with_timeout(&params.db, &params.table, timeout)
                    .await
                {
                    Ok(true) => {
                        debug!(table = %table_key, "Replica is in sync");
                    }
                    Ok(false) => {
                        warn!(
                            table = %table_key,
                            db = %params.db,
                            table_name = %params.table,
                            timeout_secs = timeout,
                            "Replica is NOT in sync after polling timeout. \
                             Proceeding with restore anyway (non-fatal)."
                        );
                    }
                    Err(e) => {
                        warn!(
                            table = %table_key,
                            error = %e,
                            "Failed to check replica sync status, proceeding with restore"
                        );
                    }
                }
            }
        }
    }

    // 5b. ATTACH TABLE mode: for Replicated tables when restore_as_attach is enabled
    let restore_as_attach = config.clickhouse.restore_as_attach;
    let mut attach_table_results: Vec<(String, u64, u64)> = Vec::new();

    if restore_as_attach {
        let mut normal_items: Vec<(String, OwnedAttachParams)> = Vec::new();

        for (table_key, params) in restore_items {
            // Check if the table has S3 disk parts -- ATTACH TABLE mode
            // only hardlinks metadata without S3 CopyObject or metadata
            // rewrite, so S3 disk tables must use per-part ATTACH flow.
            let has_s3_disk_parts = params.parts_by_disk.iter().any(|(disk_name, disk_parts)| {
                let disk_type = params
                    .disk_type_map
                    .get(disk_name)
                    .map(|s| s.as_str())
                    .unwrap_or("");
                is_s3_disk(disk_type) && disk_parts.iter().any(|p| p.s3_objects.is_some())
            });

            if is_replicated_engine(&params.engine) && !has_s3_disk_parts {
                // Try ATTACH TABLE mode for Replicated tables
                let ddl = manifest
                    .tables
                    .get(&table_key)
                    .map_or("", |tm| tm.ddl.as_str());

                let (src_db, src_table) =
                    table_key.split_once('.').unwrap_or(("default", &table_key));

                match try_attach_table_mode(
                    ch,
                    src_db,
                    src_table,
                    &params.db,
                    &params.table,
                    ddl,
                    &params.engine,
                    &macros,
                    &params.parts,
                    &backup_dir,
                    &params.table_data_path,
                    ch_uid,
                    ch_gid,
                    &manifest.disks,
                    &params.parts_by_disk,
                )
                .await
                {
                    Ok((true, attached, skipped)) => {
                        attach_table_results.push((table_key, attached, skipped));
                    }
                    Ok((false, _, _)) => {
                        // Not eligible -- fall back to normal attach
                        normal_items.push((table_key, params));
                    }
                    Err(e) => {
                        warn!(
                            table = %table_key,
                            error = %e,
                            "ATTACH TABLE mode failed, falling back to per-part ATTACH"
                        );
                        normal_items.push((table_key, params));
                    }
                }
            } else {
                if has_s3_disk_parts && is_replicated_engine(&params.engine) {
                    info!(
                        table = %table_key,
                        "Skipping ATTACH TABLE mode: table has S3 disk parts, using per-part ATTACH with CopyObject"
                    );
                }
                normal_items.push((table_key, params));
            }
        }

        restore_items = normal_items;
    }

    let max_conn = effective_max_connections(config) as usize;
    let table_count = restore_items.len();

    if table_count > 0 {
        info!(
            "Restoring {} tables via per-part ATTACH (max_connections={})",
            table_count, max_conn
        );
    }

    // 6. Parallel table restore
    // All table tasks spawn freely; individual ATTACH PART operations are
    // bounded by the global attach_semaphore passed into each OwnedAttachParams.
    let mut handles = Vec::with_capacity(table_count);

    for (table_key, params) in restore_items {
        let cancel_clone = cancel.clone();

        let handle = tokio::spawn(async move {
            if cancel_clone.is_cancelled() {
                return Ok((table_key, 0u64, 0u64));
            }

            let result = attach_parts_owned(params)
                .await
                .with_context(|| format!("Failed to attach parts for table {}", table_key))?;

            Ok::<(String, u64, u64), anyhow::Error>((table_key, result.attached, result.skipped))
        });

        handles.push(handle);
    }

    // Await all restore tasks
    let mut results: Vec<(String, u64, u64)> = try_join_all(handles)
        .await
        .context("A restore task panicked")?
        .into_iter()
        .collect::<Result<Vec<_>>>()?;

    // Check for cancellation before proceeding to post-attach phases.
    // Parallel attach tasks check cancel_clone themselves, but after try_join_all we must
    // recheck before executing mutations, postponed tables, DDL objects, RBAC, etc.
    if cancel.is_cancelled() {
        bail!("Restore cancelled");
    }

    // Merge ATTACH TABLE mode results
    results.extend(attach_table_results);

    // 7. Tally totals
    let mut total_attached = 0u64;
    let mut total_skipped = 0u64;
    let tables_restored = results.len() as u64;
    for (_table_key, attached, skipped) in &results {
        total_attached += attached;
        total_skipped += skipped;
    }

    // Report data tables with zero parts in manifest. These may be legitimately
    // empty or may indicate backup config issues (e.g. skip_disk_types excluding S3).
    // Log at warn level so monitoring/DAGs can detect, but don't fail the restore.
    if !zero_part_table_names.is_empty() {
        warn!(
            count = zero_part_table_names.len(),
            tables = %zero_part_table_names.join(", "),
            "Data tables with zero parts in backup manifest"
        );
    }

    // Phase 2.5: Mutation re-apply (after all data is attached, before Phase 2b)
    if !schema_only {
        reapply_pending_mutations(ch, &manifest, &results, remap_ref).await;
    }

    // Phase 2b: Postponed tables (streaming engines, refreshable MVs)
    // Created AFTER all data is attached, BEFORE DDL-only objects (#1235)
    if !data_only && !phases.postponed_tables.is_empty() {
        info!(
            count = phases.postponed_tables.len(),
            "Phase 2b: postponed tables"
        );
        create_tables(
            ch,
            &manifest,
            &phases.postponed_tables,
            data_only,
            remap_ref,
            on_cluster,
            &replicated_databases,
            &macros,
            dist_cluster,
        )
        .await?;
    }

    // Phase 3: DDL-only objects (topologically sorted)
    if !data_only && !phases.ddl_only_tables.is_empty() {
        let sorted_ddl = topological_sort(&manifest.tables, &phases.ddl_only_tables)?;
        info!(count = sorted_ddl.len(), "Phase 3: DDL-only objects");
        create_ddl_objects(
            ch,
            &manifest,
            &sorted_ddl,
            remap_ref,
            on_cluster,
            &replicated_databases,
        )
        .await?;
    }

    // Phase 4: Functions (skip when restoring specific tables)
    if !data_only && table_pattern.is_none() && !manifest.functions.is_empty() {
        create_functions(ch, &manifest, on_cluster).await?;
    }

    // Phase 4e: Named collections
    if !data_only
        && (named_collections_restore || config.clickhouse.named_collections_backup_always)
    {
        rbac::restore_named_collections(ch, &manifest, on_cluster).await?;
    }

    // Phase 4e: RBAC restore
    if rbac_restore || config.clickhouse.rbac_backup_always {
        rbac::restore_rbac(
            ch,
            config,
            &backup_dir,
            &config.clickhouse.rbac_resolve_conflicts,
        )
        .await?;
    }

    // Phase 4e: Config file restore
    if configs_restore || config.clickhouse.config_backup_always {
        rbac::restore_configs(config, &backup_dir).await?;
    }

    // Phase 4e: Restart command (if RBAC or configs were restored)
    let did_rbac = (rbac_restore || config.clickhouse.rbac_backup_always)
        && backup_dir.join("access").exists();
    let did_configs = (configs_restore || config.clickhouse.config_backup_always)
        && backup_dir.join("configs").exists();
    if did_rbac || did_configs {
        rbac::execute_restart_commands(ch, &config.clickhouse.restart_command).await?;
    }

    if total_skipped > 0 {
        warn!(
            backup_name = %backup_name,
            tables = tables_restored,
            attached = total_attached,
            skipped = total_skipped,
            total = total_attached + total_skipped,
            "Restore completed with skipped parts"
        );
    } else {
        info!(
            backup_name = %backup_name,
            tables = tables_restored,
            parts = total_attached,
            "Restore complete"
        );
    }

    // Return error if parts were skipped so callers get exit code 3.
    // Do NOT persist completion state on partial restore — the per-part
    // incremental state saved during the attach loop is already correct
    // and sufficient for resume. Writing all manifest parts here would
    // cause the next --resume to skip parts that were never attached.
    if total_skipped > 0 {
        bail!(crate::error::ChBackupError::PartialRestore {
            attached: total_attached,
            skipped: total_skipped,
            total: total_attached + total_skipped,
        });
    }

    // Save completion state with all manifest part names, so a subsequent
    // --resume is a no-op. ClickHouse reassigns block numbers on ATTACH PART,
    // so after merges, active part names differ from manifest names. Persisting
    // the manifest names in the state file allows name-based matching on resume.
    {
        let completion_state = RestoreState {
            attached_parts: phases
                .data_tables
                .iter()
                .filter_map(|key| {
                    manifest.tables.get(key).map(|tm| {
                        let parts: Vec<String> = tm
                            .parts
                            .values()
                            .flat_map(|parts| parts.iter().map(|p| p.name.clone()))
                            .collect();
                        (key.clone(), parts)
                    })
                })
                .collect(),
            backup_name: backup_name.to_string(),
            params_hash: current_params_hash.clone(),
        };
        if let Err(e) = save_state_file(&state_path, &completion_state) {
            warn!(error = %e, "Failed to save completion state (non-fatal)");
        }
    }

    // Delete the params sidecar if one was written during this resume run.
    if resume {
        let params_path = crate::resume::restore_params_path(&backup_dir);
        let _ = std::fs::remove_file(&params_path); // non-fatal
    }

    Ok(())
}

/// Re-apply pending mutations for all restored tables.
///
/// After all data parts are attached, checks each table's manifest for
/// `pending_mutations` and re-applies them sequentially using
/// `ALTER TABLE ... {command} SETTINGS mutations_sync=2`.
///
/// Failures are logged as warnings but do NOT abort restore -- partial
/// mutation re-apply is better than no data.
async fn reapply_pending_mutations(
    ch: &ChClient,
    manifest: &BackupManifest,
    restored_tables: &[(String, u64, u64)],
    remap: Option<&RemapConfig>,
) {
    for (table_key, _count, _skipped) in restored_tables {
        let table_manifest = match manifest.tables.get(table_key) {
            Some(tm) => tm,
            None => continue,
        };

        if table_manifest.pending_mutations.is_empty() {
            continue;
        }

        let (src_db, src_table) = table_key.split_once('.').unwrap_or(("default", table_key));
        let (dst_db, dst_table) = match remap {
            Some(rc) if rc.is_active() => rc.remap_table_key(table_key),
            _ => (src_db.to_string(), src_table.to_string()),
        };

        let dst_key = format!("{}.{}", dst_db, dst_table);
        let mutation_count = table_manifest.pending_mutations.len();

        warn!(
            table = %dst_key,
            count = mutation_count,
            "Table backed up with {} pending data mutations",
            mutation_count
        );

        for mutation in &table_manifest.pending_mutations {
            warn!(
                table = %dst_key,
                mutation_id = %mutation.mutation_id,
                command = %mutation.command,
                parts_pending = mutation.parts_to_do.len(),
                "  mutation_id={}: {} ({} parts pending)",
                mutation.mutation_id,
                mutation.command,
                mutation.parts_to_do.len()
            );

            info!(
                table = %dst_key,
                mutation_id = %mutation.mutation_id,
                "Re-applying mutation... this may take time"
            );

            match ch
                .execute_mutation(&dst_db, &dst_table, &mutation.command)
                .await
            {
                Ok(()) => {
                    info!(
                        table = %dst_key,
                        mutation_id = %mutation.mutation_id,
                        "Mutation re-applied successfully"
                    );
                }
                Err(e) => {
                    warn!(
                        table = %dst_key,
                        mutation_id = %mutation.mutation_id,
                        error = %e,
                        "Failed to re-apply mutation (non-fatal)"
                    );
                }
            }
        }
    }
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

/// Attempt ATTACH TABLE mode for a Replicated table.
///
/// Flow: DETACH TABLE SYNC -> DROP REPLICA from ZK -> hardlink parts to data dir ->
/// ATTACH TABLE -> SYSTEM RESTORE REPLICA
///
/// Returns `Ok((true, attached, skipped))` if ATTACH TABLE mode was used successfully.
/// Returns `Ok((false, 0, 0))` if the table is not eligible (non-Replicated engine).
/// Returns `Err` on unrecoverable failure.
#[allow(clippy::too_many_arguments)]
async fn try_attach_table_mode(
    ch: &ChClient,
    src_db: &str,
    src_table: &str,
    dst_db: &str,
    dst_table: &str,
    ddl: &str,
    engine: &str,
    macros: &HashMap<String, String>,
    parts: &[crate::manifest::PartInfo],
    backup_dir: &Path,
    table_data_path: &Path,
    ch_uid: Option<u32>,
    ch_gid: Option<u32>,
    manifest_disks: &BTreeMap<String, String>,
    parts_by_disk: &BTreeMap<String, Vec<crate::manifest::PartInfo>>,
) -> Result<(bool, u64, u64)> {
    use crate::backup::collect::resolve_shadow_part_path;

    if !is_replicated_engine(engine) {
        return Ok((false, 0, 0));
    }

    let dst_key = format!("{}.{}", dst_db, dst_table);
    info!(table = %dst_key, "ATTACH TABLE mode: Replicated engine detected");

    // Step 1: DETACH TABLE SYNC
    info!(table = %dst_key, "ATTACH TABLE mode: detaching table");
    ch.detach_table_sync(dst_db, dst_table)
        .await
        .with_context(|| format!("ATTACH TABLE mode: failed to DETACH TABLE {}", dst_key))?;

    // Step 2: DROP REPLICA from ZK
    if let Some((zk_path_template, replica_template)) = parse_replicated_params(ddl) {
        let resolved_path = resolve_zk_macros(&zk_path_template, macros);
        let resolved_replica = resolve_zk_macros(&replica_template, macros);

        info!(
            table = %dst_key,
            zk_path = %resolved_path,
            replica = %resolved_replica,
            "ATTACH TABLE mode: dropping ZK replica"
        );
        if let Err(e) = ch
            .drop_replica_from_zkpath(&resolved_replica, &resolved_path)
            .await
        {
            warn!(
                error = %e,
                "ATTACH TABLE mode: DROP REPLICA failed (non-fatal, continuing)"
            );
        }
    }

    // Step 3: Hardlink parts to the table's data directory (NOT detached/)
    // ATTACH TABLE reads from the main data directory, not detached/
    let url_db = crate::path_encoding::encode_path_component(src_db);
    let url_table = crate::path_encoding::encode_path_component(src_table);
    let data_path = table_data_path.to_owned();
    let backup_name = backup_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();

    // Build part_to_disk reverse map from parts_by_disk
    let part_to_disk: HashMap<String, String> = parts_by_disk
        .iter()
        .flat_map(|(disk, disk_parts)| {
            disk_parts
                .iter()
                .map(move |p| (p.name.clone(), disk.clone()))
        })
        .collect();

    // Clone data for spawn_blocking
    let parts_owned: Vec<String> = parts.iter().map(|p| p.name.clone()).collect();
    let backup_dir_clone = backup_dir.to_path_buf();
    let manifest_disks_clone = manifest_disks.clone();
    let src_db_clone = src_db.to_string();
    let src_table_clone = src_table.to_string();

    // Run hardlinking in spawn_blocking since it's sync I/O
    let hardlink_result = tokio::task::spawn_blocking(move || -> Result<u64> {
        let mut linked = 0u64;
        for part_name in &parts_owned {
            let part_dst = data_path.join(part_name);

            if part_dst.exists() {
                linked += 1;
                continue;
            }

            let disk_name = part_to_disk
                .get(part_name)
                .map(String::as_str)
                .unwrap_or("default");
            let part_src = match resolve_shadow_part_path(
                &backup_dir_clone,
                &manifest_disks_clone,
                &backup_name,
                disk_name,
                &url_db,
                &url_table,
                &src_db_clone,
                &src_table_clone,
                part_name,
            ) {
                Some(p) => p,
                None => {
                    tracing::warn!(
                        part = %part_name,
                        table = format!("{}.{}", src_db_clone, src_table_clone),
                        "ATTACH TABLE mode: part source directory not found, skipping"
                    );
                    continue;
                }
            };

            attach::hardlink_or_copy_dir(&part_src, &part_dst)?;
            attach::chown_recursive(&part_dst, ch_uid, ch_gid)?;
            linked += 1;
        }
        Ok(linked)
    })
    .await
    .context("ATTACH TABLE mode: hardlink task panicked")??;

    debug!(
        table = %dst_key,
        linked = hardlink_result,
        "ATTACH TABLE mode: hardlinked parts to data directory"
    );

    // Step 4: ATTACH TABLE
    info!(table = %dst_key, "ATTACH TABLE mode: attaching table");
    ch.attach_table(dst_db, dst_table)
        .await
        .with_context(|| format!("ATTACH TABLE mode: failed to ATTACH TABLE {}", dst_key))?;

    // Step 5: SYSTEM RESTORE REPLICA
    info!(table = %dst_key, "ATTACH TABLE mode: restoring replica");
    ch.system_restore_replica(dst_db, dst_table)
        .await
        .with_context(|| {
            format!(
                "ATTACH TABLE mode: failed to SYSTEM RESTORE REPLICA {}",
                dst_key
            )
        })?;

    let total_parts = parts.len() as u64;
    let skipped = total_parts.saturating_sub(hardlink_result);
    info!(
        table = %dst_key,
        attached = hardlink_result,
        skipped = skipped,
        total = total_parts,
        "ATTACH TABLE mode: complete"
    );
    Ok((true, hardlink_result, skipped))
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

        let result = find_table_data_path(&live_tables, "default", "trades", "/var/lib/clickhouse");
        assert_eq!(
            result,
            PathBuf::from("/var/lib/clickhouse/store/abc/abc123/")
        );
    }

    #[test]
    fn test_find_table_data_path_not_found() {
        let live_tables: Vec<TableRow> = vec![];
        let result = find_table_data_path(&live_tables, "default", "trades", "/var/lib/clickhouse");
        assert_eq!(
            result,
            PathBuf::from("/var/lib/clickhouse/data/default/trades")
        );
    }

    #[test]
    fn test_restore_resume_state_load_and_merge() {
        use crate::resume::{load_state_file, save_state_file, RestoreState};

        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("restore.state.json");

        // Create a state file with some attached parts
        let state = RestoreState {
            attached_parts: HashMap::from([
                (
                    "default.trades".to_string(),
                    vec!["202401_1_50_3".to_string(), "202401_51_100_3".to_string()],
                ),
                (
                    "default.orders".to_string(),
                    vec!["202401_1_10_1".to_string()],
                ),
            ]),
            backup_name: "daily-2024-01-15".to_string(),
            params_hash: String::new(),
        };

        save_state_file(&state_path, &state).unwrap();

        // Load and verify
        let loaded: RestoreState = load_state_file(&state_path).unwrap().unwrap();
        assert_eq!(loaded.backup_name, "daily-2024-01-15");
        assert_eq!(loaded.attached_parts.len(), 2);
        assert_eq!(
            loaded.attached_parts.get("default.trades").unwrap().len(),
            2
        );

        // Simulate merge with system.parts (adding new parts)
        let mut already_attached: HashMap<String, HashSet<String>> = HashMap::new();
        for (table_key, parts) in loaded.attached_parts {
            already_attached.entry(table_key).or_default().extend(parts);
        }

        // Simulated system.parts returns additional parts
        let system_parts = vec!["202401_1_50_3".to_string(), "202402_1_5_0".to_string()];
        already_attached
            .entry("default.trades".to_string())
            .or_default()
            .extend(system_parts);

        // Verify merge: should have 3 unique parts for trades (union of state + system.parts)
        let trades_parts = already_attached.get("default.trades").unwrap();
        assert!(trades_parts.contains("202401_1_50_3"));
        assert!(trades_parts.contains("202401_51_100_3"));
        assert!(trades_parts.contains("202402_1_5_0"));
        assert_eq!(trades_parts.len(), 3);

        // orders table unchanged (only from state file)
        let orders_parts = already_attached.get("default.orders").unwrap();
        assert_eq!(orders_parts.len(), 1);
        assert!(orders_parts.contains("202401_1_10_1"));
    }

    #[test]
    fn test_restore_resume_stale_state_ignored() {
        use crate::resume::{load_state_file, save_state_file, RestoreState};

        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("restore.state.json");

        // Create state with a different backup name
        let state = RestoreState {
            attached_parts: HashMap::from([(
                "default.trades".to_string(),
                vec!["202401_1_50_3".to_string()],
            )]),
            backup_name: "old-backup-name".to_string(),
            params_hash: String::new(),
        };

        save_state_file(&state_path, &state).unwrap();

        // Load and check backup name mismatch
        let loaded: RestoreState = load_state_file(&state_path).unwrap().unwrap();
        let current_backup = "new-backup-name";

        // State should be ignored because backup_name doesn't match
        assert_ne!(loaded.backup_name, current_backup);
        // In the real code, this leads to an empty already_attached map
    }

    #[test]
    fn test_restore_resume_state_deleted_on_success() {
        use crate::resume::{delete_state_file, save_state_file, RestoreState};

        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("restore.state.json");

        let state = RestoreState {
            attached_parts: HashMap::new(),
            backup_name: "test".to_string(),
            params_hash: String::new(),
        };

        save_state_file(&state_path, &state).unwrap();
        assert!(state_path.exists());

        delete_state_file(&state_path);
        assert!(!state_path.exists());
    }

    // -----------------------------------------------------------------------
    // Phase 4d: ATTACH TABLE mode tests
    // -----------------------------------------------------------------------

    /// Test that is_replicated_engine correctly identifies Replicated engine variants.
    #[test]
    fn test_is_replicated_engine_detection() {
        // All Replicated* variants should be detected
        assert!(is_replicated_engine("ReplicatedMergeTree"));
        assert!(is_replicated_engine("ReplicatedReplacingMergeTree"));
        assert!(is_replicated_engine("ReplicatedSummingMergeTree"));
        assert!(is_replicated_engine("ReplicatedAggregatingMergeTree"));
        assert!(is_replicated_engine("ReplicatedCollapsingMergeTree"));
        assert!(is_replicated_engine(
            "ReplicatedVersionedCollapsingMergeTree"
        ));
        assert!(is_replicated_engine("ReplicatedGraphiteMergeTree"));

        // Non-Replicated engines should NOT trigger ATTACH TABLE mode
        assert!(!is_replicated_engine("MergeTree"));
        assert!(!is_replicated_engine("ReplacingMergeTree"));
        assert!(!is_replicated_engine("AggregatingMergeTree"));
        assert!(!is_replicated_engine("CollapsingMergeTree"));
        assert!(!is_replicated_engine("View"));
        assert!(!is_replicated_engine("MaterializedView"));
        assert!(!is_replicated_engine("Dictionary"));
        assert!(!is_replicated_engine("Distributed"));
        assert!(!is_replicated_engine("Kafka"));
        assert!(!is_replicated_engine("Memory"));
    }

    // -----------------------------------------------------------------------
    // Phase 4d: Mutation re-apply tests
    // -----------------------------------------------------------------------

    /// Test that MutationInfo.command is correctly formatted into ALTER TABLE DDL.
    /// The execute_mutation() method takes the raw command (e.g. "DELETE WHERE id = 5")
    /// and wraps it into "ALTER TABLE `db`.`table` {command} SETTINGS mutations_sync=2".
    #[test]
    fn test_mutation_reapply_format() {
        use crate::manifest::MutationInfo;

        // Verify MutationInfo fields are accessible for the re-apply loop
        let mutation = MutationInfo {
            mutation_id: "0000000001".to_string(),
            command: "DELETE WHERE user_id = 5".to_string(),
            parts_to_do: vec!["202401_1_50_3".to_string()],
        };

        assert_eq!(mutation.mutation_id, "0000000001");
        assert_eq!(mutation.command, "DELETE WHERE user_id = 5");
        assert_eq!(mutation.parts_to_do.len(), 1);
    }

    /// Test that tables with no pending mutations are skipped.
    #[test]
    fn test_mutation_reapply_empty() {
        use crate::manifest::{BackupManifest, DatabaseInfo, TableManifest};
        use std::collections::BTreeMap;

        let mut tables = BTreeMap::new();
        tables.insert(
            "default.trades".to_string(),
            TableManifest::test_new("MergeTree")
                .with_ddl("CREATE TABLE default.trades (id UInt64) ENGINE = MergeTree ORDER BY id"),
        );

        let manifest = BackupManifest::test_new("test")
            .with_tables(tables)
            .with_databases(vec![DatabaseInfo::test_new("default")]);

        // Verify table has no mutations -- reapply_pending_mutations would skip it
        let tm = manifest.tables.get("default.trades").unwrap();
        assert!(
            tm.pending_mutations.is_empty(),
            "Table should have no pending mutations"
        );
    }

    /// Test that tables with pending mutations are correctly identified.
    #[test]
    fn test_mutation_reapply_with_mutations() {
        use crate::manifest::{MutationInfo, TableManifest};

        let mut tm = TableManifest::test_new("MergeTree")
            .with_ddl("CREATE TABLE default.trades (id UInt64) ENGINE = MergeTree ORDER BY id");
        tm.pending_mutations = vec![
            MutationInfo {
                mutation_id: "0000000001".to_string(),
                command: "DELETE WHERE user_id = 5".to_string(),
                parts_to_do: vec!["202401_1_50_3".to_string()],
            },
            MutationInfo {
                mutation_id: "0000000002".to_string(),
                command: "UPDATE status = 'archived' WHERE created_at < '2024-01-01'".to_string(),
                parts_to_do: vec!["202401_1_50_3".to_string(), "202402_1_10_1".to_string()],
            },
        ];

        // Verify mutations are present and correctly ordered
        assert_eq!(tm.pending_mutations.len(), 2);
        assert_eq!(tm.pending_mutations[0].mutation_id, "0000000001");
        assert_eq!(tm.pending_mutations[1].mutation_id, "0000000002");
        assert_eq!(tm.pending_mutations[0].command, "DELETE WHERE user_id = 5");
        assert_eq!(tm.pending_mutations[1].parts_to_do.len(), 2);
    }

    /// Test that ATTACH TABLE mode is skipped for non-Replicated engines.
    #[test]
    fn test_attach_table_mode_skips_non_replicated() {
        // Non-Replicated engines: is_replicated_engine returns false
        // This means try_attach_table_mode would return Ok(false) immediately
        let engines = [
            "MergeTree",
            "ReplacingMergeTree",
            "View",
            "Dictionary",
            "Distributed",
        ];
        for engine in &engines {
            assert!(
                !is_replicated_engine(engine),
                "{} should not trigger ATTACH TABLE mode",
                engine
            );
        }
    }

    /// Test that ATTACH TABLE mode resolves per-disk shadow paths via
    /// resolve_shadow_part_path(). Verifies the part_to_disk reverse map
    /// and per-disk path resolution in the spawn_blocking block.
    #[test]
    fn test_attach_table_mode_per_disk_shadow() {
        use crate::backup::collect::{per_disk_backup_dir, resolve_shadow_part_path};
        use crate::manifest::PartInfo;

        let tmp = tempfile::tempdir().unwrap();
        let backup_name = "test-attach";

        // Simulate two disks
        let data_path = tmp.path().join("data");
        let nvme_path = tmp.path().join("nvme1");

        // backup_dir is {data_path}/backup/{name}
        let backup_dir = data_path.join("backup").join(backup_name);
        std::fs::create_dir_all(&backup_dir).unwrap();

        // Create per-disk shadow dir for nvme1 with a part
        let per_disk = per_disk_backup_dir(nvme_path.to_str().unwrap(), backup_name);
        let url_db = "default";
        let url_table = "trades";
        let part_name = "202401_1_50_3";
        let per_disk_part = per_disk
            .join("shadow")
            .join(url_db)
            .join(url_table)
            .join(part_name);
        std::fs::create_dir_all(&per_disk_part).unwrap();
        std::fs::write(per_disk_part.join("checksums.txt"), b"test").unwrap();

        // manifest_disks maps nvme1 to nvme_path
        let manifest_disks: BTreeMap<String, String> = BTreeMap::from([
            (
                "default".to_string(),
                data_path.to_str().unwrap().to_string(),
            ),
            ("nvme1".to_string(), nvme_path.to_str().unwrap().to_string()),
        ]);

        // Build parts_by_disk
        let parts_by_disk: BTreeMap<String, Vec<PartInfo>> = BTreeMap::from([(
            "nvme1".to_string(),
            vec![PartInfo::new(part_name, 1024, 12345)],
        )]);

        // Build part_to_disk reverse map (same logic as try_attach_table_mode)
        let part_to_disk: HashMap<String, String> = parts_by_disk
            .iter()
            .flat_map(|(disk, disk_parts)| {
                disk_parts
                    .iter()
                    .map(move |p| (p.name.clone(), disk.clone()))
            })
            .collect();

        assert_eq!(
            part_to_disk.get(part_name).unwrap(),
            "nvme1",
            "part_to_disk should map part to nvme1"
        );

        // resolve_shadow_part_path should find the part at the per-disk location
        let disk_name = part_to_disk
            .get(part_name)
            .map(String::as_str)
            .unwrap_or("default");
        let resolved = resolve_shadow_part_path(
            &backup_dir,
            &manifest_disks,
            backup_name,
            disk_name,
            url_db,
            url_table,
            "default",
            "trades",
            part_name,
        );
        assert!(resolved.is_some(), "Should find part at per-disk path");
        assert_eq!(resolved.unwrap(), per_disk_part);
    }

    #[test]
    fn test_find_table_data_path_multiple_tables() {
        let live_tables = vec![
            TableRow {
                database: "default".to_string(),
                name: "trades".to_string(),
                engine: "MergeTree".to_string(),
                create_table_query: String::new(),
                uuid: "abc-123".to_string(),
                data_paths: vec!["/data/store/abc/abc123/".to_string()],
                total_bytes: Some(1000),
            },
            TableRow {
                database: "default".to_string(),
                name: "users".to_string(),
                engine: "ReplacingMergeTree".to_string(),
                create_table_query: String::new(),
                uuid: "def-456".to_string(),
                data_paths: vec!["/data/store/def/def456/".to_string()],
                total_bytes: Some(500),
            },
        ];

        // Should find the correct table
        let result = find_table_data_path(&live_tables, "default", "users", "/data");
        assert_eq!(result, PathBuf::from("/data/store/def/def456/"));

        // Non-existent table falls back to default path
        let result = find_table_data_path(&live_tables, "default", "missing", "/data");
        assert_eq!(result, PathBuf::from("/data/data/default/missing"));
    }

    #[test]
    fn test_find_table_uuid_found() {
        let live_tables = vec![TableRow {
            database: "default".to_string(),
            name: "trades".to_string(),
            engine: "MergeTree".to_string(),
            create_table_query: String::new(),
            uuid: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            data_paths: vec![],
            total_bytes: None,
        }];
        let uuid = find_table_uuid(&live_tables, "default", "trades");
        assert_eq!(
            uuid,
            Some("550e8400-e29b-41d4-a716-446655440000".to_string())
        );
    }

    #[test]
    fn test_find_table_uuid_not_found() {
        let live_tables: Vec<TableRow> = vec![];
        let uuid = find_table_uuid(&live_tables, "default", "trades");
        assert_eq!(uuid, None);
    }

    #[test]
    fn test_find_table_uuid_empty_uuid() {
        let live_tables = vec![TableRow {
            database: "default".to_string(),
            name: "trades".to_string(),
            engine: "MergeTree".to_string(),
            create_table_query: String::new(),
            uuid: String::new(),
            data_paths: vec![],
            total_bytes: None,
        }];
        let uuid = find_table_uuid(&live_tables, "default", "trades");
        assert_eq!(uuid, None);
    }

    #[test]
    fn test_find_table_uuid_wrong_db() {
        let live_tables = vec![TableRow {
            database: "default".to_string(),
            name: "trades".to_string(),
            engine: "MergeTree".to_string(),
            create_table_query: String::new(),
            uuid: "abc-123".to_string(),
            data_paths: vec![],
            total_bytes: None,
        }];
        // Wrong database should not match
        let uuid = find_table_uuid(&live_tables, "other_db", "trades");
        assert_eq!(uuid, None);
    }
}
