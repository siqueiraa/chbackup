use anyhow::{bail, Context, Result};
use aws_config::sts::AssumeRoleProvider;
use aws_sdk_s3::config::retry::RetryConfig as SdkRetryConfig;
use aws_sdk_s3::config::{Credentials, Region, StalledStreamProtectionConfig};
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{
    CompletedMultipartUpload, CompletedPart, Delete, ObjectIdentifier, ServerSideEncryption,
};
use chrono::{DateTime, Utc};
use std::fmt::Write as _;
use tracing::{debug, error, info, warn};

use crate::config::S3Config;

/// Percent-encode an S3 key for use in a CopySource header.
///
/// AWS S3 requires the CopySource value (`{bucket}/{key}`) to be URL-encoded.
/// `/` is preserved as a path separator; all other non-unreserved characters
/// (outside A-Z a-z 0-9 `-` `_` `.` `~`) are percent-encoded.
///
/// Reference: <https://docs.aws.amazon.com/AmazonS3/latest/API/API_CopyObject.html>
fn percent_encode_s3_key(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    for byte in key.bytes() {
        match byte {
            // RFC 3986 unreserved + '/' (path separator in S3 keys)
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(byte as char);
            }
            _ => {
                write!(out, "%{:02X}", byte).unwrap();
            }
        }
    }
    out
}

/// Retry configuration for S3 operations.
///
/// Bundles retry count, base delay, and jitter factor into a single value
/// to avoid passing many individual parameters. Constructed from
/// `crate::config::effective_retries()`.
#[derive(Debug, Clone, Copy)]
pub struct RetryConfig {
    /// Maximum number of retry attempts (0 = no retries, single attempt).
    pub max_retries: u32,
    /// Base delay between retries in seconds (exponentially increases).
    pub base_delay_secs: u64,
    /// Jitter factor (0.0-1.0) applied to each retry delay.
    pub jitter_factor: f64,
}

/// Which deadline class an S3 operation falls into.
///
/// The split is by **request shape**, not by convenience: a bodyless request's total time is
/// bounded by the service, while a body-carrying one is bounded by how much data is moving.
/// Applying a wall-clock deadline to the latter would kill legitimately slow large transfers,
/// which is why they are absent here and left to the SDK's throughput-based stalled-stream
/// protection instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum S3OpClass {
    /// Bodyless: head, list, delete, and the multipart lifecycle calls.
    Metadata,
    /// Bodyless but proportional to the bytes the service moves server-side: CopyObject,
    /// UploadPartCopy, CompleteMultipartUpload.
    Copy,
}

/// Resolved deadline settings, derived once from [`S3Config`].
#[derive(Debug, Clone, Copy)]
pub struct S3Timeouts {
    /// Base per-request deadline in seconds. 0 disables every S3 deadline.
    pub request_timeout_secs: u64,
    /// Assumed floor throughput for server-side copies, in bytes/second. 0 = no size allowance.
    pub copy_min_bytes_per_second: u64,
}

/// Best-effort cleanup deadline, deliberately fixed and not configurable.
///
/// Cleanup must never use `request_timeout`, because `"0s"` is a supported value that disables
/// deadlines -- which would leave an abort able to hang forever, i.e. exactly the failure this
/// module exists to prevent. A cleanup that gives up is strictly better than one that parks.
pub const CLEANUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Compute the deadline for one S3 request, or `None` when deadlines are disabled.
///
/// The `Copy` class adds a size allowance because a server-side copy of a multi-GiB range
/// legitimately takes minutes: `request_timeout + bytes / copy_min_bytes_per_second`. When the
/// size is unknown (a preceding HEAD failed) callers pass `None` and get the allowance for the
/// largest object a single CopyObject can legally handle, since falling back to the bare
/// metadata deadline would fail legitimate large copies.
pub fn s3_deadline(
    class: S3OpClass,
    bytes: Option<u64>,
    t: &S3Timeouts,
) -> Option<std::time::Duration> {
    if t.request_timeout_secs == 0 {
        return None;
    }
    let base = std::time::Duration::from_secs(t.request_timeout_secs);
    match class {
        S3OpClass::Metadata => Some(base),
        S3OpClass::Copy => {
            if t.copy_min_bytes_per_second == 0 {
                return Some(base);
            }
            // Unknown size: assume the legal maximum for a single CopyObject.
            let bytes = bytes.unwrap_or(S3Client::COPY_OBJECT_MAX_SIZE);
            let allowance_secs = bytes / t.copy_min_bytes_per_second;
            Some(base.saturating_add(std::time::Duration::from_secs(allowance_secs)))
        }
    }
}

/// How much of a whole-object budget is left.
///
/// - `None` budget (deadlines disabled) -> `None` (no clamp).
/// - Budget exhausted -> `Some(Err(..))`, so the caller can bail with a useful message.
/// - Otherwise `Some(Ok(remaining))`.
///
/// Returning the exhausted case as an error rather than `Duration::ZERO` keeps callers from
/// accidentally issuing a request with a zero deadline that fails with a confusing message.
fn remaining_budget(
    object_deadline: Option<std::time::Instant>,
) -> Option<Result<std::time::Duration>> {
    let deadline = object_deadline?;
    let left = deadline.saturating_duration_since(std::time::Instant::now());
    if left.is_zero() {
        Some(Err(anyhow::anyhow!(
            "{TIMEOUT_ERR_PREFIX}: multipart copy exceeded its whole-object budget"
        )))
    } else {
        Some(Ok(left))
    }
}

/// S3 canned ACL type alias for convenience.
type ObjectCannedAcl = aws_sdk_s3::types::ObjectCannedAcl;

/// Apply SSE, storage class, and ACL options to an S3 request builder.
///
/// All four S3 builder types (`PutObjectFluentBuilder`, `CreateMultipartUploadFluentBuilder`,
/// `CopyObjectFluentBuilder`, `CreateMultipartUploadFluentBuilder` in `copy_object_multipart`)
/// expose the same setter methods for these options. This macro eliminates the
/// ~20-line copy-pasted block that was previously repeated in each call site.
///
/// # Arguments
/// * `$req` - The builder variable to apply options to (must support `.storage_class()`,
///   `.server_side_encryption()`, `.ssekms_key_id()`, and `.acl()` methods)
/// * `$self` - The `S3Client` instance to read config fields from
macro_rules! apply_s3_object_options {
    ($req:expr, $self:expr) => {{
        let mut req = $req;
        if !$self.storage_class.is_empty() {
            let sc: aws_sdk_s3::types::StorageClass = $self.storage_class.as_str().into();
            req = req.storage_class(sc);
        }
        if $self.sse == "aws:kms" {
            req = req.server_side_encryption(ServerSideEncryption::AwsKms);
            if !$self.sse_kms_key_id.is_empty() {
                req = req.ssekms_key_id(&$self.sse_kms_key_id);
            }
        } else if $self.sse == "AES256" {
            req = req.server_side_encryption(ServerSideEncryption::Aes256);
        }
        if !$self.acl.is_empty() {
            let acl: ObjectCannedAcl = $self.acl.as_str().into();
            req = req.acl(acl);
        }
        req
    }};
}

/// Parse an S3 URI like `s3://bucket/prefix/` into (bucket, prefix).
///
/// Returns `(bucket, prefix)`. If the URI does not match `s3://` format,
/// returns the whole string as the prefix with an empty bucket.
/// Stable prefix for a deadline failure, matched by [`is_timeout_error`].
///
/// Kept as a constant so the classifier and the message cannot drift apart.
const TIMEOUT_ERR_PREFIX: &str = "S3 request timed out";

/// Whether an error is one of our own deadline expiries.
///
/// Deliberately matched against the exact prefix we emit rather than a loose substring like
/// "timeout" or "timed out", which would also catch service-reported timeouts and unrelated
/// errors whose context happens to mention one -- the same trap documented for
/// [`is_missing_source_error`] below.
pub fn is_timeout_error(err: &anyhow::Error) -> bool {
    format!("{err:#}").contains(TIMEOUT_ERR_PREFIX)
}

/// Identifying context for an S3 request, used only to build log fields and error messages.
#[derive(Debug, Default, Clone)]
pub struct S3OpCtx {
    pub bucket: String,
    pub key: String,
    pub part_number: Option<i32>,
    pub range: Option<String>,
    pub size: Option<u64>,
}

impl S3OpCtx {
    pub fn new(bucket: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            bucket: bucket.into(),
            key: key.into(),
            ..Default::default()
        }
    }

    pub fn with_part(mut self, part_number: i32, range: impl Into<String>) -> Self {
        self.part_number = Some(part_number);
        self.range = Some(range.into());
        self
    }

    pub fn with_size(mut self, size: u64) -> Self {
        self.size = Some(size);
        self
    }
}

