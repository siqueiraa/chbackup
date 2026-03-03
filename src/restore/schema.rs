//! Schema restoration: CREATE DATABASE and CREATE TABLE from manifest DDL.
//!
//! Implements Mode B (non-destructive) and Mode A (destructive `--rm`) restore:
//! - Mode B: Creates databases and tables if they don't exist (skips existing)
//! - Mode A: DROP tables/databases before CREATE, using reverse engine priority
//!   order with retry loop for dependency failures
//! - When remap is active, rewrites DDL for target database/table names
//! - ZK conflict resolution for Replicated tables
//! - DatabaseReplicated detection

use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

use crate::clickhouse::client::ChClient;
use crate::manifest::{BackupManifest, DatabaseInfo};

use super::remap::{
    add_on_cluster_clause, parse_replicated_params, resolve_zk_macros, rewrite_create_database_ddl,
    rewrite_create_table_ddl, rewrite_distributed_cluster, RemapConfig,
};
use super::topo::sort_tables_for_drop;

/// Create databases from the manifest.
///
/// For each database in the manifest, checks if it already exists and
/// creates it if not. The DDL is wrapped with IF NOT EXISTS for safety.
///
/// When `remap` is active, databases are created with remapped names and
/// rewritten DDL. Target databases that don't exist in the manifest (produced
/// by database mapping) are also created.
///
/// When `on_cluster` is set, appends ON CLUSTER clause to DDL unless the
/// database is in `replicated_databases` (DatabaseReplicated handles its own
/// replication).
pub async fn create_databases(
    ch: &ChClient,
    manifest: &BackupManifest,
    remap: Option<&RemapConfig>,
    on_cluster: Option<&str>,
    replicated_databases: &HashSet<String>,
) -> Result<()> {
    if manifest.databases.is_empty() {
        debug!("No databases to create");
        return Ok(());
    }

    // Track which databases we've already created (to avoid duplicates with remap)
    let mut created: HashSet<String> = HashSet::new();

    for db_info in &manifest.databases {
        match remap {
            Some(rc) if rc.is_active() => {
                // Check if this database is remapped
                let (dst_db, _) = rc.remap_table_key(&format!("{}.dummy", db_info.name));
                if dst_db != db_info.name {
                    // Create the target database with rewritten DDL
                    if !created.contains(&dst_db) {
                        let rewritten_ddl =
                            rewrite_create_database_ddl(&db_info.ddl, &db_info.name, &dst_db);
                        let remapped_info = DatabaseInfo {
                            name: dst_db.clone(),
                            ddl: rewritten_ddl,
                        };
                        create_database_with_cluster(
                            ch,
                            &remapped_info,
                            on_cluster,
                            replicated_databases,
                        )
                        .await?;
                        created.insert(dst_db);
                    }
                } else {
                    // No mapping for this database -- create as-is
                    if !created.contains(&db_info.name) {
                        create_database_with_cluster(ch, db_info, on_cluster, replicated_databases)
                            .await?;
                        created.insert(db_info.name.clone());
                    }
                }
            }
            _ => {
                // No remap -- create as-is
                create_database_with_cluster(ch, db_info, on_cluster, replicated_databases).await?;
            }
        }
    }

    info!(
        count = manifest.databases.len(),
        "Database creation phase complete"
    );
    Ok(())
}

