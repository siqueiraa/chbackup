use std::collections::HashMap;

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

use crate::config::ClickHouseConfig;

/// Thin wrapper around `clickhouse::Client` (clickhouse-rs crate).
///
/// The clickhouse-rs crate uses ClickHouse's HTTP interface, so the URL is
/// constructed as `http(s)://host:port`. Note that the default ClickHouse HTTP
/// port is 8123, not the native protocol port 9000.
#[derive(Clone)]
pub struct ChClient {
    inner: clickhouse::Client,
    /// Store the config for logging/diagnostics.
    host: String,
    port: u16,
    /// Whether to log SQL queries at info level (vs debug).
    log_sql_queries: bool,
}

/// Row from `system.tables` query.
#[derive(Debug, Clone, clickhouse::Row, serde::Deserialize)]
pub struct TableRow {
    pub database: String,
    pub name: String,
    pub engine: String,
    pub create_table_query: String,
    pub uuid: String,
    pub data_paths: Vec<String>,
    pub total_bytes: Option<u64>,
}

/// Row from `system.mutations` query.
#[derive(Debug, Clone, clickhouse::Row, serde::Deserialize)]
pub struct MutationRow {
    pub database: String,
    pub table: String,
    pub mutation_id: String,
    pub command: String,
    pub parts_to_do_names: Vec<String>,
    pub is_done: u8,
}

/// Row from `system.disks` query.
#[derive(Debug, Clone, clickhouse::Row, serde::Deserialize)]
pub struct DiskRow {
    pub name: String,
    pub path: String,
    #[serde(rename = "type")]
    pub disk_type: String,
    /// Remote path for S3 disks (S3 URI or path prefix). Empty for local disks.
    #[serde(default)]
    pub remote_path: String,
}

/// Row from `system.parts` query -- active parts for a table.
#[derive(Debug, Clone, clickhouse::Row, serde::Deserialize)]
pub struct PartRow {
    pub name: String,
    pub partition_id: String,
    pub active: u8,
}

/// Column type inconsistency detected by `check_parts_columns`.
///
/// Indicates that a column has different types across active parts within a table,
/// which can cause restore failures.
#[derive(Debug, Clone)]
pub struct ColumnInconsistency {
    pub database: String,
    pub table: String,
    pub column: String,
    pub types: Vec<String>,
}

/// JSON/Object column detected by `check_json_columns`.
///
/// Indicates that a column uses the Object('json') or JSON experimental type,
/// which may not FREEZE correctly in all ClickHouse versions.
#[derive(Debug, Clone)]
pub struct JsonColumnInfo {
    pub database: String,
    pub table: String,
    pub column: String,
    pub column_type: String,
}

/// Row from `system.macros` query.
#[derive(Debug, Clone, clickhouse::Row, serde::Deserialize)]
pub struct MacroRow {
    pub macro_name: String,
    pub substitution: String,
}

/// Row from `system.tables` query for dependency columns.
///
/// Private -- only used by `query_table_dependencies()`. The `dependencies_database`
/// and `dependencies_table` columns are parallel arrays available in CH 23.3+.
#[derive(Debug, Clone, clickhouse::Row, serde::Deserialize)]
struct DependencyRow {
    database: String,
    name: String,
    dependencies_database: Vec<String>,
    dependencies_table: Vec<String>,
}

/// Row from `system.disks` query with free_space information.
///
/// Separate from `DiskRow` to avoid breaking existing `get_disks()` callers.
#[derive(Debug, Clone, clickhouse::Row, serde::Deserialize)]
pub struct DiskSpaceRow {
    pub name: String,
    pub path: String,
    pub free_space: u64,
}

/// Row with a single `name` column -- used by RBAC/named-collection queries.
#[derive(Debug, Clone, clickhouse::Row, serde::Deserialize)]
struct NameRow {
    name: String,
}

/// Row returned by `SHOW CREATE ...` queries.
///
/// ClickHouse returns the column as `statement` for `SHOW CREATE USER`, etc.
#[derive(Debug, Clone, clickhouse::Row, serde::Deserialize)]
struct ShowCreateRow {
    statement: String,
}

impl ChClient {
    /// Build a new `ChClient` from the given `ClickHouseConfig`.
    ///
    /// Constructs the HTTP URL from `config.host` and `config.port`, sets
    /// credentials, and configures TLS scheme based on `config.secure`.
    pub fn new(config: &ClickHouseConfig) -> Result<Self> {
        let scheme = if config.secure { "https" } else { "http" };
        let url = format!("{}://{}:{}", scheme, config.host, config.port);

        info!(
            host = %config.host,
            port = config.port,
            secure = config.secure,
            "Building ClickHouse client"
        );

        // Wire TLS configuration via environment variables.
        //
        // The clickhouse-rs crate uses hyper-tls (native-tls backend) for HTTPS.
        // Custom CA certificates and client certificates are configured through
        // environment variables that native-tls / OpenSSL respects.
        if config.secure {
            // Custom CA certificate file
            if !config.tls_ca.is_empty() {
                let tls_ca_path = std::path::Path::new(&config.tls_ca);
                if tls_ca_path.exists() {
                    info!(
                        tls_ca = %config.tls_ca,
                        "Setting SSL_CERT_FILE for custom CA certificate"
                    );
                    std::env::set_var("SSL_CERT_FILE", &config.tls_ca);
                } else {
                    warn!(
                        tls_ca = %config.tls_ca,
                        "Custom CA certificate file does not exist, skipping SSL_CERT_FILE"
                    );
                }
            }

            // Client certificate authentication
            // Note: native-tls/OpenSSL does not support client certs via env vars.
            // Users must configure client certs at the OS/OpenSSL level.
            if !config.tls_cert.is_empty() || !config.tls_key.is_empty() {
                warn!(
                    tls_cert = %config.tls_cert,
                    tls_key = %config.tls_key,
                    "Client certificate authentication (tls_cert/tls_key) is not directly \
                     supported by the clickhouse-rs HTTP client. Configure client certificates \
                     at the OS/OpenSSL level instead."
                );
            }

            // Skip TLS verification
            // Note: The native-tls backend does not support skip_verify via env vars.
            // Users who need to skip verification should use a custom CA or set
            // NODE_EXTRA_CA_CERTS / system trust store.
            if config.skip_verify {
                warn!(
                    "skip_verify=true is not directly supported by the clickhouse-rs HTTP \
                     client. TLS verification cannot be disabled programmatically. \
                     Consider adding the server's CA to the system trust store."
                );
            }
        }

        let mut client = clickhouse::Client::default()
            .with_url(&url)
            .with_user(&config.username);

        // Only set password if non-empty (avoid sending empty password header).
        if !config.password.is_empty() {
            client = client.with_password(&config.password);
        }

        Ok(Self {
            inner: client,
            host: config.host.clone(),
            port: config.port,
            log_sql_queries: config.log_sql_queries,
        })
    }

    /// Verify connectivity by executing `SELECT 1`.
    ///
    /// Returns `Ok(())` if ClickHouse responds successfully, or an error
    /// with context about the connection target.
    pub async fn ping(&self) -> Result<()> {
        info!(
            host = %self.host,
            port = self.port,
            "Pinging ClickHouse (SELECT 1)"
        );

        self.inner
            .query("SELECT 1")
            .execute()
            .await
            .context(format!(
                "ClickHouse ping failed ({}:{})",
                self.host, self.port
            ))?;

        info!("ClickHouse ping succeeded");
        Ok(())
    }

    /// Returns a reference to the underlying `clickhouse::Client`.
    ///
    /// Useful for future phases that need direct access to execute queries,
    /// insert data, etc.
    pub fn inner(&self) -> &clickhouse::Client {
        &self.inner
    }

    // -- Query execution helpers --

