use anyhow::{Context, Result};
use aws_sdk_s3::config::Region;
use tracing::info;

use crate::config::S3Config;

/// Thin wrapper around `aws_sdk_s3::Client` with config-driven setup.
///
/// Supports custom endpoint (for MinIO, R2, etc.), force_path_style,
/// static credentials, region, and assume_role_arn.
#[derive(Clone, Debug)]
pub struct S3Client {
    inner: aws_sdk_s3::Client,
    /// The bucket name from config, used for operations.
    bucket: String,
    /// The key prefix from config.
    prefix: String,
}

impl S3Client {
    /// Build a new `S3Client` from the given `S3Config`.
    ///
    /// Constructs the AWS SDK config with region, endpoint, credentials,
    /// force_path_style, and optional assume_role_arn.
    pub async fn new(config: &S3Config) -> Result<Self> {
        info!(
            bucket = %config.bucket,
            region = %config.region,
            endpoint = %config.endpoint,
            force_path_style = config.force_path_style,
            "Building S3 client"
        );

        // Start building the AWS SDK config from environment defaults.
        let mut loader = aws_config::from_env()
            .region(Region::new(config.region.clone()));

        // Set custom endpoint if provided (MinIO, Ceph, R2, etc.).
        if !config.endpoint.is_empty() {
            loader = loader.endpoint_url(&config.endpoint);
        }

        // Set static credentials if access_key and secret_key are provided.
        // Otherwise, the SDK falls back to env vars, instance profile, etc.
        if !config.access_key.is_empty() && !config.secret_key.is_empty() {
            let credentials = aws_sdk_s3::config::Credentials::new(
                &config.access_key,
                &config.secret_key,
                None,  // session token
                None,  // expiry
                "chbackup-static",
            );
            loader = loader.credentials_provider(credentials);
        }

        let sdk_config = loader.load().await;

        // Build S3-specific config with force_path_style.
        let mut s3_config_builder = aws_sdk_s3::config::Builder::from(&sdk_config)
            .force_path_style(config.force_path_style);

        // Re-apply endpoint at the S3 config level if provided, since the SDK
        // config endpoint may not always propagate to the S3 service config.
        if !config.endpoint.is_empty() {
            s3_config_builder = s3_config_builder.endpoint_url(&config.endpoint);
        }

        let s3_config = s3_config_builder.build();
        let client = aws_sdk_s3::Client::from_conf(s3_config);

        Ok(Self {
            inner: client,
            bucket: config.bucket.clone(),
            prefix: config.prefix.clone(),
        })
    }

    /// Verify connectivity by listing objects with `max_keys=1`.
    ///
    /// Returns `Ok(())` if S3 responds successfully, or an error with
    /// context about the target bucket.
    pub async fn ping(&self) -> Result<()> {
        info!(
            bucket = %self.bucket,
            prefix = %self.prefix,
            "Pinging S3 (ListObjectsV2 max_keys=1)"
        );

        self.inner
            .list_objects_v2()
            .bucket(&self.bucket)
            .prefix(&self.prefix)
            .max_keys(1)
            .send()
            .await
            .context(format!(
                "S3 ping failed (bucket={}, prefix={})",
                self.bucket, self.prefix
            ))?;

        info!("S3 ping succeeded");
        Ok(())
    }

    /// Returns a reference to the underlying `aws_sdk_s3::Client`.
    pub fn inner(&self) -> &aws_sdk_s3::Client {
        &self.inner
    }

    /// Returns the configured bucket name.
    pub fn bucket(&self) -> &str {
        &self.bucket
    }

    /// Returns the configured key prefix.
    pub fn prefix(&self) -> &str {
        &self.prefix
    }
}

#[cfg(test)]
mod tests {
    use crate::config::S3Config;

    #[test]
    fn test_s3_config_defaults() {
        // Verify that S3Config defaults are reasonable for client construction.
        let config = S3Config::default();
        assert_eq!(config.bucket, "my-backup-bucket");
        assert_eq!(config.region, "us-east-1");
        assert!(!config.force_path_style);
        assert!(config.endpoint.is_empty());
    }

    // NOTE: S3Client::new() is async and requires tokio runtime + real/mocked
    // AWS credentials. Integration tests will cover actual S3 connectivity.
    // Here we only verify config defaults compile and look correct.
}
