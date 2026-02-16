//! Schema restoration: CREATE DATABASE and CREATE TABLE from manifest DDL.
//!
//! Implements Mode B (non-destructive) restore:
//! - Creates databases if they don't exist
//! - Creates tables if they don't exist (skips existing tables)

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

use crate::clickhouse::client::ChClient;
use crate::manifest::{BackupManifest, DatabaseInfo};

/// Create databases from the manifest.
///
/// For each database in the manifest, checks if it already exists and
/// creates it if not. The DDL is wrapped with IF NOT EXISTS for safety.
pub async fn create_databases(
    ch: &ChClient,
    manifest: &BackupManifest,
) -> Result<()> {
    if manifest.databases.is_empty() {
        debug!("No databases to create");
        return Ok(());
    }

    for db_info in &manifest.databases {
        create_database(ch, db_info).await?;
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
    ch.execute_ddl(&ddl)
        .await
        .with_context(|| format!("Failed to create database '{}' with DDL: {}", db_info.name, ddl))?;

    Ok(())
}

/// Create tables from the manifest.
///
/// For each table in the manifest (filtered by pattern), checks if the table
/// already exists and creates it if not. Metadata-only tables (views,
/// dictionaries) are also created since they may have DDL.
///
/// Returns the list of table keys that were processed.
pub async fn create_tables(
    ch: &ChClient,
    manifest: &BackupManifest,
    table_keys: &[String],
    data_only: bool,
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

        let (db, table) = table_key.split_once('.').unwrap_or(("default", table_key));

        // Check if table already exists
        let exists = ch
            .table_exists(db, table)
            .await
            .with_context(|| format!("Failed to check if table {}.{} exists", db, table))?;

        if exists {
            debug!(table = %table_key, "Table already exists, skipping CREATE");
            continue;
        }

        if table_manifest.ddl.is_empty() {
            warn!(
                table = %table_key,
                "Table has no DDL in manifest, cannot create"
            );
            continue;
        }

        // Ensure the DDL has IF NOT EXISTS for safety
        let ddl = ensure_if_not_exists_table(&table_manifest.ddl);

        info!(table = %table_key, "Creating table");
        ch.execute_ddl(&ddl)
            .await
            .with_context(|| format!("Failed to create table {} with DDL: {}", table_key, ddl))?;
    }

    info!(
        count = table_keys.len(),
        "Table creation phase complete"
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
        assert_eq!(result, "CREATE DATABASE IF NOT EXISTS default ENGINE = Atomic");
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
}