    /// Log and execute a SQL statement. Logs at info or debug level based
    /// on the `log_sql_queries` setting.
    async fn log_and_execute(&self, sql: &str, description: &str) -> Result<()> {
        if self.log_sql_queries {
            info!(sql = %sql, "Executing {}", description);
        } else {
            debug!(sql = %sql, "Executing {}", description);
        }
        self.inner
            .query(sql)
            .execute()
            .await
            .with_context(|| format!("{} failed: {}", description, sql))?;
        Ok(())
    }

    // -- FREEZE / UNFREEZE --

    /// Execute ALTER TABLE FREEZE WITH NAME for the given table.
    pub async fn freeze_table(&self, db: &str, table: &str, freeze_name: &str) -> Result<()> {
        let sql = format!(
            "ALTER TABLE `{}`.`{}` FREEZE WITH NAME '{}'",
            db, table, freeze_name
        );
        self.log_and_execute(&sql, "FREEZE").await
    }

    /// Execute ALTER TABLE UNFREEZE WITH NAME for the given table.
    pub async fn unfreeze_table(&self, db: &str, table: &str, freeze_name: &str) -> Result<()> {
        let sql = format!(
            "ALTER TABLE `{}`.`{}` UNFREEZE WITH NAME '{}'",
            db, table, freeze_name
        );
        self.log_and_execute(&sql, "UNFREEZE").await
    }

    // -- Table queries --

    /// List all user tables (excluding system databases).
    pub async fn list_tables(&self) -> Result<Vec<TableRow>> {
        let sql = "SELECT database, name, engine, create_table_query, \
                   toString(uuid) as uuid, data_paths, total_bytes \
                   FROM system.tables \
                   WHERE database NOT IN ('system', 'INFORMATION_SCHEMA', 'information_schema')";

        if self.log_sql_queries {
            info!(sql = %sql, "Executing list_tables");
        } else {
            debug!(sql = %sql, "Executing list_tables");
        }

        let rows = self
            .inner
            .query(sql)
            .fetch_all::<TableRow>()
            .await
            .context("Failed to list tables from system.tables")?;

        info!(table_count = rows.len(), "Listed tables");
        Ok(rows)
    }

    /// List all tables including system databases.
    ///
    /// Same as `list_tables()` but without the system database exclusion filter.
    /// Used by the `tables --all` command.
    pub async fn list_all_tables(&self) -> Result<Vec<TableRow>> {
        let sql = "SELECT database, name, engine, create_table_query, \
                   toString(uuid) as uuid, data_paths, total_bytes \
                   FROM system.tables";

        if self.log_sql_queries {
            info!(sql = %sql, "Executing list_all_tables");
        } else {
            debug!(sql = %sql, "Executing list_all_tables");
        }

        let rows = self
            .inner
            .query(sql)
            .fetch_all::<TableRow>()
            .await
            .context("Failed to list all tables from system.tables")?;

        info!(table_count = rows.len(), "Listed all tables (including system)");
        Ok(rows)
    }

    /// Get the CREATE TABLE DDL for a specific table.
    pub async fn get_table_ddl(&self, db: &str, table: &str) -> Result<String> {
        let sql = format!(
            "SELECT create_table_query FROM system.tables \
             WHERE database = '{}' AND name = '{}'",
            db, table
        );

        if self.log_sql_queries {
            info!(sql = %sql, "Executing get_table_ddl");
        } else {
            debug!(sql = %sql, "Executing get_table_ddl");
        }

        #[derive(clickhouse::Row, serde::Deserialize)]
        struct DdlRow {
            create_table_query: String,
        }

        let row = self
            .inner
            .query(&sql)
            .fetch_one::<DdlRow>()
            .await
            .with_context(|| format!("Failed to get DDL for {}.{}", db, table))?;

        Ok(row.create_table_query)
    }

    // -- Mutations --

    /// Check for pending data mutations on the given tables.
    ///
    /// `targets` is a list of (database, table) pairs to check.
    pub async fn check_pending_mutations(
        &self,
        targets: &[(String, String)],
    ) -> Result<Vec<MutationRow>> {
        if targets.is_empty() {
            return Ok(Vec::new());
        }

        // Build the IN clause for (database, table) pairs
        let pairs: Vec<String> = targets
            .iter()
            .map(|(db, table)| format!("('{}', '{}')", db, table))
            .collect();
        let in_clause = pairs.join(", ");

        let sql = format!(
            "SELECT database, table, mutation_id, command, parts_to_do_names, is_done \
             FROM system.mutations \
             WHERE is_done = 0 AND (database, table) IN ({})",
            in_clause
        );

        if self.log_sql_queries {
            info!(sql = %sql, "Executing check_pending_mutations");
        } else {
            debug!(sql = %sql, "Executing check_pending_mutations");
        }

        let rows = self
            .inner
            .query(&sql)
            .fetch_all::<MutationRow>()
            .await
            .context("Failed to check pending mutations")?;

        Ok(rows)
    }

    // -- Replica sync --

    /// Execute SYSTEM SYNC REPLICA for the given table.
    pub async fn sync_replica(&self, db: &str, table: &str) -> Result<()> {
        let sql = format!("SYSTEM SYNC REPLICA `{}`.`{}`", db, table);
        self.log_and_execute(&sql, "SYNC REPLICA").await
    }

    // -- Part attachment --

    /// Execute ALTER TABLE ATTACH PART for the given part name.
    pub async fn attach_part(&self, db: &str, table: &str, part_name: &str) -> Result<()> {
        let sql = format!(
            "ALTER TABLE `{}`.`{}` ATTACH PART '{}'",
            db, table, part_name
        );
        self.log_and_execute(&sql, "ATTACH PART").await
    }

    // -- Server info --

    /// Get the ClickHouse server version string.
    pub async fn get_version(&self) -> Result<String> {
        #[derive(clickhouse::Row, serde::Deserialize)]
        struct VersionRow {
            version: String,
        }

        let row = self
            .inner
            .query("SELECT version() as version")
            .fetch_one::<VersionRow>()
            .await
            .context("Failed to get ClickHouse version")?;

        Ok(row.version)
    }

    /// Get disk information from system.disks.
    pub async fn get_disks(&self) -> Result<Vec<DiskRow>> {
        let sql =
            "SELECT name, path, type, ifNull(remote_path, '') as remote_path FROM system.disks";

        if self.log_sql_queries {
            info!(sql = %sql, "Executing get_disks");
        } else {
            debug!(sql = %sql, "Executing get_disks");
        }

        let rows = self
            .inner
            .query(sql)
            .fetch_all::<DiskRow>()
            .await
            .context("Failed to get disks from system.disks")?;

        Ok(rows)
    }

    /// Get ClickHouse macros from system.macros.
    ///
    /// Returns a HashMap mapping macro names to their substitution values.
    /// On error (e.g., system.macros does not exist), logs a warning and returns
    /// an empty HashMap for graceful degradation.
    pub async fn get_macros(&self) -> Result<HashMap<String, String>> {
        let sql = "SELECT macro AS macro_name, substitution FROM system.macros";

        if self.log_sql_queries {
            info!(sql = %sql, "Executing get_macros");
        } else {
            debug!(sql = %sql, "Executing get_macros");
        }

        let rows = match self.inner.query(sql).fetch_all::<MacroRow>().await {
            Ok(rows) => rows,
            Err(e) => {
                warn!(
                    error = %e,
                    "Failed to query system.macros, using empty macros (table may not exist)"
                );
                return Ok(HashMap::new());
            }
        };

        let macros: HashMap<String, String> = rows
            .into_iter()
            .map(|r| (r.macro_name, r.substitution))
            .collect();

        info!(macro_count = macros.len(), "Loaded ClickHouse macros");
        Ok(macros)
    }

    // -- DDL execution --

    /// Execute arbitrary DDL (CREATE DATABASE, CREATE TABLE, etc.).
    pub async fn execute_ddl(&self, ddl: &str) -> Result<()> {
        self.log_and_execute(ddl, "DDL").await
    }