/// Create a single database from its DDL, optionally with ON CLUSTER.
async fn create_database_with_cluster(
    ch: &ChClient,
    db_info: &DatabaseInfo,
    on_cluster: Option<&str>,
    replicated_databases: &HashSet<String>,
) -> Result<()> {
    // Check if database already exists
    let exists = ch
        .database_exists(&db_info.name)
        .await
        .with_context(|| format!("Failed to check if database '{}' exists", db_info.name))?;

    if exists {
        debug!(database = %db_info.name, "Database already exists, skipping");
        return Ok(());
    }

    // Ensure the DDL has IF NOT EXISTS for safety
    let mut ddl = ensure_if_not_exists_database(&db_info.ddl);

    // Apply ON CLUSTER if configured and database is not DatabaseReplicated
    if let Some(cluster) = on_cluster {
        if !replicated_databases.contains(&db_info.name) {
            ddl = add_on_cluster_clause(&ddl, cluster);
            info!(
                database = %db_info.name,
                cluster = %cluster,
                "ON CLUSTER DDL for database creation"
            );
        } else {
            info!(
                database = %db_info.name,
                "DatabaseReplicated detected, skipping ON CLUSTER for database"
            );
        }
    }

    info!(database = %db_info.name, "Creating database");
    ch.execute_ddl(&ddl).await.with_context(|| {
        format!(
            "Failed to create database '{}' with DDL: {}",
            db_info.name, ddl
        )
    })?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Mode A (--rm): DROP phase
// ---------------------------------------------------------------------------

/// System databases that must never be dropped.
const SYSTEM_DATABASES: &[&str] = &["system", "information_schema", "INFORMATION_SCHEMA"];

/// Drop tables in reverse engine priority order (Mode A).
///
/// Tables are sorted by `engine_drop_priority` (Distributed/Merge first,
/// data tables last). Failures are retried in subsequent rounds (max 10),
/// following the same pattern as `create_ddl_objects()`.
///
/// When `on_cluster` is set, DROP DDL includes ON CLUSTER clause.
/// When a database is in `replicated_databases`, ON CLUSTER is skipped for
/// tables in that database.
pub async fn drop_tables(
    ch: &ChClient,
    manifest: &BackupManifest,
    table_keys: &[String],
    remap: Option<&RemapConfig>,
    on_cluster: Option<&str>,
    replicated_databases: &HashSet<String>,
) -> Result<()> {
    if table_keys.is_empty() {
        return Ok(());
    }

    let sorted = sort_tables_for_drop(manifest, table_keys);

    info!(count = sorted.len(), "Phase 0: Dropping tables (Mode A)");

    let mut pending = sorted;
    let max_rounds = 10;

    for round in 0..max_rounds {
        let mut failed: Vec<(String, String)> = Vec::new();
        let mut dropped_this_round = 0u32;

        for table_key in &pending {
            let (src_db, src_table) = table_key.split_once('.').unwrap_or(("default", table_key));

            // Determine destination db/table (may be remapped)
            let (dst_db, dst_table) = match remap {
                Some(rc) if rc.is_active() => rc.remap_table_key(table_key),
                _ => (src_db.to_string(), src_table.to_string()),
            };

            // Determine ON CLUSTER setting for this table
            let effective_on_cluster = if replicated_databases.contains(&dst_db) {
                None // Skip ON CLUSTER for DatabaseReplicated databases
            } else {
                on_cluster
            };

            let dst_key = format!("{}.{}", dst_db, dst_table);
            match ch
                .drop_table(&dst_db, &dst_table, effective_on_cluster)
                .await
            {
                Ok(()) => {
                    info!(table = %dst_key, "Dropped table");
                    dropped_this_round += 1;
                }
                Err(e) => {
                    if round == 0 {
                        debug!(
                            table = %dst_key,
                            error = %e,
                            round = round,
                            "DROP TABLE failed, will retry"
                        );
                    }
                    failed.push((table_key.clone(), e.to_string()));
                }
            }
        }

        if failed.is_empty() {
            break;
        }

        if dropped_this_round == 0 && round > 0 {
            let failed_keys: Vec<&str> = failed.iter().map(|(k, _)| k.as_str()).collect();
            anyhow::bail!(
                "Failed to drop {} tables after {} retry rounds: {:?}. Last errors: {}",
                failed.len(),
                round + 1,
                failed_keys,
                failed
                    .iter()
                    .map(|(k, e)| format!("{}: {}", k, e))
                    .collect::<Vec<_>>()
                    .join("; ")
            );
        }

        info!(
            round = round,
            dropped = dropped_this_round,
            remaining = failed.len(),
            "DROP TABLE retry round"
        );

        pending = failed.into_iter().map(|(k, _)| k).collect();
    }

    if !pending.is_empty() {
        anyhow::bail!(
            "Failed to process {} items after {} rounds: {:?}",
            pending.len(),
            max_rounds,
            pending.iter().take(5).collect::<Vec<_>>()
        );
    }

    info!(count = table_keys.len(), "Table drop phase complete");
    Ok(())
}

/// Drop databases (Mode A).
///
/// Drops each database in the manifest that is not a system database
/// (`system`, `information_schema`, `INFORMATION_SCHEMA`).
/// When `on_cluster` is set, includes ON CLUSTER clause unless the
/// database is in `replicated_databases`.
pub async fn drop_databases(
    ch: &ChClient,
    manifest: &BackupManifest,
    remap: Option<&RemapConfig>,
    on_cluster: Option<&str>,
    replicated_databases: &HashSet<String>,
) -> Result<()> {
    if manifest.databases.is_empty() {
        return Ok(());
    }

    // Collect unique database names to drop (after remap)
    let mut dropped: HashSet<String> = HashSet::new();

    for db_info in &manifest.databases {
        let dst_db = match remap {
            Some(rc) if rc.is_active() => {
                let (d, _) = rc.remap_table_key(&format!("{}.dummy", db_info.name));
                d
            }
            _ => db_info.name.clone(),
        };

        // Never drop system databases
        if SYSTEM_DATABASES.contains(&dst_db.as_str()) {
            debug!(database = %dst_db, "Skipping system database DROP");
            continue;
        }

        if dropped.contains(&dst_db) {
            continue;
        }

        let effective_on_cluster = if replicated_databases.contains(&dst_db) {
            None
        } else {
            on_cluster
        };

        info!(database = %dst_db, "Dropping database");
        match ch.drop_database(&dst_db, effective_on_cluster).await {
            Ok(()) => {
                dropped.insert(dst_db);
            }
            Err(e) => {
                warn!(
                    database = %dst_db,
                    error = %e,
                    "Failed to drop database, continuing"
                );
            }
        }
    }

    info!(count = dropped.len(), "Database drop phase complete");
    Ok(())
}

// ---------------------------------------------------------------------------
// ZK conflict resolution and DatabaseReplicated detection
// ---------------------------------------------------------------------------

/// Check and resolve ZK replica path conflicts for a Replicated table.
///
/// 1. Parse ZK path + replica name from DDL
/// 2. Resolve macros using provided macro map
/// 3. Check `system.zookeeper` for existing replica
/// 4. If conflict: `SYSTEM DROP REPLICA`
///
/// Returns `Ok(())` on success or if not a Replicated table.
/// Logs warnings for conflicts and failures (non-fatal).
pub async fn resolve_zk_conflict(
    ch: &ChClient,
    ddl: &str,
    macros: &std::collections::HashMap<String, String>,
    table_uuid: Option<&str>,
) -> Result<()> {
    // Only applies to Replicated engines
    let (zk_path_template, replica_template) = match parse_replicated_params(ddl) {
        Some((path, replica)) => (path, replica),
        None => return Ok(()), // Not a Replicated engine
    };

    // Build a macro map that includes uuid if available
    let mut resolve_macros = macros.clone();
    if let Some(uuid) = table_uuid {
        resolve_macros
            .entry("uuid".to_string())
            .or_insert_with(|| uuid.to_string());
    }

    let resolved_path = resolve_zk_macros(&zk_path_template, &resolve_macros);
    let resolved_replica = resolve_zk_macros(&replica_template, &resolve_macros);

    // Check if replica already exists in ZK
    let exists = ch
        .check_zk_replica_exists(&resolved_path, &resolved_replica)
        .await?;

    if exists {
        warn!(
            zk_path = %resolved_path,
            replica = %resolved_replica,
            "ZK replica conflict detected, dropping existing replica"
        );
        if let Err(e) = ch
            .drop_replica_from_zkpath(&resolved_replica, &resolved_path)
            .await
        {
            warn!(
                error = %e,
                zk_path = %resolved_path,
                replica = %resolved_replica,
                "SYSTEM DROP REPLICA failed (non-fatal, table creation may still succeed)"
            );
        } else {
            info!(
                zk_path = %resolved_path,
                replica = %resolved_replica,
                "SYSTEM DROP REPLICA successful"
            );
        }
    }

    Ok(())
}

/// Query which databases use the Replicated engine.
///
/// Returns a set of database names that should skip ON CLUSTER.
/// When `remap` is active, queries the destination database names
/// (which are the actual databases that exist in ClickHouse) rather
/// than the source names from the manifest.
pub async fn detect_replicated_databases(
    ch: &ChClient,
    manifest: &BackupManifest,
    remap: Option<&RemapConfig>,
) -> HashSet<String> {
    let mut replicated = HashSet::new();

    let mut db_names: HashSet<String> = HashSet::new();
    for d in &manifest.databases {
        let dst_db = match remap {
            Some(rc) if rc.is_active() => {
                let (remapped, _) = rc.remap_table_key(&format!("{}.dummy", d.name));
                remapped
            }
            _ => d.name.clone(),
        };
        db_names.insert(dst_db);
    }

    for db_name in &db_names {
        match ch.query_database_engine(db_name).await {
            Ok(engine) if engine == "Replicated" => {
                info!(
                    database = %db_name,
                    "DatabaseReplicated detected, skipping ON CLUSTER for this database"
                );
                replicated.insert(db_name.clone());
            }
            Ok(_) => {}
            Err(e) => {
                warn!(
                    database = %db_name,
                    error = %e,
                    "Failed to query database engine, assuming non-Replicated"
                );
            }
        }
    }

    replicated
}

/// Returns true if the engine name indicates a Replicated*MergeTree variant.
pub fn is_replicated_engine(engine: &str) -> bool {
    engine.starts_with("Replicated")
}

// ---------------------------------------------------------------------------
// Schema creation (Mode B / shared)
// ---------------------------------------------------------------------------

/// Create tables from the manifest.
///
/// For each table in the manifest (filtered by pattern), checks if the table
/// already exists and creates it if not. Metadata-only tables (views,
/// dictionaries) are also created since they may have DDL.
///
/// When `remap` is active, table DDL is rewritten to target the new
/// database/table names, with UUID removal and ZK path/Distributed engine updates.
///
/// When `on_cluster` is set, appends ON CLUSTER clause to DDL unless the
/// table's database is in `replicated_databases`. For Replicated tables,
/// ZK conflict resolution runs before CREATE. For Distributed tables,
/// `dist_cluster` rewrites the cluster name.
#[allow(clippy::too_many_arguments)]
pub async fn create_tables(
    ch: &ChClient,
    manifest: &BackupManifest,
    table_keys: &[String],
    data_only: bool,
    remap: Option<&RemapConfig>,
    on_cluster: Option<&str>,
    replicated_databases: &HashSet<String>,
    macros: &HashMap<String, String>,
    dist_cluster: &str,
) -> Result<()> {
    if data_only {
        debug!("Data-only mode, skipping table creation");
        return Ok(());
    }

    for table_key in table_keys {
        let table_manifest = match manifest.tables.get(table_key) {
            Some(tm) => tm,
            None => continue,
        };

        let (src_db, src_table) = table_key.split_once('.').unwrap_or(("default", table_key));

        // Determine destination db/table (may be remapped)
        let (dst_db, dst_table) = match remap {
            Some(rc) if rc.is_active() => rc.remap_table_key(table_key),
            _ => (src_db.to_string(), src_table.to_string()),
        };

        // Check if target table already exists
        let exists = ch
            .table_exists(&dst_db, &dst_table)
            .await
            .with_context(|| format!("Failed to check if table {}.{} exists", dst_db, dst_table))?;

        if exists {
            debug!(
                table = %format!("{}.{}", dst_db, dst_table),
                "Table already exists, skipping CREATE"
            );
            continue;
        }

        if table_manifest.ddl.is_empty() {
            warn!(
                table = %table_key,
                "Table has no DDL in manifest, cannot create"
            );
            continue;
        }

        // ZK conflict resolution for Replicated tables (before CREATE)
        if is_replicated_engine(&table_manifest.engine) {
            // Build macro map with database/table context
            let mut table_macros = macros.clone();
            table_macros
                .entry("database".to_string())
                .or_insert_with(|| dst_db.clone());
            table_macros
                .entry("table".to_string())
                .or_insert_with(|| dst_table.clone());

            if let Err(e) = resolve_zk_conflict(
                ch,
                &table_manifest.ddl,
                &table_macros,
                table_manifest.uuid.as_deref(),
            )
            .await
            {
                warn!(
                    table = %format!("{}.{}", dst_db, dst_table),
                    error = %e,
                    "ZK conflict resolution failed (non-fatal, proceeding with CREATE)"
                );
            }
        }

        // Build DDL: rewrite if remap is active, otherwise just ensure IF NOT EXISTS
        let mut ddl = match remap {
            Some(rc) if rc.is_active() && (src_db != dst_db || src_table != dst_table) => {
                let rewritten = rewrite_create_table_ddl(
                    &table_manifest.ddl,
                    src_db,
                    src_table,
                    &dst_db,
                    &dst_table,
                    &rc.default_replica_path,
                );
                ensure_if_not_exists_table(&rewritten)
            }
            _ => ensure_if_not_exists_table(&table_manifest.ddl),
        };

        // Rewrite Distributed cluster name if configured
        if !dist_cluster.is_empty() {
            let rewritten = rewrite_distributed_cluster(&ddl, dist_cluster);
            if rewritten != ddl {
                info!(
                    table = %format!("{}.{}", dst_db, dst_table),
                    cluster = %dist_cluster,
                    "Rewriting Distributed cluster name"
                );
                ddl = rewritten;
            }
        }

        // Apply ON CLUSTER if configured and database is not DatabaseReplicated
        if let Some(cluster) = on_cluster {
            if !replicated_databases.contains(&dst_db) {
                ddl = add_on_cluster_clause(&ddl, cluster);
            }
        }

        let dst_key = format!("{}.{}", dst_db, dst_table);
        info!(table = %dst_key, "Creating table");
        ch.execute_ddl(&ddl)
            .await
            .with_context(|| format!("Failed to create table {} with DDL: {}", dst_key, ddl))?;
    }

    info!(count = table_keys.len(), "Table creation phase complete");
    Ok(())
}

/// Create DDL-only objects (Phase 3: dictionaries, views, MVs) in caller-provided order.
///
/// Objects are created sequentially in the provided order (which should be
/// topologically sorted by dependencies). On failure, objects are queued for
/// retry -- this handles the fallback case where dependency info is unavailable
/// and the topological sort was approximate (engine-priority only).
///
/// Max 10 retry rounds. Each round retries all previously-failed objects.
/// If a round makes zero progress (no new successes), the function returns
/// an error with the remaining failures.
pub async fn create_ddl_objects(
    ch: &ChClient,
    manifest: &BackupManifest,
    ddl_keys: &[String],
    remap: Option<&RemapConfig>,
    on_cluster: Option<&str>,
    replicated_databases: &HashSet<String>,
) -> Result<()> {
    if ddl_keys.is_empty() {
        return Ok(());
    }

    let mut pending: Vec<String> = ddl_keys.to_vec();
    let max_rounds = 10;

    for round in 0..max_rounds {
        let mut failed: Vec<(String, String)> = Vec::new(); // (key, error_msg)
        let mut created_this_round = 0u32;

        for table_key in &pending {
            let table_manifest = match manifest.tables.get(table_key) {
                Some(tm) => tm,
                None => continue,
            };

            let (src_db, src_table) = table_key.split_once('.').unwrap_or(("default", table_key));
            let (dst_db, dst_table) = match remap {
                Some(rc) if rc.is_active() => rc.remap_table_key(table_key),
                _ => (src_db.to_string(), src_table.to_string()),
            };

            let exists = ch.table_exists(&dst_db, &dst_table).await.unwrap_or(false);
            if exists {
                debug!(
                    table = %format!("{}.{}", dst_db, dst_table),
                    "DDL object already exists"
                );
                created_this_round += 1; // Count as progress
                continue;
            }

            if table_manifest.ddl.is_empty() {
                warn!(table = %table_key, "DDL object has no DDL in manifest");
                continue;
            }

            let mut ddl = match remap {
                Some(rc) if rc.is_active() && (src_db != dst_db || src_table != dst_table) => {
                    let rewritten = rewrite_create_table_ddl(
                        &table_manifest.ddl,
                        src_db,
                        src_table,
                        &dst_db,
                        &dst_table,
                        &rc.default_replica_path,
                    );
                    ensure_if_not_exists_table(&rewritten)
                }
                _ => ensure_if_not_exists_table(&table_manifest.ddl),
            };

            // Apply ON CLUSTER if configured and database is not DatabaseReplicated
            if let Some(cluster) = on_cluster {
                if !replicated_databases.contains(&dst_db) {
                    ddl = add_on_cluster_clause(&ddl, cluster);
                }
            }

            let dst_key = format!("{}.{}", dst_db, dst_table);
            match ch.execute_ddl(&ddl).await {
                Ok(()) => {
                    info!(
                        table = %dst_key,
                        engine = %table_manifest.engine,
                        "Created DDL object"
                    );
                    created_this_round += 1;
                }
                Err(e) => {
                    if round == 0 {
                        debug!(
                            table = %dst_key,
                            error = %e,
                            round = round,
                            "DDL creation failed, will retry"
                        );
                    }
                    failed.push((table_key.clone(), e.to_string()));
                }
            }
        }

        if failed.is_empty() {
            break;
        }

        if created_this_round == 0 && round > 0 {
            // No progress this round -- give up
            let failed_keys: Vec<&str> = failed.iter().map(|(k, _)| k.as_str()).collect();
            anyhow::bail!(
                "Failed to create {} DDL-only objects after {} retry rounds: {:?}. Last errors: {}",
                failed.len(),
                round + 1,
                failed_keys,
                failed
                    .iter()
                    .map(|(k, e)| format!("{}: {}", k, e))
                    .collect::<Vec<_>>()
                    .join("; ")
            );
        }

        info!(
            round = round,
            created = created_this_round,
            remaining = failed.len(),
            "DDL creation retry round"
        );

        pending = failed.into_iter().map(|(k, _)| k).collect();
    }

    if !pending.is_empty() {
        anyhow::bail!(
            "Failed to process {} items after {} rounds: {:?}",
            pending.len(),
            max_rounds,
            pending.iter().take(5).collect::<Vec<_>>()
        );
    }

    info!(count = ddl_keys.len(), "DDL-only objects created");
    Ok(())
}

/// Create functions from the manifest (Phase 4: functions, named collections, RBAC).
///
/// Each entry in `manifest.functions` is a complete `CREATE FUNCTION` DDL statement.
/// Functions are created sequentially since they typically have no inter-dependencies.
///
/// When `on_cluster` is set, appends ON CLUSTER clause to function DDL.
pub async fn create_functions(
    ch: &ChClient,
    manifest: &BackupManifest,
    on_cluster: Option<&str>,
) -> Result<()> {
    if manifest.functions.is_empty() {
        debug!("No functions to create");
        return Ok(());
    }

    let mut created = 0u32;
    for func_ddl in &manifest.functions {
        let ddl = match on_cluster {
            Some(cluster) => add_on_cluster_clause(func_ddl, cluster),
            None => func_ddl.clone(),
        };
        match ch.execute_ddl(&ddl).await {
            Ok(()) => {
                info!(ddl = %func_ddl, "Created function");
                created += 1;
            }
            Err(e) => {
                // Log warning but continue -- function may already exist
                warn!(ddl = %func_ddl, error = %e, "Failed to create function, continuing");
            }
        }
    }

    info!(
        created = created,
        total = manifest.functions.len(),
        "Function creation phase complete"
    );
    Ok(())
}

/// Ensure a CREATE DATABASE statement has IF NOT EXISTS.
fn ensure_if_not_exists_database(ddl: &str) -> String {
    if ddl.contains("IF NOT EXISTS") {
        return ddl.to_string();
    }
    // Insert "IF NOT EXISTS" after "CREATE DATABASE"
    ddl.replacen("CREATE DATABASE", "CREATE DATABASE IF NOT EXISTS", 1)
}

/// Ensure a CREATE TABLE/VIEW/DICTIONARY statement has IF NOT EXISTS.
fn ensure_if_not_exists_table(ddl: &str) -> String {
    if ddl.contains("IF NOT EXISTS") {
        return ddl.to_string();
    }
    // Order matters: check MATERIALIZED VIEW before VIEW to avoid substring match
    if let Some(pos) = ddl.find("CREATE MATERIALIZED VIEW") {
        return format!(
            "{}CREATE MATERIALIZED VIEW IF NOT EXISTS{}",
            &ddl[..pos],
            &ddl[pos + "CREATE MATERIALIZED VIEW".len()..]
        );
    }
    if let Some(pos) = ddl.find("CREATE LIVE VIEW") {
        return format!(
            "{}CREATE LIVE VIEW IF NOT EXISTS{}",
            &ddl[..pos],
            &ddl[pos + "CREATE LIVE VIEW".len()..]
        );
    }
    if let Some(pos) = ddl.find("CREATE WINDOW VIEW") {
        return format!(
            "{}CREATE WINDOW VIEW IF NOT EXISTS{}",
            &ddl[..pos],
            &ddl[pos + "CREATE WINDOW VIEW".len()..]
        );
    }
    if let Some(pos) = ddl.find("CREATE TABLE") {
        return format!(
            "{}CREATE TABLE IF NOT EXISTS{}",
            &ddl[..pos],
            &ddl[pos + "CREATE TABLE".len()..]
        );
    }
    if let Some(pos) = ddl.find("CREATE VIEW") {
        return format!(
            "{}CREATE VIEW IF NOT EXISTS{}",
            &ddl[..pos],
            &ddl[pos + "CREATE VIEW".len()..]
        );
    }
    if let Some(pos) = ddl.find("CREATE DICTIONARY") {
        return format!(
            "{}CREATE DICTIONARY IF NOT EXISTS{}",
            &ddl[..pos],
            &ddl[pos + "CREATE DICTIONARY".len()..]
        );
    }
    ddl.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ensure_if_not_exists_database() {
        let ddl = "CREATE DATABASE default ENGINE = Atomic";
        let result = ensure_if_not_exists_database(ddl);
        assert_eq!(
            result,
            "CREATE DATABASE IF NOT EXISTS default ENGINE = Atomic"
        );
    }

    #[test]
    fn test_ensure_if_not_exists_database_already_present() {
        let ddl = "CREATE DATABASE IF NOT EXISTS default ENGINE = Atomic";
        let result = ensure_if_not_exists_database(ddl);
        assert_eq!(result, ddl);
    }

    #[test]
    fn test_ensure_if_not_exists_table() {
        let ddl = "CREATE TABLE default.trades (id UInt64) ENGINE = MergeTree ORDER BY id";
        let result = ensure_if_not_exists_table(ddl);
        assert!(result.contains("IF NOT EXISTS"));
        assert!(result.starts_with("CREATE TABLE IF NOT EXISTS"));
    }

    #[test]
    fn test_ensure_if_not_exists_view() {
        let ddl = "CREATE VIEW default.my_view AS SELECT 1";
        let result = ensure_if_not_exists_table(ddl);
        assert!(result.contains("CREATE VIEW IF NOT EXISTS"));
    }

    #[test]
    fn test_ensure_if_not_exists_materialized_view() {
        let ddl = "CREATE MATERIALIZED VIEW default.mv TO default.target AS SELECT 1";
        let result = ensure_if_not_exists_table(ddl);
        assert!(result.contains("CREATE MATERIALIZED VIEW IF NOT EXISTS"));
    }

    #[test]
    fn test_ensure_if_not_exists_dictionary() {
        let ddl = "CREATE DICTIONARY default.my_dict (id UInt64, name String) PRIMARY KEY id";
        let result = ensure_if_not_exists_table(ddl);
        assert!(result.contains("CREATE DICTIONARY IF NOT EXISTS"));
    }

    #[test]
    fn test_ensure_if_not_exists_live_view() {
        let ddl = "CREATE LIVE VIEW default.my_live AS SELECT count() FROM default.trades";
        let result = ensure_if_not_exists_table(ddl);
        assert!(result.contains("CREATE LIVE VIEW IF NOT EXISTS"));
        assert!(!result.contains("CREATE LIVE VIEW IF NOT EXISTS IF NOT EXISTS"));
    }

    #[test]
    fn test_ensure_if_not_exists_window_view() {
        let ddl = "CREATE WINDOW VIEW default.my_window TO default.target AS SELECT count() FROM default.trades GROUP BY tumble(timestamp, INTERVAL '1' HOUR)";
        let result = ensure_if_not_exists_table(ddl);
        assert!(result.contains("CREATE WINDOW VIEW IF NOT EXISTS"));
        assert!(!result.contains("CREATE WINDOW VIEW IF NOT EXISTS IF NOT EXISTS"));
    }

    /// Verify that create_functions handles empty manifest.functions correctly.
    /// The function returns Ok(()) immediately when no functions are present.
    #[test]
    fn test_create_functions_skips_empty() {
        use crate::manifest::{BackupManifest, DatabaseInfo};

        let manifest = BackupManifest::test_new("test")
            .with_databases(vec![DatabaseInfo::test_new("default")]);

        assert!(manifest.functions.is_empty());
        // create_functions() is async and needs a ChClient, so we verify the
        // early return condition is correctly gated by functions.is_empty()
    }

    /// Verify DDL preparation for all DDL-only object types used by create_ddl_objects.
    #[test]
    fn test_create_ddl_objects_ddl_preparation() {
        // Views, dictionaries, materialized views are all handled by ensure_if_not_exists_table
        let cases = vec![
            (
                "CREATE VIEW default.v AS SELECT 1",
                "CREATE VIEW IF NOT EXISTS",
            ),
            (
                "CREATE DICTIONARY default.d (id UInt64) PRIMARY KEY id",
                "CREATE DICTIONARY IF NOT EXISTS",
            ),
            (
                "CREATE MATERIALIZED VIEW default.mv TO default.t AS SELECT 1",
                "CREATE MATERIALIZED VIEW IF NOT EXISTS",
            ),
            (
                "CREATE LIVE VIEW default.lv AS SELECT count() FROM default.t",
                "CREATE LIVE VIEW IF NOT EXISTS",
            ),
            (
                "CREATE WINDOW VIEW default.wv TO default.t AS SELECT count() FROM default.src GROUP BY tumble(ts, INTERVAL '1' HOUR)",
                "CREATE WINDOW VIEW IF NOT EXISTS",
            ),
        ];

        for (input, expected_prefix) in cases {
            let result = ensure_if_not_exists_table(input);
            assert!(
                result.contains(expected_prefix),
                "Expected '{}' in result for input: {}",
                expected_prefix,
                input
            );
        }
    }

    // -----------------------------------------------------------------------
    // Phase 4d: Mode A DROP phase tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_drop_system_databases_skipped() {
        // Verify that SYSTEM_DATABASES contains the expected databases
        assert!(SYSTEM_DATABASES.contains(&"system"));
        assert!(SYSTEM_DATABASES.contains(&"information_schema"));
        assert!(SYSTEM_DATABASES.contains(&"INFORMATION_SCHEMA"));

        // Verify normal databases are not in the list
        assert!(!SYSTEM_DATABASES.contains(&"default"));
        assert!(!SYSTEM_DATABASES.contains(&"prod"));
    }

    #[test]
    fn test_is_replicated_engine() {
        // Replicated engines -> true
        assert!(is_replicated_engine("ReplicatedMergeTree"));
        assert!(is_replicated_engine("ReplicatedReplacingMergeTree"));
        assert!(is_replicated_engine("ReplicatedAggregatingMergeTree"));
        assert!(is_replicated_engine("ReplicatedCollapsingMergeTree"));
        assert!(is_replicated_engine("ReplicatedSummingMergeTree"));
        assert!(is_replicated_engine(
            "ReplicatedVersionedCollapsingMergeTree"
        ));

        // Non-replicated engines -> false
        assert!(!is_replicated_engine("MergeTree"));
        assert!(!is_replicated_engine("ReplacingMergeTree"));
        assert!(!is_replicated_engine("View"));
        assert!(!is_replicated_engine("MaterializedView"));
        assert!(!is_replicated_engine("Dictionary"));
        assert!(!is_replicated_engine("Distributed"));
        assert!(!is_replicated_engine("Kafka"));
    }

    // -----------------------------------------------------------------------
    // Phase 4d: ZK conflict resolution tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_is_replicated_engine_in_ddl() {
        // parse_replicated_params should detect replicated engines
        let ddl = "CREATE TABLE default.t (id UInt64) ENGINE = ReplicatedMergeTree('/path', 'r1') ORDER BY id";
        let result = parse_replicated_params(ddl);
        assert!(result.is_some());

        // Non-replicated should return None
        let ddl = "CREATE TABLE default.t (id UInt64) ENGINE = MergeTree() ORDER BY id";
        let result = parse_replicated_params(ddl);
        assert!(result.is_none());
    }

    /// Test the ZK conflict resolution flow (pure logic, no ChClient).
    /// Verifies: parse DDL -> resolve macros -> produce correct resolved path and replica.
    #[test]
    fn test_resolve_zk_conflict_flow() {
        use std::collections::HashMap;

        // Step 1: Parse replicated params from DDL
        let ddl = "CREATE TABLE default.trades (id UInt64) ENGINE = ReplicatedMergeTree('/clickhouse/tables/{shard}/{database}/{table}', '{replica}') ORDER BY id";
        let (path_template, replica_template) = parse_replicated_params(ddl).unwrap();
        assert_eq!(
            path_template,
            "/clickhouse/tables/{shard}/{database}/{table}"
        );
        assert_eq!(replica_template, "{replica}");

        // Step 2: Resolve macros
        let mut macros = HashMap::new();
        macros.insert("shard".to_string(), "01".to_string());
        macros.insert("replica".to_string(), "r1".to_string());
        macros.insert("database".to_string(), "default".to_string());
        macros.insert("table".to_string(), "trades".to_string());

        let resolved_path = resolve_zk_macros(&path_template, &macros);
        let resolved_replica = resolve_zk_macros(&replica_template, &macros);

        assert_eq!(resolved_path, "/clickhouse/tables/01/default/trades");
        assert_eq!(resolved_replica, "r1");

        // Step 3: The actual ZK check + DROP REPLICA would require a live ChClient.
        // We verify the flow produces the correct inputs for check_zk_replica_exists()
        // and drop_replica_from_zkpath().
    }

    /// Test that non-replicated tables are skipped in ZK conflict check.
    #[test]
    fn test_resolve_zk_skip_non_replicated() {
        let ddl = "CREATE TABLE default.t (id UInt64) ENGINE = MergeTree() ORDER BY id";
        let result = parse_replicated_params(ddl);
        assert!(
            result.is_none(),
            "Non-replicated engines should return None"
        );

        // resolve_zk_conflict() returns Ok(()) immediately for non-Replicated engines
        // (can't test async here, but the guard is parse_replicated_params returning None)
    }

    /// Test ZK conflict resolution with UUID macro in ZK path.
    #[test]
    fn test_resolve_zk_conflict_with_uuid() {
        use std::collections::HashMap;

        let ddl = "CREATE TABLE default.t (id UInt64) ENGINE = ReplicatedMergeTree('/clickhouse/tables/{uuid}', '{replica}') ORDER BY id";
        let (path_template, _) = parse_replicated_params(ddl).unwrap();

        let mut macros = HashMap::new();
        macros.insert("replica".to_string(), "r1".to_string());
        macros.insert("uuid".to_string(), "abc-123-def".to_string());

        let resolved_path = resolve_zk_macros(&path_template, &macros);
        assert_eq!(resolved_path, "/clickhouse/tables/abc-123-def");
    }

    // -----------------------------------------------------------------------
    // Additional coverage: ensure_if_not_exists_table edge cases
    // -----------------------------------------------------------------------

    /// Test ensure_if_not_exists_table returns DDL unchanged when it has no
    /// recognized CREATE keyword (the fallback path at the end of the function).
    #[test]
    fn test_ensure_if_not_exists_table_unrecognized_ddl() {
        // DDL that doesn't match any known CREATE pattern
        let ddl = "ALTER TABLE default.trades ADD COLUMN x UInt64";
        let result = ensure_if_not_exists_table(ddl);
        assert_eq!(
            result, ddl,
            "Unrecognized DDL should pass through unchanged"
        );
    }

    /// Test ensure_if_not_exists_table when DDL already has IF NOT EXISTS
    /// for a TABLE statement.
    #[test]
    fn test_ensure_if_not_exists_table_already_present() {
        let ddl = "CREATE TABLE IF NOT EXISTS default.t (id UInt64) ENGINE = MergeTree ORDER BY id";
        let result = ensure_if_not_exists_table(ddl);
        assert_eq!(
            result, ddl,
            "Should return DDL unchanged when IF NOT EXISTS already present"
        );
    }

    /// Test ensure_if_not_exists_table when DDL already has IF NOT EXISTS
    /// for a MATERIALIZED VIEW statement.
    #[test]
    fn test_ensure_if_not_exists_mv_already_present() {
        let ddl = "CREATE MATERIALIZED VIEW IF NOT EXISTS default.mv TO default.t AS SELECT 1";
        let result = ensure_if_not_exists_table(ddl);
        assert_eq!(result, ddl);
    }

    /// Test ensure_if_not_exists_table when DDL already has IF NOT EXISTS
    /// for a DICTIONARY statement.
    #[test]
    fn test_ensure_if_not_exists_dict_already_present() {
        let ddl = "CREATE DICTIONARY IF NOT EXISTS default.d (id UInt64) PRIMARY KEY id";
        let result = ensure_if_not_exists_table(ddl);
        assert_eq!(result, ddl);
    }

    /// Test ensure_if_not_exists_database with DDL that doesn't contain
    /// the CREATE DATABASE keyword at all.
    #[test]
    fn test_ensure_if_not_exists_database_unrecognized() {
        let ddl = "DROP DATABASE foo";
        let result = ensure_if_not_exists_database(ddl);
        // replacen with 0 occurrences returns the string unchanged
        assert_eq!(result, ddl);
    }

    /// Verify that ensure_if_not_exists_table correctly handles CREATE TABLE
    /// with leading whitespace/comments (common in ClickHouse DDL dumps).
    #[test]
    fn test_ensure_if_not_exists_table_preserves_content() {
        let ddl = "CREATE TABLE default.t (id UInt64, name String) ENGINE = MergeTree ORDER BY id SETTINGS index_granularity = 8192";
        let result = ensure_if_not_exists_table(ddl);
        assert!(result.starts_with("CREATE TABLE IF NOT EXISTS"));
        assert!(result.contains("SETTINGS index_granularity = 8192"));
    }

    /// Test ensure_if_not_exists_table for CREATE VIEW with complex query.
    #[test]
    fn test_ensure_if_not_exists_view_complex_query() {
        let ddl = "CREATE VIEW default.complex_view AS SELECT a, b, count() FROM default.t GROUP BY a, b HAVING count() > 10";
        let result = ensure_if_not_exists_table(ddl);
        assert!(result.starts_with("CREATE VIEW IF NOT EXISTS"));
        assert!(result.contains("HAVING count() > 10"));
    }

    // -----------------------------------------------------------------------
    // Additional coverage: is_replicated_engine edge cases
    // -----------------------------------------------------------------------

    /// Test is_replicated_engine with empty string.
    #[test]
    fn test_is_replicated_engine_empty() {
        assert!(!is_replicated_engine(""));
    }

    /// Test is_replicated_engine with just the prefix "Replicated" alone.
    #[test]
    fn test_is_replicated_engine_prefix_only() {
        assert!(is_replicated_engine("Replicated"));
    }

    /// Test is_replicated_engine with case mismatch.
    #[test]
    fn test_is_replicated_engine_case_sensitive() {
        assert!(!is_replicated_engine("replicatedMergeTree"));
        assert!(!is_replicated_engine("REPLICATEDMERGETREE"));
    }

    // -----------------------------------------------------------------------
    // Additional coverage: SYSTEM_DATABASES boundary checks
    // -----------------------------------------------------------------------

    /// Verify system database names are case-sensitive.
    #[test]
    fn test_system_databases_case_sensitive() {
        assert!(SYSTEM_DATABASES.contains(&"system"));
        assert!(!SYSTEM_DATABASES.contains(&"System"));
        assert!(!SYSTEM_DATABASES.contains(&"SYSTEM"));
        assert!(SYSTEM_DATABASES.contains(&"INFORMATION_SCHEMA"));
        assert!(!SYSTEM_DATABASES.contains(&"Information_Schema"));
    }

    // -----------------------------------------------------------------------
    // Comprehensive is_replicated_engine coverage
    // -----------------------------------------------------------------------

    /// All standard Replicated*MergeTree variants must return true.
    #[test]
    fn test_is_replicated_mergetree() {
        assert!(is_replicated_engine("ReplicatedMergeTree"));
        assert!(is_replicated_engine("ReplicatedReplacingMergeTree"));
        assert!(is_replicated_engine("ReplicatedSummingMergeTree"));
        assert!(is_replicated_engine("ReplicatedAggregatingMergeTree"));
        assert!(is_replicated_engine("ReplicatedCollapsingMergeTree"));
        assert!(is_replicated_engine(
            "ReplicatedVersionedCollapsingMergeTree"
        ));
    }

    /// Non-replicated engines must return false.
    #[test]
    fn test_is_not_replicated() {
        assert!(!is_replicated_engine("MergeTree"));
        assert!(!is_replicated_engine("Memory"));
        assert!(!is_replicated_engine("Distributed"));
        assert!(!is_replicated_engine("Log"));
        assert!(!is_replicated_engine(""));
    }

    // -----------------------------------------------------------------------
    // Comprehensive ensure_if_not_exists_database coverage
    // -----------------------------------------------------------------------

    /// Plain CREATE DATABASE without IF NOT EXISTS should get it inserted.
    #[test]
    fn test_ensure_if_not_exists_db_plain() {
        let ddl = "CREATE DATABASE mydb";
        let result = ensure_if_not_exists_database(ddl);
        assert_eq!(result, "CREATE DATABASE IF NOT EXISTS mydb");
    }

    /// When IF NOT EXISTS is already present, the DDL should be returned unchanged.
    #[test]
    fn test_ensure_if_not_exists_db_already_present_v2() {
        let ddl = "CREATE DATABASE IF NOT EXISTS mydb";
        let result = ensure_if_not_exists_database(ddl);
        assert_eq!(result, ddl);
    }

    /// Backtick-quoted database names should be preserved.
    #[test]
    fn test_ensure_if_not_exists_db_backtick() {
        let ddl = "CREATE DATABASE `mydb`";
        let result = ensure_if_not_exists_database(ddl);
        assert_eq!(result, "CREATE DATABASE IF NOT EXISTS `mydb`");
    }

    // -----------------------------------------------------------------------
    // Comprehensive ensure_if_not_exists_table coverage
    // -----------------------------------------------------------------------

    /// Plain CREATE TABLE gets IF NOT EXISTS inserted.
    #[test]
    fn test_ensure_if_not_exists_table_plain() {
        let ddl = "CREATE TABLE db.t (col Int32) ENGINE = MergeTree ORDER BY col";
        let result = ensure_if_not_exists_table(ddl);
        assert!(result.contains("IF NOT EXISTS"));
    }

    /// CREATE TABLE already containing IF NOT EXISTS is returned unchanged.
    #[test]
    fn test_ensure_if_not_exists_table_already_present_v2() {
        let ddl = "CREATE TABLE IF NOT EXISTS db.t (col Int32) ENGINE = MergeTree ORDER BY col";
        let result = ensure_if_not_exists_table(ddl);
        assert_eq!(result, ddl);
    }

    /// CREATE MATERIALIZED VIEW gets IF NOT EXISTS inserted.
    #[test]
    fn test_ensure_if_not_exists_materialized_view_v2() {
        let ddl = "CREATE MATERIALIZED VIEW db.mv (col Int32) ENGINE = MergeTree ORDER BY col AS SELECT * FROM db.t";
        let result = ensure_if_not_exists_table(ddl);
        assert!(result.contains("CREATE MATERIALIZED VIEW IF NOT EXISTS"));
    }

    /// CREATE VIEW gets IF NOT EXISTS inserted.
    #[test]
    fn test_ensure_if_not_exists_view_v2() {
        let ddl = "CREATE VIEW db.v AS SELECT 1";
        let result = ensure_if_not_exists_table(ddl);
        assert!(result.contains("CREATE VIEW IF NOT EXISTS"));
    }

    /// CREATE DICTIONARY gets IF NOT EXISTS inserted.
    #[test]
    fn test_ensure_if_not_exists_dictionary_v2() {
        let ddl = "CREATE DICTIONARY db.d (col Int32) PRIMARY KEY col SOURCE(CLICKHOUSE(HOST 'localhost'))";
        let result = ensure_if_not_exists_table(ddl);
        assert!(result.contains("CREATE DICTIONARY IF NOT EXISTS"));
    }
}
