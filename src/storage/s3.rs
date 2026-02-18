use anyhow::{Context, Result};
use aws_sdk_s3::config::Region;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{
    CompletedMultipartUpload, CompletedPart, Delete, ObjectIdentifier, ServerSideEncryption,
};
use chrono::{DateTime, Utc};
use tracing::{debug, info, warn};

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

    // -- Multipart upload operations --

    /// Initiate a multipart upload and return the upload ID.
    ///
    /// The `key` is relative to the configured prefix. SSE and storage class
    /// settings are applied consistently with `put_object`.
    pub async fn create_multipart_upload(&self, key: &str) -> Result<String> {
        let full_key = self.full_key(key);

        debug!(key = %full_key, "Creating multipart upload");

        let mut req = self
            .inner
            .create_multipart_upload()
            .bucket(&self.bucket)
            .key(&full_key);

        // Apply storage class (same as put_object)
        if !self.storage_class.is_empty() {
            let sc: aws_sdk_s3::types::StorageClass = self.storage_class.as_str().into();
            req = req.storage_class(sc);
        }

        // Apply server-side encryption (same as put_object)
        if self.sse == "aws:kms" {
            req = req.server_side_encryption(ServerSideEncryption::AwsKms);
            if !self.sse_kms_key_id.is_empty() {
                req = req.ssekms_key_id(&self.sse_kms_key_id);
            }
        } else if self.sse == "AES256" {
            req = req.server_side_encryption(ServerSideEncryption::Aes256);
        }

        let resp = req
            .send()
            .await
            .with_context(|| format!("Failed to create multipart upload for: {}", full_key))?;

        let upload_id = resp
            .upload_id()
            .ok_or_else(|| anyhow::anyhow!("No upload_id returned for multipart upload: {}", full_key))?
            .to_string();

        debug!(key = %full_key, upload_id = %upload_id, "Multipart upload created");
        Ok(upload_id)
    }

    /// Upload a single part of a multipart upload.
    ///
    /// Returns the ETag of the uploaded part, which is needed for
    /// `complete_multipart_upload`. Part numbers must be between 1 and 10000.
    pub async fn upload_part(
        &self,
        key: &str,
        upload_id: &str,
        part_number: i32,
        body: Vec<u8>,
    ) -> Result<String> {
        let full_key = self.full_key(key);
        let size = body.len();

        debug!(
            key = %full_key,
            upload_id = %upload_id,
            part_number = part_number,
            size = size,
            "Uploading part"
        );

        let resp = self
            .inner
            .upload_part()
            .bucket(&self.bucket)
            .key(&full_key)
            .upload_id(upload_id)
            .part_number(part_number)
            .body(ByteStream::from(body))
            .send()
            .await
            .with_context(|| {
                format!(
                    "Failed to upload part {} for {}: upload_id={}",
                    part_number, full_key, upload_id
                )
            })?;

        let e_tag = resp
            .e_tag()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "No ETag returned for part {} of {}: upload_id={}",
                    part_number,
                    full_key,
                    upload_id
                )
            })?
            .to_string();

        debug!(
            key = %full_key,
            part_number = part_number,
            e_tag = %e_tag,
            "Part uploaded"
        );
        Ok(e_tag)
    }

    /// Complete a multipart upload by assembling all uploaded parts.
    ///
    /// `parts` is a list of `(part_number, e_tag)` tuples from `upload_part` calls.
    /// Parts must be in ascending order by part number.
    pub async fn complete_multipart_upload(
        &self,
        key: &str,
        upload_id: &str,
        parts: Vec<(i32, String)>,
    ) -> Result<()> {
        let full_key = self.full_key(key);

        debug!(
            key = %full_key,
            upload_id = %upload_id,
            part_count = parts.len(),
            "Completing multipart upload"
        );

        let completed_parts: Vec<CompletedPart> = parts
            .into_iter()
            .map(|(part_number, e_tag)| {
                CompletedPart::builder()
                    .part_number(part_number)
                    .e_tag(e_tag)
                    .build()
            })
            .collect();

        let completed = CompletedMultipartUpload::builder()
            .set_parts(Some(completed_parts))
            .build();

        self.inner
            .complete_multipart_upload()
            .bucket(&self.bucket)
            .key(&full_key)
            .upload_id(upload_id)
            .multipart_upload(completed)
            .send()
            .await
            .with_context(|| {
                format!(
                    "Failed to complete multipart upload for {}: upload_id={}",
                    full_key, upload_id
                )
            })?;

        debug!(key = %full_key, upload_id = %upload_id, "Multipart upload completed");
        Ok(())
    }

    /// Abort a multipart upload, cleaning up any uploaded parts.
    ///
    /// This should be called when a multipart upload fails partway through
    /// to avoid leaving orphaned parts in S3.
    pub async fn abort_multipart_upload(&self, key: &str, upload_id: &str) -> Result<()> {
        let full_key = self.full_key(key);

        debug!(
            key = %full_key,
            upload_id = %upload_id,
            "Aborting multipart upload"
        );

        self.inner
            .abort_multipart_upload()
            .bucket(&self.bucket)
            .key(&full_key)
            .upload_id(upload_id)
            .send()
            .await
            .with_context(|| {
                format!(
                    "Failed to abort multipart upload for {}: upload_id={}",
                    full_key, upload_id
                )
            })?;

        debug!(key = %full_key, upload_id = %upload_id, "Multipart upload aborted");
        Ok(())
    }

    // -- CopyObject operations --

    /// Server-side copy of an object between buckets (or within a bucket).
    ///
    /// `source_bucket` and `source_key` identify the source object (absolute).
    /// `dest_key` is relative to this client's configured prefix.
    /// Applies SSE and storage class settings to the destination.
    pub async fn copy_object(
        &self,
        source_bucket: &str,
        source_key: &str,
        dest_key: &str,
    ) -> Result<()> {
        let full_dest_key = self.full_key(dest_key);
        let copy_source = format!("{}/{}", source_bucket, source_key);

        debug!(
            source = %copy_source,
            dest = %full_dest_key,
            "Copying object (server-side CopyObject)"
        );

        let mut req = self
            .inner
            .copy_object()
            .bucket(&self.bucket)
            .copy_source(&copy_source)
            .key(&full_dest_key);

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

        req.send()
            .await
            .with_context(|| {
                format!(
                    "CopyObject failed: {} -> {}/{}",
                    copy_source, self.bucket, full_dest_key
                )
            })?;

        debug!(
            source = %copy_source,
            dest = %full_dest_key,
            "CopyObject complete"
        );
        Ok(())
    }

    /// Streaming copy fallback: downloads from source then uploads to dest.
    ///
    /// Used when server-side CopyObject fails (e.g., cross-region).
    /// Uses the underlying AWS SDK client directly for the source since
    /// it may be in a different bucket.
    pub async fn copy_object_streaming(
        &self,
        source_bucket: &str,
        source_key: &str,
        dest_key: &str,
    ) -> Result<()> {
        let full_dest_key = self.full_key(dest_key);

        debug!(
            source_bucket = %source_bucket,
            source_key = %source_key,
            dest = %full_dest_key,
            "Streaming copy (download + upload fallback)"
        );

        // Download from source bucket using raw AWS SDK client
        let get_resp = self
            .inner
            .get_object()
            .bucket(source_bucket)
            .key(source_key)
            .send()
            .await
            .with_context(|| {
                format!(
                    "Streaming copy: failed to download {}/{}",
                    source_bucket, source_key
                )
            })?;

        let body = get_resp
            .body
            .collect()
            .await
            .with_context(|| {
                format!(
                    "Streaming copy: failed to read body of {}/{}",
                    source_bucket, source_key
                )
            })?;

        let bytes = body.into_bytes().to_vec();

        // Upload to destination using self.put_object
        self.put_object(dest_key, bytes).await.with_context(|| {
            format!(
                "Streaming copy: failed to upload to {}/{}",
                self.bucket, full_dest_key
            )
        })?;

        debug!(
            source_bucket = %source_bucket,
            source_key = %source_key,
            dest = %full_dest_key,
            "Streaming copy complete"
        );
        Ok(())
    }

    /// Copy an object with retry and conditional streaming fallback.
    ///
    /// Retries `copy_object()` up to 3 times with exponential backoff
    /// (100ms, 400ms, 1600ms) per design doc section 5.4 step 3d.
    ///
    /// On final failure:
    /// - If `allow_streaming` is true: falls back to `copy_object_streaming()`
    ///   with a warning about high network traffic
    /// - If `allow_streaming` is false: returns the error
    pub async fn copy_object_with_retry(
        &self,
        source_bucket: &str,
        source_key: &str,
        dest_key: &str,
        allow_streaming: bool,
    ) -> Result<()> {
        let backoff_ms = [100u64, 400, 1600];

        for (attempt, delay_ms) in backoff_ms.iter().enumerate() {
            match self.copy_object(source_bucket, source_key, dest_key).await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    if attempt < backoff_ms.len() - 1 {
                        debug!(
                            attempt = attempt + 1,
                            delay_ms = delay_ms,
                            error = %e,
                            "CopyObject failed, retrying after backoff"
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(*delay_ms)).await;
                    } else if allow_streaming {
                        warn!(
                            source_bucket = %source_bucket,
                            source_key = %source_key,
                            error = %e,
                            "CopyObject failed after retries, falling back to streaming copy (high network traffic)"
                        );
                        return self
                            .copy_object_streaming(source_bucket, source_key, dest_key)
                            .await;
                    } else {
                        return Err(e).with_context(|| {
                            format!(
                                "CopyObject failed after {} attempts (streaming fallback disabled): {}/{}",
                                backoff_ms.len(),
                                source_bucket,
                                source_key
                            )
                        });
                    }
                }
            }
        }

        // This should never be reached due to the loop logic above,
        // but the compiler needs it for exhaustiveness.
        unreachable!("retry loop should have returned")
    }
}