/// Run an S3 request under a deadline.
///
/// Generic over the future's output `T` rather than over `Result`, and deliberately so: callers
/// like `head_object` classify the SDK's own error type (`into_service_error().is_not_found()`)
/// to turn a 404 into `Ok(None)`. Flattening that into `anyhow` here would not compile, and if
/// forced through would convert "object absent" into "head failed" on a path `copy_object`
/// depends on. So: wrap the `send()` future, leave every SDK-error `match` at the call site.
///
/// `None` deadline means unbounded, which is what `s3.request_timeout = "0s"` selects.
///
/// The `warn!` lives here rather than at the call sites so a deadline expiry is always visible
/// even when a caller swallows the error.
pub async fn with_deadline<T>(
    op: &str,
    ctx: &S3OpCtx,
    deadline: Option<std::time::Duration>,
    fut: impl std::future::Future<Output = T>,
) -> Result<T> {
    let Some(deadline) = deadline else {
        return Ok(fut.await);
    };

    let started = std::time::Instant::now();
    match tokio::time::timeout(deadline, fut).await {
        Ok(v) => Ok(v),
        Err(_elapsed) => {
            let elapsed = started.elapsed();
            warn!(
                op = op,
                bucket = %ctx.bucket,
                key = %ctx.key,
                part_number = ctx.part_number,
                range = ctx.range.as_deref().unwrap_or(""),
                size = ctx.size,
                deadline_secs = deadline.as_secs_f64(),
                elapsed_secs = elapsed.as_secs_f64(),
                "S3 request exceeded deadline -- aborting request"
            );
            bail!(
                "{TIMEOUT_ERR_PREFIX}: {op} on {}/{} after {:.1}s (deadline {:.1}s). \
                 Raise s3.request_timeout or s3.copy_min_bytes_per_second, \
                 or set s3.request_timeout: 0s to disable deadlines.",
                ctx.bucket,
                ctx.key,
                elapsed.as_secs_f64(),
                deadline.as_secs_f64()
            )
        }
    }
}

/// Whether an S3 error means the referenced key or bucket does not exist.
///
/// Used to classify a CopyObject failure as permanent so it is not retried. Kept narrow on
/// purpose: matching `404` or `"not found"` would catch transient conditions and unrelated
/// errors whose messages contain those substrings.
pub fn is_missing_source_error(err: &anyhow::Error) -> bool {
    let msg = format!("{err:#}");
    msg.contains("NoSuchKey") || msg.contains("NoSuchBucket")
}

pub fn parse_s3_uri(uri: &str) -> (String, String) {
    let stripped = uri
        .strip_prefix("s3://")
        .or_else(|| uri.strip_prefix("S3://"));

    match stripped {
        Some(rest) => {
            let rest = rest.trim_end_matches('/');
            if let Some(slash_pos) = rest.find('/') {
                let bucket = rest[..slash_pos].to_string();
                let prefix = rest[slash_pos + 1..].to_string();
                (bucket, prefix)
            } else {
                (rest.to_string(), String::new())
            }
        }
        None => {
            // Not an S3 URI -- treat as a plain path prefix
            (String::new(), uri.trim_end_matches('/').to_string())
        }
    }
}

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
    /// S3 storage class for new objects (uppercased).
    storage_class: String,
    /// Server-side encryption type ("", "AES256", "aws:kms").
    sse: String,
    /// KMS key ID for aws:kms encryption.
    sse_kms_key_id: String,
    /// S3 canned ACL to apply to new objects.
    acl: String,
    /// Resolved request deadlines. See [`s3_deadline`].
    timeouts: S3Timeouts,
    /// Configured multipart chunk size (0 = auto).
    chunk_size: u64,
    /// Configured multipart part-count ceiling.
    max_parts_count: u32,
    /// Promote this client's own per-request `debug!` events to `info!` (`s3.debug`).
    log_requests: bool,
}