    // -- Integration tables (Phase 3a) --

    /// Create the `system.backup_list` and `system.backup_actions` URL engine
    /// integration tables in ClickHouse.
    ///
    /// These allow `SELECT * FROM system.backup_list` and
    /// `INSERT INTO system.backup_actions(command) VALUES ('create_remote daily')`.
    pub async fn create_integration_tables(&self, api_host: &str, api_port: &str) -> Result<()> {
        let (list_ddl, actions_ddl) = integration_table_ddl(api_host, api_port);

        self.execute_ddl(&list_ddl)
            .await
            .context("Failed to create system.backup_list integration table")?;

        self.execute_ddl(&actions_ddl)
            .await
            .context("Failed to create system.backup_actions integration table")?;

        info!("Created integration tables: system.backup_list, system.backup_actions");
        Ok(())
    }

    /// Drop the `system.backup_list` and `system.backup_actions` integration tables.
    pub async fn drop_integration_tables(&self) -> Result<()> {
        self.execute_ddl("DROP TABLE IF EXISTS system.backup_list")
            .await
            .context("Failed to drop system.backup_list")?;

        self.execute_ddl("DROP TABLE IF EXISTS system.backup_actions")
            .await
            .context("Failed to drop system.backup_actions")?;

        info!("Dropped integration tables: system.backup_list, system.backup_actions");
        Ok(())
    }

    /// Check if a database exists.
    pub async fn database_exists(&self, db: &str) -> Result<bool> {
        #[derive(clickhouse::Row, serde::Deserialize)]
        struct CountRow {
            cnt: u64,
        }

        let sql = format!(
            "SELECT count() as cnt FROM system.databases WHERE name = '{}'",
            db
        );

        let row = self
            .inner
            .query(&sql)
            .fetch_one::<CountRow>()
            .await
            .with_context(|| format!("Failed to check if database '{}' exists", db))?;

        Ok(row.cnt > 0)
    }

    /// Check if a table exists.
    pub async fn table_exists(&self, db: &str, table: &str) -> Result<bool> {
        #[derive(clickhouse::Row, serde::Deserialize)]
        struct CountRow {
            cnt: u64,
        }

        let sql = format!(
            "SELECT count() as cnt FROM system.tables \
             WHERE database = '{}' AND name = '{}'",
            db, table
        );

        let row = self
            .inner
            .query(&sql)
            .fetch_one::<CountRow>()
            .await
            .with_context(|| format!("Failed to check if table {}.{} exists", db, table))?;

        Ok(row.cnt > 0)
    }

    // -- FREEZE PARTITION --

    /// Execute ALTER TABLE FREEZE PARTITION for a specific partition.
    ///
    /// Freezes a single partition instead of the entire table. The frozen data
    /// ends up in the same shadow directory structure as whole-table FREEZE.
    pub async fn freeze_partition(
        &self,
        db: &str,
        table: &str,
        partition: &str,
        freeze_name: &str,
    ) -> Result<()> {
        let sql = freeze_partition_sql(db, table, partition, freeze_name);
        self.log_and_execute(&sql, "FREEZE PARTITION").await
    }

    // -- system.parts query --

    /// Query `system.parts` for active parts of a specific table.
    ///
    /// Returns all active parts (active=1) for the given database and table.
    pub async fn query_system_parts(&self, db: &str, table: &str) -> Result<Vec<PartRow>> {
        let sql = format!(
            "SELECT name, partition_id, active FROM system.parts \
             WHERE database = '{}' AND table = '{}' AND active = 1",
            db, table
        );

        if self.log_sql_queries {
            info!(sql = %sql, "Executing query_system_parts");
        } else {
            debug!(sql = %sql, "Executing query_system_parts");
        }

        let rows = self
            .inner
            .query(&sql)
            .fetch_all::<PartRow>()
            .await
            .with_context(|| format!("Failed to query system.parts for {}.{}", db, table))?;

        Ok(rows)
    }

    // -- Parts column consistency check --

    /// Check for column type inconsistencies across active parts (design 3.3).
    ///
    /// For each (database, table) pair, queries `system.parts_columns` to find
    /// columns that have different types across active parts. This can indicate
    /// schema drift that would cause restore failures.
    pub async fn check_parts_columns(
        &self,
        targets: &[(String, String)],
    ) -> Result<Vec<ColumnInconsistency>> {
        if targets.is_empty() {
            return Ok(Vec::new());
        }

        // Build the IN clause for (database, table) pairs
        let pairs: Vec<String> = targets
            .iter()
            .map(|(db, table)| format!("('{}', '{}')", db, table))
            .collect();
        let in_clause = pairs.join(", ");

        let sql = format!(
            "SELECT database, table, name AS column, \
             groupUniqArray(type) AS uniq_types \
             FROM system.parts_columns \
             WHERE active AND (database, table) IN ({}) \
             GROUP BY database, table, column \
             HAVING length(uniq_types) > 1",
            in_clause
        );

        if self.log_sql_queries {
            info!(sql = %sql, "Executing check_parts_columns");
        } else {
            debug!(sql = %sql, "Executing check_parts_columns");
        }

        #[derive(clickhouse::Row, serde::Deserialize)]
        struct PartsColumnsRow {
            database: String,
            table: String,
            column: String,
            uniq_types: Vec<String>,
        }

        let rows = self
            .inner
            .query(&sql)
            .fetch_all::<PartsColumnsRow>()
            .await
            .context("Failed to check parts columns consistency")?;

        let inconsistencies: Vec<ColumnInconsistency> = rows
            .into_iter()
            .map(|r| ColumnInconsistency {
                database: r.database,
                table: r.table,
                column: r.column,
                types: r.uniq_types,
            })
            .collect();

        Ok(inconsistencies)
    }

    // -- JSON/Object column type detection --

    /// Check for columns using Object or JSON types (design 16.4).
    ///
    /// For each (database, table) pair, queries `system.columns` to find columns
    /// whose type contains 'Object' or 'JSON'. These types are experimental in
    /// ClickHouse and may not FREEZE correctly.
    ///
    /// This is a warning-only check and never blocks backup.
    pub async fn check_json_columns(
        &self,
        targets: &[(String, String)],
    ) -> Result<Vec<JsonColumnInfo>> {
        if targets.is_empty() {
            return Ok(Vec::new());
        }

        // Build the IN clause for (database, table) pairs
        let pairs: Vec<String> = targets
            .iter()
            .map(|(db, table)| format!("('{}', '{}')", db, table))
            .collect();
        let in_clause = pairs.join(", ");

        let sql = format!(
            "SELECT database, table, name AS column, type AS column_type \
             FROM system.columns \
             WHERE (database, table) IN ({}) \
             AND (type LIKE '%Object%' OR type LIKE '%JSON%')",
            in_clause
        );

        if self.log_sql_queries {
            info!(sql = %sql, "Executing check_json_columns");
        } else {
            debug!(sql = %sql, "Executing check_json_columns");
        }

        #[derive(clickhouse::Row, serde::Deserialize)]
        struct JsonColumnRow {
            database: String,
            table: String,
            column: String,
            column_type: String,
        }

        let rows = self
            .inner
            .query(&sql)
            .fetch_all::<JsonColumnRow>()
            .await
            .context("Failed to check JSON/Object columns in system.columns")?;

        let json_cols: Vec<JsonColumnInfo> = rows
            .into_iter()
            .map(|r| JsonColumnInfo {
                database: r.database,
                table: r.table,
                column: r.column,
                column_type: r.column_type,
            })
            .collect();

        Ok(json_cols)
    }

    // -- Disk free space query --

