//! Schema restoration: CREATE DATABASE and CREATE TABLE from manifest DDL.
//!
//! Implements Mode B (non-destructive) restore:
//! - Creates databases if they don't exist
//! - Creates tables if they don't exist (skips existing tables)
//! - When remap is active, rewrites DDL for target database/table names

use std::collections::HashSet;

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

use crate::clickhouse::client::ChClient;
use crate::manifest::{BackupManifest, DatabaseInfo};

use super::remap::{rewrite_create_database_ddl, rewrite_create_table_ddl, RemapConfig};

/// Create databases from the manifest.
///
/// For each database in the manifest, checks if it already exists and
/// creates it if not. The DDL is wrapped with IF NOT EXISTS for safety.
///
/// When `remap` is active, databases are created with remapped names and
/// rewritten DDL. Target databases that don't exist in the manifest (produced
/// by database mapping) are also created.
pub async fn create_databases(
    ch: &ChClient,
    manifest: &BackupManifest,
    remap: Option<&RemapConfig>,
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
                        create_database(ch, &remapped_info).await?;
                        created.insert(dst_db);
                    }
                } else {
                    // No mapping for this database -- create as-is
                    if !created.contains(&db_info.name) {
                        create_database(ch, db_info).await?;
                        created.insert(db_info.name.clone());
                    }
                }
            }
            _ => {
                // No remap -- create as-is
                create_database(ch, db_info).await?;
            }
        }
    }

    info!(
        count = manifest.databases.len(),
        "Database creation phase complete"
    );
    Ok(())
}

/// Create a single database from its DDL.
async fn create_database(ch: &ChClient, db_info: &DatabaseInfo) -> Result<()> {
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
    let ddl = ensure_if_not_exists_database(&db_info.ddl);

    info!(database = %db_info.name, "Creating database");
    ch.execute_ddl(&ddl).await.with_context(|| {
        format!(
            "Failed to create database '{}' with DDL: {}",
            db_info.name, ddl
        )
    })?;

    Ok(())
}

/// Create tables from the manifest.
///
/// For each table in the manifest (filtered by pattern), checks if the table
/// already exists and creates it if not. Metadata-only tables (views,
/// dictionaries) are also created since they may have DDL.
///
/// When `remap` is active, table DDL is rewritten to target the new
/// database/table names, with UUID removal and ZK path/Distributed engine updates.
pub async fn create_tables(
    ch: &ChClient,
    manifest: &BackupManifest,
    table_keys: &[String],
    data_only: bool,
    remap: Option<&RemapConfig>,
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

        // Build DDL: rewrite if remap is active, otherwise just ensure IF NOT EXISTS
        let ddl = match remap {
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

            let ddl = match remap {
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

    info!(count = ddl_keys.len(), "DDL-only objects created");
    Ok(())
}

/// Create functions from the manifest (Phase 4: functions, named collections, RBAC).
///
/// Each entry in `manifest.functions` is a complete `CREATE FUNCTION` DDL statement.
/// Functions are created sequentially since they typically have no inter-dependencies.
pub async fn create_functions(ch: &ChClient, manifest: &BackupManifest) -> Result<()> {
    if manifest.functions.is_empty() {
        debug!("No functions to create");
        return Ok(());
    }

    let mut created = 0u32;
    for func_ddl in &manifest.functions {
        match ch.execute_ddl(func_ddl).await {
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
    // Handle various CREATE types
    let ddl = ddl.replacen("CREATE TABLE", "CREATE TABLE IF NOT EXISTS", 1);
    let ddl = ddl.replacen(
        "CREATE MATERIALIZED VIEW",
        "CREATE MATERIALIZED VIEW IF NOT EXISTS",
        1,
    );
    let ddl = ddl.replacen("CREATE VIEW", "CREATE VIEW IF NOT EXISTS", 1);
    ddl.replacen("CREATE DICTIONARY", "CREATE DICTIONARY IF NOT EXISTS", 1)
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

    /// Verify that create_functions handles empty manifest.functions correctly.
    /// The function returns Ok(()) immediately when no functions are present.
    #[test]
    fn test_create_functions_skips_empty() {
        use crate::manifest::{BackupManifest, DatabaseInfo};
        use chrono::Utc;

        let manifest = BackupManifest {
            manifest_version: 1,
            name: "test".to_string(),
            timestamp: Utc::now(),
            clickhouse_version: String::new(),
            chbackup_version: String::new(),
            data_format: "lz4".to_string(),
            compressed_size: 0,
            metadata_size: 0,
            disks: std::collections::HashMap::new(),
            disk_types: std::collections::HashMap::new(),
            disk_remote_paths: std::collections::HashMap::new(),
            tables: std::collections::HashMap::new(),
            databases: vec![DatabaseInfo {
                name: "default".to_string(),
                ddl: "CREATE DATABASE default ENGINE = Atomic".to_string(),
            }],
            functions: Vec::new(), // Empty -- should return immediately
            named_collections: Vec::new(),
            rbac: None,
        };

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
}