/// Build an auto-refreshing STS AssumeRole credentials provider.
///
/// The provider is lazy: it calls STS on first use and again whenever the temporary
/// credentials near expiry, so long-running server and watch modes keep working past
/// the (default 1 hour) session lifetime.
///
/// `base_credentials` are the credentials used to call STS itself; when `None` the
/// default AWS credential chain (env vars, instance profile, ...) is used. `endpoint`
/// is applied to the STS client too, so S3-compatible stacks that implement STS on
/// their own endpoint (MinIO) are reachable.
async fn assume_role_provider(
    role_arn: &str,
    region: &str,
    endpoint: Option<&str>,
    base_credentials: Option<Credentials>,
) -> AssumeRoleProvider {
    let mut loader = aws_config::from_env().region(Region::new(region.to_string()));
    if let Some(endpoint) = endpoint {
        loader = loader.endpoint_url(endpoint);
    }
    if let Some(credentials) = base_credentials {
        loader = loader.credentials_provider(credentials);
    }
    let base_config = loader.load().await;

    AssumeRoleProvider::builder(role_arn)
        .session_name("chbackup")
        .region(Region::new(region.to_string()))
        .configure(&base_config)
        .build()
        .await
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

        // Compute the effective endpoint, applying disable_ssl if configured.
        let mut effective_endpoint = if config.disable_ssl && !config.endpoint.is_empty() {
            let rewritten = config.endpoint.replacen("https://", "http://", 1);
            info!("S3 disable_ssl=true: forcing HTTP endpoint");
            rewritten
        } else {
            if config.disable_ssl && config.endpoint.is_empty() {
                warn!(
                    "S3 disable_ssl is true but no endpoint configured; \
                     default AWS endpoints always use HTTPS"
                );
            }
            config.endpoint.clone()
        };

        // Wire disable_cert_verification: force HTTP endpoint to bypass TLS entirely.
        // The AWS SDK for Rust (aws-smithy-http-client v1.1.10) does NOT expose a public
        // API for danger_accept_invalid_certs. The pragmatic fix is to force HTTP when
        // cert verification is disabled, matching Go clickhouse-backup behavior.
        if config.disable_cert_verification {
            if !effective_endpoint.is_empty() {
                effective_endpoint = effective_endpoint.replacen("https://", "http://", 1);
                warn!(
                    "S3 disable_cert_verification=true: forcing HTTP endpoint \
                     (TLS cert verification bypass via HTTP)"
                );
            } else {
                error!(
                    "S3 disable_cert_verification=true but no endpoint configured; \
                     cannot downgrade default AWS HTTPS"
                );
                bail!(
                    "disable_cert_verification requires an explicit endpoint URL \
                     (cannot downgrade default AWS HTTPS)"
                );
            }
        }

        // Start building the AWS SDK config from environment defaults.
        let mut loader = aws_config::from_env().region(Region::new(config.region.clone()));

        // Set custom endpoint if provided (MinIO, Ceph, R2, etc.).
        if !effective_endpoint.is_empty() {
            loader = loader.endpoint_url(&effective_endpoint);
        }

        // Static credentials from config, if provided. Otherwise the SDK falls back to
        // env vars, instance profile, etc.
        let static_credentials = (!config.access_key.is_empty() && !config.secret_key.is_empty())
            .then(|| {
                Credentials::new(
                    &config.access_key,
                    &config.secret_key,
                    None, // session token
                    None, // expiry
                    "chbackup-static",
                )
            });

        // With assume_role_arn set, the credentials on the loader are the refreshing STS
        // provider (which uses the static credentials, if any, to call STS).
        if !config.assume_role_arn.is_empty() {
            info!(assume_role_arn = %config.assume_role_arn, "Assuming IAM role via STS");
            let provider = assume_role_provider(
                &config.assume_role_arn,
                &config.region,
                (!effective_endpoint.is_empty()).then_some(effective_endpoint.as_str()),
                static_credentials,
            )
            .await;
            loader = loader.credentials_provider(provider);
        } else if let Some(credentials) = static_credentials {
            loader = loader.credentials_provider(credentials);
        }

        let sdk_config = loader.load().await;

        // Build S3-specific config with force_path_style.
        let mut s3_config_builder = aws_sdk_s3::config::Builder::from(&sdk_config)
            .force_path_style(config.force_path_style);

        // Re-apply endpoint at the S3 config level if provided, since the SDK
        // config endpoint may not always propagate to the S3 service config.
        if !effective_endpoint.is_empty() {
            s3_config_builder = s3_config_builder.endpoint_url(&effective_endpoint);
        }

        // Make the SDK's protections explicit rather than inherited.
        //
        // Stalled-stream protection is already ON by default (behavior-version-latest), with a
        // 5s grace period applied by the runtime plugin. Setting it here is not a change in
        // behaviour -- it is what makes the value visible and configurable. The grace period MUST
        // be passed explicitly: the builder's own DEFAULT_GRACE_PERIOD is 20s, so
        // `enabled().build()` would silently *loosen* protection from 5s to 20s.
        let grace_secs = crate::config::parse_duration_secs(&config.stalled_stream_grace_period)
            .unwrap_or_else(|e| {
                warn!(
                    value = %config.stalled_stream_grace_period,
                    error = %e,
                    "Invalid s3.stalled_stream_grace_period, falling back to 5s"
                );
                5
            });
        let ssp = if grace_secs == 0 {
            StalledStreamProtectionConfig::disabled()
        } else {
            StalledStreamProtectionConfig::enabled()
                .grace_period(std::time::Duration::from_secs(grace_secs))
                .build()
        };
        s3_config_builder = s3_config_builder.stalled_stream_protection(ssp);

        // SDK-level retries multiply with chbackup's own retry wrappers, so make the factor
        // explicit and smaller than the SDK default of 3. Keeping some SDK retries is still
        // worthwhile: they are far cheaper than a project-level retry, which re-runs the HEAD
        // and restarts an entire multipart copy.
        s3_config_builder = s3_config_builder.retry_config(
            SdkRetryConfig::standard().with_max_attempts(config.sdk_max_attempts.max(1)),
        );

        let s3_config = s3_config_builder.build();
        let client = aws_sdk_s3::Client::from_conf(s3_config);

        let request_timeout_secs = crate::config::parse_duration_secs(&config.request_timeout)
            .unwrap_or_else(|e| {
                warn!(
                    value = %config.request_timeout,
                    error = %e,
                    "Invalid s3.request_timeout, falling back to 60s"
                );
                60
            });
        if request_timeout_secs == 0 {
            warn!(
                "s3.request_timeout is 0: S3 request deadlines are DISABLED, so a stalled \
                 bodyless request (CopyObject, UploadPartCopy, HEAD, LIST) can hang indefinitely"
            );
        } else {
            info!(
                request_timeout_secs = request_timeout_secs,
                copy_min_bytes_per_second = config.copy_min_bytes_per_second,
                stalled_stream_grace_secs = grace_secs,
                sdk_max_attempts = config.sdk_max_attempts,
                "S3 request deadlines active"
            );
        }

        if config.debug {
            // Two distinct mechanisms: raising the aws_* tracing targets (done in
            // logging::init_logging, which is what produces real SDK request/response logs) and
            // promoting this client's own per-request events, which is what log_requests does.
            info!(
                "s3.debug enabled: chbackup S3 request events promoted to info, and the aws_* \
                 tracing targets raised to debug (may print credentials)"
            );
        }

        // Uppercase storage class to match AWS SDK expected format
        // (lowercase values like "standard" produce Unknown SDK variant)
        let storage_class = config.storage_class.to_uppercase();

        Ok(Self {
            inner: client,
            bucket: config.bucket.clone(),
            prefix: config.prefix.clone(),
            storage_class,
            sse: config.sse.clone(),
            sse_kms_key_id: config.sse_kms_key_id.clone(),
            acl: config.acl.clone(),
            timeouts: S3Timeouts {
                request_timeout_secs,
                copy_min_bytes_per_second: config.copy_min_bytes_per_second,
            },
            chunk_size: config.chunk_size,
            max_parts_count: config.max_parts_count,
            log_requests: config.debug,
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

        with_deadline(
            "ListObjectsV2(ping)",
            &S3OpCtx::new(&self.bucket, &self.prefix),
            s3_deadline(S3OpClass::Metadata, None, &self.timeouts),
            self.inner
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(&self.prefix)
                .max_keys(1)
                .send(),
        )
        .await?
        .context(format!(
            "S3 ping failed (bucket={}, prefix={})",
            self.bucket, self.prefix
        ))?;

        info!("S3 ping succeeded");
        Ok(())
    }

    /// Returns the configured bucket name.
    pub fn bucket(&self) -> &str {
        &self.bucket
    }

    /// Returns the configured key prefix.
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// Log one of this client's own per-request events.
    ///
    /// Promoted from `debug!` to `info!` when `s3.debug` is set. This is chbackup's own request
    /// tracing and is a **separate mechanism** from real AWS SDK request/response logs, which
    /// come only from raising the `aws_*` tracing targets in `logging::init_logging`. Mirrors
    /// the `clickhouse.debug` -> `log_sql_queries` pattern.
    fn log_s3(&self, op: &str, key: &str, detail: &str) {
        if self.log_requests {
            info!(op = op, key = %key, detail = %detail, "S3 request");
        } else {
            debug!(op = op, key = %key, detail = %detail, "S3 request");
        }
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

    /// Create a clone of this client targeting a different bucket and prefix.
    ///
    /// Reuses the same underlying AWS SDK client (connection pool, credentials)
    /// but overrides bucket and prefix. Useful when restoring S3 disk parts
    /// that live in a different bucket/prefix than the backup.
    pub fn with_bucket_and_prefix(&self, bucket: &str, prefix: &str) -> Self {
        S3Client {
            inner: self.inner.clone(),
            bucket: bucket.to_string(),
            prefix: prefix.to_string(),
            storage_class: self.storage_class.clone(),
            sse: self.sse.clone(),
            sse_kms_key_id: self.sse_kms_key_id.clone(),
            acl: self.acl.clone(),
            // Carried, not defaulted: this clone is used on the restore path to reach the
            // object-disk bucket, which is precisely where copy deadlines matter most.
            timeouts: self.timeouts,
            chunk_size: self.chunk_size,
            max_parts_count: self.max_parts_count,
            log_requests: self.log_requests,
        }
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

        let req = self
            .inner
            .put_object()
            .bucket(&self.bucket)
            .key(&full_key)
            .body(ByteStream::from(body));

        // Apply SSE, storage class, and ACL
        let mut req = apply_s3_object_options!(req, self);

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
    pub async fn list_common_prefixes(&self, prefix: &str, delimiter: &str) -> Result<Vec<String>> {
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

            let resp = with_deadline(
                "ListObjectsV2(prefixes)",
                &S3OpCtx::new(&self.bucket, &full_prefix),
                s3_deadline(S3OpClass::Metadata, None, &self.timeouts),
                req.send(),
            )
            .await?
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

            let resp = with_deadline(
                "ListObjectsV2",
                &S3OpCtx::new(&self.bucket, &full_prefix),
                s3_deadline(S3OpClass::Metadata, None, &self.timeouts),
                req.send(),
            )
            .await?
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

        with_deadline(
            "DeleteObject",
            &S3OpCtx::new(&self.bucket, &full_key),
            s3_deadline(S3OpClass::Metadata, None, &self.timeouts),
            self.inner
                .delete_object()
                .bucket(&self.bucket)
                .key(&full_key)
                .send(),
        )
        .await?
        .with_context(|| format!("Failed to delete object: {}", full_key))?;

        Ok(())
    }

    /// Delete multiple objects from S3 in batches of 1000.
    ///
    /// The `keys` are relative to the configured prefix.
    /// Returns the total number of individual object deletions that failed
    /// across all batches (0 on full success). When ALL keys in a batch fail,
    /// returns `Err` immediately. Partial failures return `Ok(failed_count)` —
    /// GC on the next run will clean up any leftovers.
    pub async fn delete_objects(&self, keys: Vec<String>) -> Result<u64> {
        if keys.is_empty() {
            return Ok(0);
        }

        let mut total_failed = 0u64;

        // S3 DeleteObjects supports max 1000 objects per request
        for chunk in keys.chunks(1000) {
            let identifiers: Vec<ObjectIdentifier> = chunk
                .iter()
                .map(|key| {
                    let full_key = self.full_key(key);
                    ObjectIdentifier::builder()
                        .key(full_key)
                        .build()
                        .context("Failed to build ObjectIdentifier")
                })
                .collect::<Result<Vec<_>>>()?;

            let delete = Delete::builder()
                .set_objects(Some(identifiers))
                .build()
                .context("Failed to build Delete request")?;

            debug!(count = chunk.len(), "Batch deleting objects from S3");

            let resp = with_deadline(
                "DeleteObjects",
                &S3OpCtx::new(&self.bucket, format!("<{} keys>", chunk.len())),
                s3_deadline(S3OpClass::Metadata, None, &self.timeouts),
                self.inner
                    .delete_objects()
                    .bucket(&self.bucket)
                    .delete(delete)
                    .send(),
            )
            .await?
            .context("Failed to batch delete objects")?;

            let errors = resp.errors();
            if !errors.is_empty() {
                let failed_keys: Vec<_> = errors.iter().filter_map(|e| e.key()).take(5).collect();
                let sample_errors: Vec<_> =
                    errors.iter().filter_map(|e| e.message()).take(3).collect();
                warn!(
                    failed_count = errors.len(),
                    total_count = chunk.len(),
                    sample_keys = ?failed_keys,
                    sample_errors = ?sample_errors,
                    "Some objects failed to delete in batch"
                );
                if errors.len() == chunk.len() {
                    bail!(
                        "Batch delete failed for all {} objects (sample errors: {:?})",
                        errors.len(),
                        sample_errors
                    );
                }
                total_failed += errors.len() as u64;
            }
        }

        Ok(total_failed)
    }

    // -- HEAD operations --

    /// Check if an object exists and return its size.
    ///
    /// Returns `Some(size)` if the object exists, `None` if not found.
    /// The `key` is relative to the configured prefix.
    pub async fn head_object(&self, key: &str) -> Result<Option<u64>> {
        let full_key = self.full_key(key);

        debug!(key = %full_key, "Checking object existence in S3");

        let head = with_deadline(
            "HeadObject",
            &S3OpCtx::new(&self.bucket, &full_key),
            s3_deadline(S3OpClass::Metadata, None, &self.timeouts),
            self.inner
                .head_object()
                .bucket(&self.bucket)
                .key(&full_key)
                .send(),
        )
        .await?;

        match head {
            Ok(resp) => {
                let size = resp.content_length().unwrap_or(0).max(0) as u64;
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

        let req = self
            .inner
            .create_multipart_upload()
            .bucket(&self.bucket)
            .key(&full_key);

        // Apply SSE, storage class, and ACL (same as put_object)
        let req = apply_s3_object_options!(req, self);

        let resp = with_deadline(
            "CreateMultipartUpload",
            &S3OpCtx::new(&self.bucket, &full_key),
            s3_deadline(S3OpClass::Metadata, None, &self.timeouts),
            req.send(),
        )
        .await?
        .with_context(|| format!("Failed to create multipart upload for: {}", full_key))?;

        let upload_id = resp
            .upload_id()
            .ok_or_else(|| {
                anyhow::anyhow!("No upload_id returned for multipart upload: {}", full_key)
            })?
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

        with_deadline(
            "CompleteMultipartUpload",
            &S3OpCtx::new(&self.bucket, &full_key),
            s3_deadline(S3OpClass::Copy, None, &self.timeouts),
            self.inner
                .complete_multipart_upload()
                .bucket(&self.bucket)
                .key(&full_key)
                .upload_id(upload_id)
                .multipart_upload(completed)
                .send(),
        )
        .await?
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

        with_deadline(
            "AbortMultipartUpload",
            &S3OpCtx::new(&self.bucket, &full_key),
            // Fixed cleanup bound, not request_timeout: "0s" is a supported value for the latter
            // and would leave this abort able to hang forever.
            Some(CLEANUP_TIMEOUT),
            self.inner
                .abort_multipart_upload()
                .bucket(&self.bucket)
                .key(&full_key)
                .upload_id(upload_id)
                .send(),
        )
        .await?
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

    /// S3 CopyObject size limit: 5 GiB. Objects larger than this require
    /// multipart copy (upload_part_copy).
    pub const COPY_OBJECT_MAX_SIZE: u64 = 5_368_709_120;

    /// Server-side copy of an object between buckets (or within a bucket).
    ///
    /// `source_bucket` and `source_key` identify the source object (absolute).
    /// `dest_key` is relative to this client's configured prefix.
    /// Applies SSE and storage class settings to the destination.
    ///
    /// For objects larger than 5 GiB, automatically uses multipart copy
    /// (upload_part_copy) since the S3 CopyObject API has a 5 GiB limit.
    pub async fn copy_object(
        &self,
        source_bucket: &str,
        source_key: &str,
        dest_key: &str,
    ) -> Result<()> {
        // Check source object size to determine if we need multipart copy.
        //
        // NOTE: this calls self.inner.head_object() directly rather than the public
        // head_object() wrapper, so it needs its own deadline -- wrapping only the wrapper
        // leaves the copy path (the one that actually stalled in production) uncovered.
        let head_ctx = S3OpCtx::new(source_bucket, source_key);
        let head_deadline = s3_deadline(S3OpClass::Metadata, None, &self.timeouts);
        let head_result = with_deadline(
            "HeadObject",
            &head_ctx,
            head_deadline,
            self.inner
                .head_object()
                .bucket(source_bucket)
                .key(source_key)
                .send(),
        )
        .await;

        let source_size = match head_result {
            // A timeout is NOT evidence about the object's size. Falling through to a single
            // CopyObject on a >5 GiB object sends an illegal request, which 400s, burns the
            // retries, and then downloads the whole object via the streaming fallback. Only a
            // genuine not-found/permission error justifies the unknown-size path.
            Err(timeout_err) => return Err(timeout_err),
            Ok(Ok(resp)) => Some(resp.content_length().unwrap_or(0).max(0) as u64),
            Ok(Err(_sdk_err)) => None,
        };

        if let Some(size) = source_size {
            if size > Self::COPY_OBJECT_MAX_SIZE {
                info!(
                    source_key = %source_key,
                    size = size,
                    "Source object exceeds 5GB, using multipart copy"
                );
                return self
                    .copy_object_multipart(source_bucket, source_key, dest_key, size)
                    .await;
            }
        }

        // Single CopyObject for objects <= 5 GiB (or when size is unknown)
        let full_dest_key = self.full_key(dest_key);
        let copy_source = format!("{}/{}", source_bucket, percent_encode_s3_key(source_key));

        self.log_s3(
            "CopyObject",
            &full_dest_key,
            &format!("source={copy_source}"),
        );

        let req = self
            .inner
            .copy_object()
            .bucket(&self.bucket)
            .copy_source(&copy_source)
            .key(&full_dest_key);

        // Apply SSE, storage class, and ACL
        let req = apply_s3_object_options!(req, self);

        let mut copy_ctx = S3OpCtx::new(&self.bucket, &full_dest_key);
        if let Some(size) = source_size {
            copy_ctx = copy_ctx.with_size(size);
        }
        with_deadline(
            "CopyObject",
            &copy_ctx,
            s3_deadline(S3OpClass::Copy, source_size, &self.timeouts),
            req.send(),
        )
        .await?
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

    /// Multipart server-side copy for objects larger than 5 GiB.
    ///
    /// Uses S3 `upload_part_copy` to copy byte ranges of the source object
    /// into a multipart upload on the destination. Automatically calculates
    /// chunk size to stay within the 10,000 part limit.
    ///
    /// On any error during part copying, aborts the multipart upload to
    /// avoid leaving orphaned parts.
    async fn copy_object_multipart(
        &self,
        source_bucket: &str,
        source_key: &str,
        dest_key: &str,
        source_size: u64,
    ) -> Result<()> {
        let full_dest_key = self.full_key(dest_key);

        // Create multipart upload with same settings as put_object/copy_object
        let create_req = self
            .inner
            .create_multipart_upload()
            .bucket(&self.bucket)
            .key(&full_dest_key);

        // Apply SSE, storage class, and ACL
        let create_req = apply_s3_object_options!(create_req, self);

        // Whole-object budget, computed ONCE before the create so the create, every part, and
        // the completion all draw from the same allowance. Without it, per-request deadlines
        // alone permit part_count * per_part_deadline in total -- for the 1238-part object that
        // triggered this work, roughly 22 hours.
        //
        // None means deadlines are disabled (request_timeout = 0), i.e. no clamp anywhere.
        let object_deadline: Option<std::time::Instant> =
            s3_deadline(S3OpClass::Copy, Some(source_size), &self.timeouts)
                .map(|d| std::time::Instant::now() + d);

        let create_ctx = S3OpCtx::new(&self.bucket, &full_dest_key).with_size(source_size);
        let create_resp = with_deadline(
            "CreateMultipartUpload",
            &create_ctx,
            s3_deadline(S3OpClass::Metadata, None, &self.timeouts),
            create_req.send(),
        )
        .await?
        .with_context(|| {
            format!(
                "Multipart copy: failed to create multipart upload for {}",
                full_dest_key
            )
        })?;

        let upload_id = create_resp
            .upload_id()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Multipart copy: no upload_id returned for {}",
                    full_dest_key
                )
            })?
            .to_string();

        // Honour the configured chunk size. Hardcoding auto mode here was the direct cause of
        // 1238 sequential UploadPartCopy round-trips for a single 6.5 GB object: auto mode
        // divides by max_parts_count and then floors at the 5 MiB minimum. With
        // s3.chunk_size = 512 MiB the same object needs 13 parts.
        let chunk_size = calculate_chunk_size(source_size, self.chunk_size, self.max_parts_count);
        let part_count = source_size.div_ceil(chunk_size);

        info!(
            source_key = %source_key,
            source_size = source_size,
            chunk_size = chunk_size,
            part_count = part_count,
            "Starting multipart copy"
        );

        // Copy parts; on any error, abort the multipart upload
        let copy_source = format!("{}/{}", source_bucket, percent_encode_s3_key(source_key));
        let result = self
            .copy_parts(
                &full_dest_key,
                &upload_id,
                &copy_source,
                source_size,
                chunk_size,
                part_count,
                object_deadline,
            )
            .await;

        // Resolve copy AND completion into one Result so a single failure path funnels both to
        // the abort. Do NOT hoist completion back into an Ok arm with `?`: that is the shape
        // that leaked parts.
        let outcome: Result<()> = async {
            let completed_parts = result?;
            let completed = CompletedMultipartUpload::builder()
                .set_parts(Some(completed_parts))
                .build();

            // Completion draws from what is left of the object budget rather than getting a
            // fresh allowance, or the total could overrun by one full completion deadline.
            let complete_deadline = remaining_budget(object_deadline).transpose()?;
            let complete_ctx = S3OpCtx::new(&self.bucket, &full_dest_key).with_size(source_size);
            with_deadline(
                "CompleteMultipartUpload",
                &complete_ctx,
                complete_deadline,
                self.inner
                    .complete_multipart_upload()
                    .bucket(&self.bucket)
                    .key(&full_dest_key)
                    .upload_id(&upload_id)
                    .multipart_upload(completed)
                    .send(),
            )
            .await?
            .with_context(|| {
                format!(
                    "Multipart copy: failed to complete upload for {}",
                    full_dest_key
                )
            })?;
            Ok(())
        }
        .await;

        match outcome {
            Ok(()) => {
                info!(
                    dest = %full_dest_key,
                    part_count = part_count,
                    "Multipart copy completed successfully"
                );
                Ok(())
            }
            Err(e) => {
                warn!(
                    dest = %full_dest_key,
                    upload_id = %upload_id,
                    error = %e,
                    "Multipart copy failed, aborting upload"
                );
                self.abort_upload_best_effort(&full_dest_key, &upload_id)
                    .await;
                Err(e)
            }
        }
    }

    /// Abort a multipart upload on the cleanup path.
    ///
    /// Uses the fixed [`CLEANUP_TIMEOUT`] rather than `request_timeout`, because `"0s"` is a
    /// supported value for the latter -- which would leave this able to hang forever, i.e. the
    /// exact failure it exists to prevent. Errors are logged, never propagated: this runs on a
    /// path that already has a real error to report.
    async fn abort_upload_best_effort(&self, full_dest_key: &str, upload_id: &str) {
        let ctx = S3OpCtx::new(&self.bucket, full_dest_key);
        let attempt = with_deadline(
            "AbortMultipartUpload",
            &ctx,
            Some(CLEANUP_TIMEOUT),
            self.inner
                .abort_multipart_upload()
                .bucket(&self.bucket)
                .key(full_dest_key)
                .upload_id(upload_id)
                .send(),
        )
        .await;

        let failure: Option<String> = match attempt {
            Ok(Ok(_)) => None,
            Ok(Err(e)) => Some(format!("{e}")),
            Err(e) => Some(format!("{e:#}")),
        };
        if let Some(error) = failure {
            warn!(
                upload_id = %upload_id,
                error = %error,
                "Failed to abort multipart upload (orphaned parts may remain). An \
                 AbortIncompleteMultipartUpload lifecycle rule on the bucket is the only \
                 complete remedy for this."
            );
        }
    }

    /// Copy byte-range parts from source to destination using upload_part_copy.
    ///
    /// Returns the completed parts on success, or an error on first failure.
    #[allow(clippy::too_many_arguments)]
    async fn copy_parts(
        &self,
        full_dest_key: &str,
        upload_id: &str,
        copy_source: &str,
        source_size: u64,
        chunk_size: u64,
        part_count: u64,
        object_deadline: Option<std::time::Instant>,
    ) -> Result<Vec<CompletedPart>> {
        let mut completed_parts = Vec::with_capacity(part_count as usize);

        for part_idx in 0..part_count {
            let start = part_idx * chunk_size;
            let end = ((part_idx + 1) * chunk_size - 1).min(source_size - 1);
            let range = format!("bytes={}-{}", start, end);
            let part_number = (part_idx + 1) as i32;
            let range_len = end - start + 1;

            // Bail early once the whole-object budget is gone, naming how far we got.
            let remaining = match remaining_budget(object_deadline) {
                Some(Err(e)) => {
                    return Err(e).with_context(|| {
                        format!(
                            "after {}/{} parts of {}",
                            part_idx, part_count, full_dest_key
                        )
                    })
                }
                Some(Ok(left)) => Some(left),
                None => None,
            };

            // The per-part deadline must never exceed what is left of the object budget. The
            // pre-check above only provides the early exit -- THIS clamp is what actually bounds
            // the total, since otherwise a part starting just under the deadline would still run
            // a further full part timeout.
            let part_deadline = match (
                s3_deadline(S3OpClass::Copy, Some(range_len), &self.timeouts),
                remaining,
            ) {
                (Some(d), Some(left)) => Some(d.min(left)),
                (Some(d), None) => Some(d),
                (None, _) => None,
            };

            // HEARTBEAT HOOK: this loop is the long silent stretch (1238 iterations took 2m41s
            // for one object, all at debug!). When the progress registry lands, report
            // part_number, part_count, bytes_copied, elapsed, and budget_remaining from here.
            self.log_s3(
                "UploadPartCopy",
                full_dest_key,
                &format!("part={part_number}/{part_count} range={range}"),
            );

            let resp = with_deadline(
                "UploadPartCopy",
                &S3OpCtx::new(&self.bucket, full_dest_key)
                    .with_part(part_number, &range)
                    .with_size(range_len),
                part_deadline,
                self.inner
                    .upload_part_copy()
                    .bucket(&self.bucket)
                    .key(full_dest_key)
                    .upload_id(upload_id)
                    .part_number(part_number)
                    .copy_source(copy_source)
                    .copy_source_range(&range)
                    .send(),
            )
            .await?
            .with_context(|| {
                format!(
                    "Multipart copy: upload_part_copy failed for part {} (range {})",
                    part_number, range
                )
            })?;

            let e_tag = resp
                .copy_part_result()
                .and_then(|r| r.e_tag().map(|s| s.to_string()))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "Missing ETag in UploadPartCopy response for part {}",
                        part_number
                    )
                })?;

            completed_parts.push(
                CompletedPart::builder()
                    .part_number(part_number)
                    .e_tag(e_tag)
                    .build(),
            );
        }

        Ok(completed_parts)
    }

    /// Streaming copy fallback: downloads from source then uploads to dest.
    ///
    /// Used when server-side CopyObject fails (e.g., cross-region).
    /// For small objects (≤ 32 MiB) the body is buffered and uploaded with a
    /// single PutObject.  For large objects the body is read chunk-by-chunk and
    /// uploaded via S3 multipart upload so memory usage is bounded to one chunk
    /// at a time instead of the entire object.
    pub async fn copy_object_streaming(
        &self,
        source_bucket: &str,
        source_key: &str,
        dest_key: &str,
    ) -> Result<()> {
        // 32 MiB: below this, buffer for simplicity; above, use multipart.
        const STREAMING_COPY_THRESHOLD: u64 = 32 * 1024 * 1024;

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

        let content_length = get_resp.content_length().unwrap_or(0).max(0) as u64;

        if content_length <= STREAMING_COPY_THRESHOLD {
            // Small object: buffer and use single PutObject
            let body = get_resp.body.collect().await.with_context(|| {
                format!(
                    "Streaming copy: failed to read body of {}/{}",
                    source_bucket, source_key
                )
            })?;
            let bytes = body.into_bytes().to_vec();
            return self.put_object(dest_key, bytes).await.with_context(|| {
                format!(
                    "Streaming copy: failed to upload to {}/{}",
                    self.bucket, full_dest_key
                )
            });
        }

        // Large object: stream through multipart upload to bound memory usage.
        // Chunk size is at least S3_MIN_PART_SIZE (5 MiB).
        let chunk_size =
            calculate_chunk_size(content_length, 0, 10_000).max(S3_MIN_PART_SIZE) as usize;

        debug!(
            content_length = content_length,
            chunk_size = chunk_size,
            "Streaming copy using multipart upload"
        );

        let upload_id = self.create_multipart_upload(dest_key).await?;

        let result: Result<()> = async {
            let mut body = get_resp.body;
            let mut buffer: Vec<u8> = Vec::with_capacity(chunk_size);
            let mut completed_parts: Vec<(i32, String)> = Vec::new();
            let mut part_number = 1i32;

            while let Some(chunk_result) = body.next().await {
                let bytes = chunk_result.with_context(|| {
                    format!(
                        "Streaming copy: error reading body of {}/{}",
                        source_bucket, source_key
                    )
                })?;
                buffer.extend_from_slice(&bytes);

                // Upload full chunks as multipart parts
                while buffer.len() >= chunk_size {
                    let part_data: Vec<u8> = buffer.drain(..chunk_size).collect();
                    let e_tag = self
                        .upload_part(dest_key, &upload_id, part_number, part_data)
                        .await
                        .with_context(|| {
                            format!(
                                "Streaming copy: upload_part {} failed for {}/{}",
                                part_number, source_bucket, source_key
                            )
                        })?;
                    completed_parts.push((part_number, e_tag));
                    part_number += 1;
                }
            }

            // Upload remaining bytes as the final part
            if !buffer.is_empty() {
                let e_tag = self
                    .upload_part(dest_key, &upload_id, part_number, buffer)
                    .await
                    .with_context(|| {
                        format!(
                            "Streaming copy: final upload_part failed for {}/{}",
                            source_bucket, source_key
                        )
                    })?;
                completed_parts.push((part_number, e_tag));
            }

            self.complete_multipart_upload(dest_key, &upload_id, completed_parts)
                .await
                .with_context(|| {
                    format!(
                        "Streaming copy: complete_multipart_upload failed for dest {}",
                        full_dest_key
                    )
                })
        }
        .await;

        if let Err(e) = result {
            let _ = self.abort_multipart_upload(dest_key, &upload_id).await;
            return Err(e);
        }

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
    /// (100ms, 400ms, 1600ms) plus jitter per design doc section 5.4 step 3d.
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
        self.copy_object_with_retry_jitter(
            source_bucket,
            source_key,
            dest_key,
            allow_streaming,
            0.0,
        )
        .await
    }

    /// Upload an object to S3 with retry logic.
    ///
    /// Retries `put_object()` up to `retry.max_retries` times with exponential
    /// backoff and configurable jitter. Only retries transient errors; happy
    /// path is unchanged.
    pub async fn put_object_with_retry(
        &self,
        key: &str,
        body: Vec<u8>,
        retry: RetryConfig,
    ) -> Result<()> {
        let full_key = self.full_key(key);
        retry_with_backoff(&retry, "PutObject", &full_key, || {
            let body_clone = body.clone();
            async move { self.put_object(key, body_clone).await }
        })
        .await
    }

    /// Upload a single part of a multipart upload with retry logic.
    ///
    /// Retries `upload_part()` up to `retry.max_retries` times with exponential
    /// backoff and configurable jitter. Returns the ETag of the uploaded part.
    pub async fn upload_part_with_retry(
        &self,
        key: &str,
        upload_id: &str,
        part_number: i32,
        body: Vec<u8>,
        retry: RetryConfig,
    ) -> Result<String> {
        let context_msg = format!("UploadPart (part {}) {}", part_number, self.full_key(key));
        retry_with_backoff(&retry, "UploadPart", &context_msg, || {
            let body_clone = body.clone();
            async move {
                self.upload_part(key, upload_id, part_number, body_clone)
                    .await
            }
        })
        .await
    }

    /// Copy with retry, backoff, and configurable jitter factor.
    /// Whether an error means the copy source is permanently absent, making retries futile.
    ///
    /// Deliberately narrow -- only the S3 codes that mean "this key/bucket does not exist".
    /// A broader match on `404` or `"not found"` would misclassify transient conditions and
    /// unrelated errors whose messages happen to contain those substrings.
    pub fn is_missing_source(err: &anyhow::Error) -> bool {
        is_missing_source_error(err)
    }

    pub async fn copy_object_with_retry_jitter(
        &self,
        source_bucket: &str,
        source_key: &str,
        dest_key: &str,
        allow_streaming: bool,
        jitter_factor: f64,
    ) -> Result<()> {
        let backoff_ms = [100u64, 400, 1600];

        for (attempt, delay_ms) in backoff_ms.iter().enumerate() {
            match self.copy_object(source_bucket, source_key, dest_key).await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    // A missing source object will never appear by waiting. Fail fast rather
                    // than burning the full backoff, and skip the streaming fallback too --
                    // copy_object_streaming GETs the very same key and would also 404.
                    if is_missing_source_error(&e) {
                        return Err(e).with_context(|| {
                            format!(
                                "CopyObject source object does not exist: {}/{} \
                                 (not retried -- a missing object is permanent)",
                                source_bucket, source_key
                            )
                        });
                    }
                    if attempt < backoff_ms.len() - 1 {
                        let actual_delay = crate::config::apply_jitter(*delay_ms, jitter_factor);
                        debug!(
                            attempt = attempt + 1,
                            delay_ms = actual_delay,
                            error = %e,
                            "CopyObject failed, retrying after backoff"
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(actual_delay)).await;
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

/// Generic retry helper with exponential backoff and jitter.
///
/// Executes the closure `f` up to `retry.max_retries + 1` times. On each
/// transient failure, waits with exponential backoff (`base_delay * 2^attempt`)
/// plus configurable jitter before the next attempt. On final failure, wraps
/// the error with `op_name` and `context_msg` for diagnostics.
///
/// Used by `put_object_with_retry` and `upload_part_with_retry` to avoid
/// duplicating the retry/backoff/jitter loop.
async fn retry_with_backoff<F, Fut, T>(
    retry: &RetryConfig,
    op_name: &str,
    context_msg: &str,
    mut f: F,
) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let total_attempts = retry.max_retries + 1;
    for attempt in 0..total_attempts {
        match f().await {
            Ok(val) => return Ok(val),
            Err(e) => {
                if attempt < total_attempts - 1 {
                    let delay_ms = (retry.base_delay_secs.saturating_mul(1000))
                        .saturating_mul(2u64.saturating_pow(attempt));
                    let actual_delay = crate::config::apply_jitter(delay_ms, retry.jitter_factor);
                    warn!(
                        attempt = attempt + 1,
                        max_retries = retry.max_retries,
                        delay_ms = actual_delay,
                        error = %e,
                        "{} failed, retrying after backoff", op_name
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(actual_delay)).await;
                } else {
                    return Err(e).with_context(|| {
                        format!(
                            "{} failed after {} attempts: {}",
                            op_name, total_attempts, context_msg
                        )
                    });
                }
            }
        }
    }
    unreachable!("retry loop should have returned")
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
    let chunk = chunk.max(S3_MIN_PART_SIZE);

    // Enforce the S3 maximum part size too. This was latent while auto mode was the only path
    // (dividing by max_parts_count never exceeds 5 GiB in practice), but becomes reachable the
    // moment a configured s3.chunk_size is honoured -- and an over-large part is rejected by
    // UploadPart/UploadPartCopy at request time, mid-upload.
    if chunk > S3Client::COPY_OBJECT_MAX_SIZE {
        warn!(
            requested_chunk_size = chunk,
            clamped_to = S3Client::COPY_OBJECT_MAX_SIZE,
            "s3.chunk_size exceeds S3's 5 GiB maximum part size, clamping"
        );
        return S3Client::COPY_OBJECT_MAX_SIZE;
    }
    chunk
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::S3Config;
    use std::time::Duration;

    /// The provider must construct without contacting STS -- it resolves credentials
    /// lazily, which is what makes it refresh them in long-running modes.
    fn test_timeouts(secs: u64, bps: u64) -> S3Timeouts {
        S3Timeouts {
            request_timeout_secs: secs,
            copy_min_bytes_per_second: bps,
        }
    }

    #[test]
    fn test_s3_deadline_metadata_ignores_bytes() {
        let t = test_timeouts(60, 1024 * 1024);
        let a = s3_deadline(S3OpClass::Metadata, None, &t);
        let b = s3_deadline(S3OpClass::Metadata, Some(5 * 1024 * 1024 * 1024), &t);
        assert_eq!(a, Some(Duration::from_secs(60)));
        assert_eq!(a, b, "size must not affect a metadata deadline");
    }

    #[test]
    fn test_s3_deadline_copy_adds_size_allowance() {
        let t = test_timeouts(60, 1024 * 1024); // 1 MiB/s
                                                // 5 MiB part -> 60s + 5s
        assert_eq!(
            s3_deadline(S3OpClass::Copy, Some(5 * 1024 * 1024), &t),
            Some(Duration::from_secs(65))
        );
        // 5 GiB -> 60s + 5120s == 86.3 min. Spelled out because the MiB-vs-MB distinction
        // matters: at 1 MB/s decimal this would be ~90 min instead.
        assert_eq!(
            s3_deadline(S3OpClass::Copy, Some(5 * 1024 * 1024 * 1024), &t),
            Some(Duration::from_secs(60 + 5120))
        );
    }

    #[test]
    fn test_s3_deadline_disabled_when_request_timeout_zero() {
        let t = test_timeouts(0, 1024 * 1024);
        assert_eq!(s3_deadline(S3OpClass::Metadata, None, &t), None);
        assert_eq!(s3_deadline(S3OpClass::Copy, Some(1 << 30), &t), None);
    }

    #[test]
    fn test_s3_deadline_copy_zero_rate_is_flat() {
        let t = test_timeouts(60, 0);
        assert_eq!(
            s3_deadline(S3OpClass::Copy, Some(5 * 1024 * 1024 * 1024), &t),
            Some(Duration::from_secs(60)),
            "copy_min_bytes_per_second = 0 means no size allowance"
        );
    }

    /// Unknown size means a preceding HEAD failed. Falling back to the bare metadata deadline
    /// would fail legitimate large copies, so assume the largest a single CopyObject can handle.
    #[test]
    fn test_s3_deadline_copy_unknown_size_assumes_legal_max() {
        let t = test_timeouts(60, 1024 * 1024);
        let unknown = s3_deadline(S3OpClass::Copy, None, &t).unwrap();
        let max = s3_deadline(S3OpClass::Copy, Some(S3Client::COPY_OBJECT_MAX_SIZE), &t).unwrap();
        assert_eq!(unknown, max);
        assert!(unknown > Duration::from_secs(60));
    }

    #[test]
    fn test_s3_deadline_saturates_on_absurd_size() {
        let t = test_timeouts(60, 1);
        // Must not panic or wrap.
        let d = s3_deadline(S3OpClass::Copy, Some(u64::MAX), &t);
        assert!(d.is_some());
    }

    #[test]
    fn test_is_timeout_error_is_narrow() {
        let ours = anyhow::anyhow!("{TIMEOUT_ERR_PREFIX}: CopyObject on b/k after 1.0s");
        assert!(is_timeout_error(&ours));

        // Must NOT match a missing-source error, nor an unrelated message that merely mentions
        // a timeout -- the same over-broad-substring trap is_missing_source_error documents.
        let missing = anyhow::anyhow!("NoSuchKey: The specified key does not exist");
        assert!(!is_timeout_error(&missing));
        let unrelated = anyhow::anyhow!("connection timed out while reading response");
        assert!(!is_timeout_error(&unrelated));
    }

    #[test]
    fn test_remaining_budget_states() {
        // Disabled budget -> no clamp.
        assert!(remaining_budget(None).is_none());

        // Live budget -> Some(Ok(..)).
        let future = std::time::Instant::now() + Duration::from_secs(30);
        match remaining_budget(Some(future)) {
            Some(Ok(left)) => assert!(left <= Duration::from_secs(30) && !left.is_zero()),
            other => panic!("expected Some(Ok(..)), got {other:?}"),
        }

        // Exhausted budget -> Some(Err(..)) carrying the timeout prefix so is_timeout_error
        // classifies it.
        let past = std::time::Instant::now() - Duration::from_secs(1);
        match remaining_budget(Some(past)) {
            Some(Err(e)) => assert!(is_timeout_error(&e), "got: {e:#}"),
            other => panic!("expected Some(Err(..)), got {:?}", other.map(|r| r.is_ok())),
        }
    }

    /// The incident: a 6,487,078,358-byte object took 1238 sequential UploadPartCopy calls
    /// because the copy path hardcoded auto mode. Honouring s3.chunk_size fixes it.
    #[test]
    fn test_calculate_chunk_size_reproduces_incident_and_fix() {
        const INCIDENT_SIZE: u64 = 6_487_078_358;

        let auto = calculate_chunk_size(INCIDENT_SIZE, 0, 10_000);
        assert_eq!(
            auto,
            5 * 1024 * 1024,
            "auto mode floors at the 5 MiB minimum"
        );
        assert_eq!(
            INCIDENT_SIZE.div_ceil(auto),
            1238,
            "the observed part count"
        );

        let configured = calculate_chunk_size(INCIDENT_SIZE, 512 * 1024 * 1024, 10_000);
        assert_eq!(configured, 512 * 1024 * 1024);
        assert_eq!(
            INCIDENT_SIZE.div_ceil(configured),
            13,
            "tens, not thousands"
        );
    }

    #[test]
    fn test_calculate_chunk_size_clamps_to_s3_maximum() {
        // Latent while auto mode was the only path; reachable once s3.chunk_size is honoured.
        let over = calculate_chunk_size(1 << 40, 10 * 1024 * 1024 * 1024, 10_000);
        assert_eq!(over, S3Client::COPY_OBJECT_MAX_SIZE, "must clamp to 5 GiB");

        // The lower clamp still holds.
        let under = calculate_chunk_size(1024, 1024, 10_000);
        assert_eq!(under, 5 * 1024 * 1024);
    }

    /// with_bucket_and_prefix is used on the restore path to reach the object-disk bucket, so a
    /// forgotten field there yields zero deadlines exactly where copy deadlines matter most.
    #[test]
    fn test_with_bucket_and_prefix_carries_timeout_fields() {
        let base = mock_s3_fields("bucket-a", "prefix-a");
        let derived = base.with_bucket_and_prefix("bucket-b", "prefix-b");

        assert_eq!(derived.bucket, "bucket-b");
        assert_eq!(
            derived.timeouts.request_timeout_secs,
            base.timeouts.request_timeout_secs
        );
        assert_eq!(
            derived.timeouts.copy_min_bytes_per_second,
            base.timeouts.copy_min_bytes_per_second
        );
        assert_eq!(derived.chunk_size, base.chunk_size);
        assert_eq!(derived.max_parts_count, base.max_parts_count);
        assert_eq!(derived.log_requests, base.log_requests);
    }

    /// with_deadline must not flatten the inner output: head_object classifies the SDK's own
    /// error type to turn a 404 into Ok(None), which an anyhow-typed wrapper would destroy.
    #[tokio::test]
    async fn test_with_deadline_preserves_inner_output_type() {
        let ctx = S3OpCtx::new("b", "k");

        // Inner Result::Err survives as a value, not as the wrapper's error.
        let inner: Result<u32, &str> = Err("inner");
        let got = with_deadline(
            "Op",
            &ctx,
            Some(Duration::from_secs(5)),
            async move { inner },
        )
        .await
        .expect("wrapper must succeed");
        assert_eq!(got, Err("inner"));

        // No deadline configured -> passthrough.
        let got = with_deadline("Op", &ctx, None, async { 7u32 })
            .await
            .unwrap();
        assert_eq!(got, 7);
    }

    #[tokio::test]
    async fn test_with_deadline_expires_and_is_classified() {
        let ctx = S3OpCtx::new("bucket", "key");
        let err = with_deadline(
            "UploadPartCopy",
            &ctx,
            Some(Duration::from_millis(10)),
            async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                1u32
            },
        )
        .await
        .expect_err("should time out");

        assert!(is_timeout_error(&err), "got: {err:#}");
        let msg = format!("{err:#}");
        assert!(msg.contains("bucket/key"), "should name the object: {msg}");
        assert!(
            msg.contains("s3.request_timeout"),
            "should name the knob: {msg}"
        );
    }

    #[tokio::test]
    async fn test_assume_role_provider_constructs_without_network() {
        let provider = assume_role_provider(
            "arn:aws:iam::123456789012:role/chbackup-test",
            "us-east-1",
            Some("http://127.0.0.1:9000"),
            Some(Credentials::new("ak", "sk", None, None, "test")),
        )
        .await;

        // The provider is opaque; its Debug output carries the role it will assume.
        assert!(format!("{provider:?}").contains("chbackup-test"));
    }

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
        let client = mock_s3_fields("my-bucket", "chbackup");
        assert_eq!(
            client.full_key("backup/metadata.json"),
            "chbackup/backup/metadata.json"
        );
    }

    #[test]
    fn test_full_key_with_trailing_slash_prefix() {
        let client = mock_s3_fields("my-bucket", "chbackup/");
        assert_eq!(
            client.full_key("backup/metadata.json"),
            "chbackup/backup/metadata.json"
        );
    }

    #[test]
    fn test_full_key_empty_prefix() {
        let client = mock_s3_fields("my-bucket", "");
        assert_eq!(
            client.full_key("backup/metadata.json"),
            "backup/metadata.json"
        );
    }

    #[test]
    fn test_full_key_nested_prefix() {
        let client = mock_s3_fields("my-bucket", "prod/region1/chbackup");
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
        // Verify the CopySource format is "{bucket}/{percent-encoded-key}"
        let client = mock_s3_fields("dest-bucket", "dest-prefix");

        // ASCII-safe key: slashes preserved, no encoding needed
        let source_bucket = "source-bucket";
        let source_key = "path/to/object.bin";
        let copy_source = format!("{}/{}", source_bucket, percent_encode_s3_key(source_key));
        assert_eq!(copy_source, "source-bucket/path/to/object.bin");

        // Key with space and special chars: should be percent-encoded
        let source_key_special = "path/to/my file (v2).bin";
        let copy_source_special = format!(
            "{}/{}",
            source_bucket,
            percent_encode_s3_key(source_key_special)
        );
        assert_eq!(
            copy_source_special,
            "source-bucket/path/to/my%20file%20%28v2%29.bin"
        );

        // Verify dest key uses prefix
        let dest_key = "backup/objects/data.bin";
        let full_dest = client.full_key(dest_key);
        assert_eq!(full_dest, "dest-prefix/backup/objects/data.bin");
    }

    #[test]
    fn test_percent_encode_s3_key() {
        // Unreserved chars + slashes pass through unchanged
        assert_eq!(percent_encode_s3_key("abc/def_123.~-"), "abc/def_123.~-");
        // Space and parentheses are encoded
        assert_eq!(percent_encode_s3_key("a b"), "a%20b");
        assert_eq!(percent_encode_s3_key("a(b)"), "a%28b%29");
        // Unicode bytes are encoded
        assert_eq!(percent_encode_s3_key("ñ"), "%C3%B1");
        // Empty string is fine
        assert_eq!(percent_encode_s3_key(""), "");
    }

    #[tokio::test]
    #[ignore] // Requires network: tests real S3 error paths
    async fn test_copy_object_with_retry_no_streaming_when_disabled() {
        // When allow_streaming is false, copy_object_with_retry should return
        // an error after retries without attempting streaming fallback.
        let client = mock_s3_fields("dest-bucket", "prefix");

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

    #[tokio::test]
    #[ignore] // Requires network: tests real S3 error paths
    async fn test_put_object_retry_config() {
        // Verify put_object_with_retry exists, accepts retry params, and fails
        // with descriptive error when no real S3 endpoint is available.
        let client = mock_s3_fields("test-bucket", "prefix");

        // 0 retries = single attempt, should fail quickly
        let retry = RetryConfig {
            max_retries: 0,
            base_delay_secs: 10,
            jitter_factor: 0.0,
        };
        let result = client
            .put_object_with_retry("test/key.bin", vec![1, 2, 3], retry)
            .await;

        assert!(result.is_err());
        let err_msg = format!("{:#}", result.unwrap_err());
        assert!(
            err_msg.contains("PutObject failed after 1 attempts")
                || err_msg.contains("Failed to upload object"),
            "Error should mention PutObject failure, got: {}",
            err_msg
        );
    }

    #[tokio::test]
    #[ignore] // Requires network: tests real S3 error paths
    async fn test_upload_part_retry_config() {
        // Verify upload_part_with_retry exists, accepts retry params, and fails
        // with descriptive error when no real S3 endpoint is available.
        let client = mock_s3_fields("test-bucket", "prefix");

        // 0 retries = single attempt
        let retry = RetryConfig {
            max_retries: 0,
            base_delay_secs: 10,
            jitter_factor: 0.0,
        };
        let result = client
            .upload_part_with_retry("test/key.bin", "fake-upload-id", 1, vec![1, 2, 3], retry)
            .await;

        assert!(result.is_err());
        let err_msg = format!("{:#}", result.unwrap_err());
        assert!(
            err_msg.contains("UploadPart (part 1) failed after 1 attempts")
                || err_msg.contains("Failed to upload part"),
            "Error should mention UploadPart failure, got: {}",
            err_msg
        );
    }

    #[test]
    fn test_disable_ssl_forces_http_scheme() {
        // When disable_ssl=true and endpoint is https://, the effective endpoint
        // should be rewritten to http://
        let config = S3Config {
            disable_ssl: true,
            endpoint: "https://minio:9000".to_string(),
            ..S3Config::default()
        };

        // Simulate the rewriting logic from S3Client::new()
        let effective_endpoint = if config.disable_ssl && !config.endpoint.is_empty() {
            config.endpoint.replacen("https://", "http://", 1)
        } else {
            config.endpoint.clone()
        };

        assert_eq!(effective_endpoint, "http://minio:9000");
    }

    #[test]
    fn test_disable_ssl_no_change_when_already_http() {
        // When disable_ssl=true and endpoint is already http://, no change needed
        let config = S3Config {
            disable_ssl: true,
            endpoint: "http://minio:9000".to_string(),
            ..S3Config::default()
        };

        let effective_endpoint = if config.disable_ssl && !config.endpoint.is_empty() {
            config.endpoint.replacen("https://", "http://", 1)
        } else {
            config.endpoint.clone()
        };

        assert_eq!(effective_endpoint, "http://minio:9000");
    }

    #[test]
    fn test_disable_ssl_empty_endpoint() {
        // When disable_ssl=true but endpoint is empty, endpoint stays empty
        // (a warning is logged in the real code, but no crash)
        let config = S3Config {
            disable_ssl: true,
            endpoint: String::new(),
            ..S3Config::default()
        };

        let effective_endpoint = if config.disable_ssl && !config.endpoint.is_empty() {
            config.endpoint.replacen("https://", "http://", 1)
        } else {
            config.endpoint.clone()
        };

        assert!(effective_endpoint.is_empty());
    }

    #[test]
    fn test_disable_cert_verification_removes_env_var_approach() {
        // Structural test: verify that the broken AWS_CA_BUNDLE env var approach
        // is not present in the production code (non-test) section of the source file.
        let source = include_str!("s3.rs");
        // Build the search needle dynamically to avoid self-matching in this test.
        let needle = format!("set_var(\"{}_BUNDLE\"", "AWS_CA");
        // Split source at the test module boundary and only check production code.
        let prod_code = source
            .split("#[cfg(test)]")
            .next()
            .expect("should have non-test section");
        assert!(
            !prod_code.contains(&needle),
            "Broken env var approach should be removed from production code in s3.rs"
        );
    }

    #[test]
    fn test_disable_cert_verification_forces_http() {
        // When disable_cert_verification=true and endpoint is https://,
        // the effective endpoint should be rewritten to http://
        let endpoint = "https://minio:9000".to_string();

        // Simulate the disable_ssl block (disable_ssl=false, no rewrite)
        let mut effective_endpoint = endpoint.clone();

        // Simulate the disable_cert_verification block
        let disable_cert_verification = true;
        if disable_cert_verification && !effective_endpoint.is_empty() {
            effective_endpoint = effective_endpoint.replacen("https://", "http://", 1);
        }

        assert_eq!(effective_endpoint, "http://minio:9000");

        // Also verify idempotency: if already http:// (from disable_ssl), no double rewrite
        let mut already_http = "http://minio:9000".to_string();
        if disable_cert_verification && !already_http.is_empty() {
            already_http = already_http.replacen("https://", "http://", 1);
        }
        assert_eq!(already_http, "http://minio:9000");
    }

    #[tokio::test]
    async fn test_disable_cert_verification_empty_endpoint_bails() {
        // When disable_cert_verification=true and endpoint is empty,
        // S3Client::new() should return an error.
        let config = S3Config {
            disable_cert_verification: true,
            endpoint: String::new(),
            ..S3Config::default()
        };

        let result = S3Client::new(&config).await;
        assert!(
            result.is_err(),
            "Expected error when disable_cert_verification=true with empty endpoint"
        );
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("disable_cert_verification requires an explicit endpoint URL"),
            "Error should mention explicit endpoint requirement, got: {}",
            err_msg
        );
    }

    #[test]
    fn test_is_missing_source_error_matches_permanent_absence() {
        assert!(is_missing_source_error(&anyhow::anyhow!(
            "service error: unhandled error (NoSuchKey): Error {{ code: \"NoSuchKey\", \
             message: \"The specified key does not exist.\" }}"
        )));
        assert!(is_missing_source_error(&anyhow::anyhow!(
            "NoSuchBucket: the bucket is gone"
        )));
    }

    #[test]
    fn test_is_missing_source_error_ignores_transient_and_unrelated() {
        // Deliberately narrow: a broad "404"/"not found" match would misclassify these and
        // turn retryable failures into hard ones.
        for msg in [
            "connection reset by peer",
            "timed out",
            "SlowDown: please reduce your request rate",
            "InternalError: we encountered an internal error",
            "AccessDenied",
            "HTTP status 404 from an unrelated endpoint",
            "table not found",
        ] {
            assert!(
                !is_missing_source_error(&anyhow::anyhow!(msg)),
                "{msg:?} must not be classified as a permanently missing source"
            );
        }
    }

    #[test]
    fn test_parse_s3_uri_bucket_and_prefix() {
        let (bucket, prefix) = parse_s3_uri("s3://my-bucket/my/prefix");
        assert_eq!(bucket, "my-bucket");
        assert_eq!(prefix, "my/prefix");
    }

    #[test]
    fn test_parse_s3_uri_bucket_only() {
        let (bucket, prefix) = parse_s3_uri("s3://my-bucket");
        assert_eq!(bucket, "my-bucket");
        assert_eq!(prefix, "");
    }

    #[test]
    fn test_parse_s3_uri_trailing_slash() {
        let (bucket, prefix) = parse_s3_uri("s3://my-bucket/prefix/");
        assert_eq!(bucket, "my-bucket");
        assert_eq!(prefix, "prefix");
    }

    #[test]
    fn test_parse_s3_uri_uppercase_scheme() {
        let (bucket, prefix) = parse_s3_uri("S3://my-bucket/prefix");
        assert_eq!(bucket, "my-bucket");
        assert_eq!(prefix, "prefix");
    }

    #[test]
    fn test_parse_s3_uri_not_s3() {
        let (bucket, prefix) = parse_s3_uri("some/path/here");
        assert_eq!(bucket, "");
        assert_eq!(prefix, "some/path/here");
    }

    #[test]
    fn test_parse_s3_uri_no_prefix_trailing_slash() {
        let (bucket, prefix) = parse_s3_uri("s3://my-bucket/");
        assert_eq!(bucket, "my-bucket");
        assert_eq!(prefix, "");
    }

    #[test]
    fn test_percent_encode_s3_key_already_safe() {
        assert_eq!(
            percent_encode_s3_key("backups/daily/2024-01-15/metadata.json"),
            "backups/daily/2024-01-15/metadata.json"
        );
    }

    #[test]
    fn test_percent_encode_s3_key_unicode_multibyte() {
        // Multi-byte UTF-8 chars get percent-encoded byte-by-byte
        assert_eq!(percent_encode_s3_key("café"), "caf%C3%A9");
    }

    #[test]
    fn test_percent_encode_s3_key_slashes_preserved() {
        assert_eq!(percent_encode_s3_key("a/b/c"), "a/b/c");
    }

    #[test]
    fn test_calculate_chunk_size_at_threshold() {
        // Exactly at the 5MB minimum boundary
        let chunk = calculate_chunk_size(S3_MIN_PART_SIZE * 10000, 0, 10000);
        assert_eq!(chunk, S3_MIN_PART_SIZE);
    }

    #[test]
    fn test_calculate_chunk_size_max_parts_boundary() {
        // With 2 max parts, chunk should be half the data (rounded up)
        let data_len = 100 * 1024 * 1024_u64; // 100 MB
        let chunk = calculate_chunk_size(data_len, 0, 2);
        let expected = data_len.div_ceil(2);
        assert_eq!(chunk, expected);
        assert!(data_len.div_ceil(chunk) <= 2);
    }

    #[test]
    fn test_retry_config_defaults() {
        let rc = RetryConfig {
            max_retries: 3,
            base_delay_secs: 1,
            jitter_factor: 0.1,
        };
        assert_eq!(rc.max_retries, 3);
        assert_eq!(rc.base_delay_secs, 1);
        assert!((rc.jitter_factor - 0.1).abs() < f64::EPSILON);
    }

    /// Create a minimal S3Client for unit testing without triggering TLS initialization.
    ///
    /// Constructs an S3Client with a dummy `inner` via
    /// `aws_sdk_s3::Client::from_conf(Builder::new().behavior_version_latest().build())`.
    /// This does NOT trigger native TLS root certificate loading, making these tests
    /// safe to run offline (`cargo test --locked --offline`).
    ///
    /// Only the bucket/prefix/storage_class/sse/sse_kms_key_id/acl fields are
    /// meaningful; the inner client will fail on any real S3 operation.
    fn mock_s3_fields(bucket: &str, prefix: &str) -> S3Client {
        let s3_config = aws_sdk_s3::config::Builder::new()
            .behavior_version_latest()
            .region(Region::new("us-east-1"))
            .build();
        let inner = aws_sdk_s3::Client::from_conf(s3_config);

        S3Client {
            inner,
            bucket: bucket.to_string(),
            prefix: prefix.to_string(),
            storage_class: "STANDARD".to_string(),
            sse: String::new(),
            sse_kms_key_id: String::new(),
            acl: String::new(),
            timeouts: S3Timeouts {
                request_timeout_secs: 60,
                copy_min_bytes_per_second: 1024 * 1024,
            },
            chunk_size: 0,
            max_parts_count: 10_000,
            log_requests: false,
        }
    }
}