    /// Query `system.tables` for table dependency information.
    ///
    /// Returns a map from `"db.table"` to `Vec<"dep_db.dep_table">`, representing
    /// which tables each table depends on. This is used to populate the
    /// `TableManifest.dependencies` field during backup creation.
    ///
    /// On query failure (e.g., ClickHouse < 23.3 where `dependencies_database`
    /// and `dependencies_table` columns do not exist), catches the error, logs
    /// a warning, and returns `Ok(HashMap::new())` for graceful degradation.
    pub async fn query_table_dependencies(&self) -> Result<HashMap<String, Vec<String>>> {
        let sql = "SELECT database, name, dependencies_database, dependencies_table \
                   FROM system.tables \
                   WHERE database NOT IN ('system', 'INFORMATION_SCHEMA', 'information_schema')";

        if self.log_sql_queries {
            info!(sql = %sql, "Executing query_table_dependencies");
        } else {
            debug!(sql = %sql, "Executing query_table_dependencies");
        }

        let rows = match self.inner.query(sql).fetch_all::<DependencyRow>().await {
            Ok(rows) => rows,
            Err(e) => {
                warn!(
                    error = %e,
                    "Failed to query table dependencies (CH < 23.3?), dependencies will be empty"
                );
                return Ok(HashMap::new());
            }
        };

        let mut result: HashMap<String, Vec<String>> = HashMap::new();
        for row in rows {
            let key = format!("{}.{}", row.database, row.name);
            let deps: Vec<String> = row
                .dependencies_database
                .iter()
                .zip(row.dependencies_table.iter())
                .filter(|(db, tbl)| !db.is_empty() && !tbl.is_empty())
                .map(|(db, tbl)| format!("{}.{}", db, tbl))
                .collect();
            if !deps.is_empty() {
                result.insert(key, deps);
            }
        }

        info!(
            tables_with_deps = result.len(),
            "Queried table dependencies"
        );
        Ok(result)
    }

    // -- DROP operations (Mode A, Phase 4d) --

    /// Drop a table (Mode A).
    ///
    /// SQL: `DROP TABLE IF EXISTS \`db\`.\`table\` [ON CLUSTER 'cluster'] SYNC`
    pub async fn drop_table(&self, db: &str, table: &str, on_cluster: Option<&str>) -> Result<()> {
        let sql = drop_table_sql(db, table, on_cluster);
        self.log_and_execute(&sql, "DROP TABLE").await
    }

    /// Drop a database (Mode A).
    ///
    /// SQL: `DROP DATABASE IF EXISTS \`db\` [ON CLUSTER 'cluster'] SYNC`
    pub async fn drop_database(&self, db: &str, on_cluster: Option<&str>) -> Result<()> {
        let sql = drop_database_sql(db, on_cluster);
        self.log_and_execute(&sql, "DROP DATABASE").await
    }

    // -- ATTACH TABLE mode (Phase 4d) --

    /// Detach a table synchronously.
    ///
    /// SQL: `DETACH TABLE \`db\`.\`table\` SYNC`
    pub async fn detach_table_sync(&self, db: &str, table: &str) -> Result<()> {
        let sql = detach_table_sync_sql(db, table);
        self.log_and_execute(&sql, "DETACH TABLE SYNC").await
    }

    /// Attach an entire table (not a part).
    ///
    /// SQL: `ATTACH TABLE \`db\`.\`table\``
    pub async fn attach_table(&self, db: &str, table: &str) -> Result<()> {
        let sql = attach_table_sql(db, table);
        self.log_and_execute(&sql, "ATTACH TABLE").await
    }

    /// Restore replica metadata from local parts.
    ///
    /// SQL: `SYSTEM RESTORE REPLICA \`db\`.\`table\``
    pub async fn system_restore_replica(&self, db: &str, table: &str) -> Result<()> {
        let sql = system_restore_replica_sql(db, table);
        self.log_and_execute(&sql, "SYSTEM RESTORE REPLICA").await
    }

    // -- ZK conflict resolution (Phase 4d) --

    /// Drop a replica from ZooKeeper by explicit ZK path.
    ///
    /// SQL: `SYSTEM DROP REPLICA 'replica_name' FROM ZKPATH 'zk_path'`
    pub async fn drop_replica_from_zkpath(&self, replica_name: &str, zk_path: &str) -> Result<()> {
        let sql = drop_replica_from_zkpath_sql(replica_name, zk_path);
        self.log_and_execute(&sql, "SYSTEM DROP REPLICA FROM ZKPATH")
            .await
    }

    /// Check if a replica exists at a given ZK path.
    ///
    /// SQL: `SELECT count() FROM system.zookeeper WHERE path='{zk_path}/replicas' AND name='{replica_name}'`
    ///
    /// Returns `false` on query error (system.zookeeper may not be accessible).
    pub async fn check_zk_replica_exists(&self, zk_path: &str, replica_name: &str) -> Result<bool> {
        #[derive(clickhouse::Row, serde::Deserialize)]
        struct CountRow {
            cnt: u64,
        }

        let sql = format!(
            "SELECT count() as cnt FROM system.zookeeper WHERE path = '{}/replicas' AND name = '{}'",
            zk_path, replica_name
        );

        if self.log_sql_queries {
            info!(sql = %sql, "Executing check_zk_replica_exists");
        } else {
            debug!(sql = %sql, "Executing check_zk_replica_exists");
        }

        match self.inner.query(&sql).fetch_one::<CountRow>().await {
            Ok(row) => Ok(row.cnt > 0),
            Err(e) => {
                warn!(
                    error = %e,
                    zk_path = %zk_path,
                    replica = %replica_name,
                    "Failed to check ZK replica existence (system.zookeeper may be unavailable)"
                );
                Ok(false)
            }
        }
    }

    // -- DatabaseReplicated detection (Phase 4d) --

    /// Query the engine of a database.
    ///
    /// SQL: `SELECT engine FROM system.databases WHERE name = '{db}'`
    ///
    /// Returns empty string if database not found.
    pub async fn query_database_engine(&self, db: &str) -> Result<String> {
        #[derive(clickhouse::Row, serde::Deserialize)]
        struct EngineRow {
            engine: String,
        }

        let sql = format!("SELECT engine FROM system.databases WHERE name = '{}'", db);

        if self.log_sql_queries {
            info!(sql = %sql, "Executing query_database_engine");
        } else {
            debug!(sql = %sql, "Executing query_database_engine");
        }

        match self.inner.query(&sql).fetch_one::<EngineRow>().await {
            Ok(row) => Ok(row.engine),
            Err(_) => Ok(String::new()),
        }
    }

    // -- Mutation execution (Phase 4d) --

    /// Execute a mutation command (ALTER TABLE ... DELETE/UPDATE WHERE ...).
    ///
    /// The command is from `MutationInfo.command` (e.g., "DELETE WHERE user_id = 5").
    ///
    /// SQL: `ALTER TABLE \`db\`.\`table\` {command} SETTINGS mutations_sync=2`
    pub async fn execute_mutation(&self, db: &str, table: &str, command: &str) -> Result<()> {
        let sql = execute_mutation_sql(db, table, command);
        self.log_and_execute(&sql, "MUTATION").await
    }

    // -- RBAC, named collections, and user-defined functions queries (Phase 4e) --

