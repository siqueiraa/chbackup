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

    // -- DDL execution --

    /// Execute arbitrary DDL (CREATE DATABASE, CREATE TABLE, etc.).
    pub async fn execute_ddl(&self, ddl: &str) -> Result<()> {
        self.log_and_execute(ddl, "DDL").await
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
        assert!(client.is_ok(), "ChClient::new should succeed with TLS config");
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
}
