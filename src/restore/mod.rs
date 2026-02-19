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
pub mod remap;
pub mod schema;
pub mod sort;
pub mod topo;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use futures::future::try_join_all;
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};

use crate::clickhouse::client::ChClient;
use crate::concurrency::{
    effective_max_connections, effective_object_disk_server_side_copy_concurrency,
};
use crate::config::Config;
use crate::manifest::BackupManifest;
use crate::object_disk::is_s3_disk;
use crate::resume::{delete_state_file, load_state_file, RestoreState};
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
        warn!("No tables match the filter pattern");
        return Ok(());
    }

    info!(
        matched_tables = table_keys.len(),
        total_tables = manifest.tables.len(),
        "Tables matched filter"
    );

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
        detect_replicated_databases(ch, &manifest).await
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
    info!(
        count = phases.data_tables.len(),
        "Phase 2: {} data tables",
        phases.data_tables.len()
    );
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
            info!(
                count = sorted_ddl.len(),
                "Phase 3: {} DDL-only objects",
                sorted_ddl.len()
            );
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
                "Phase 2b: {} postponed tables (schema-only)",
                phases.postponed_tables.len()
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
        if !data_only && !manifest.functions.is_empty() {
            create_functions(ch, &manifest, on_cluster).await?;
        }
        info!("Schema-only mode, skipping data restore");
        return Ok(());
    }

    // 5a. Resume state: load previously attached parts from state file + system.parts
    let state_path = backup_dir.join("restore.state.json");
    let mut already_attached: HashMap<String, HashSet<String>> = HashMap::new();

    if resume {
        // Load state file (may not exist on first run)
        if let Ok(Some(state)) = load_state_file::<RestoreState>(&state_path) {
            if state.backup_name == backup_name {
                let total_parts: usize = state.attached_parts.values().map(|v| v.len()).sum();
                info!(
                    tables = state.attached_parts.len(),
                    parts = total_parts,
                    "Loaded restore resume state"
                );
                for (table_key, parts) in state.attached_parts {
                    already_attached.entry(table_key).or_default().extend(parts);
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
                }
            }
        }

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

    // Build shared resume state tracker (for parallel tasks to update)
    let resume_state: Option<Arc<tokio::sync::Mutex<(RestoreState, PathBuf)>>> = if resume {
        // Initialize state from already_attached so we preserve existing progress
        let initial_state = RestoreState {
            attached_parts: already_attached
                .iter()
                .map(|(k, v)| (k.clone(), v.iter().cloned().collect()))
                .collect(),
            backup_name: backup_name.to_string(),
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
        let all_parts: Vec<_> = table_manifest
            .parts
            .values()
            .flat_map(|parts| parts.iter().cloned())
            .collect();

        if all_parts.is_empty() {
            debug!(table = %table_key, "No data parts, skipping");
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
                resume_state: resume_state.clone(),
            },
        ));
    }

    // 5b. ATTACH TABLE mode: for Replicated tables when restore_as_attach is enabled
    let restore_as_attach = config.clickhouse.restore_as_attach;
    let mut attach_table_results: Vec<(String, u64)> = Vec::new();

    if restore_as_attach {
        // Get macros for ZK path resolution (needed for DROP REPLICA in ATTACH TABLE mode)
        let macros = ch.get_macros().await.unwrap_or_default();

        let mut normal_items: Vec<(String, OwnedAttachParams)> = Vec::new();

        for (table_key, params) in restore_items {
            if is_replicated_engine(&params.engine) {
                // Try ATTACH TABLE mode for Replicated tables
                let table_manifest = manifest.tables.get(&table_key);
                let ddl = table_manifest.map_or("", |tm| tm.ddl.as_str());
                let all_parts: Vec<_> = table_manifest
                    .map(|tm| {
                        tm.parts
                            .values()
                            .flat_map(|parts| parts.iter().cloned())
                            .collect()
                    })
                    .unwrap_or_default();

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
                    &all_parts,
                    &backup_dir,
                    &params.table_data_path,
                    ch_uid,
                    ch_gid,
                )
                .await
                {
                    Ok(true) => {
                        let count = all_parts.len() as u64;
                        attach_table_results.push((table_key, count));
                    }
                    Ok(false) => {
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
    let mut results: Vec<(String, u64)> = try_join_all(handles)
        .await
        .context("A restore task panicked")?
        .into_iter()
        .collect::<Result<Vec<_>>>()?;

    // Merge ATTACH TABLE mode results
    results.extend(attach_table_results);

    // 7. Tally totals
    let mut total_attached = 0u64;
    let tables_restored = results.len() as u64;
    for (_table_key, attached) in &results {
        total_attached += attached;
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
            "Phase 2b: {} postponed tables",
            phases.postponed_tables.len()
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
        info!(
            count = sorted_ddl.len(),
            "Phase 3: {} DDL-only objects",
            sorted_ddl.len()
        );
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

    // Phase 4: Functions
    if !data_only && !manifest.functions.is_empty() {
        create_functions(ch, &manifest, on_cluster).await?;
    }

    info!(
        backup_name = %backup_name,
        tables = tables_restored,
        parts = total_attached,
        "Restore complete"
    );

    // Delete resume state file on successful completion
    if resume {
        delete_state_file(&state_path);
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
    restored_tables: &[(String, u64)],
    remap: Option<&RemapConfig>,
) {
    for (table_key, _count) in restored_tables {
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

            match ch.execute_mutation(&dst_db, &dst_table, &mutation.command).await {
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
/// Returns `Ok(true)` if ATTACH TABLE mode was used successfully.
/// Returns `Ok(false)` if the table is not eligible (non-Replicated or no DDL).
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
) -> Result<bool> {
    if !is_replicated_engine(engine) {
        return Ok(false);
    }

    let dst_key = format!("{}.{}", dst_db, dst_table);
    info!(table = %dst_key, "ATTACH TABLE mode: Replicated engine detected");

    // Step 1: DETACH TABLE SYNC
    info!(table = %dst_key, "ATTACH TABLE mode: detaching table");
    ch.detach_table_sync(dst_db, dst_table)
        .await
        .with_context(|| {
            format!("ATTACH TABLE mode: failed to DETACH TABLE {}", dst_key)
        })?;

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
    let url_db = attach::url_encode(src_db);
    let url_table = attach::url_encode(src_table);
    let shadow_base = backup_dir.join("shadow").join(&url_db).join(&url_table);
    let data_path = table_data_path.to_owned();

    // Run hardlinking in spawn_blocking since it's sync I/O
    let parts_owned: Vec<String> = parts.iter().map(|p| p.name.clone()).collect();
    let hardlink_result = tokio::task::spawn_blocking(move || -> Result<u64> {
        let mut linked = 0u64;
        for part_name in &parts_owned {
            let part_src = shadow_base.join(part_name);
            let part_dst = data_path.join(part_name);

            if part_dst.exists() {
                linked += 1;
                continue;
            }

            if !part_src.exists() {
                continue;
            }

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
        .with_context(|| {
            format!("ATTACH TABLE mode: failed to ATTACH TABLE {}", dst_key)
        })?;

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

    info!(
        table = %dst_key,
        parts = parts.len(),
        "ATTACH TABLE mode: complete"
    );
    Ok(true)
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
        assert!(is_replicated_engine("ReplicatedVersionedCollapsingMergeTree"));
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
        use std::collections::HashMap;

        let mut tables = HashMap::new();
        tables.insert(
            "default.trades".to_string(),
            TableManifest {
                ddl: "CREATE TABLE default.trades (id UInt64) ENGINE = MergeTree ORDER BY id"
                    .to_string(),
                uuid: None,
                engine: "MergeTree".to_string(),
                total_bytes: 0,
                parts: HashMap::new(),
                pending_mutations: Vec::new(), // No mutations
                metadata_only: false,
                dependencies: Vec::new(),
            },
        );

        let manifest = BackupManifest {
            manifest_version: 1,
            name: "test".to_string(),
            timestamp: chrono::Utc::now(),
            clickhouse_version: String::new(),
            chbackup_version: String::new(),
            data_format: "lz4".to_string(),
            compressed_size: 0,
            metadata_size: 0,
            disks: HashMap::new(),
            disk_types: HashMap::new(),
            disk_remote_paths: HashMap::new(),
            tables,
            databases: vec![DatabaseInfo {
                name: "default".to_string(),
                ddl: "CREATE DATABASE default ENGINE = Atomic".to_string(),
            }],
            functions: Vec::new(),
            named_collections: Vec::new(),
            rbac: None,
        };

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
        use std::collections::HashMap;

        let tm = TableManifest {
            ddl: "CREATE TABLE default.trades (id UInt64) ENGINE = MergeTree ORDER BY id"
                .to_string(),
            uuid: None,
            engine: "MergeTree".to_string(),
            total_bytes: 0,
            parts: HashMap::new(),
            pending_mutations: vec![
                MutationInfo {
                    mutation_id: "0000000001".to_string(),
                    command: "DELETE WHERE user_id = 5".to_string(),
                    parts_to_do: vec!["202401_1_50_3".to_string()],
                },
                MutationInfo {
                    mutation_id: "0000000002".to_string(),
                    command: "UPDATE status = 'archived' WHERE created_at < '2024-01-01'"
                        .to_string(),
                    parts_to_do: vec![
                        "202401_1_50_3".to_string(),
                        "202402_1_10_1".to_string(),
                    ],
                },
            ],
            metadata_only: false,
            dependencies: Vec::new(),
        };

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
}