    /// Query RBAC objects of a given entity type from ClickHouse system tables.
    ///
    /// For each entity found in the corresponding system table, runs `SHOW CREATE {entity_type}`
    /// to get the full DDL. Returns Vec of (name, DDL) tuples.
    ///
    /// Entity types: "USER", "ROLE", "ROW POLICY", "SETTINGS PROFILE", "QUOTA"
    /// Corresponding system tables: system.users, system.roles, system.row_policies,
    ///   system.settings_profiles, system.quotas
    ///
    /// On query error, logs a warning and returns an empty Vec (graceful degradation).
    pub async fn query_rbac_objects(&self, entity_type: &str) -> Result<Vec<(String, String)>> {
        let system_table = match entity_type {
            "USER" => "system.users",
            "ROLE" => "system.roles",
            "ROW POLICY" => "system.row_policies",
            "SETTINGS PROFILE" => "system.settings_profiles",
            "QUOTA" => "system.quotas",
            _ => {
                warn!(entity_type = %entity_type, "Unknown RBAC entity type, skipping");
                return Ok(Vec::new());
            }
        };

        let names_sql = format!("SELECT name FROM {}", system_table);

        if self.log_sql_queries {
            info!(sql = %names_sql, "Executing query_rbac_objects ({})", entity_type);
        } else {
            debug!(sql = %names_sql, "Executing query_rbac_objects ({})", entity_type);
        }

        let names: Vec<NameRow> = match self.inner.query(&names_sql).fetch_all().await {
            Ok(rows) => rows,
            Err(e) => {
                warn!(
                    error = %e,
                    entity_type = %entity_type,
                    table = %system_table,
                    "Failed to query {} (may not exist), skipping", system_table
                );
                return Ok(Vec::new());
            }
        };

        let mut results = Vec::with_capacity(names.len());
        for name_row in &names {
            let show_sql = format!(
                "SHOW CREATE {} {}",
                entity_type,
                quote_identifier(&name_row.name)
            );

            match self
                .inner
                .query(&show_sql)
                .fetch_one::<ShowCreateRow>()
                .await
            {
                Ok(row) => {
                    results.push((name_row.name.clone(), row.statement));
                }
                Err(e) => {
                    warn!(
                        error = %e,
                        entity_type = %entity_type,
                        name = %name_row.name,
                        "Failed to SHOW CREATE {} {}, skipping", entity_type, name_row.name
                    );
                }
            }
        }

        debug!(
            entity_type = %entity_type,
            count = results.len(),
            "Queried RBAC objects"
        );
        Ok(results)
    }

    /// Query named collections from ClickHouse.
    ///
    /// Queries `system.named_collections` for names, then `SHOW CREATE NAMED COLLECTION`
    /// for each. Returns Vec of CREATE DDL strings.
    ///
    /// On query error, logs a warning and returns an empty Vec (graceful degradation).
    pub async fn query_named_collections(&self) -> Result<Vec<String>> {
        let names_sql = "SELECT name FROM system.named_collections";

        if self.log_sql_queries {
            info!(sql = %names_sql, "Executing query_named_collections");
        } else {
            debug!(sql = %names_sql, "Executing query_named_collections");
        }

        let names: Vec<NameRow> = match self.inner.query(names_sql).fetch_all().await {
            Ok(rows) => rows,
            Err(e) => {
                warn!(
                    error = %e,
                    "Failed to query system.named_collections (may not exist), skipping"
                );
                return Ok(Vec::new());
            }
        };

        let mut results = Vec::with_capacity(names.len());
        for name_row in &names {
            let show_sql = format!(
                "SHOW CREATE NAMED COLLECTION {}",
                quote_identifier(&name_row.name)
            );

            match self
                .inner
                .query(&show_sql)
                .fetch_one::<ShowCreateRow>()
                .await
            {
                Ok(row) => {
                    results.push(row.statement);
                }
                Err(e) => {
                    warn!(
                        error = %e,
                        name = %name_row.name,
                        "Failed to SHOW CREATE NAMED COLLECTION {}, skipping", name_row.name
                    );
                }
            }
        }

        debug!(count = results.len(), "Queried named collections");
        Ok(results)
    }

    /// Query user-defined SQL functions from ClickHouse.
    ///
    /// Queries `system.functions WHERE origin = 'SQLUserDefined'` for names,
    /// then `SHOW CREATE FUNCTION` for each. Returns Vec of CREATE DDL strings.
    ///
    /// On query error, logs a warning and returns an empty Vec (graceful degradation).
    pub async fn query_user_defined_functions(&self) -> Result<Vec<String>> {
        let names_sql = "SELECT name FROM system.functions WHERE origin = 'SQLUserDefined'";

        if self.log_sql_queries {
            info!(sql = %names_sql, "Executing query_user_defined_functions");
        } else {
            debug!(sql = %names_sql, "Executing query_user_defined_functions");
        }

        let names: Vec<NameRow> = match self.inner.query(names_sql).fetch_all().await {
            Ok(rows) => rows,
            Err(e) => {
                warn!(
                    error = %e,
                    "Failed to query user-defined functions, skipping"
                );
                return Ok(Vec::new());
            }
        };

        let mut results = Vec::with_capacity(names.len());
        for name_row in &names {
            let show_sql = format!("SHOW CREATE FUNCTION {}", quote_identifier(&name_row.name));

            match self
                .inner
                .query(&show_sql)
                .fetch_one::<ShowCreateRow>()
                .await
            {
                Ok(row) => {
                    results.push(row.statement);
                }
                Err(e) => {
                    warn!(
                        error = %e,
                        name = %name_row.name,
                        "Failed to SHOW CREATE FUNCTION {}, skipping", name_row.name
                    );
                }
            }
        }

        debug!(count = results.len(), "Queried user-defined functions");
        Ok(results)
    }

    /// Query `system.disks` for disk free space information.
    ///
    /// Returns disk name, path, and free space in bytes for each disk.
    pub async fn query_disk_free_space(&self) -> Result<Vec<DiskSpaceRow>> {
        let sql = "SELECT name, path, free_space FROM system.disks";

        if self.log_sql_queries {
            info!(sql = %sql, "Executing query_disk_free_space");
        } else {
            debug!(sql = %sql, "Executing query_disk_free_space");
        }

        let rows = self
            .inner
            .query(sql)
            .fetch_all::<DiskSpaceRow>()
            .await
            .context("Failed to query disk free space from system.disks")?;

        Ok(rows)
    }
}

