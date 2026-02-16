use anyhow::{Context, Result};
use tracing::info;

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
}