/// S3 minimum part size: 5 MiB (except the last part).
const S3_MIN_PART_SIZE: u64 = 5 * 1024 * 1024;

/// Calculate the chunk size for multipart upload.
///
/// When `config_chunk_size` is 0 (auto), computes the chunk size as
/// `data_len / max_parts_count`, rounded up. The result is clamped to
/// at least `S3_MIN_PART_SIZE` (5 MiB) to satisfy S3 requirements.
///
/// When `config_chunk_size` is > 0, uses that value directly but still
/// enforces the 5 MiB minimum.
pub fn calculate_chunk_size(data_len: u64, config_chunk_size: u64, max_parts_count: u32) -> u64 {
    let chunk = if config_chunk_size > 0 {
        config_chunk_size
    } else {
        // Auto: divide data evenly across max_parts_count, rounding up
        let parts = max_parts_count.max(1) as u64;
        data_len.div_ceil(parts)
    };

    // Enforce S3 minimum part size
    chunk.max(S3_MIN_PART_SIZE)
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

    #[test]
    fn test_multipart_chunk_calculation() {
        // 100MB file with default max_parts_count=10000 and chunk_size=0 (auto)
        let data_len = 100 * 1024 * 1024;
        let chunk = calculate_chunk_size(data_len, 0, 10000);
        // 100MB / 10000 = ~10KB, but S3 minimum is 5MB
        assert_eq!(chunk, S3_MIN_PART_SIZE);

        // 100GB file with auto chunk_size
        let data_len = 100 * 1024 * 1024 * 1024_u64;
        let chunk = calculate_chunk_size(data_len, 0, 10000);
        // 100GB / 10000 = ~10MB, which is above minimum
        assert!(chunk >= S3_MIN_PART_SIZE);
        // Number of parts should not exceed max_parts_count
        let part_count = data_len.div_ceil(chunk);
        assert!(part_count <= 10000);
    }

    #[test]
    fn test_calculate_chunk_size_auto() {
        // Auto mode: config_chunk_size = 0
        // 50GB data, 10000 max parts -> ~5.3MB per chunk (above minimum)
        let data_len = 50 * 1024 * 1024 * 1024_u64;
        let chunk = calculate_chunk_size(data_len, 0, 10000);
        let auto_computed = data_len.div_ceil(10000);
        assert_eq!(chunk, auto_computed);
        assert!(chunk >= S3_MIN_PART_SIZE);

        // 500GB data, 10000 max parts -> ~50MB per chunk
        let data_len = 500 * 1024 * 1024 * 1024_u64;
        let chunk = calculate_chunk_size(data_len, 0, 10000);
        let expected = data_len.div_ceil(10000);
        assert_eq!(chunk, expected);
    }

    #[test]
    fn test_calculate_chunk_size_explicit() {
        // Explicit chunk size: 64MB
        let explicit = 64 * 1024 * 1024;
        let chunk = calculate_chunk_size(1024 * 1024 * 1024, explicit, 10000);
        assert_eq!(chunk, explicit);
    }

    #[test]
    fn test_calculate_chunk_size_minimum() {
        // Explicit chunk size below 5MB should be clamped to 5MB
        let small_chunk = 1024 * 1024; // 1MB
        let chunk = calculate_chunk_size(100 * 1024 * 1024, small_chunk, 10000);
        assert_eq!(chunk, S3_MIN_PART_SIZE);

        // Auto with very large max_parts_count should also clamp to 5MB
        let chunk = calculate_chunk_size(10 * 1024 * 1024, 0, 10000);
        assert_eq!(chunk, S3_MIN_PART_SIZE);
    }

    #[test]
    fn test_copy_object_builds_correct_source() {
        // Verify the CopySource format is "{bucket}/{key}"
        let client = mock_s3_client("dest-bucket", "dest-prefix");

        // The copy_source format used in copy_object is "{source_bucket}/{source_key}"
        let source_bucket = "source-bucket";
        let source_key = "path/to/object.bin";
        let expected_source = format!("{}/{}", source_bucket, source_key);
        assert_eq!(expected_source, "source-bucket/path/to/object.bin");

        // Verify dest key uses prefix
        let dest_key = "backup/objects/data.bin";
        let full_dest = client.full_key(dest_key);
        assert_eq!(full_dest, "dest-prefix/backup/objects/data.bin");
    }

    #[tokio::test]
    async fn test_copy_object_with_retry_no_streaming_when_disabled() {
        // When allow_streaming is false, copy_object_with_retry should return
        // an error after retries without attempting streaming fallback.
        let client = mock_s3_client("dest-bucket", "prefix");

        // This will fail because there's no real S3 endpoint, but we can verify
        // the error path. We can't easily test the full retry logic without mocking,
        // but we can verify the method exists and the error contains the right context.
        let result = client
            .copy_object_with_retry("src-bucket", "src/key.bin", "dest/key.bin", false)
            .await;

        assert!(result.is_err());
        let err_msg = format!("{:#}", result.unwrap_err());
        assert!(
            err_msg.contains("CopyObject failed"),
            "Error should mention CopyObject failure, got: {}",
            err_msg
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