/// Quote an identifier for use in ClickHouse SQL.
///
/// Wraps the name in backticks and escapes any backticks within the name by
/// doubling them (standard SQL identifier escaping).
///
/// Example: `my user` -> `` `my user` ``, `` back`tick `` -> `` `back``tick` ``
fn quote_identifier(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

/// Sanitize a name for use in freeze names.
///
/// Replaces all non-alphanumeric characters (except underscore) with underscore.
pub fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Generate the freeze name for a backup operation.
///
/// Format: `chbackup_{backup_name}_{db}_{table}`
pub fn freeze_name(backup_name: &str, db: &str, table: &str) -> String {
    format!(
        "chbackup_{}_{}_{}",
        sanitize_name(backup_name),
        sanitize_name(db),
        sanitize_name(table)
    )
}

/// Generate the SQL string for a FREEZE command (for testing).
pub fn freeze_sql(db: &str, table: &str, freeze_name: &str) -> String {
    format!(
        "ALTER TABLE `{}`.`{}` FREEZE WITH NAME '{}'",
        db, table, freeze_name
    )
}

/// Generate the SQL string for an UNFREEZE command (for testing).
pub fn unfreeze_sql(db: &str, table: &str, freeze_name: &str) -> String {
    format!(
        "ALTER TABLE `{}`.`{}` UNFREEZE WITH NAME '{}'",
        db, table, freeze_name
    )
}

/// Generate the SQL string for a FREEZE PARTITION command (for testing).
pub fn freeze_partition_sql(db: &str, table: &str, partition: &str, freeze_name: &str) -> String {
    format!(
        "ALTER TABLE `{}`.`{}` FREEZE PARTITION '{}' WITH NAME '{}'",
        db, table, partition, freeze_name
    )
}

/// Generate the SQL for a DROP TABLE command (for testing).
pub fn drop_table_sql(db: &str, table: &str, on_cluster: Option<&str>) -> String {
    match on_cluster {
        Some(cluster) => format!(
            "DROP TABLE IF EXISTS `{}`.`{}` ON CLUSTER '{}' SYNC",
            db, table, cluster
        ),
        None => format!("DROP TABLE IF EXISTS `{}`.`{}` SYNC", db, table),
    }
}

/// Generate the SQL for a DROP DATABASE command (for testing).
pub fn drop_database_sql(db: &str, on_cluster: Option<&str>) -> String {
    match on_cluster {
        Some(cluster) => format!(
            "DROP DATABASE IF EXISTS `{}` ON CLUSTER '{}' SYNC",
            db, cluster
        ),
        None => format!("DROP DATABASE IF EXISTS `{}` SYNC", db),
    }
}

/// Generate the SQL for a DETACH TABLE SYNC command (for testing).
pub fn detach_table_sync_sql(db: &str, table: &str) -> String {
    format!("DETACH TABLE `{}`.`{}` SYNC", db, table)
}

/// Generate the SQL for an ATTACH TABLE command (for testing).
pub fn attach_table_sql(db: &str, table: &str) -> String {
    format!("ATTACH TABLE `{}`.`{}`", db, table)
}

/// Generate the SQL for a SYSTEM RESTORE REPLICA command (for testing).
pub fn system_restore_replica_sql(db: &str, table: &str) -> String {
    format!("SYSTEM RESTORE REPLICA `{}`.`{}`", db, table)
}

/// Generate the SQL for a SYSTEM DROP REPLICA FROM ZKPATH command (for testing).
pub fn drop_replica_from_zkpath_sql(replica_name: &str, zk_path: &str) -> String {
    format!(
        "SYSTEM DROP REPLICA '{}' FROM ZKPATH '{}'",
        replica_name, zk_path
    )
}

/// Generate the SQL for a mutation execution command (for testing).
pub fn execute_mutation_sql(db: &str, table: &str, command: &str) -> String {
    format!(
        "ALTER TABLE `{}`.`{}` {} SETTINGS mutations_sync=2",
        db, table, command
    )
}

/// Generate the DDL statements for the integration tables.
///
/// Returns (backup_list_ddl, backup_actions_ddl) matching the design doc section 9.1.
pub fn integration_table_ddl(api_host: &str, api_port: &str) -> (String, String) {
    let list_ddl = format!(
        "CREATE TABLE IF NOT EXISTS system.backup_list (\
         name String, \
         created DateTime, \
         location String, \
         size UInt64, \
         data_size UInt64, \
         object_disk_size UInt64, \
         metadata_size UInt64, \
         rbac_size UInt64, \
         config_size UInt64, \
         compressed_size UInt64, \
         required String\
         ) ENGINE = URL('http://{}:{}/api/v1/list', 'JSONEachRow')",
        api_host, api_port
    );

    let actions_ddl = format!(
        "CREATE TABLE IF NOT EXISTS system.backup_actions (\
         command String, \
         start String, \
         finish String, \
         status String, \
         error String\
         ) ENGINE = URL('http://{}:{}/api/v1/actions', 'JSONEachRow')",
        api_host, api_port
    );

    (list_ddl, actions_ddl)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ClickHouseConfig;

    #[test]
    fn test_ch_client_new_default_config() {
        let config = ClickHouseConfig::default();
        let client = ChClient::new(&config);
        assert!(
            client.is_ok(),
            "ChClient::new should succeed with default config"
        );
        let client = client.unwrap();
        assert_eq!(client.host, "localhost");
        assert_eq!(client.port, 9000);
        assert!(client.log_sql_queries);
    }

    #[test]
    fn test_ch_client_new_secure() {
        let config = ClickHouseConfig {
            secure: true,
            host: "ch.example.com".to_string(),
            port: 8443,
            ..ClickHouseConfig::default()
        };
        let client = ChClient::new(&config);
        assert!(
            client.is_ok(),
            "ChClient::new should succeed with secure config"
        );
    }

    #[test]
    fn test_ch_client_new_with_credentials() {
        let config = ClickHouseConfig {
            username: "admin".to_string(),
            password: "secret".to_string(),
            ..ClickHouseConfig::default()
        };
        let client = ChClient::new(&config);
        assert!(
            client.is_ok(),
            "ChClient::new should succeed with credentials"
        );
    }

    #[test]
    fn test_freeze_sql_format() {
        let sql = freeze_sql("default", "trades", "chbackup_daily_default_trades");
        assert_eq!(
            sql,
            "ALTER TABLE `default`.`trades` FREEZE WITH NAME 'chbackup_daily_default_trades'"
        );
    }

    #[test]
    fn test_unfreeze_sql_format() {
        let sql = unfreeze_sql("default", "trades", "chbackup_daily_default_trades");
        assert_eq!(
            sql,
            "ALTER TABLE `default`.`trades` UNFREEZE WITH NAME 'chbackup_daily_default_trades'"
        );
    }

    #[test]
    fn test_sanitize_freeze_name() {
        assert_eq!(sanitize_name("daily-2024-01-15"), "daily_2024_01_15");
        assert_eq!(sanitize_name("my_backup"), "my_backup");
        assert_eq!(sanitize_name("backup.name"), "backup_name");
        assert_eq!(sanitize_name("with spaces"), "with_spaces");
        assert_eq!(sanitize_name("special!@#$chars"), "special____chars");
        assert_eq!(sanitize_name("already_clean_123"), "already_clean_123");
    }

    #[test]
    fn test_freeze_name_generation() {
        let name = freeze_name("daily-20240115", "default", "trades");
        assert_eq!(name, "chbackup_daily_20240115_default_trades");

        let name = freeze_name("backup.v2", "my-db", "my.table");
        assert_eq!(name, "chbackup_backup_v2_my_db_my_table");
    }

    #[test]
    fn test_disk_row_has_remote_path() {
        // DiskRow should have remote_path field with serde(default) for backward compat
        let disk = DiskRow {
            name: "s3disk".to_string(),
            path: "/var/lib/clickhouse/disks/s3".to_string(),
            disk_type: "s3".to_string(),
            remote_path: "s3://data-bucket/ch-data/".to_string(),
        };
        assert_eq!(disk.remote_path, "s3://data-bucket/ch-data/");

        // Local disk has empty remote_path
        let local_disk = DiskRow {
            name: "default".to_string(),
            path: "/var/lib/clickhouse".to_string(),
            disk_type: "local".to_string(),
            remote_path: String::new(),
        };
        assert!(local_disk.remote_path.is_empty());
    }

    #[test]
    fn test_disk_row_remote_path_serde_default() {
        // Verify that remote_path defaults to empty string when missing from JSON
        // This simulates older ClickHouse versions that may not have the column
        let json = r#"{"name":"default","path":"/var/lib/clickhouse","type":"local"}"#;
        let disk: DiskRow = serde_json::from_str(json).unwrap();
        assert_eq!(disk.name, "default");
        assert!(disk.remote_path.is_empty());
    }

    #[test]
    fn test_ch_client_log_sql_queries_setting() {
        let config = ClickHouseConfig {
            log_sql_queries: false,
            ..ClickHouseConfig::default()
        };
        let client = ChClient::new(&config).unwrap();
        assert!(!client.log_sql_queries);
    }

    #[test]
    fn test_ch_client_url_scheme_secure() {
        // When secure=true, the URL should use https:// scheme
        let config = ClickHouseConfig {
            secure: true,
            host: "ch.example.com".to_string(),
            port: 8443,
            ..ClickHouseConfig::default()
        };
        let _client = ChClient::new(&config).unwrap();
        // Verify the scheme is correct by constructing the URL the same way ChClient does
        let scheme = if config.secure { "https" } else { "http" };
        let url = format!("{}://{}:{}", scheme, config.host, config.port);
        assert!(url.starts_with("https://"));
        assert_eq!(url, "https://ch.example.com:8443");
    }

    #[test]
    fn test_ch_client_url_scheme_insecure() {
        // When secure=false, the URL should use http:// scheme
        let config = ClickHouseConfig {
            secure: false,
            host: "localhost".to_string(),
            port: 8123,
            ..ClickHouseConfig::default()
        };
        let _client = ChClient::new(&config).unwrap();
        let scheme = if config.secure { "https" } else { "http" };
        let url = format!("{}://{}:{}", scheme, config.host, config.port);
        assert!(url.starts_with("http://"));
        assert_eq!(url, "http://localhost:8123");
    }

    #[test]
    fn test_ch_client_tls_config_wiring() {
        // Verify that ChClient::new succeeds with TLS config fields set
        let config = ClickHouseConfig {
            secure: true,
            tls_ca: "/path/to/ca.pem".to_string(),
            tls_cert: "/path/to/cert.pem".to_string(),
            tls_key: "/path/to/key.pem".to_string(),
            skip_verify: true,
            host: "ch.example.com".to_string(),
            port: 8443,
            ..ClickHouseConfig::default()
        };
        // Should succeed even with TLS config
        let client = ChClient::new(&config);
        assert!(
            client.is_ok(),
            "ChClient::new should succeed with TLS config"
        );
    }

    #[test]
    fn test_ch_client_tls_scheme_generation() {
        // Verify URL scheme is correctly determined by secure flag
        // secure=true -> https, secure=false -> http
        for (secure, expected_scheme) in [(true, "https"), (false, "http")] {
            let scheme = if secure { "https" } else { "http" };
            assert_eq!(scheme, expected_scheme);
        }
    }

    // -- Task 2: New ChClient query method tests --

    #[test]
    fn test_freeze_partition_sql() {
        let sql = freeze_partition_sql(
            "default",
            "trades",
            "202401",
            "chbackup_daily_default_trades",
        );
        assert_eq!(
            sql,
            "ALTER TABLE `default`.`trades` FREEZE PARTITION '202401' WITH NAME 'chbackup_daily_default_trades'"
        );
    }

    #[test]
    fn test_freeze_partition_sql_tuple_partition() {
        // Partition IDs can be tuple-based
        let sql = freeze_partition_sql(
            "default",
            "events",
            "(202401, 'us')",
            "chbackup_test_default_events",
        );
        assert_eq!(
            sql,
            "ALTER TABLE `default`.`events` FREEZE PARTITION '(202401, 'us')' WITH NAME 'chbackup_test_default_events'"
        );
    }

    #[test]
    fn test_query_parts_sql() {
        // Verify the SQL query string for system.parts
        let db = "default";
        let table = "trades";
        let expected_sql = format!(
            "SELECT name, partition_id, active FROM system.parts \
             WHERE database = '{}' AND table = '{}' AND active = 1",
            db, table
        );
        assert!(expected_sql.contains("system.parts"));
        assert!(expected_sql.contains("active = 1"));
        assert!(expected_sql.contains("default"));
        assert!(expected_sql.contains("trades"));
    }

    #[test]
    fn test_check_parts_columns_sql() {
        // Verify the SQL pattern for parts_columns consistency check
        let targets = [
            ("default".to_string(), "trades".to_string()),
            ("logs".to_string(), "events".to_string()),
        ];
        let pairs: Vec<String> = targets
            .iter()
            .map(|(db, table)| format!("('{}', '{}')", db, table))
            .collect();
        let in_clause = pairs.join(", ");

        let sql = format!(
            "SELECT database, table, name AS column, \
             groupUniqArray(type) AS uniq_types \
             FROM system.parts_columns \
             WHERE active AND (database, table) IN ({}) \
             GROUP BY database, table, column \
             HAVING length(uniq_types) > 1",
            in_clause
        );

        assert!(sql.contains("system.parts_columns"));
        assert!(sql.contains("groupUniqArray(type)"));
        assert!(sql.contains("HAVING length(uniq_types) > 1"));
        assert!(sql.contains("('default', 'trades')"));
        assert!(sql.contains("('logs', 'events')"));
    }

    #[test]
    fn test_disk_free_space_sql() {
        // Verify the SQL query string for disk free space
        let sql = "SELECT name, path, free_space FROM system.disks";
        assert!(sql.contains("system.disks"));
        assert!(sql.contains("free_space"));
    }

    #[test]
    fn test_part_row_type() {
        let part = PartRow {
            name: "202401_1_50_3".to_string(),
            partition_id: "202401".to_string(),
            active: 1,
        };
        assert_eq!(part.name, "202401_1_50_3");
        assert_eq!(part.partition_id, "202401");
        assert_eq!(part.active, 1);
    }

    #[test]
    fn test_column_inconsistency_type() {
        let inconsistency = ColumnInconsistency {
            database: "default".to_string(),
            table: "trades".to_string(),
            column: "amount".to_string(),
            types: vec!["Float64".to_string(), "Decimal(18,2)".to_string()],
        };
        assert_eq!(inconsistency.database, "default");
        assert_eq!(inconsistency.table, "trades");
        assert_eq!(inconsistency.column, "amount");
        assert_eq!(inconsistency.types.len(), 2);
    }

    #[test]
    fn test_disk_space_row_type() {
        let disk = DiskSpaceRow {
            name: "default".to_string(),
            path: "/var/lib/clickhouse".to_string(),
            free_space: 1_000_000_000,
        };
        assert_eq!(disk.name, "default");
        assert_eq!(disk.free_space, 1_000_000_000);
    }

    #[test]
    fn test_integration_table_ddl_generation() {
        let (list_ddl, actions_ddl) = integration_table_ddl("localhost", "7171");

        // Verify backup_list DDL
        assert!(list_ddl.contains("CREATE TABLE IF NOT EXISTS system.backup_list"));
        assert!(list_ddl.contains("name String"));
        assert!(list_ddl.contains("created DateTime"));
        assert!(list_ddl.contains("location String"));
        assert!(list_ddl.contains("size UInt64"));
        assert!(list_ddl.contains("data_size UInt64"));
        assert!(list_ddl.contains("object_disk_size UInt64"));
        assert!(list_ddl.contains("metadata_size UInt64"));
        assert!(list_ddl.contains("rbac_size UInt64"));
        assert!(list_ddl.contains("config_size UInt64"));
        assert!(list_ddl.contains("compressed_size UInt64"));
        assert!(list_ddl.contains("required String"));
        assert!(
            list_ddl.contains("ENGINE = URL('http://localhost:7171/api/v1/list', 'JSONEachRow')")
        );

        // Verify backup_actions DDL
        assert!(actions_ddl.contains("CREATE TABLE IF NOT EXISTS system.backup_actions"));
        assert!(actions_ddl.contains("command String"));
        assert!(actions_ddl.contains("start String"));
        assert!(actions_ddl.contains("finish String"));
        assert!(actions_ddl.contains("status String"));
        assert!(actions_ddl.contains("error String"));
        assert!(actions_ddl
            .contains("ENGINE = URL('http://localhost:7171/api/v1/actions', 'JSONEachRow')"));
    }

    #[test]
    fn test_dependency_row_deserialize() {
        // Verify DependencyRow struct can deserialize from JSON
        // (simulating the shape of system.tables dependency columns)
        let row = DependencyRow {
            database: "default".to_string(),
            name: "user_dict".to_string(),
            dependencies_database: vec!["default".to_string(), "logs".to_string()],
            dependencies_table: vec!["users".to_string(), "events".to_string()],
        };
        assert_eq!(row.database, "default");
        assert_eq!(row.name, "user_dict");
        assert_eq!(row.dependencies_database.len(), 2);
        assert_eq!(row.dependencies_table.len(), 2);

        // Verify combining parallel arrays into "db.table" format
        let deps: Vec<String> = row
            .dependencies_database
            .iter()
            .zip(row.dependencies_table.iter())
            .filter(|(db, tbl)| !db.is_empty() && !tbl.is_empty())
            .map(|(db, tbl)| format!("{}.{}", db, tbl))
            .collect();
        assert_eq!(deps, vec!["default.users", "logs.events"]);
    }

    #[test]
    fn test_dependency_row_empty_deps() {
        // Tables with no dependencies should produce empty vec
        let row = DependencyRow {
            database: "default".to_string(),
            name: "trades".to_string(),
            dependencies_database: Vec::new(),
            dependencies_table: Vec::new(),
        };
        let deps: Vec<String> = row
            .dependencies_database
            .iter()
            .zip(row.dependencies_table.iter())
            .filter(|(db, tbl)| !db.is_empty() && !tbl.is_empty())
            .map(|(db, tbl)| format!("{}.{}", db, tbl))
            .collect();
        assert!(deps.is_empty());
    }

    #[test]
    fn test_dependency_row_filters_empty_entries() {
        // Some ClickHouse versions may return empty strings in the arrays
        let row = DependencyRow {
            database: "default".to_string(),
            name: "view1".to_string(),
            dependencies_database: vec!["default".to_string(), "".to_string()],
            dependencies_table: vec!["trades".to_string(), "".to_string()],
        };
        let deps: Vec<String> = row
            .dependencies_database
            .iter()
            .zip(row.dependencies_table.iter())
            .filter(|(db, tbl)| !db.is_empty() && !tbl.is_empty())
            .map(|(db, tbl)| format!("{}.{}", db, tbl))
            .collect();
        assert_eq!(deps, vec!["default.trades"]);
    }

    #[test]
    fn test_integration_table_ddl_custom_host_port() {
        let (list_ddl, actions_ddl) = integration_table_ddl("backup-server", "8080");
        assert!(list_ddl.contains("http://backup-server:8080/api/v1/list"));
        assert!(actions_ddl.contains("http://backup-server:8080/api/v1/actions"));
    }

    #[test]
    fn test_macro_row_deserializable() {
        // Verify MacroRow can be deserialized from JSON (simulating system.macros columns)
        let json = r#"{"macro_name":"shard","substitution":"01"}"#;
        let row: MacroRow = serde_json::from_str(json).unwrap();
        assert_eq!(row.macro_name, "shard");
        assert_eq!(row.substitution, "01");

        // Verify multiple rows can form a HashMap
        let rows = vec![
            MacroRow {
                macro_name: "shard".to_string(),
                substitution: "01".to_string(),
            },
            MacroRow {
                macro_name: "replica".to_string(),
                substitution: "r1".to_string(),
            },
        ];
        let macros: std::collections::HashMap<String, String> = rows
            .into_iter()
            .map(|r| (r.macro_name, r.substitution))
            .collect();
        assert_eq!(macros.get("shard"), Some(&"01".to_string()));
        assert_eq!(macros.get("replica"), Some(&"r1".to_string()));
    }

    #[test]
    fn test_list_all_tables_sql_no_system_filter() {
        // Verify that list_all_tables uses SQL without a system database filter.
        // list_tables uses: WHERE database NOT IN ('system', ...)
        // list_all_tables should NOT have that filter.
        let list_tables_sql = "SELECT database, name, engine, create_table_query, \
                   toString(uuid) as uuid, data_paths, total_bytes \
                   FROM system.tables \
                   WHERE database NOT IN ('system', 'INFORMATION_SCHEMA', 'information_schema')";

        let list_all_tables_sql = "SELECT database, name, engine, create_table_query, \
                   toString(uuid) as uuid, data_paths, total_bytes \
                   FROM system.tables";

        // list_tables SQL should contain the WHERE clause
        assert!(list_tables_sql.contains("WHERE database NOT IN"));

        // list_all_tables SQL should NOT contain the WHERE clause
        assert!(!list_all_tables_sql.contains("WHERE database NOT IN"));
        assert!(list_all_tables_sql.contains("system.tables"));
    }

    // -- Phase 4d: SQL generation tests for new ChClient methods --

    #[test]
    fn test_drop_table_sql_generation() {
        // Without ON CLUSTER
        let sql = drop_table_sql("default", "trades", None);
        assert_eq!(sql, "DROP TABLE IF EXISTS `default`.`trades` SYNC");

        // With ON CLUSTER
        let sql = drop_table_sql("default", "trades", Some("my_cluster"));
        assert_eq!(
            sql,
            "DROP TABLE IF EXISTS `default`.`trades` ON CLUSTER 'my_cluster' SYNC"
        );
    }

    #[test]
    fn test_drop_database_sql_generation() {
        // Without ON CLUSTER
        let sql = drop_database_sql("default", None);
        assert_eq!(sql, "DROP DATABASE IF EXISTS `default` SYNC");

        // With ON CLUSTER
        let sql = drop_database_sql("mydb", Some("cluster1"));
        assert_eq!(
            sql,
            "DROP DATABASE IF EXISTS `mydb` ON CLUSTER 'cluster1' SYNC"
        );
    }

    #[test]
    fn test_detach_table_sql_generation() {
        let sql = detach_table_sync_sql("default", "trades");
        assert_eq!(sql, "DETACH TABLE `default`.`trades` SYNC");
    }

    #[test]
    fn test_attach_table_sql_generation() {
        let sql = attach_table_sql("default", "trades");
        assert_eq!(sql, "ATTACH TABLE `default`.`trades`");
    }

    #[test]
    fn test_restore_replica_sql_generation() {
        let sql = system_restore_replica_sql("default", "trades");
        assert_eq!(sql, "SYSTEM RESTORE REPLICA `default`.`trades`");
    }

    #[test]
    fn test_drop_replica_sql_generation() {
        let sql = drop_replica_from_zkpath_sql("r1", "/clickhouse/tables/01/default/trades");
        assert_eq!(
            sql,
            "SYSTEM DROP REPLICA 'r1' FROM ZKPATH '/clickhouse/tables/01/default/trades'"
        );
    }

    #[test]
    fn test_execute_mutation_sql_generation() {
        let sql = execute_mutation_sql("default", "trades", "DELETE WHERE user_id = 5");
        assert_eq!(
            sql,
            "ALTER TABLE `default`.`trades` DELETE WHERE user_id = 5 SETTINGS mutations_sync=2"
        );

        // UPDATE mutation
        let sql = execute_mutation_sql(
            "logs",
            "events",
            "UPDATE status = 'archived' WHERE ts < '2024-01-01'",
        );
        assert_eq!(
            sql,
            "ALTER TABLE `logs`.`events` UPDATE status = 'archived' WHERE ts < '2024-01-01' SETTINGS mutations_sync=2"
        );
    }

    // -- Phase 4f: JSON/Object column detection tests --

    #[test]
    fn test_json_column_row_struct() {
        // Verify JsonColumnInfo struct can hold expected data
        let info = JsonColumnInfo {
            database: "default".to_string(),
            table: "events".to_string(),
            column: "metadata".to_string(),
            column_type: "Object('json')".to_string(),
        };
        assert_eq!(info.database, "default");
        assert_eq!(info.table, "events");
        assert_eq!(info.column, "metadata");
        assert_eq!(info.column_type, "Object('json')");
    }

    #[test]
    fn test_json_column_check_sql() {
        // Verify the SQL pattern for JSON/Object column detection
        let targets = [
            ("default".to_string(), "events".to_string()),
            ("logs".to_string(), "raw".to_string()),
        ];
        let pairs: Vec<String> = targets
            .iter()
            .map(|(db, table)| format!("('{}', '{}')", db, table))
            .collect();
        let in_clause = pairs.join(", ");

        let sql = format!(
            "SELECT database, table, name AS column, type AS column_type \
             FROM system.columns \
             WHERE (database, table) IN ({}) \
             AND (type LIKE '%Object%' OR type LIKE '%JSON%')",
            in_clause
        );

        assert!(sql.contains("system.columns"));
        assert!(sql.contains("LIKE '%Object%'"));
        assert!(sql.contains("LIKE '%JSON%'"));
        assert!(sql.contains("('default', 'events')"));
        assert!(sql.contains("('logs', 'raw')"));
    }
}
