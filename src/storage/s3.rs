use anyhow::{Context, Result};
use aws_sdk_s3::config::Region;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{Delete, ObjectIdentifier, ServerSideEncryption};
use chrono::{DateTime, Utc};
use tracing::{debug, info};

use crate::config::S3Config;

/// Metadata about an S3 object returned by list operations.
#[derive(Debug, Clone)]
pub struct S3Object {
    pub key: String,
    pub size: i64,
    pub last_modified: Option<DateTime<Utc>>,
}

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
    /// S3 storage class for new objects.
    storage_class: String,
    /// Server-side encryption type ("", "AES256", "aws:kms").
    sse: String,
    /// KMS key ID for aws:kms encryption.
    sse_kms_key_id: String,
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
        let mut loader = aws_config::from_env().region(Region::new(config.region.clone()));

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
                None, // session token
                None, // expiry
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
            storage_class: config.storage_class.clone(),
            sse: config.sse.clone(),
            sse_kms_key_id: config.sse_kms_key_id.clone(),
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

    // -- Key helpers --

    /// Prepend the configured prefix to a relative key.
    ///
    /// If the prefix is empty, returns the key as-is. Otherwise, ensures
    /// a single `/` separator between prefix and key.
    pub fn full_key(&self, relative_key: &str) -> String {
        if self.prefix.is_empty() {
            return relative_key.to_string();
        }
        let prefix = self.prefix.trim_end_matches('/');
        format!("{}/{}", prefix, relative_key)
    }

    // -- PUT operations --

    /// Upload an object to S3 with the configured storage class and encryption.
    ///
    /// The `key` is relative to the configured prefix (prefix is prepended).
    pub async fn put_object(&self, key: &str, body: Vec<u8>) -> Result<()> {
        self.put_object_with_options(key, body, None).await
    }

    /// Upload an object to S3 with optional content type.
    ///
    /// The `key` is relative to the configured prefix (prefix is prepended).
    pub async fn put_object_with_options(
        &self,
        key: &str,
        body: Vec<u8>,
        content_type: Option<&str>,
    ) -> Result<()> {
        let full_key = self.full_key(key);
        let size = body.len();

        debug!(
            key = %full_key,
            size = size,
            "Uploading object to S3"
        );

        let mut req = self
            .inner
            .put_object()
            .bucket(&self.bucket)
            .key(&full_key)
            .body(ByteStream::from(body));

        // Apply storage class
        if !self.storage_class.is_empty() {
            let sc: aws_sdk_s3::types::StorageClass = self.storage_class.as_str().into();
            req = req.storage_class(sc);
        }

        // Apply server-side encryption
        if self.sse == "aws:kms" {
            req = req.server_side_encryption(ServerSideEncryption::AwsKms);
            if !self.sse_kms_key_id.is_empty() {
                req = req.ssekms_key_id(&self.sse_kms_key_id);
            }
        } else if self.sse == "AES256" {
            req = req.server_side_encryption(ServerSideEncryption::Aes256);
        }

        // Apply content type
        if let Some(ct) = content_type {
            req = req.content_type(ct);
        }

        req.send()
            .await
            .with_context(|| format!("Failed to upload object: {}", full_key))?;

        debug!(key = %full_key, size = size, "Upload complete");
        Ok(())
    }

    // -- GET operations --

    /// Download a full object from S3 into memory.
    ///
    /// The `key` is relative to the configured prefix (prefix is prepended).
    pub async fn get_object(&self, key: &str) -> Result<Vec<u8>> {
        let full_key = self.full_key(key);

        debug!(key = %full_key, "Downloading object from S3");

        let resp = self
            .inner
            .get_object()
            .bucket(&self.bucket)
            .key(&full_key)
            .send()
            .await
            .with_context(|| format!("Failed to download object: {}", full_key))?;

        let body = resp
            .body
            .collect()
            .await
            .with_context(|| format!("Failed to read body of object: {}", full_key))?;

        let bytes = body.into_bytes().to_vec();
        debug!(key = %full_key, size = bytes.len(), "Download complete");
        Ok(bytes)
    }

    /// Download an object from S3 as a streaming body.
    ///
    /// The `key` is relative to the configured prefix (prefix is prepended).
    pub async fn get_object_stream(&self, key: &str) -> Result<ByteStream> {
        let full_key = self.full_key(key);

        debug!(key = %full_key, "Getting object stream from S3");

        let resp = self
            .inner
            .get_object()
            .bucket(&self.bucket)
            .key(&full_key)
            .send()
            .await
            .with_context(|| format!("Failed to get object stream: {}", full_key))?;

        Ok(resp.body)
    }

    // -- LIST operations --

    /// List common prefixes (directories) under the given prefix with a delimiter.
    ///
    /// The `prefix` is relative to the configured prefix.
    pub async fn list_common_prefixes(
        &self,
        prefix: &str,
        delimiter: &str,
    ) -> Result<Vec<String>> {
        let full_prefix = self.full_key(prefix);
        let mut prefixes = Vec::new();
        let mut continuation_token: Option<String> = None;

        loop {
            let mut req = self
                .inner
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(&full_prefix)
                .delimiter(delimiter);

            if let Some(token) = &continuation_token {
                req = req.continuation_token(token);
            }

            let resp = req
                .send()
                .await
                .with_context(|| format!("Failed to list prefixes under: {}", full_prefix))?;

            for cp in resp.common_prefixes() {
                if let Some(p) = cp.prefix() {
                    prefixes.push(p.to_string());
                }
            }

            if resp.is_truncated() == Some(true) {
                continuation_token = resp.next_continuation_token().map(|s| s.to_string());
            } else {
                break;
            }
        }

        Ok(prefixes)
    }

    /// List all objects under the given prefix.
    ///
    /// The `prefix` is relative to the configured prefix. Handles pagination.
    pub async fn list_objects(&self, prefix: &str) -> Result<Vec<S3Object>> {
        let full_prefix = self.full_key(prefix);
        let mut objects = Vec::new();
        let mut continuation_token: Option<String> = None;

        loop {
            let mut req = self
                .inner
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(&full_prefix);

            if let Some(token) = &continuation_token {
                req = req.continuation_token(token);
            }

            let resp = req
                .send()
                .await
                .with_context(|| format!("Failed to list objects under: {}", full_prefix))?;

            for obj in resp.contents() {
                let key = obj.key().unwrap_or_default().to_string();
                let size = obj.size().unwrap_or(0);
                let last_modified = obj.last_modified().and_then(|dt| {
                    let secs = dt.secs();
                    DateTime::from_timestamp(secs, dt.subsec_nanos())
                });

                objects.push(S3Object {
                    key,
                    size,
                    last_modified,
                });
            }

            if resp.is_truncated() == Some(true) {
                continuation_token = resp.next_continuation_token().map(|s| s.to_string());
            } else {
                break;
            }
        }

        Ok(objects)
    }

    // -- DELETE operations --

    /// Delete a single object from S3.
    ///
    /// The `key` is relative to the configured prefix.
    pub async fn delete_object(&self, key: &str) -> Result<()> {
        let full_key = self.full_key(key);

        debug!(key = %full_key, "Deleting object from S3");

        self.inner
            .delete_object()
            .bucket(&self.bucket)
            .key(&full_key)
            .send()
            .await
            .with_context(|| format!("Failed to delete object: {}", full_key))?;

        Ok(())
    }

    /// Delete multiple objects from S3 in batches of 1000.
    ///
    /// The `keys` are relative to the configured prefix.
    pub async fn delete_objects(&self, keys: Vec<String>) -> Result<()> {
        if keys.is_empty() {
            return Ok(());
        }

        // S3 DeleteObjects supports max 1000 objects per request
        for chunk in keys.chunks(1000) {
            let identifiers: Vec<ObjectIdentifier> = chunk
                .iter()
                .map(|key| {
                    let full_key = self.full_key(key);
                    ObjectIdentifier::builder()
                        .key(full_key)
                        .build()
                        .expect("ObjectIdentifier key is required")
                })
                .collect();

            let delete = Delete::builder()
                .set_objects(Some(identifiers))
                .build()
                .context("Failed to build Delete request")?;

            debug!(
                count = chunk.len(),
                "Batch deleting objects from S3"
            );

            self.inner
                .delete_objects()
                .bucket(&self.bucket)
                .delete(delete)
                .send()
                .await
                .context("Failed to batch delete objects")?;
        }

        Ok(())
    }

    // -- HEAD operations --

    /// Check if an object exists and return its size.
    ///
    /// Returns `Some(size)` if the object exists, `None` if not found.
    /// The `key` is relative to the configured prefix.
    pub async fn head_object(&self, key: &str) -> Result<Option<u64>> {
        let full_key = self.full_key(key);

        debug!(key = %full_key, "Checking object existence in S3");

        match self
            .inner
            .head_object()
            .bucket(&self.bucket)
            .key(&full_key)
            .send()
            .await
        {
            Ok(resp) => {
                let size = resp.content_length().unwrap_or(0) as u64;
                Ok(Some(size))
            }
            Err(err) => {
                // Check if it's a 404 Not Found
                let service_err = err.into_service_error();
                if service_err.is_not_found() {
                    Ok(None)
                } else {
                    Err(anyhow::anyhow!(
                        "Failed to head object {}: {}",
                        full_key,
                        service_err
                    ))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[test]
    fn test_full_key_with_prefix() {
        let client = mock_s3_client("my-bucket", "chbackup");
        assert_eq!(
            client.full_key("backup/metadata.json"),
            "chbackup/backup/metadata.json"
        );
    }

    #[test]
    fn test_full_key_with_trailing_slash_prefix() {
        let client = mock_s3_client("my-bucket", "chbackup/");
        assert_eq!(
            client.full_key("backup/metadata.json"),
            "chbackup/backup/metadata.json"
        );
    }

    #[test]
    fn test_full_key_empty_prefix() {
        let client = mock_s3_client("my-bucket", "");
        assert_eq!(
            client.full_key("backup/metadata.json"),
            "backup/metadata.json"
        );
    }

    #[test]
    fn test_full_key_nested_prefix() {
        let client = mock_s3_client("my-bucket", "prod/region1/chbackup");
        assert_eq!(
            client.full_key("daily/metadata.json"),
            "prod/region1/chbackup/daily/metadata.json"
        );
    }

    /// Create a minimal S3Client for unit testing (no real AWS connection).
    /// Only the bucket/prefix fields are meaningful for key computation tests.
    fn mock_s3_client(bucket: &str, prefix: &str) -> S3Client {
        let config = S3Config {
            bucket: bucket.to_string(),
            region: "us-east-1".to_string(),
            ..S3Config::default()
        };

        // Build a minimal AWS S3 client config (won't make real calls)
        let s3_config = aws_sdk_s3::config::Builder::new()
            .behavior_version_latest()
            .region(Region::new("us-east-1"))
            .build();
        let inner = aws_sdk_s3::Client::from_conf(s3_config);

        S3Client {
            inner,
            bucket: config.bucket,
            prefix: prefix.to_string(),
            storage_class: "STANDARD".to_string(),
            sse: String::new(),
            sse_kms_key_id: String::new(),
        }
    }
}
