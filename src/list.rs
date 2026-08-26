//! List and delete commands for local and remote backups.
//!
//! The `list` function scans local backup directories and/or queries S3 to
//! produce a summary of available backups. The `delete` function removes
//! backups from local disk or S3.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use tracing::{debug, info, warn};

use crate::backup::collect::per_disk_backup_dir;
use crate::clickhouse::{freeze_prefix, legacy_freeze_prefix, ChClient};
use crate::config::Config;
use crate::error::ChBackupError;
use crate::manifest::BackupManifest;
use crate::path_encoding::validate_disk_path;
use crate::resume::{load_state_file, DownloadState};
use crate::storage::S3Client;

/// Location specifier matching the CLI `Location` enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    Local,
    Remote,
}

/// Output format for list commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListFormat {
    /// Default human-readable table format.
    Default,
    /// JSON array output.
    Json,
    /// YAML output.
    Yaml,
    /// CSV with header row.
    Csv,
    /// Tab-separated values with header row.
    Tsv,
}

/// Summary of a single backup for display in list output.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BackupSummary {
    /// Backup name.
    pub name: String,
    /// Timestamp when the backup was created.
    pub timestamp: Option<DateTime<Utc>>,
    /// Total uncompressed size in bytes.
    pub size: u64,
    /// Compressed size in bytes (from manifest).
    pub compressed_size: u64,
    /// Number of tables in the backup.
    pub table_count: usize,
    /// Size of the manifest metadata in bytes.
    pub metadata_size: u64,
    /// Size of RBAC (access/) files in bytes.
    pub rbac_size: u64,
    /// Size of ClickHouse config backup files in bytes.
    pub config_size: u64,
    /// Total size of S3 object disk parts in bytes.
    /// Computed by summing s3_objects[].size across all manifest parts.
    #[serde(default)]
    pub object_disk_size: u64,
    /// Name of the base backup this backup depends on (for incremental backups).
    /// Empty string for full backups. Extracted from the first `carried:{base}` source.
    #[serde(default)]
    pub required: String,
    /// Whether the backup manifest is missing or corrupt.
    pub is_broken: bool,
    /// Reason why the backup is broken (e.g., "metadata.json not found").
    /// None for valid backups.
    pub broken_reason: Option<String>,
}

/// In-memory cache for remote backup summaries (design 8.4).
/// TTL-based expiry, invalidated on mutating operations.
pub struct ManifestCache {
    summaries: Option<Vec<BackupSummary>>,
    populated_at: Option<Instant>,
    ttl: Duration,
}

impl ManifestCache {
    /// Create a new empty cache with the given TTL.
    pub fn new(ttl: Duration) -> Self {
        Self {
            summaries: None,
            populated_at: None,
            ttl,
        }
    }

    /// Get cached summaries if they exist and have not expired.
    pub fn get(&self) -> Option<&Vec<BackupSummary>> {
        let populated_at = self.populated_at?;
        if populated_at.elapsed() >= self.ttl {
            return None;
        }
        self.summaries.as_ref()
    }

    /// Store summaries in the cache, resetting the TTL timer.
    pub fn set(&mut self, summaries: Vec<BackupSummary>) {
        self.populated_at = Some(Instant::now());
        self.summaries = Some(summaries);
    }

    /// Clear cached data, forcing the next get() to return None.
    pub fn invalidate(&mut self) {
        self.summaries = None;
        self.populated_at = None;
    }

    /// Update the TTL used for cache expiry checks.
    ///
    /// Called after config reload/restart so the cache picks up any change
    /// to `general.remote_cache_ttl_secs`.
    pub fn set_ttl(&mut self, ttl: Duration) {
        self.ttl = ttl;
    }
}

/// List remote backups using the cache if available, otherwise fetching from S3.
///
/// On cache miss, holds the lock while fetching from S3 to prevent a thundering
/// herd where multiple concurrent callers all fetch independently on cache miss.
pub async fn list_remote_cached(
    s3: &S3Client,
    cache: &tokio::sync::Mutex<ManifestCache>,
) -> Result<Vec<BackupSummary>> {
    // Check cache first under lock
    {
        let guard = cache.lock().await;
        if let Some(cached) = guard.get() {
            debug!("ManifestCache: hit, returning {} summaries", cached.len());
            return Ok(cached.clone());
        }
        // Cache miss -- but we drop the lock here to avoid holding it during
        // the S3 fetch. A second caller racing here will also fetch (acceptable:
        // idempotent write, avoids holding an async lock across S3 I/O).
    }

    // Cache miss: fetch from S3 without holding lock
    let summaries = list_remote(s3).await?;
    info!("ManifestCache: populated, count={}", summaries.len());

    // Store in cache (second caller's write is idempotent)
    {
        let mut guard = cache.lock().await;
        guard.set(summaries.clone());
    }

    Ok(summaries)
}

/// List backups based on the requested location and output format.
///
/// If `location` is `None`, shows both local and remote backups.
/// If `Some(Local)`, shows only local backups.
/// If `Some(Remote)`, shows only remote backups.
///
/// The `format` parameter controls output format (default table, JSON, YAML, CSV, TSV).
pub async fn list(
    data_path: &str,
    s3: Option<&S3Client>,
    location: Option<&Location>,
    format: &ListFormat,
) -> Result<()> {
    let show_local = location.is_none() || location == Some(&Location::Local);
    let show_remote = location.is_none() || location == Some(&Location::Remote);

    match format {
        ListFormat::Default => {
            // Original human-readable table format
            if show_local {
                let local_backups = list_local(data_path)?;
                println!("Local backups:");
                if local_backups.is_empty() {
                    println!("  (none)");
                } else {
                    print_backup_table(&local_backups);
                }
                println!();
            }

            if show_remote {
                let s3 =
                    s3.ok_or_else(|| anyhow::anyhow!("S3 client required for remote listing"))?;
                let remote_backups = list_remote(s3).await?;
                println!("Remote backups:");
                if remote_backups.is_empty() {
                    println!("  (none)");
                } else {
                    print_backup_table(&remote_backups);
                }
                println!();
            }
        }
        _ => {
            // Structured formats: collect all requested backups then format
            let mut all_backups = Vec::new();

            if show_local {
                let local_backups = list_local(data_path)?;
                all_backups.extend(local_backups);
            }

            if show_remote {
                let s3 =
                    s3.ok_or_else(|| anyhow::anyhow!("S3 client required for remote listing"))?;
                let remote_backups = list_remote(s3).await?;
                all_backups.extend(remote_backups);
            }

            let output = format_list_output(&all_backups, format)?;
            println!("{output}");
        }
    }

    Ok(())
}

/// Format a list of backup summaries according to the specified format.
///
/// Returns the formatted string. Supports JSON, YAML, CSV, TSV, and default table format.
pub fn format_list_output(summaries: &[BackupSummary], format: &ListFormat) -> Result<String> {
    match format {
        ListFormat::Default => {
            // Build the default table format as a string
            let mut output = String::new();
            for s in summaries {
                let status = if s.is_broken {
                    match &s.broken_reason {
                        Some(reason) => format!(" [BROKEN: {}]", reason),
                        None => " [BROKEN]".to_string(),
                    }
                } else {
                    String::new()
                };
                let ts = match &s.timestamp {
                    Some(t) => t.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
                    None => "unknown".to_string(),
                };
                let size_str = format_size(s.size);
                let compressed_str = format_size(s.compressed_size);
                output.push_str(&format!(
                    "  {}{}\t{}\t{}\t{}\t{} tables\n",
                    s.name, status, ts, size_str, compressed_str, s.table_count
                ));
            }
            Ok(output.trim_end().to_string())
        }
        ListFormat::Json => {
            let json = serde_json::to_string_pretty(summaries)
                .context("Failed to serialize backup list to JSON")?;
            Ok(json)
        }
        ListFormat::Yaml => {
            let yaml = serde_yaml::to_string(summaries)
                .context("Failed to serialize backup list to YAML")?;
            Ok(yaml.trim_end().to_string())
        }
        ListFormat::Csv => Ok(format_delimited(summaries, ',')),
        ListFormat::Tsv => Ok(format_delimited(summaries, '\t')),
    }
}

/// Quote a field for CSV output per RFC 4180.
///
/// Fields containing the delimiter, double-quote, newline (`\n`), or carriage
/// return (`\r`) are wrapped in double-quotes, with internal double-quotes
/// escaped as `""`.
fn csv_quote(field: &str, delimiter: char) -> String {
    if field.contains(delimiter)
        || field.contains('"')
        || field.contains('\n')
        || field.contains('\r')
    {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

/// Format backup summaries as delimited text (CSV or TSV).
fn format_delimited(summaries: &[BackupSummary], delimiter: char) -> String {
    let mut output = String::new();

    // Header row (headers never contain special chars, no quoting needed)
    let headers = [
        "name",
        "timestamp",
        "size",
        "compressed_size",
        "table_count",
        "metadata_size",
        "rbac_size",
        "config_size",
        "object_disk_size",
        "required",
        "is_broken",
        "broken_reason",
    ];
    let d = delimiter.to_string();
    output.push_str(&headers.join(&d));
    output.push('\n');

    // Data rows
    for s in summaries {
        let ts = match &s.timestamp {
            Some(t) => t.to_rfc3339(),
            None => String::new(),
        };
        let broken_reason = s.broken_reason.as_deref().unwrap_or("");

        let fields = [
            csv_quote(&s.name, delimiter),
            csv_quote(&ts, delimiter),
            s.size.to_string(),
            s.compressed_size.to_string(),
            s.table_count.to_string(),
            s.metadata_size.to_string(),
            s.rbac_size.to_string(),
            s.config_size.to_string(),
            s.object_disk_size.to_string(),
            csv_quote(&s.required, delimiter),
            s.is_broken.to_string(),
            csv_quote(broken_reason, delimiter),
        ];
        output.push_str(&fields.join(&d));
        output.push('\n');
    }

    output.trim_end().to_string()
}

/// Resolve the "latest" or "previous" backup name shortcut from a sorted backup list.
///
/// - `"latest"` resolves to the most recent (last) backup by timestamp.
/// - `"previous"` resolves to the second-most-recent backup.
/// - Any other value is returned as-is.
///
/// The provided backups should be sorted by name/timestamp ascending (as returned
/// by [`list_local`] and [`list_remote`]). Only non-broken backups are considered
/// for shortcut resolution.
pub fn resolve_backup_shortcut(name: &str, backups: &[BackupSummary]) -> Result<String> {
    let mut valid: Vec<&BackupSummary> = backups.iter().filter(|b| !b.is_broken).collect();
    // Sort by timestamp ascending; None timestamps sort first (before all Some values).
    valid.sort_by_key(|s| s.timestamp);

    match name {
        "latest" => valid
            .last()
            .map(|b| b.name.clone())
            .ok_or_else(|| anyhow::anyhow!("No backups found to resolve 'latest'")),
        "previous" => {
            if valid.len() < 2 {
                anyhow::bail!(
                    "Not enough backups for 'previous' (found {} valid backups)",
                    valid.len()
                );
            }
            Ok(valid[valid.len() - 2].name.clone())
        }
        _ => Ok(name.to_string()),
    }
}

/// Scan local backup directories and parse their manifests.
///
/// Looks for `{data_path}/backup/*/metadata.json` and parses each manifest.
/// If a manifest is missing or corrupt, the backup is marked as broken.
pub fn list_local(data_path: &str) -> Result<Vec<BackupSummary>> {
    let backup_base = PathBuf::from(data_path).join("backup");
    let mut summaries = Vec::new();

    if !backup_base.exists() {
        debug!(
            path = %backup_base.display(),
            "Backup directory does not exist, returning empty list"
        );
        return Ok(summaries);
    }

    let entries = std::fs::read_dir(&backup_base)
        .with_context(|| format!("Failed to read backup directory: {}", backup_base.display()))?;

    for entry in entries {
        let entry = entry.context("Failed to read directory entry")?;
        let path = entry.path();

        if !path.is_dir() {
            continue;
        }

        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };

        let metadata_path = path.join("metadata.json");
        let summary = parse_backup_summary(&name, &metadata_path);
        summaries.push(summary);
    }

    // Sort by timestamp (falling back to name for broken backups with None timestamp)
    summaries.sort_by(|a, b| {
        a.timestamp
            .cmp(&b.timestamp)
            .then_with(|| a.name.cmp(&b.name))
    });

    info!(count = summaries.len(), "Listed local backups");
    Ok(summaries)
}

/// Construct a broken `BackupSummary` with zeroed sizes and the given reason.
fn broken_summary(name: String, reason: String) -> BackupSummary {
    BackupSummary {
        name,
        timestamp: None,
        size: 0,
        compressed_size: 0,
        table_count: 0,
        metadata_size: 0,
        rbac_size: 0,
        config_size: 0,
        object_disk_size: 0,
        required: String::new(),
        is_broken: true,
        broken_reason: Some(reason),
    }
}

/// List remote backups from S3 by scanning common prefixes.
///
/// Each backup is stored under `{prefix}/{backup_name}/`. We list common
/// prefixes to discover backup names, then download each manifest.
pub async fn list_remote(s3: &S3Client) -> Result<Vec<BackupSummary>> {
    let mut summaries = Vec::new();

    // List top-level "directories" under the S3 prefix.
    // Each directory name corresponds to a backup name.
    let prefixes = s3.list_common_prefixes("", "/").await?;

    for prefix in &prefixes {
        // prefix looks like "chbackup/daily-2024-01-15/" or "daily-2024-01-15/"
        // We need to extract the backup name from it.
        let name = extract_backup_name_from_prefix(prefix, s3.prefix());
        if name.is_empty() {
            continue;
        }

        let manifest_key = format!("{}/metadata.json", name);
        match s3.get_object(&manifest_key).await {
            Ok(data) => match BackupManifest::from_json_bytes(&data) {
                Ok(manifest) => {
                    summaries.push(summary_from_manifest(&manifest, &name));
                }
                Err(e) => {
                    let reason = format!("manifest parse error: {e:#}");
                    warn!(
                        backup = %name,
                        error = format_args!("{e:#}"),
                        "Failed to parse remote manifest, marking as broken"
                    );
                    summaries.push(broken_summary(name, reason));
                }
            },
            Err(e) => {
                let reason = format!("metadata.json not found: {e}");
                debug!(
                    backup = %name,
                    error = %e,
                    "No manifest found for remote backup, marking as broken"
                );
                summaries.push(broken_summary(name, reason));
            }
        }
    }

    // Sort by timestamp (falling back to name for broken backups with None timestamp)
    summaries.sort_by(|a, b| {
        a.timestamp
            .cmp(&b.timestamp)
            .then_with(|| a.name.cmp(&b.name))
    });

    info!(count = summaries.len(), "Listed remote backups");
    Ok(summaries)
}

// -- Delete functions --

/// Delete a backup from local disk or remote S3.
pub async fn delete(
    data_path: &str,
    s3: &S3Client,
    location: &Location,
    backup_name: &str,
) -> Result<()> {
    match location {
        Location::Local => delete_local(data_path, backup_name),
        Location::Remote => delete_remote(s3, backup_name).await,
    }
}

/// Delete a local backup directory and any per-disk backup directories.
///
/// Discovers per-disk backup dirs from the manifest (metadata.json) or falls
/// back to the download state file (download.state.json) when the manifest is
/// unavailable (e.g., broken or incomplete download). All paths are canonicalized
/// and deduped via `HashSet` to prevent double-delete when symlinks or equivalent
/// paths resolve to the same directory.
///
/// Per-disk dirs are deleted first (non-fatal), then the default backup_dir last
/// (fatal on failure, preserving existing error propagation semantics).
pub fn delete_local(data_path: &str, backup_name: &str) -> Result<()> {
    let backup_dir = PathBuf::from(data_path).join("backup").join(backup_name);

    if !backup_dir.exists() {
        return Err(ChBackupError::BackupNotFound(format!(
            "local backup '{}' not found at: {}",
            backup_name,
            backup_dir.display()
        ))
        .into());
    }

    // Refuse while a deferred S3 object-disk freeze is held for this backup.
    //
    // The backup directory contains `deferred_freeze.json`, so removing it would destroy the
    // only record of that freeze -- leaking it with nothing left to find it by, and no
    // UNFREEZE issued. That matters most on the retention path, which runs automatically
    // after every successful upload: without this guard, a later backup's retention sweep
    // silently strands an earlier backup's freeze.
    //
    // Deliberately an error rather than a silent skip. Retention treats it as a warning and
    // retries next cycle, by which point the freeze has been released by its upload or reaped
    // as expired -- so this defers a deletion rather than blocking it forever.
    if crate::backup::deferred::blocks_destructive_op(data_path, backup_name, "delete_local") {
        return Err(anyhow::anyhow!(
            "refusing to delete local backup '{}': a deferred S3 object-disk freeze is still \
             held for it. Deleting now would strand the freeze in ClickHouse. Wait for its \
             upload to finish, or run `chbackup release-deferred {}` to release it. \
             (`chbackup clean` cannot: it holds the global lock, so it can never acquire the \
             per-backup lock the release needs.)",
            backup_name,
            backup_name
        ));
    }

    // Discover disk map: manifest first, download state file as fallback
    let disk_map: HashMap<String, String> = {
        let manifest_path = backup_dir.join("metadata.json");
        match BackupManifest::load_from_file(&manifest_path) {
            Ok(m) => m.disks.into_iter().collect(),
            Err(_) => {
                // Fallback: try download state file (persisted unconditionally during download)
                let state_path = backup_dir.join("download.state.json");
                match load_state_file::<DownloadState>(&state_path) {
                    Ok(Some(s)) => s.disk_map,
                    _ => HashMap::new(), // No manifest, no state -- only default dir
                }
            }
        }
    };

    info!(
        backup = %backup_name,
        path = %backup_dir.display(),
        "Deleting local backup"
    );

    // Collect all dirs to delete, deduped by canonical path
    let mut dirs_to_delete: Vec<PathBuf> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();

    // Default backup_dir always included (deleted last, separately)
    let canonical_default =
        std::fs::canonicalize(&backup_dir).unwrap_or_else(|_| backup_dir.clone());
    seen.insert(canonical_default);

    // Per-disk dirs (skip if same canonical path as default)
    for (disk_name, disk_path) in &disk_map {
        if !validate_disk_path(disk_path) {
            warn!(
                disk_path = %disk_path,
                disk_name = %disk_name,
                "Disk path failed validation, skipping deletion"
            );
            continue;
        }
        let per_disk = per_disk_backup_dir(disk_path.trim_end_matches('/'), backup_name);
        if per_disk.exists() {
            let canonical = std::fs::canonicalize(&per_disk).unwrap_or_else(|_| per_disk.clone());
            if seen.insert(canonical) {
                dirs_to_delete.push(per_disk);
            }
        }
    }

    // Delete per-disk dirs first (non-fatal)
    for dir in &dirs_to_delete {
        info!(path = %dir.display(), "Deleting per-disk backup dir");
        if let Err(e) = std::fs::remove_dir_all(dir) {
            warn!(path = %dir.display(), error = %e, "Failed to remove per-disk backup dir");
        }
    }

    // Delete default backup_dir last (fatal on failure)
    std::fs::remove_dir_all(&backup_dir).with_context(|| {
        format!(
            "Failed to delete local backup directory: {}",
            backup_dir.display()
        )
    })?;

    info!(backup = %backup_name, "Local backup deleted");
    Ok(())
}

/// Delete a remote backup from S3.
///
/// Lists all objects under `{backup_name}/` and batch-deletes them.
pub async fn delete_remote(s3: &S3Client, backup_name: &str) -> Result<()> {
    let prefix = format!("{}/", backup_name);

    info!(
        backup = %backup_name,
        "Listing remote objects for deletion"
    );

    let objects = s3.list_objects(&prefix).await?;

    if objects.is_empty() {
        return Err(ChBackupError::BackupNotFound(format!(
            "remote backup '{}' not found (no objects under prefix '{}')",
            backup_name, prefix
        ))
        .into());
    }

    // Collect all keys (relative to the S3Client prefix, since list_objects
    // returns full keys with the prefix already prepended).
    // We need to strip the S3Client prefix to get relative keys for delete_objects.
    let s3_prefix = s3.prefix();
    let keys: Vec<String> = objects
        .iter()
        .map(|obj| strip_s3_prefix(&obj.key, s3_prefix))
        .collect();

    info!(
        backup = %backup_name,
        object_count = keys.len(),
        "Deleting remote backup objects"
    );

    let failed = s3.delete_objects(keys).await?;
    if failed > 0 {
        warn!(
            failed_count = failed,
            "Some S3 objects failed to delete, will be retried on next GC"
        );
    }

    info!(backup = %backup_name, "Remote backup deleted");
    Ok(())
}

// -- Clean broken functions --

/// Delete all broken local backups (missing or corrupt metadata.json).
///
/// Returns the count of deleted broken backups.
/// Delete every local backup with a missing or corrupt manifest.
///
/// # Why this takes a ChClient
///
/// A broken backup may still hold a deferred S3 object-disk freeze, and delete_local rightly
/// refuses to destroy the directory while one exists -- the record lives *in* that directory,
/// so deleting it would strand the FREEZE in ClickHouse with nothing left to describe it.
/// Without a way to release the freeze, clean_broken could never clean such a backup until the
/// record TTL expired (24h by default), which defeats the point of the command.
///
/// Releasing here is safe because clean_broken holds the *Global* lock, and lock.rs makes the
/// Global and Backup(name) tiers mutually exclusive: no create or upload can be running, so
/// nothing can own the freeze being released. That is a stronger guarantee than the per-backup
/// lock that release-deferred relies on.
///
/// `ch` is optional so callers without a ClickHouse client keep the old behaviour: the freeze is
/// left alone and the backup is skipped rather than silently stranded.
pub async fn clean_broken_local(data_path: &str, ch: Option<&ChClient>) -> Result<usize> {
    let backups = list_local(data_path)?;
    let broken: Vec<&BackupSummary> = backups.iter().filter(|b| b.is_broken).collect();

    if broken.is_empty() {
        info!("No broken local backups found");
        return Ok(0);
    }

    let mut deleted = 0;
    for b in &broken {
        // Release any deferred freeze first, so delete_local has nothing to refuse.
        if let Some(ch) = ch {
            let record = crate::backup::deferred::record_path_for(data_path, &b.name);
            if record.exists() {
                info!(
                    backup = %b.name,
                    "Broken backup holds a deferred S3 object-disk freeze; releasing it before \
                     deletion (safe: clean_broken holds the global lock, so no create or upload \
                     can own it)"
                );
                match crate::backup::deferred::release_now(ch, data_path, &b.name).await {
                    Ok(n) => info!(backup = %b.name, tables = n, "Released deferred freeze"),
                    Err(e) => {
                        // Leave the backup in place rather than strand the freeze.
                        warn!(
                            backup = %b.name,
                            error = %e,
                            "Failed to release deferred freeze; leaving this broken backup in \
                             place so the FREEZE is not stranded"
                        );
                        continue;
                    }
                }
            }
        }

        match delete_local(data_path, &b.name) {
            Ok(()) => {
                info!(backup = %b.name, "Deleted broken local backup");
                deleted += 1;
            }
            Err(e) => {
                warn!(
                    backup = %b.name,
                    error = %e,
                    "Failed to delete broken local backup"
                );
            }
        }
    }

    info!("clean_broken: deleted {} broken backups", deleted);
    Ok(deleted)
}

/// What `clean_broken` should do with one broken remote backup.
#[derive(Debug, PartialEq, Eq)]
pub enum CleanBrokenPlan {
    /// Leave the backup completely intact, for the stated reason.
    Skip { reason: String },
    /// Delete exactly these keys, and nothing else.
    Delete { keys: Vec<String> },
}

/// Decide what to do with one backup that `list_remote` reported as broken.
///
/// "Broken" only means "no readable manifest", and upload writes the manifest
/// **last**, so an upload still in flight is indistinguishable from a backup
/// that will never gain one. This encodes all three guards that tell them apart,
/// in one place, so the caller has no decision left to make:
///
/// - `lock_is_live` -- the backup's PID lock is held by a live process, so some
///   process is still writing it.
/// - `newest_last_modified` / `min_age_secs` -- nothing under the prefix may have
///   been written within the last `min_age_secs`. An absent timestamp means the
///   age cannot be established, which is treated exactly like "too young".
/// - `protected` -- keys a surviving manifest still references are excluded from
///   the deletion, so a broken backup that shares data with a healthy one cannot
///   take that data down with it (see [`is_key_protected`]).
pub fn plan_clean_broken_deletion(
    candidate_keys: &[String],
    newest_last_modified: Option<i64>,
    now_secs: i64,
    min_age_secs: u64,
    lock_is_live: bool,
    protected: &HashSet<String>,
) -> CleanBrokenPlan {
    if lock_is_live {
        return CleanBrokenPlan::Skip {
            reason: "its PID lock is held by a live process, an operation is still writing it"
                .to_string(),
        };
    }

    let Some(newest) = newest_last_modified else {
        return CleanBrokenPlan::Skip {
            reason: "its age is unknown (no object under the prefix carries a last_modified \
                     timestamp)"
                .to_string(),
        };
    };
    let age_secs = now_secs.saturating_sub(newest).max(0) as u64;
    if age_secs < min_age_secs {
        return CleanBrokenPlan::Skip {
            reason: format!(
                "its newest object is {age_secs}s old, younger than \
                 clean_broken_min_age_secs ({min_age_secs}s), so an upload may still be in flight"
            ),
        };
    }

    let keys: Vec<String> = candidate_keys
        .iter()
        .filter(|key| !is_key_protected(key, protected))
        .cloned()
        .collect();
    if keys.is_empty() {
        return CleanBrokenPlan::Skip {
            reason: "every key under its prefix is still referenced by a surviving backup"
                .to_string(),
        };
    }

    CleanBrokenPlan::Delete { keys }
}

/// Carry out `plan` for one backup, deleting exactly the keys it names.
///
/// The deletion is injected because an `S3Client` cannot be constructed in a unit
/// test, and "the keys that reach S3 are the planned ones" is the property worth
/// testing (see `clean_broken_deletes_only_planned`). Returns whether the backup
/// was deleted; a `Skip` and a failed deletion are both logged and counted as not
/// deleted, the latter being retried by the next `clean_broken` run.
async fn apply_clean_broken_plan<DK, DKFut>(
    backup_name: &str,
    plan: CleanBrokenPlan,
    delete_keys: DK,
) -> bool
where
    DK: FnOnce(Vec<String>) -> DKFut,
    DKFut: Future<Output = Result<()>>,
{
    match plan {
        CleanBrokenPlan::Skip { reason } => {
            info!(
                backup = %backup_name,
                reason = %reason,
                "clean_broken: kept broken remote backup"
            );
            false
        }
        CleanBrokenPlan::Delete { keys } => match delete_keys(keys).await {
            Ok(()) => {
                info!(backup = %backup_name, "Deleted broken remote backup");
                true
            }
            Err(e) => {
                warn!(
                    backup = %backup_name,
                    error = %e,
                    "Failed to delete broken remote backup"
                );
                false
            }
        },
    }
}

/// Delete broken remote backups (missing or corrupt metadata.json) that
/// [`plan_clean_broken_deletion`] clears for deletion.
///
/// `min_age_secs` is `general.clean_broken_min_age_secs`. Every deletion decision
/// is the planner's; this function only gathers its inputs -- the keys under each
/// broken prefix, their newest `last_modified`, the backup's PID lock state, and
/// the keys the surviving *valid* manifests still reference.
///
/// Like `retention_remote_inner`, the protected-key collection fails **closed**:
/// an unreadable valid manifest aborts the whole run before anything is deleted,
/// because its keys are exactly the ones we would otherwise fail to protect.
///
/// Returns the count of deleted broken backups.
pub async fn clean_broken_remote(s3: &S3Client, min_age_secs: u64) -> Result<usize> {
    let backups = list_remote(s3).await?;
    let broken: Vec<&BackupSummary> = backups.iter().filter(|b| b.is_broken).collect();

    if broken.is_empty() {
        info!("No broken remote backups found");
        return Ok(0);
    }

    let mut protected: HashSet<String> = HashSet::new();
    for valid in backups.iter().filter(|b| !b.is_broken) {
        let manifest = s3
            .get_object(&format!("{}/metadata.json", valid.name))
            .await
            .and_then(|data| BackupManifest::from_json_bytes(&data))
            .with_context(|| {
                format!(
                    "clean_broken aborted before deleting anything: cannot read the manifest of \
                     valid backup '{}', so its keys cannot be protected",
                    valid.name
                )
            })?;
        protected.extend(collect_key_prefixes_from_manifest(&manifest));
    }

    let now_secs = Utc::now().timestamp();
    let s3_prefix = s3.prefix();
    let mut deleted = 0;

    for b in &broken {
        let objects = match s3.list_objects(&format!("{}/", b.name)).await {
            Ok(objects) => objects,
            Err(e) => {
                warn!(backup = %b.name, error = %e, "clean_broken: failed to list keys");
                continue;
            }
        };
        let candidate_keys: Vec<String> = objects
            .iter()
            .map(|obj| strip_s3_prefix(&obj.key, s3_prefix))
            .collect();
        // `None` when any object lacks a timestamp: the most recent write is what
        // proves an upload has finished, so one unknown timestamp makes the age of
        // the prefix as a whole unknown.
        let newest_last_modified = objects
            .iter()
            .map(|obj| obj.last_modified.map(|t| t.timestamp()))
            .collect::<Option<Vec<i64>>>()
            .and_then(|stamps| stamps.into_iter().max());
        let lock_is_live = crate::lock::lock_path_for_scope(
            crate::lock::default_lock_dir(),
            &crate::lock::LockScope::Backup(b.name.clone()),
        )
        .is_some_and(|path| crate::lock::is_lock_file_active(&path));

        let plan = plan_clean_broken_deletion(
            &candidate_keys,
            newest_last_modified,
            now_secs,
            min_age_secs,
            lock_is_live,
            &protected,
        );
        if apply_clean_broken_plan(&b.name, plan, |keys| async move {
            let failed = s3.delete_objects(keys).await?;
            if failed > 0 {
                warn!(
                    failed_count = failed,
                    "clean_broken: some S3 deletions failed, retried on the next run"
                );
            }
            Ok(())
        })
        .await
        {
            deleted += 1;
        }
    }

    info!("clean_broken: deleted {} broken backups", deleted);
    Ok(deleted)
}

/// Clean broken backups by location (local or remote).
pub async fn clean_broken(
    data_path: &str,
    s3: &S3Client,
    ch: Option<&ChClient>,
    location: &Location,
    min_age_secs: u64,
) -> Result<()> {
    match location {
        Location::Local => {
            let count = clean_broken_local(data_path, ch).await?;
            info!(count = count, "Clean broken local complete");
        }
        Location::Remote => {
            let count = clean_broken_remote(s3, min_age_secs).await?;
            info!(count = count, "Clean broken remote complete");
        }
    }
    Ok(())
}

// -- Retention functions --

/// Resolve the effective local retention count.
///
/// Returns `retention.backups_to_keep_local` when non-zero, otherwise falls back
/// to `general.backups_to_keep_local`. This matches clickhouse-backup behavior
/// where the `retention:` section overrides the `general:` section.
pub fn effective_retention_local(config: &Config) -> i32 {
    if config.retention.backups_to_keep_local != 0 {
        config.retention.backups_to_keep_local
    } else {
        config.general.backups_to_keep_local
    }
}

/// Resolve the effective remote retention count.
///
/// Returns `retention.backups_to_keep_remote` when non-zero, otherwise falls back
/// to `general.backups_to_keep_remote`. This matches clickhouse-backup behavior
/// where the `retention:` section overrides the `general:` section.
pub fn effective_retention_remote(config: &Config) -> i32 {
    if config.retention.backups_to_keep_remote != 0 {
        config.retention.backups_to_keep_remote
    } else {
        config.general.backups_to_keep_remote
    }
}

/// Delete oldest local backups exceeding the `keep` count.
///
/// Follows the `clean_broken_local` pattern: list -> filter -> sort -> delete -> count.
/// Broken backups are excluded from retention counting and deletion.
///
/// - `keep == 0` or `keep == -1`: no retention action (return Ok(0)).
///   `-1` means "delete after upload" which is handled by the upload module.
/// - `keep > 0`: keep the N newest valid backups, delete the rest.
///
/// Returns the number of deleted backups.
pub fn retention_local(data_path: &str, keep: i32) -> Result<usize> {
    if keep <= 0 {
        return Ok(0);
    }

    let backups = list_local(data_path)?;

    // Filter to valid (non-broken) backups only
    let mut valid: Vec<&BackupSummary> = backups.iter().filter(|b| !b.is_broken).collect();

    let keep = keep as usize;
    if valid.len() <= keep {
        return Ok(0);
    }

    // Sort by timestamp ascending (oldest first).
    // None timestamps (should not happen for valid backups) treated as very old.
    valid.sort_by_key(|s| s.timestamp);

    let to_delete = valid.len() - keep;
    let mut deleted = 0;

    for b in valid.iter().take(to_delete) {
        match delete_local(data_path, &b.name) {
            Ok(()) => {
                info!(backup = %b.name, "retention_local: deleted old backup");
                deleted += 1;
            }
            Err(e) => {
                warn!(
                    backup = %b.name,
                    error = %e,
                    "retention_local: failed to delete backup"
                );
            }
        }
    }

    info!(
        deleted = deleted,
        total = backups.len(),
        "retention_local: deleted N of M local backups"
    );
    Ok(deleted)
}

/// Apply local and remote retention after a successful upload.
///
/// Follows the same best-effort pattern as the watch loop (watch/mod.rs:490-527):
/// errors are logged as warnings, never fatal.
///
/// - `retention_local` is sync -- called via `spawn_blocking`
/// - `retention_remote` is async -- called directly
/// - `backup_name` is `Some` when called after a specific upload so that
///   `keep_local == -1` (design §8.3: auto-delete after upload) can delete
///   the just-uploaded backup immediately
/// - `manifest_cache` is `Option` because CLI mode has no cache
///
/// Design doc section 3.6 step 7: "Apply retention: delete oldest remote backups
/// exceeding `backups_to_keep_remote`" after upload.
pub async fn apply_retention_after_upload(
    config: &Config,
    s3: &S3Client,
    backup_name: Option<&str>,
    manifest_cache: Option<&tokio::sync::Mutex<ManifestCache>>,
) {
    let keep_local = effective_retention_local(config);
    if keep_local == -1 {
        // -1 means "delete local backup immediately after upload" (design §8.3)
        if let Some(name) = backup_name {
            let data_path = config.clickhouse.data_path.clone();
            let name_owned = name.to_string();
            match tokio::task::spawn_blocking(move || delete_local(&data_path, &name_owned))
                .await
                .unwrap_or_else(|e| Err(anyhow::anyhow!("spawn_blocking failed: {}", e)))
            {
                Ok(()) => info!(backup = %name, "retention_local: deleted local backup (keep=-1)"),
                Err(e) => warn!(
                    backup = %name,
                    error = %e,
                    "retention_local: failed to delete local backup (best-effort)"
                ),
            }
        }
    } else if keep_local > 0 {
        let data_path = config.clickhouse.data_path.clone();
        match tokio::task::spawn_blocking(move || retention_local(&data_path, keep_local))
            .await
            .unwrap_or_else(|e| Err(anyhow::anyhow!("spawn_blocking failed: {}", e)))
        {
            Ok(deleted) => {
                if deleted > 0 {
                    info!(
                        deleted = deleted,
                        "retention applied after upload: local retention"
                    );
                }
            }
            Err(e) => {
                warn!(
                    error = %e,
                    "retention after upload: local retention failed (best-effort)"
                );
            }
        }
    }

    let keep_remote = effective_retention_remote(config);
    if keep_remote > 0 {
        match retention_remote(s3, keep_remote).await {
            Ok(deleted) => {
                if deleted > 0 {
                    info!(
                        deleted = deleted,
                        "retention applied after upload: remote retention"
                    );
                    // Invalidate manifest cache after remote retention changes backup set
                    if let Some(cache) = manifest_cache {
                        cache.lock().await.invalidate();
                        info!("ManifestCache: invalidated");
                    }
                }
            }
            Err(e) => {
                warn!(
                    error = %e,
                    "retention after upload: remote retention failed (best-effort)"
                );
            }
        }
    }
}

// -- GC functions --

/// Extract everything a backup manifest references, as a set of key *prefixes*.
///
/// Collects `backup_key` from every `PartInfo` and every `S3ObjectInfo` within
/// each table's parts. Most entries are whole object keys (a local part's
/// archive, an S3-disk data object), but an S3-disk part's metadata key is
/// directory-like and ends in `/` (see the key built in `upload::upload_inner`,
/// `{backup}/data/{db}/{table}/{disk}/{part}/`): it stands for every metadata
/// file uploaded under it. That trailing `/` is load-bearing and must be
/// preserved -- `is_key_protected` uses it to distinguish an exact key from a
/// prefix that covers its children.
///
/// A set built from the surviving manifests is transitively complete over an
/// incremental chain, so no chain walk (and no new manifest field) is needed:
/// `backup::diff::diff_parts` copies the base part's *physical* `backup_key`
/// into the incremental's `PartInfo`, so a surviving incremental manifest
/// already names every key its data lives at -- including keys first written by
/// an intermediate backup that has itself since been deleted.
pub fn collect_key_prefixes_from_manifest(manifest: &BackupManifest) -> HashSet<String> {
    let mut keys = HashSet::new();

    for table in manifest.tables.values() {
        for parts in table.parts.values() {
            for part in parts {
                if !part.backup_key.is_empty() {
                    keys.insert(part.backup_key.clone());
                }
                if let Some(ref s3_objects) = part.s3_objects {
                    for s3_obj in s3_objects {
                        if !s3_obj.backup_key.is_empty() {
                            keys.insert(s3_obj.backup_key.clone());
                        }
                    }
                }
            }
        }
    }

    keys
}

/// Whether a relative S3 key is still referenced, and so must not be deleted.
///
/// A key is protected when it is itself a protected entry, or when it lives
/// under a protected entry that ends in `/`. The prefix case is what makes
/// S3-disk metadata safe: a manifest references the part's metadata *directory*
/// (`.../{part}/`), never the individual `.../{part}/checksums.txt` files
/// underneath it, so exact set membership alone would report every one of those
/// files as unreferenced and delete a surviving backup's metadata.
///
/// Protected entries without a trailing `/` match exactly and never by prefix,
/// so a key under a sibling part (`a/b/part10/x` vs protected `a/b/part1/`) is
/// not accidentally protected.
pub fn is_key_protected(relative_key: &str, protected: &HashSet<String>) -> bool {
    protected.contains(relative_key)
        || protected
            .iter()
            .any(|entry| entry.ends_with('/') && relative_key.starts_with(entry))
}

/// Which backups a retention pass should delete, and which it keeps.
#[derive(Debug)]
pub struct RetentionPlan {
    /// Names of the backups to delete, oldest first.
    pub to_delete: Vec<String>,
    /// Names of the backups that survive the pass, oldest first.
    pub surviving: Vec<String>,
}

/// Decide which backups to delete to bring the valid backup count down to `keep`.
///
/// Broken backups are ignored entirely: they neither count towards `keep` nor
/// appear in either list, because retention does not delete them (that is
/// `clean_broken`'s job).
///
/// `keep` is the literal number of newest valid backups to keep -- `keep == 0`
/// plans every valid backup for deletion. The `keep == 0` means "unlimited"
/// config sentinel is the caller's concern, handled before calling this.
pub fn plan_retention_deletions(summaries: &[BackupSummary], keep: usize) -> RetentionPlan {
    let mut valid: Vec<&BackupSummary> = summaries.iter().filter(|b| !b.is_broken).collect();

    // Oldest first, so the deletions are the leading slice.
    valid.sort_by_key(|s| s.timestamp);

    let to_delete = valid.len().saturating_sub(keep);
    RetentionPlan {
        to_delete: valid
            .iter()
            .take(to_delete)
            .map(|b| b.name.clone())
            .collect(),
        surviving: valid
            .iter()
            .skip(to_delete)
            .map(|b| b.name.clone())
            .collect(),
    }
}

/// Backup names a manifest names directly as an incremental base
/// (`PartInfo.source == "carried:{base}"`).
///
/// This is only a cheap extra guard, and it is one hop: it sees the bases a
/// surviving manifest names, not the bases of *those* bases. Key-prefix
/// protection via `is_key_protected` is the real mechanism, and unlike this it
/// is transitively complete over a chain (see
/// `collect_key_prefixes_from_manifest`).
fn collect_incremental_bases(manifest: &BackupManifest) -> HashSet<String> {
    let mut bases = HashSet::new();

    for table in manifest.tables.values() {
        for parts in table.parts.values() {
            for part in parts {
                if let Some(base_name) = part.source.strip_prefix("carried:") {
                    bases.insert(base_name.to_string());
                }
            }
        }
    }

    bases
}

/// What a retention pass did to the backups it was asked to delete.
///
/// A planned backup that appears in neither list is one whose own S3 calls
/// failed; that is logged and retried by the next pass.
#[derive(Debug, Default)]
pub struct RetentionOutcome {
    /// Backups whose keys and manifest were deleted.
    pub deleted: Vec<String>,
    /// Backups deliberately left intact, manifest included.
    pub skipped: Vec<String>,
}

/// Run one retention pass over `plan`, with the S3 operations injected.
///
/// The three operations are parameters rather than direct `S3Client` calls
/// because an `S3Client` cannot be constructed in a unit test, and the
/// fail-closed abort below is precisely what needs a test:
///
/// - `fetch_manifest(backup_name)` -- parsed manifest of a surviving backup.
/// - `list_prefix(prefix)` -- keys under a prefix, relative to the S3 prefix.
/// - `delete_keys(keys)` -- batch delete, keys relative to the S3 prefix.
///
/// The pass fails **closed**: if any surviving backup's manifest cannot be
/// fetched or parsed, it returns `Err` before deleting anything at all. An
/// unreadable manifest is one whose keys we cannot protect, so continuing would
/// let GC delete data a survivor still points at -- and skipping just that
/// manifest, or just that candidate, is not enough, because every later
/// candidate would then be judged against the same short protected set.
///
/// For each candidate, deletion is all-or-nothing: if *any* key under its prefix
/// is still protected, the backup is left completely intact. Deleting the
/// unreferenced keys and the manifest anyway would manufacture a backup that
/// still owns live data but has no manifest -- exactly the broken-but-referenced
/// state `clean_broken` later destroys.
pub async fn retention_remote_inner<FM, FMFut, LP, LPFut, DK, DKFut>(
    plan: &RetentionPlan,
    fetch_manifest: FM,
    list_prefix: LP,
    delete_keys: DK,
) -> Result<RetentionOutcome>
where
    FM: Fn(String) -> FMFut,
    FMFut: Future<Output = Result<BackupManifest>>,
    LP: Fn(String) -> LPFut,
    LPFut: Future<Output = Result<Vec<String>>>,
    DK: Fn(Vec<String>) -> DKFut,
    DKFut: Future<Output = Result<()>>,
{
    let mut protected: HashSet<String> = HashSet::new();
    let mut incremental_bases: HashSet<String> = HashSet::new();

    for name in &plan.surviving {
        let manifest = fetch_manifest(name.clone()).await.with_context(|| {
            format!(
                "retention aborted before deleting anything: cannot read the manifest of \
                 surviving backup '{}', so its keys cannot be protected",
                name
            )
        })?;
        protected.extend(collect_key_prefixes_from_manifest(&manifest));
        incremental_bases.extend(collect_incremental_bases(&manifest));
    }

    let mut outcome = RetentionOutcome::default();

    for name in &plan.to_delete {
        if incremental_bases.contains(name) {
            warn!(
                backup = %name,
                "retention_remote: kept, a surviving backup names it as an incremental base"
            );
            outcome.skipped.push(name.clone());
            continue;
        }

        let keys = match list_prefix(format!("{}/", name)).await {
            Ok(keys) => keys,
            Err(e) => {
                warn!(backup = %name, error = %e, "retention_remote: failed to list keys");
                continue;
            }
        };
        if keys.is_empty() {
            warn!(backup = %name, "retention_remote: no objects under its prefix, nothing to delete");
            continue;
        }

        let manifest_key = format!("{}/metadata.json", name);
        let mut data_keys: Vec<String> = Vec::new();
        let mut protected_count: usize = 0;
        for key in keys {
            if key == manifest_key {
                continue;
            }
            if is_key_protected(&key, &protected) {
                protected_count += 1;
            } else {
                data_keys.push(key);
            }
        }

        if protected_count > 0 {
            warn!(
                backup = %name,
                protected_keys = protected_count,
                "retention_remote: kept intact, some of its keys are still referenced"
            );
            outcome.skipped.push(name.clone());
            continue;
        }

        if let Err(e) = delete_keys(data_keys).await {
            warn!(backup = %name, error = %e, "retention_remote: failed to delete data keys");
            continue;
        }
        // Manifest last: a crash in between leaves a broken backup `clean_broken`
        // can finish off, never a manifest pointing at keys that are already gone.
        if let Err(e) = delete_keys(vec![manifest_key]).await {
            warn!(backup = %name, error = %e, "retention_remote: failed to delete manifest");
            continue;
        }

        info!(backup = %name, "retention_remote: deleted old remote backup");
        outcome.deleted.push(name.clone());
    }

    Ok(outcome)
}

/// Delete oldest remote backups exceeding the `keep` count with GC-safe deletion.
///
/// Thin wrapper: it lists the remote backups, plans the pass with
/// `plan_retention_deletions`, and hands `retention_remote_inner` the real
/// S3-backed operations. All the policy -- fail-closed key protection,
/// all-or-nothing candidate deletion, manifest-last ordering -- lives there.
///
/// Broken backups are neither counted towards `keep` nor deleted (that is
/// `clean_broken`'s job); they have no manifest to protect keys from, so each is
/// logged as ignored.
///
/// - `keep <= 0`: unlimited, no retention action.
/// - `keep > 0`: keep the N newest valid backups, delete the rest.
///
/// Returns the number of successfully deleted backups.
pub async fn retention_remote(s3: &S3Client, keep: i32) -> Result<usize> {
    if keep <= 0 {
        return Ok(0);
    }

    let backups = list_remote(s3).await?;
    for broken in backups.iter().filter(|b| b.is_broken) {
        warn!(
            backup = %broken.name,
            "retention_remote: ignoring broken backup, it has no manifest to read"
        );
    }

    let plan = plan_retention_deletions(&backups, keep as usize);
    if plan.to_delete.is_empty() {
        return Ok(0);
    }

    let s3_prefix = s3.prefix();
    let outcome = retention_remote_inner(
        &plan,
        |name: String| async move {
            let data = s3.get_object(&format!("{}/metadata.json", name)).await?;
            BackupManifest::from_json_bytes(&data)
        },
        |prefix: String| async move {
            let objects = s3.list_objects(&prefix).await?;
            Ok(objects
                .iter()
                .map(|obj| strip_s3_prefix(&obj.key, s3_prefix))
                .collect())
        },
        |keys: Vec<String>| async move {
            let failed = s3.delete_objects(keys).await?;
            if failed > 0 {
                warn!(
                    failed_count = failed,
                    "retention_remote: some S3 deletions failed, GC retries them next pass"
                );
            }
            Ok(())
        },
    )
    .await?;

    info!(
        deleted = outcome.deleted.len(),
        skipped = outcome.skipped.len(),
        total = backups.len(),
        "retention_remote: deleted N of M remote backups"
    );
    Ok(outcome.deleted.len())
}

// -- Shadow cleanup functions --

/// Build the set of sanitized freeze-name prefixes for all backups whose PID
/// lock files are currently held by a live process.
///
/// Scans `/tmp/chbackup.*.pid`, skipping `global.pid`. For each live-PID file
/// the backup name is extracted from the filename and sanitized.
fn active_freeze_prefixes() -> HashSet<String> {
    let mut prefixes = HashSet::new();
    let tmp_dir = std::path::Path::new("/tmp");
    let entries = match std::fs::read_dir(tmp_dir) {
        Ok(e) => e,
        Err(_) => return prefixes,
    };
    for entry in entries.flatten() {
        let fname = match entry.file_name().into_string() {
            Ok(s) => s,
            Err(_) => continue,
        };
        // Match "chbackup.{name}.pid" but not "chbackup.global.pid"
        if !fname.starts_with("chbackup.") || !fname.ends_with(".pid") {
            continue;
        }
        let inner = &fname["chbackup.".len()..fname.len() - ".pid".len()];
        if inner == "global" {
            continue;
        }
        if crate::lock::is_lock_file_active(&entry.path()) {
            // Both schemes: a live backup may hold shadows written under the current
            // collision-free naming, or legacy ones from a binary predating it. Emitting
            // only one prefix would leave the other unprotected against unfiltered cleanup.
            prefixes.insert(freeze_prefix(inner));
            prefixes.insert(legacy_freeze_prefix(inner));
        }
    }
    prefixes
}

/// Remove `chbackup_*` directories from a single disk's shadow path (sync helper).
///
/// If `name` is provided, only removes entries matching that backup's freeze prefix --
/// both the current `chbackup__{enc(name)}__*` form and the legacy
/// `chbackup_{sanitize_name(name)}_*` form, so shadows written by binaries predating the
/// collision-free naming change are still reaped.
/// If `name` is `None`, removes all entries matching `chbackup_*`.
///
/// Skips any freeze directories that belong to a currently-active backup (PID
/// lock file exists and held by a live process) to avoid racing with in-progress
/// `backup::create` operations.
///
/// Returns the number of directories removed.
fn clean_shadow_dir(disk_path: &str, name: Option<&str>, force: bool) -> Result<usize> {
    let shadow_path = PathBuf::from(disk_path).join("shadow");

    if !shadow_path.exists() {
        return Ok(0);
    }

    let entries = std::fs::read_dir(&shadow_path)
        .with_context(|| format!("Failed to read shadow directory: {}", shadow_path.display()))?;

    // Match both the current and legacy freeze prefixes. The legacy branch is a
    // deprecation shim: remove it once no deployment can still hold shadows written
    // before the collision-free naming change.
    let prefix_filters: Vec<String> = name
        .map(|n| vec![freeze_prefix(n), legacy_freeze_prefix(n)])
        .unwrap_or_default();

    // When cleaning a specific backup, check its per-backup PID lock once up front.
    // Skip this check when force=true (called from cleanup_failed_backup which holds the lock).
    if !force {
        if let Some(n) = name {
            let lock_path = std::path::PathBuf::from(format!("/tmp/chbackup.{n}.pid"));
            if crate::lock::is_lock_file_active(&lock_path) {
                warn!(
                    backup = %n,
                    disk = %disk_path,
                    "clean_shadow: skipping disk, backup is currently active"
                );
                return Ok(0);
            }
        }
    }

    // When cleaning all backups, collect the prefixes of every live backup so
    // we can skip individual freeze directories that are still in use.
    let active_prefixes: HashSet<String> = if name.is_none() {
        active_freeze_prefixes()
    } else {
        HashSet::new()
    };

    let mut removed = 0;
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                warn!(error = %e, "Failed to read shadow directory entry");
                continue;
            }
        };

        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let dir_name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };

        let should_remove = if prefix_filters.is_empty() {
            dir_name.starts_with("chbackup_")
        } else {
            prefix_filters.iter().any(|p| dir_name.starts_with(p))
        };

        if !should_remove {
            continue;
        }

        // Skip directories that belong to a currently-active backup.
        if active_prefixes
            .iter()
            .any(|p| dir_name.starts_with(p.as_str()))
        {
            warn!(
                freeze_name = %dir_name,
                disk = %disk_path,
                "clean_shadow: skipping directory, backup is currently active"
            );
            continue;
        }

        match std::fs::remove_dir_all(&path) {
            Ok(()) => {
                info!(
                    freeze_name = %dir_name,
                    disk = %disk_path,
                    "clean_shadow: removed shadow directory"
                );
                removed += 1;
            }
            Err(e) => {
                warn!(
                    freeze_name = %dir_name,
                    disk = %disk_path,
                    error = %e,
                    "clean_shadow: failed to remove shadow directory"
                );
            }
        }
    }

    Ok(removed)
}

/// Remove `chbackup_*` shadow directories from all non-backup disks.
///
/// Queries ClickHouse for all disks, filters out backup-type disks (per design 13),
/// and removes matching shadow directories from each remaining disk.
///
/// If `name` is provided, only removes entries matching `chbackup_{sanitized_name}_*`.
/// Returns the total number of directories removed across all disks.
///
/// When `force` is true, skip PID lock checks (used by cleanup_failed_backup
/// which already holds the lock).
pub async fn clean_shadow(ch: &ChClient, data_path: &str, name: Option<&str>) -> Result<usize> {
    clean_shadow_inner(ch, data_path, name, false).await
}

/// Like `clean_shadow` but skips PID lock checks. Use when the caller already
/// holds the lock (e.g., cleanup after a failed backup).
pub async fn clean_shadow_force(
    ch: &ChClient,
    data_path: &str,
    name: Option<&str>,
) -> Result<usize> {
    clean_shadow_inner(ch, data_path, name, true).await
}

async fn clean_shadow_inner(
    ch: &ChClient,
    data_path: &str,
    name: Option<&str>,
    force: bool,
) -> Result<usize> {
    // Release expired orphaned deferred freezes first, so their shadow directories become
    // eligible for removal below. Without this, `clean` could never clear them: the guard
    // (correctly) refuses to rm -rf shadow data whose freeze is still registered with
    // ClickHouse, since that is not an UNFREEZE.
    //
    // `force` is exactly the "caller already holds this backup's lock" case, so it is also the
    // condition under which the reaper must ignore that lock as self-noise. Reached without
    // `force`, the caller holds `Global` and the reaper can acquire nothing -- it logs a debug
    // breadcrumb and releases nothing, which is expected rather than broken.
    let own_lock = if force { name } else { None };
    let reaped = crate::backup::deferred::reap_expired(ch, data_path, own_lock).await;
    if reaped > 0 {
        info!(
            count = reaped,
            "Released expired deferred S3 object-disk freezes before shadow cleanup"
        );
    }

    // Backstop: refuse to delete shadow data that another live process is deliberately
    // holding frozen so its S3 object-disk objects stay pinned until upload copies them.
    //
    // This is checked once up front and gates every disk, and it applies even when
    // `force` is set -- `force` exists to bypass the *PID lock* check for callers that
    // already hold that lock, not to override freeze ownership.
    //
    // Ownership is evidenced by holding the per-backup PID lock, NOT by PID equality: in
    // server mode `create` and `upload` share one long-lived process, so every record carries
    // the server's PID and a same-PID test would let an unrelated request in that process
    // destroy a live freeze. An orphaned record still blocks until its TTL expires, and an
    // unreadable record fails closed.
    if let Some(n) = name {
        if crate::backup::deferred::blocks_destructive_op(data_path, n, "clean_shadow") {
            return Ok(0);
        }
    }

    let disks = ch.get_disks().await?;

    let mut total = 0;
    for disk in &disks {
        // Skip backup-type disks per design 13
        if disk.disk_type == "backup" {
            debug!(disk = %disk.name, "Skipping backup-type disk for shadow cleanup");
            continue;
        }

        let disk_path = disk.path.clone();
        let name_owned = name.map(|n| n.to_string());
        let count = tokio::task::spawn_blocking(move || {
            clean_shadow_dir(&disk_path, name_owned.as_deref(), force)
        })
        .await
        .context("Shadow cleanup task panicked")??;

        total += count;
    }

    // Also check data_path itself in case it's not listed as a disk
    // (the default disk path may differ from system.disks entries)
    let data_path_in_disks = disks.iter().any(|d| d.path == data_path);
    if !data_path_in_disks {
        let dp = data_path.to_string();
        let name_owned = name.map(|n| n.to_string());
        let count = tokio::task::spawn_blocking(move || {
            clean_shadow_dir(&dp, name_owned.as_deref(), force)
        })
        .await
        .context("Shadow cleanup task panicked")??;
        total += count;
    }

    info!(total = total, "clean_shadow: removed N shadow directories");
    Ok(total)
}

// -- Internal helpers --

/// Build a valid `BackupSummary` from a parsed `BackupManifest`.
///
/// Computes `object_disk_size` (sum of S3 object sizes), `required` (diff-from
/// base name), and populates all size/count fields. Used by both `list_remote()`
/// and `parse_backup_summary()` to avoid duplicating this logic.
fn summary_from_manifest(manifest: &BackupManifest, backup_name: &str) -> BackupSummary {
    // Never trust manifest.name for operation targeting; callers pass the name
    // from the local directory or S3 prefix.
    let effective_name = if backup_name.is_empty() {
        &manifest.name
    } else {
        backup_name
    };
    let object_disk_size = compute_object_disk_size(manifest);
    let required = extract_required_backup(manifest);
    BackupSummary {
        name: effective_name.to_string(),
        timestamp: Some(manifest.timestamp),
        size: total_uncompressed_size(manifest),
        compressed_size: manifest.compressed_size,
        table_count: manifest.tables.len(),
        metadata_size: manifest.metadata_size,
        rbac_size: manifest.rbac_size,
        config_size: manifest.config_size,
        object_disk_size,
        required,
        is_broken: false,
        broken_reason: None,
    }
}

/// Compute the total size of S3 object disk parts in a manifest.
///
/// Sums `s3_objects[].size` across all parts in all tables. The `s3_objects`
/// field is only populated for S3 disk parts, so no disk type check is needed.
fn compute_object_disk_size(manifest: &BackupManifest) -> u64 {
    let mut total: u64 = 0;
    for table in manifest.tables.values() {
        for parts in table.parts.values() {
            for part in parts {
                if let Some(ref s3_objects) = part.s3_objects {
                    for obj in s3_objects {
                        total = total.saturating_add(obj.size);
                    }
                }
            }
        }
    }
    total
}

/// Extract the base backup name from incremental parts in a manifest.
///
/// Scans all parts for the first `source = "carried:{base_name}"` entry and
/// returns the base name. Returns an empty string for full backups (no carried parts).
fn extract_required_backup(manifest: &BackupManifest) -> String {
    for table in manifest.tables.values() {
        for parts in table.parts.values() {
            for part in parts {
                if let Some(base_name) = part.source.strip_prefix("carried:") {
                    return base_name.to_string();
                }
            }
        }
    }
    String::new()
}

/// Parse a backup summary from a metadata.json file path.
fn parse_backup_summary(name: &str, metadata_path: &Path) -> BackupSummary {
    if !metadata_path.exists() {
        return broken_summary(name.to_string(), "metadata.json not found".to_string());
    }

    match BackupManifest::load_from_file(metadata_path) {
        Ok(manifest) => summary_from_manifest(&manifest, name),
        Err(e) => {
            let reason = format!("manifest parse error: {e:#}");
            warn!(
                backup = %name,
                path = %metadata_path.display(),
                error = format_args!("{e:#}"),
                "Failed to parse manifest, marking as broken"
            );
            broken_summary(name.to_string(), reason)
        }
    }
}

/// Compute total uncompressed size from all table parts.
fn total_uncompressed_size(manifest: &BackupManifest) -> u64 {
    manifest.tables.values().map(|t| t.total_bytes).sum()
}

/// Extract backup name from an S3 common prefix.
///
/// Common prefixes look like "chbackup/daily-2024-01-15/" where "chbackup"
/// is the S3Client prefix. We strip the prefix and trailing slash to get
/// just "daily-2024-01-15".
fn extract_backup_name_from_prefix(common_prefix: &str, s3_prefix: &str) -> String {
    let stripped = strip_s3_prefix(common_prefix, s3_prefix);
    stripped.trim_matches('/').to_string()
}

/// Strip the S3 client prefix from a key.
///
/// If key starts with `"{prefix}/"`, remove that part. Otherwise return as-is.
fn strip_s3_prefix(key: &str, prefix: &str) -> String {
    if prefix.is_empty() {
        return key.to_string();
    }
    let prefix_with_slash = if prefix.ends_with('/') {
        prefix.to_string()
    } else {
        format!("{}/", prefix)
    };
    if key.starts_with(&prefix_with_slash) {
        key[prefix_with_slash.len()..].to_string()
    } else {
        key.to_string()
    }
}

/// Format a byte count into human-readable units.
pub fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    const TB: u64 = 1024 * GB;

    if bytes >= TB {
        format!("{:.2} TB", bytes as f64 / TB as f64)
    } else if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

/// Print a formatted table of backup summaries.
///
/// Delegates to [`format_list_output`] with [`ListFormat::Default`] to avoid
/// duplicating the human-readable table formatting logic.
fn print_backup_table(summaries: &[BackupSummary]) {
    // format_list_output with Default never fails (no serialization involved).
    if let Ok(output) = format_list_output(summaries, &ListFormat::Default) {
        if !output.is_empty() {
            println!("{output}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, HashMap};

    #[test]
    fn test_parse_local_backup_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let backup_base = dir.path().join("backup");
        std::fs::create_dir_all(&backup_base).unwrap();

        // Create a valid backup with metadata.json
        let backup1 = backup_base.join("daily-2024-01-15");
        std::fs::create_dir_all(&backup1).unwrap();
        let manifest = BackupManifest::test_new("daily-2024-01-15")
            .with_compressed_size(1024)
            .with_metadata_size(256);
        manifest
            .save_to_file(&backup1.join("metadata.json"))
            .unwrap();

        // Create a broken backup (no metadata.json)
        let backup2 = backup_base.join("broken-backup");
        std::fs::create_dir_all(&backup2).unwrap();

        let summaries = list_local(dir.path().to_str().unwrap()).unwrap();
        assert_eq!(summaries.len(), 2);

        // Results are sorted by timestamp (name used as tiebreaker)
        let broken = summaries
            .iter()
            .find(|s| s.name == "broken-backup")
            .unwrap();
        assert!(broken.is_broken);
        assert!(broken.timestamp.is_none());

        let valid = summaries
            .iter()
            .find(|s| s.name == "daily-2024-01-15")
            .unwrap();
        assert!(!valid.is_broken);
        assert!(valid.timestamp.is_some());
        assert_eq!(valid.table_count, 0);
    }

    #[test]
    fn test_parse_local_backup_with_tables() {
        use crate::manifest::TableManifest;
        let dir = tempfile::tempdir().unwrap();
        let backup_base = dir.path().join("backup");
        std::fs::create_dir_all(&backup_base).unwrap();

        let backup1 = backup_base.join("test-backup");
        std::fs::create_dir_all(&backup1).unwrap();

        let mut tables = BTreeMap::new();
        tables.insert(
            "default.trades".to_string(),
            TableManifest::test_new("MergeTree")
                .with_ddl("CREATE TABLE default.trades (id UInt64) ENGINE = MergeTree ORDER BY id")
                .with_total_bytes(1_000_000),
        );

        let manifest = BackupManifest::test_new("test-backup")
            .with_tables(tables)
            .with_compressed_size(500_000)
            .with_metadata_size(256);
        manifest
            .save_to_file(&backup1.join("metadata.json"))
            .unwrap();

        let summaries = list_local(dir.path().to_str().unwrap()).unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].name, "test-backup");
        assert_eq!(summaries[0].table_count, 1);
        assert_eq!(summaries[0].size, 1_000_000);
        assert!(!summaries[0].is_broken);
    }

    #[test]
    fn test_list_local_no_backup_dir() {
        let dir = tempfile::tempdir().unwrap();
        // Don't create backup dir
        let summaries = list_local(dir.path().to_str().unwrap()).unwrap();
        assert!(summaries.is_empty());
    }

    #[test]
    fn test_delete_local_backup() {
        let dir = tempfile::tempdir().unwrap();
        let backup_base = dir.path().join("backup");
        let backup_dir = backup_base.join("test-delete");
        std::fs::create_dir_all(&backup_dir).unwrap();
        std::fs::write(backup_dir.join("metadata.json"), "{}").unwrap();

        assert!(backup_dir.exists());
        delete_local(dir.path().to_str().unwrap(), "test-delete").unwrap();
        assert!(!backup_dir.exists());
    }

    #[test]
    fn test_delete_local_refuses_while_deferred_freeze_is_held() {
        // Regression: the backup dir contains deferred_freeze.json, so deleting it destroyed
        // the only record of a held S3 object-disk freeze -- leaking it with nothing left to
        // find it by and no UNFREEZE issued. This fired automatically on the retention path
        // after every successful upload.
        use crate::backup::deferred::{publish, DeferredFreezeRecord, DEFAULT_TTL_SECS};
        use crate::backup::freeze::FreezeInfo;

        let dir = tempfile::tempdir().unwrap();
        let backup_dir = dir.path().join("backup").join("held");
        std::fs::create_dir_all(&backup_dir).unwrap();
        std::fs::write(backup_dir.join("metadata.json"), "{}").unwrap();

        let rec = DeferredFreezeRecord::new(
            "held",
            vec![FreezeInfo {
                database: "db".into(),
                table: "t".into(),
                freeze_name: crate::clickhouse::freeze_name("held", "db", "t"),
            }],
            DEFAULT_TTL_SECS,
        );
        publish(&backup_dir, &rec).unwrap();

        let result = delete_local(dir.path().to_str().unwrap(), "held");
        assert!(
            result.is_err(),
            "must refuse, not silently destroy the record"
        );
        assert!(
            backup_dir.exists(),
            "backup dir must survive so the freeze stays discoverable"
        );
        let msg = format!("{:#}", result.unwrap_err());
        assert!(
            msg.contains("deferred") && msg.contains("freeze"),
            "error should explain why: {msg}"
        );
    }

    #[test]
    fn test_delete_local_proceeds_when_no_deferred_freeze() {
        // The guard must not block ordinary deletion.
        let dir = tempfile::tempdir().unwrap();
        let backup_dir = dir.path().join("backup").join("free");
        std::fs::create_dir_all(&backup_dir).unwrap();
        std::fs::write(backup_dir.join("metadata.json"), "{}").unwrap();

        delete_local(dir.path().to_str().unwrap(), "free").unwrap();
        assert!(!backup_dir.exists());
    }

    #[test]
    fn test_delete_local_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let backup_base = dir.path().join("backup");
        std::fs::create_dir_all(&backup_base).unwrap();

        let result = delete_local(dir.path().to_str().unwrap(), "nonexistent");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("not found"));
    }

    #[test]
    fn test_format_size() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1024), "1.00 KB");
        assert_eq!(format_size(1_048_576), "1.00 MB");
        assert_eq!(format_size(1_073_741_824), "1.00 GB");
        assert_eq!(format_size(1_099_511_627_776), "1.00 TB");
    }

    #[test]
    fn test_print_backup_table_shows_compressed_size() {
        let summaries = [BackupSummary {
            name: "test-backup".to_string(),
            timestamp: Some(chrono::Utc::now()),
            size: 1_048_576,          // 1 MB
            compressed_size: 524_288, // 512 KB
            table_count: 3,
            metadata_size: 0,
            rbac_size: 0,
            config_size: 0,
            object_disk_size: 0,
            required: String::new(),
            is_broken: false,
            broken_reason: None,
        }];

        // Verify format_size calls produce expected strings and that
        // both values appear in the formatted output.
        let size_str = format_size(1_048_576);
        let compressed_str = format_size(524_288);
        assert_eq!(size_str, "1.00 MB");
        assert_eq!(compressed_str, "512.00 KB");

        // Verify the print function includes both size columns by building
        // the expected output line manually and checking it matches what
        // print_backup_table would produce.
        let s = &summaries[0];
        let ts = s
            .timestamp
            .unwrap()
            .format("%Y-%m-%d %H:%M:%S UTC")
            .to_string();
        let expected_line = format!(
            "  {}\t{}\t{}\t{}\t{} tables",
            s.name, ts, size_str, compressed_str, s.table_count
        );
        assert!(
            expected_line.contains("1.00 MB"),
            "Expected line to contain '1.00 MB'"
        );
        assert!(
            expected_line.contains("512.00 KB"),
            "Expected line to contain '512.00 KB'"
        );
        assert!(
            expected_line.contains("3 tables"),
            "Expected line to contain '3 tables'"
        );
    }

    #[test]
    fn test_extract_backup_name_from_prefix() {
        assert_eq!(
            extract_backup_name_from_prefix("chbackup/daily-2024-01-15/", "chbackup"),
            "daily-2024-01-15"
        );
        assert_eq!(
            extract_backup_name_from_prefix("daily-2024-01-15/", ""),
            "daily-2024-01-15"
        );
        assert_eq!(
            extract_backup_name_from_prefix(
                "prod/region1/chbackup/daily/",
                "prod/region1/chbackup"
            ),
            "daily"
        );
    }

    #[test]
    fn test_strip_s3_prefix() {
        assert_eq!(
            strip_s3_prefix("chbackup/daily/metadata.json", "chbackup"),
            "daily/metadata.json"
        );
        assert_eq!(
            strip_s3_prefix("daily/metadata.json", ""),
            "daily/metadata.json"
        );
        assert_eq!(strip_s3_prefix("other/key", "chbackup"), "other/key");
    }

    #[test]
    fn test_broken_backup_display_reason() {
        // A broken backup with missing metadata.json should show the reason
        let dir = tempfile::tempdir().unwrap();
        let backup_base = dir.path().join("backup");
        std::fs::create_dir_all(&backup_base).unwrap();

        // Create a broken backup (no metadata.json)
        let broken_dir = backup_base.join("broken-no-meta");
        std::fs::create_dir_all(&broken_dir).unwrap();

        // Create a broken backup with invalid metadata.json
        let broken_invalid = backup_base.join("broken-invalid");
        std::fs::create_dir_all(&broken_invalid).unwrap();
        std::fs::write(broken_invalid.join("metadata.json"), "not valid json").unwrap();

        let summaries = list_local(dir.path().to_str().unwrap()).unwrap();
        assert_eq!(summaries.len(), 2);

        let no_meta = summaries
            .iter()
            .find(|s| s.name == "broken-no-meta")
            .unwrap();
        assert!(no_meta.is_broken);
        assert!(no_meta.broken_reason.is_some());
        assert!(
            no_meta
                .broken_reason
                .as_ref()
                .unwrap()
                .contains("metadata.json not found"),
            "Expected 'metadata.json not found' but got: {:?}",
            no_meta.broken_reason
        );

        let invalid = summaries
            .iter()
            .find(|s| s.name == "broken-invalid")
            .unwrap();
        assert!(invalid.is_broken);
        assert!(invalid.broken_reason.is_some());
        assert!(
            invalid
                .broken_reason
                .as_ref()
                .unwrap()
                .contains("manifest parse error"),
            "Expected 'manifest parse error' but got: {:?}",
            invalid.broken_reason
        );
    }

    #[tokio::test]
    async fn test_clean_broken_local() {
        let dir = tempfile::tempdir().unwrap();
        let backup_base = dir.path().join("backup");
        std::fs::create_dir_all(&backup_base).unwrap();

        // Create a broken backup (no metadata.json)
        let broken_dir = backup_base.join("broken-backup");
        std::fs::create_dir_all(&broken_dir).unwrap();

        // Create another broken backup with invalid JSON
        let broken_dir2 = backup_base.join("broken-invalid");
        std::fs::create_dir_all(&broken_dir2).unwrap();
        std::fs::write(broken_dir2.join("metadata.json"), "bad json").unwrap();

        // Verify both exist
        assert!(broken_dir.exists());
        assert!(broken_dir2.exists());

        // Clean broken
        let count = clean_broken_local(dir.path().to_str().unwrap(), None)
            .await
            .unwrap();
        assert_eq!(count, 2, "Should have deleted 2 broken backups");

        // Verify both are gone
        assert!(!broken_dir.exists());
        assert!(!broken_dir2.exists());
    }

    #[tokio::test]
    async fn test_clean_broken_local_preserves_valid() {
        let dir = tempfile::tempdir().unwrap();
        let backup_base = dir.path().join("backup");
        std::fs::create_dir_all(&backup_base).unwrap();

        // Create a valid backup
        let valid_dir = backup_base.join("valid-backup");
        std::fs::create_dir_all(&valid_dir).unwrap();
        let manifest = BackupManifest::test_new("valid-backup")
            .with_compressed_size(1024)
            .with_metadata_size(256);
        manifest
            .save_to_file(&valid_dir.join("metadata.json"))
            .unwrap();

        // Create a broken backup
        let broken_dir = backup_base.join("broken-backup");
        std::fs::create_dir_all(&broken_dir).unwrap();

        // Clean broken
        let count = clean_broken_local(dir.path().to_str().unwrap(), None)
            .await
            .unwrap();
        assert_eq!(count, 1, "Should have deleted 1 broken backup");

        // Verify valid backup is preserved
        assert!(valid_dir.exists());
        // Verify broken backup is gone
        assert!(!broken_dir.exists());
    }

    #[test]
    fn test_clean_shadow_removes_chbackup_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let shadow_dir = dir.path().join("shadow");
        std::fs::create_dir_all(&shadow_dir).unwrap();

        // Create chbackup shadow directories (should be removed)
        let chbackup1 = shadow_dir.join("chbackup_daily_mon_default_trades");
        std::fs::create_dir_all(&chbackup1).unwrap();
        // Add a file inside to ensure remove_dir_all works
        std::fs::write(chbackup1.join("data.bin"), b"test").unwrap();

        let chbackup2 = shadow_dir.join("chbackup_weekly_default_events");
        std::fs::create_dir_all(&chbackup2).unwrap();

        // Create non-chbackup shadow directory (should NOT be removed)
        let other = shadow_dir.join("other_freeze_data");
        std::fs::create_dir_all(&other).unwrap();

        let count = clean_shadow_dir(dir.path().to_str().unwrap(), None, false).unwrap();

        assert_eq!(count, 2, "Should have removed 2 chbackup shadow dirs");
        assert!(!chbackup1.exists(), "chbackup_daily_mon should be removed");
        assert!(!chbackup2.exists(), "chbackup_weekly should be removed");
        assert!(other.exists(), "other_freeze_data should NOT be removed");
    }

    #[test]
    fn test_clean_shadow_name_filter_matches_current_format() {
        let dir = tempfile::tempdir().unwrap();
        let shadow_dir = dir.path().join("shadow");
        std::fs::create_dir_all(&shadow_dir).unwrap();

        // Current collision-free naming: chbackup__{enc(name)}__{enc(db)}__{enc(table)}
        let current = shadow_dir.join(crate::clickhouse::freeze_name(
            "daily-mon",
            "default",
            "trades",
        ));
        std::fs::create_dir_all(&current).unwrap();
        std::fs::write(current.join("data.bin"), b"test").unwrap();

        let count =
            clean_shadow_dir(dir.path().to_str().unwrap(), Some("daily-mon"), false).unwrap();

        assert_eq!(count, 1, "current-format shadow should be removed");
        assert!(!current.exists());
    }

    #[test]
    fn test_clean_shadow_name_filter_handles_both_formats_together() {
        let dir = tempfile::tempdir().unwrap();
        let shadow_dir = dir.path().join("shadow");
        std::fs::create_dir_all(&shadow_dir).unwrap();

        // One shadow from an older binary, one from the current one, same backup.
        let legacy = shadow_dir.join("chbackup_daily_mon_default_trades");
        std::fs::create_dir_all(&legacy).unwrap();
        let current = shadow_dir.join(crate::clickhouse::freeze_name(
            "daily-mon",
            "default",
            "users",
        ));
        std::fs::create_dir_all(&current).unwrap();

        let count =
            clean_shadow_dir(dir.path().to_str().unwrap(), Some("daily-mon"), false).unwrap();

        assert_eq!(count, 2, "both legacy and current shadows should be reaped");
        assert!(!legacy.exists());
        assert!(!current.exists());
    }

    #[test]
    fn test_clean_shadow_current_format_no_cross_name_collision() {
        // The whole point of collision-free naming: cleaning `daily-mon` must not touch
        // `daily_mon`, which the old sanitize_name scheme mapped to the same prefix.
        let dir = tempfile::tempdir().unwrap();
        let shadow_dir = dir.path().join("shadow");
        std::fs::create_dir_all(&shadow_dir).unwrap();

        let hyphen = shadow_dir.join(crate::clickhouse::freeze_name("daily-mon", "db", "t"));
        std::fs::create_dir_all(&hyphen).unwrap();
        let underscore = shadow_dir.join(crate::clickhouse::freeze_name("daily_mon", "db", "t"));
        std::fs::create_dir_all(&underscore).unwrap();
        assert_ne!(hyphen, underscore, "fixture requires distinct dir names");

        let count =
            clean_shadow_dir(dir.path().to_str().unwrap(), Some("daily-mon"), false).unwrap();

        assert_eq!(
            count, 1,
            "only the hyphen backup's shadow should be removed"
        );
        assert!(!hyphen.exists());
        assert!(
            underscore.exists(),
            "daily_mon's shadow must survive cleanup of daily-mon"
        );
    }

    #[test]
    fn test_clean_shadow_legacy_format_remains_ambiguous() {
        // Known limitation of the legacy shim: old-format directories were written with
        // the many-to-one sanitize_name mapping, so `daily-mon` and `daily_mon` are
        // indistinguishable in that format. Nothing can recover the original name
        // retroactively; this is documented rather than fixed, and disappears when the
        // legacy branch is removed.
        assert_eq!(
            legacy_freeze_prefix("daily-mon"),
            legacy_freeze_prefix("daily_mon")
        );
        // The current format is unambiguous, which is what protects live backups.
        assert_ne!(freeze_prefix("daily-mon"), freeze_prefix("daily_mon"));
    }

    #[test]
    fn test_clean_shadow_with_name_filter() {
        let dir = tempfile::tempdir().unwrap();
        let shadow_dir = dir.path().join("shadow");
        std::fs::create_dir_all(&shadow_dir).unwrap();

        // Create chbackup shadow directories
        let chbackup1 = shadow_dir.join("chbackup_daily_mon_default_trades");
        std::fs::create_dir_all(&chbackup1).unwrap();

        let chbackup2 = shadow_dir.join("chbackup_weekly_default_events");
        std::fs::create_dir_all(&chbackup2).unwrap();

        // Filter by backup name "daily-mon" -> sanitized to "daily_mon"
        let count =
            clean_shadow_dir(dir.path().to_str().unwrap(), Some("daily-mon"), false).unwrap();

        assert_eq!(count, 1, "Should have removed 1 matching shadow dir");
        assert!(!chbackup1.exists(), "chbackup_daily_mon should be removed");
        assert!(
            chbackup2.exists(),
            "chbackup_weekly should NOT be removed (different backup name)"
        );
    }

    #[test]
    fn test_clean_shadow_no_shadow_dir() {
        let dir = tempfile::tempdir().unwrap();
        // No shadow directory created
        let count = clean_shadow_dir(dir.path().to_str().unwrap(), None, false).unwrap();
        assert_eq!(count, 0, "Should return 0 when no shadow dir exists");
    }

    #[test]
    fn test_clean_shadow_empty_shadow_dir() {
        let dir = tempfile::tempdir().unwrap();
        let shadow_dir = dir.path().join("shadow");
        std::fs::create_dir_all(&shadow_dir).unwrap();
        // Shadow dir exists but empty
        let count = clean_shadow_dir(dir.path().to_str().unwrap(), None, false).unwrap();
        assert_eq!(count, 0, "Should return 0 when shadow dir is empty");
    }

    // -- Retention tests --

    /// Helper to create a valid backup with a specific timestamp in the temp dir.
    fn create_backup_with_timestamp(
        backup_base: &std::path::Path,
        name: &str,
        timestamp: DateTime<Utc>,
    ) {
        let backup_dir = backup_base.join(name);
        std::fs::create_dir_all(&backup_dir).unwrap();
        let manifest = BackupManifest::test_new(name)
            .with_timestamp(timestamp)
            .with_compressed_size(1024)
            .with_metadata_size(256);
        manifest
            .save_to_file(&backup_dir.join("metadata.json"))
            .unwrap();
    }

    #[test]
    fn test_effective_retention_local() {
        use crate::config::Config;

        // retention overrides general when non-zero
        let mut config = Config::default();
        config.retention.backups_to_keep_local = 3;
        config.general.backups_to_keep_local = 5;
        assert_eq!(effective_retention_local(&config), 3);

        // fallback to general when retention is 0
        let mut config2 = Config::default();
        config2.retention.backups_to_keep_local = 0;
        config2.general.backups_to_keep_local = 5;
        assert_eq!(effective_retention_local(&config2), 5);

        // both zero => 0
        let config3 = Config::default();
        assert_eq!(effective_retention_local(&config3), 0);

        // remote variant
        let mut config4 = Config::default();
        config4.retention.backups_to_keep_remote = 7;
        config4.general.backups_to_keep_remote = 10;
        assert_eq!(effective_retention_remote(&config4), 7);

        // remote fallback
        let mut config5 = Config::default();
        config5.retention.backups_to_keep_remote = 0;
        config5.general.backups_to_keep_remote = 10;
        assert_eq!(effective_retention_remote(&config5), 10);
    }

    #[test]
    fn test_retention_local_deletes_oldest() {
        let dir = tempfile::tempdir().unwrap();
        let backup_base = dir.path().join("backup");
        std::fs::create_dir_all(&backup_base).unwrap();

        let base_ts = chrono::Utc::now();

        // Create 5 backups with timestamps spread 1 day apart
        for i in 0..5 {
            let ts = base_ts - chrono::Duration::days(4 - i);
            create_backup_with_timestamp(&backup_base, &format!("backup-day-{}", i), ts);
        }

        // Keep 3
        let deleted = retention_local(dir.path().to_str().unwrap(), 3).unwrap();
        assert_eq!(deleted, 2, "Should have deleted 2 oldest backups");

        // The 3 newest (day-2, day-3, day-4) should remain
        assert!(backup_base.join("backup-day-2").exists());
        assert!(backup_base.join("backup-day-3").exists());
        assert!(backup_base.join("backup-day-4").exists());

        // The 2 oldest (day-0, day-1) should be gone
        assert!(!backup_base.join("backup-day-0").exists());
        assert!(!backup_base.join("backup-day-1").exists());
    }

    #[test]
    fn test_retention_local_skips_broken() {
        let dir = tempfile::tempdir().unwrap();
        let backup_base = dir.path().join("backup");
        std::fs::create_dir_all(&backup_base).unwrap();

        let base_ts = chrono::Utc::now();

        // Create 4 valid backups
        for i in 0..4 {
            let ts = base_ts - chrono::Duration::days(3 - i);
            create_backup_with_timestamp(&backup_base, &format!("backup-{}", i), ts);
        }

        // Create 1 broken backup (no metadata.json)
        let broken_dir = backup_base.join("broken-backup");
        std::fs::create_dir_all(&broken_dir).unwrap();

        // Keep 3 => should delete 1 oldest valid backup, leaving broken untouched
        let deleted = retention_local(dir.path().to_str().unwrap(), 3).unwrap();
        assert_eq!(deleted, 1, "Should have deleted 1 oldest valid backup");

        // Broken backup should still exist
        assert!(broken_dir.exists(), "Broken backup should be untouched");

        // Oldest valid (backup-0) should be gone
        assert!(!backup_base.join("backup-0").exists());

        // Newer 3 valid backups should remain
        assert!(backup_base.join("backup-1").exists());
        assert!(backup_base.join("backup-2").exists());
        assert!(backup_base.join("backup-3").exists());
    }

    #[test]
    fn test_retention_local_zero_means_unlimited() {
        let dir = tempfile::tempdir().unwrap();
        let backup_base = dir.path().join("backup");
        std::fs::create_dir_all(&backup_base).unwrap();

        let base_ts = chrono::Utc::now();

        // Create 5 backups
        for i in 0..5 {
            let ts = base_ts - chrono::Duration::days(4 - i);
            create_backup_with_timestamp(&backup_base, &format!("backup-{}", i), ts);
        }

        // keep=0 means unlimited, should delete nothing
        let deleted = retention_local(dir.path().to_str().unwrap(), 0).unwrap();
        assert_eq!(deleted, 0, "Should not delete anything when keep=0");

        // All 5 should still exist
        for i in 0..5 {
            assert!(backup_base.join(format!("backup-{}", i)).exists());
        }

        // keep=-1 also means no retention action
        let deleted = retention_local(dir.path().to_str().unwrap(), -1).unwrap();
        assert_eq!(deleted, 0, "Should not delete anything when keep=-1");
    }

    // -- GC key collection tests --

    #[test]
    fn test_collect_referenced_keys_from_manifest() {
        use crate::manifest::{PartInfo, S3ObjectInfo, TableManifest};

        let mut parts = BTreeMap::new();

        // Local disk parts with backup_key
        parts.insert(
            "default".to_string(),
            vec![
                {
                    let mut p = PartInfo::new("202401_1_50_3", 100, 123);
                    p.backup_key =
                        "daily/data/default/trades/default/202401_1_50_3.tar.lz4".to_string();
                    p
                },
                {
                    let mut p = PartInfo::new("202402_1_1_0", 50, 456);
                    p.backup_key =
                        "daily/data/default/trades/default/202402_1_1_0.tar.lz4".to_string();
                    p
                },
            ],
        );

        // S3 disk parts with s3_objects
        parts.insert(
            "s3disk".to_string(),
            vec![{
                let mut p = PartInfo::new("202403_1_1_0", 200, 789).with_s3_objects(vec![
                    S3ObjectInfo {
                        path: "store/abc/def/data.bin".to_string(),
                        size: 190,
                        backup_key: "daily/objects/store/abc/def/data.bin".to_string(),
                    },
                    S3ObjectInfo {
                        path: "store/abc/def/index.bin".to_string(),
                        size: 10,
                        backup_key: "daily/objects/store/abc/def/index.bin".to_string(),
                    },
                ]);
                p.backup_key = "daily/data/default/trades/s3disk/202403_1_1_0.tar.lz4".to_string();
                p
            }],
        );

        let mut tables = BTreeMap::new();
        tables.insert(
            "default.trades".to_string(),
            TableManifest::test_new("MergeTree")
                .with_ddl("CREATE TABLE ...")
                .with_total_bytes(350)
                .with_parts(parts),
        );

        let manifest = BackupManifest::test_new("daily")
            .with_tables(tables)
            .with_compressed_size(350)
            .with_metadata_size(256);

        let keys = collect_key_prefixes_from_manifest(&manifest);

        // Should have 5 keys total: 2 local parts + 1 s3 disk part + 2 s3 objects
        assert_eq!(keys.len(), 5);

        // Local disk part keys
        assert!(keys.contains("daily/data/default/trades/default/202401_1_50_3.tar.lz4"));
        assert!(keys.contains("daily/data/default/trades/default/202402_1_1_0.tar.lz4"));

        // S3 disk part key
        assert!(keys.contains("daily/data/default/trades/s3disk/202403_1_1_0.tar.lz4"));

        // S3 object keys
        assert!(keys.contains("daily/objects/store/abc/def/data.bin"));
        assert!(keys.contains("daily/objects/store/abc/def/index.bin"));
    }

    #[test]
    fn test_collect_keys_from_empty_manifest() {
        let manifest = BackupManifest::test_new("empty");

        let keys = collect_key_prefixes_from_manifest(&manifest);
        assert!(keys.is_empty(), "Empty manifest should produce no keys");
    }

    #[test]
    fn test_collect_key_prefixes_preserves_trailing_slash() {
        use crate::manifest::{PartInfo, TableManifest};

        let mut parts = BTreeMap::new();
        parts.insert("s3disk".to_string(), {
            let mut p = PartInfo::new("202401_1_1_0", 100, 0);
            // S3-disk metadata keys are directory-like (upload::upload_inner).
            p.backup_key = "daily/data/default/trades/s3disk/202401_1_1_0/".to_string();
            vec![p]
        });

        let mut tables = BTreeMap::new();
        tables.insert(
            "default.trades".to_string(),
            TableManifest::test_new("MergeTree").with_parts(parts),
        );

        let keys = collect_key_prefixes_from_manifest(
            &BackupManifest::test_new("daily").with_tables(tables),
        );

        assert!(keys.contains("daily/data/default/trades/s3disk/202401_1_1_0/"));
    }

    // -- is_key_protected tests --

    fn protected_set(entries: &[&str]) -> HashSet<String> {
        entries.iter().map(|e| e.to_string()).collect()
    }

    #[test]
    fn test_is_key_protected_exact_key() {
        let protected = protected_set(&["daily/data/default/trades/part1.tar.lz4"]);

        assert!(is_key_protected(
            "daily/data/default/trades/part1.tar.lz4",
            &protected
        ));
        assert!(!is_key_protected(
            "daily/data/default/trades/part2.tar.lz4",
            &protected
        ));
    }

    #[test]
    fn test_is_key_protected_directory_prefix() {
        // An S3-disk metadata directory protects the files uploaded under it.
        let protected = protected_set(&["daily/data/default/trades/s3disk/202401_1_1_0/"]);

        assert!(is_key_protected(
            "daily/data/default/trades/s3disk/202401_1_1_0/checksums.txt",
            &protected
        ));
        assert!(is_key_protected(
            "daily/data/default/trades/s3disk/202401_1_1_0/",
            &protected
        ));
    }

    #[test]
    fn test_is_key_protected_sibling_prefix_does_not_match() {
        let protected = protected_set(&["a/b/part1/"]);

        assert!(!is_key_protected("a/b/part10/x", &protected));
    }

    #[test]
    fn test_is_key_protected_exact_entry_does_not_match_by_prefix() {
        // Without a trailing '/' an entry is a whole object key, not a directory.
        let protected = protected_set(&["a/b/part1"]);

        assert!(!is_key_protected("a/b/part1/x", &protected));
    }

    // -- plan_retention_deletions tests --

    fn retention_summary(name: &str, timestamp_secs: i64, is_broken: bool) -> BackupSummary {
        BackupSummary {
            name: name.to_string(),
            timestamp: DateTime::from_timestamp(timestamp_secs, 0),
            size: 0,
            compressed_size: 0,
            table_count: 0,
            metadata_size: 0,
            rbac_size: 0,
            config_size: 0,
            object_disk_size: 0,
            required: String::new(),
            is_broken,
            broken_reason: None,
        }
    }

    #[test]
    fn test_plan_retention_deletions_deletes_oldest_first() {
        let summaries = [
            retention_summary("newest", 300, false),
            retention_summary("oldest", 100, false),
            retention_summary("middle", 200, false),
        ];

        let plan = plan_retention_deletions(&summaries, 1);

        assert_eq!(plan.to_delete, vec!["oldest", "middle"]);
        assert_eq!(plan.surviving, vec!["newest"]);
    }

    #[test]
    fn test_plan_retention_deletions_keep_zero_deletes_all() {
        let summaries = [
            retention_summary("a", 100, false),
            retention_summary("b", 200, false),
        ];

        let plan = plan_retention_deletions(&summaries, 0);

        assert_eq!(plan.to_delete, vec!["a", "b"]);
        assert!(plan.surviving.is_empty());
    }

    #[test]
    fn test_plan_retention_deletions_keep_at_least_len_deletes_nothing() {
        let summaries = [
            retention_summary("a", 100, false),
            retention_summary("b", 200, false),
        ];

        for keep in [2, 3, 100] {
            let plan = plan_retention_deletions(&summaries, keep);
            assert!(plan.to_delete.is_empty(), "keep={} deleted something", keep);
            assert_eq!(plan.surviving, vec!["a", "b"]);
        }
    }

    #[test]
    fn test_plan_retention_deletions_excludes_broken_backups() {
        let summaries = [
            retention_summary("broken-old", 50, true),
            retention_summary("a", 100, false),
            retention_summary("b", 200, false),
        ];

        // The broken backup neither counts towards `keep` nor gets planned.
        let plan = plan_retention_deletions(&summaries, 1);

        assert_eq!(plan.to_delete, vec!["a"]);
        assert_eq!(plan.surviving, vec!["b"]);
    }

    // -- retention_remote_inner tests --

    /// Manifest whose single part lives at `backup_key`, optionally carried from
    /// an incremental base.
    fn manifest_with_key(name: &str, backup_key: &str, source: Option<&str>) -> BackupManifest {
        use crate::manifest::{PartInfo, TableManifest};

        let mut part = PartInfo::new("202401_1_1_0", 100, 42);
        part.backup_key = backup_key.to_string();
        if let Some(source) = source {
            part.source = source.to_string();
        }

        let mut parts = BTreeMap::new();
        parts.insert("default".to_string(), vec![part]);

        let mut tables = BTreeMap::new();
        tables.insert(
            "default.trades".to_string(),
            TableManifest::test_new("MergeTree").with_parts(parts),
        );

        BackupManifest::test_new(name).with_tables(tables)
    }

    /// Records every `delete_keys` call so a test can assert nothing was deleted.
    #[derive(Default)]
    struct DeleteLog(std::sync::Mutex<Vec<Vec<String>>>);

    impl DeleteLog {
        fn calls(&self) -> Vec<Vec<String>> {
            self.0.lock().expect("delete log poisoned").clone()
        }

        fn deleted_keys(&self) -> Vec<String> {
            self.calls().into_iter().flatten().collect()
        }
    }

    #[tokio::test]
    async fn retention_abort_on_manifest_error() {
        // Two candidates, and a surviving backup whose manifest cannot be read.
        // The pass must abort before deleting anything -- including "old-b", which
        // is ordered after the failure.
        let plan = RetentionPlan {
            to_delete: vec!["old-a".to_string(), "old-b".to_string()],
            surviving: vec!["keeper".to_string()],
        };
        let log = DeleteLog::default();

        let result = retention_remote_inner(
            &plan,
            |name: String| async move { Err(anyhow::anyhow!("S3 timeout reading {}", name)) },
            |_prefix: String| async { Ok(vec!["old-a/data/x.tar.lz4".to_string()]) },
            |keys: Vec<String>| {
                let log = &log;
                async move {
                    log.0.lock().expect("delete log poisoned").push(keys);
                    Ok(())
                }
            },
        )
        .await;

        let err = result.expect_err("an unreadable surviving manifest must abort the pass");
        assert!(
            err.to_string().contains("keeper"),
            "error should name the unreadable backup: {}",
            err
        );
        assert!(
            log.calls().is_empty(),
            "no candidate may be deleted after a manifest error, got {:?}",
            log.calls()
        );
    }

    #[tokio::test]
    async fn manifest_preserved_when_referenced() {
        // "old" shares a part with the surviving backup, which carried it forward.
        // The whole backup must survive, manifest included.
        let shared_key = "old/data/default/trades/default/202401_1_1_0.tar.lz4";
        let plan = RetentionPlan {
            to_delete: vec!["old".to_string()],
            surviving: vec!["keeper".to_string()],
        };
        let log = DeleteLog::default();

        let outcome = retention_remote_inner(
            &plan,
            |_name: String| async move { Ok(manifest_with_key("keeper", shared_key, None)) },
            |_prefix: String| async move {
                Ok(vec![
                    shared_key.to_string(),
                    "old/metadata.json".to_string(),
                ])
            },
            |keys: Vec<String>| {
                let log = &log;
                async move {
                    log.0.lock().expect("delete log poisoned").push(keys);
                    Ok(())
                }
            },
        )
        .await
        .expect("a protected candidate is skipped, not an error");

        assert_eq!(outcome.skipped, vec!["old"]);
        assert!(
            outcome.deleted.is_empty(),
            "the backup must be reported skipped, not deleted"
        );
        assert!(
            !log.deleted_keys()
                .contains(&"old/metadata.json".to_string()),
            "the manifest must stay in place, deleted keys: {:?}",
            log.deleted_keys()
        );
    }

    #[tokio::test]
    async fn test_retention_inner_deletes_unreferenced_backup_manifest_last() {
        let plan = RetentionPlan {
            to_delete: vec!["old".to_string()],
            surviving: vec!["keeper".to_string()],
        };
        let log = DeleteLog::default();

        let outcome = retention_remote_inner(
            &plan,
            |_name: String| async move {
                Ok(manifest_with_key("keeper", "keeper/data/own.tar.lz4", None))
            },
            |_prefix: String| async move {
                Ok(vec![
                    "old/metadata.json".to_string(),
                    "old/data/x.tar.lz4".to_string(),
                ])
            },
            |keys: Vec<String>| {
                let log = &log;
                async move {
                    log.0.lock().expect("delete log poisoned").push(keys);
                    Ok(())
                }
            },
        )
        .await
        .expect("nothing protects 'old'");

        assert_eq!(outcome.deleted, vec!["old"]);
        assert_eq!(
            log.calls(),
            vec![
                vec!["old/data/x.tar.lz4".to_string()],
                vec!["old/metadata.json".to_string()],
            ],
            "data keys first, manifest last"
        );
    }

    #[tokio::test]
    async fn test_retention_inner_skips_incremental_base_by_name() {
        // The name-chain guard: the survivor carried parts from "old" but the
        // physical keys it names live under its own prefix.
        let plan = RetentionPlan {
            to_delete: vec!["old".to_string()],
            surviving: vec!["keeper".to_string()],
        };
        let log = DeleteLog::default();

        let outcome = retention_remote_inner(
            &plan,
            |_name: String| async move {
                Ok(manifest_with_key(
                    "keeper",
                    "keeper/data/x.tar.lz4",
                    Some("carried:old"),
                ))
            },
            |_prefix: String| async move { Ok(vec!["old/metadata.json".to_string()]) },
            |keys: Vec<String>| {
                let log = &log;
                async move {
                    log.0.lock().expect("delete log poisoned").push(keys);
                    Ok(())
                }
            },
        )
        .await
        .expect("a named incremental base is skipped, not an error");

        assert_eq!(outcome.skipped, vec!["old"]);
        assert!(log.calls().is_empty(), "nothing may be deleted");
    }

    // -- plan_clean_broken_deletion tests --

    /// Fixed "now" and a prefix holding two keys, one of them older than an hour.
    const NOW: i64 = 1_700_000_000;
    const HOUR: u64 = 3600;

    fn broken_keys() -> Vec<String> {
        vec![
            "broken/data/default/trades/default/202401_1_1_0.tar.lz4".to_string(),
            "broken/metadata.json".to_string(),
        ]
    }

    #[test]
    fn plan_clean_broken_deletion_skips_while_the_lock_is_live() {
        // Old enough and nothing is protected, but a live process holds the lock.
        let plan = plan_clean_broken_deletion(
            &broken_keys(),
            Some(NOW - 10 * HOUR as i64),
            NOW,
            HOUR,
            true,
            &HashSet::new(),
        );

        match plan {
            CleanBrokenPlan::Skip { reason } => assert!(
                reason.contains("PID lock"),
                "the reason must name the live lock, got: {reason}"
            ),
            CleanBrokenPlan::Delete { keys } => {
                panic!("a live lock must never plan a deletion, planned: {keys:?}")
            }
        }
    }

    #[test]
    fn plan_clean_broken_deletion_skips_a_prefix_younger_than_the_threshold() {
        // Written 5 minutes ago: this is what an in-flight upload looks like,
        // because upload writes the manifest last.
        let plan = plan_clean_broken_deletion(
            &broken_keys(),
            Some(NOW - 300),
            NOW,
            HOUR,
            false,
            &HashSet::new(),
        );

        match plan {
            CleanBrokenPlan::Skip { reason } => assert!(
                reason.contains("300s old"),
                "the reason must state the age, got: {reason}"
            ),
            CleanBrokenPlan::Delete { keys } => {
                panic!("a young prefix must never plan a deletion, planned: {keys:?}")
            }
        }
    }

    #[test]
    fn plan_clean_broken_deletion_skips_when_the_timestamp_is_absent() {
        let plan =
            plan_clean_broken_deletion(&broken_keys(), None, NOW, HOUR, false, &HashSet::new());

        match plan {
            CleanBrokenPlan::Skip { reason } => assert!(
                reason.contains("age is unknown"),
                "the reason must say the age could not be established, got: {reason}"
            ),
            CleanBrokenPlan::Delete { keys } => {
                panic!("an unknown age must never plan a deletion, planned: {keys:?}")
            }
        }
    }

    #[test]
    fn plan_clean_broken_deletion_deletes_an_old_unlocked_prefix() {
        let plan = plan_clean_broken_deletion(
            &broken_keys(),
            Some(NOW - 2 * HOUR as i64),
            NOW,
            HOUR,
            false,
            &HashSet::new(),
        );

        assert_eq!(
            plan,
            CleanBrokenPlan::Delete {
                keys: broken_keys()
            }
        );
    }

    #[test]
    fn plan_clean_broken_deletion_never_deletes_a_protected_key() {
        // The broken prefix holds one key a surviving backup still references
        // (carried into its manifest by an incremental) and one it does not.
        let shared = "broken/data/default/trades/default/202401_1_1_0.tar.lz4".to_string();
        let own = "broken/metadata.json".to_string();
        let protected = protected_set(&[
            shared.as_str(),
            "keeper/data/default/trades/s3/202401_1_1_0/",
        ]);

        let plan = plan_clean_broken_deletion(
            &[shared, own.clone()],
            Some(NOW - 2 * HOUR as i64),
            NOW,
            HOUR,
            false,
            &protected,
        );

        // The protected key is absent: only the broken backup's own key is planned.
        assert_eq!(plan, CleanBrokenPlan::Delete { keys: vec![own] });
    }

    #[tokio::test]
    async fn clean_broken_deletes_only_planned() {
        // The keys that reach S3 must be exactly the ones the planner returned:
        // a planner that excludes protected keys is worthless if the caller then
        // deletes the whole prefix anyway.
        let candidates = vec![
            "broken/data/default/trades/default/202401_1_1_0.tar.lz4".to_string(),
            "broken/data/default/trades/default/202402_1_1_0.tar.lz4".to_string(),
            "broken/metadata.json".to_string(),
        ];
        let protected = protected_set(&[&candidates[0]]);

        let plan = plan_clean_broken_deletion(
            &candidates,
            Some(NOW - 2 * HOUR as i64),
            NOW,
            HOUR,
            false,
            &protected,
        );
        let planned_keys = match &plan {
            CleanBrokenPlan::Delete { keys } => keys.clone(),
            CleanBrokenPlan::Skip { reason } => panic!("expected a deletion, got skip: {reason}"),
        };

        let log = DeleteLog::default();
        let deleted = apply_clean_broken_plan("broken", plan, |keys| {
            let log = &log;
            async move {
                log.0.lock().expect("delete log poisoned").push(keys);
                Ok(())
            }
        })
        .await;

        assert!(deleted, "an old, unlocked backup is reported as deleted");
        assert_eq!(
            log.deleted_keys(),
            planned_keys,
            "exactly the planned keys are deleted"
        );
        assert!(
            !log.deleted_keys().contains(&candidates[0]),
            "the protected key survives"
        );
    }

    #[tokio::test]
    async fn clean_broken_skip_deletes_nothing() {
        let log = DeleteLog::default();

        let deleted = apply_clean_broken_plan(
            "broken",
            CleanBrokenPlan::Skip {
                reason: "an upload may still be in flight".to_string(),
            },
            |keys| {
                let log = &log;
                async move {
                    log.0.lock().expect("delete log poisoned").push(keys);
                    Ok(())
                }
            },
        )
        .await;

        assert!(!deleted, "a skipped backup is not counted as deleted");
        assert!(log.calls().is_empty(), "a skip issues no deletion at all");
    }

    #[test]
    fn test_backup_summary_has_metadata_size() {
        let summary = BackupSummary {
            name: "test".to_string(),
            timestamp: None,
            size: 1000,
            compressed_size: 500,
            table_count: 2,
            metadata_size: 256,
            rbac_size: 0,
            config_size: 0,
            object_disk_size: 0,
            required: String::new(),
            is_broken: false,
            broken_reason: None,
        };
        assert_eq!(summary.metadata_size, 256);
    }

    #[test]
    fn test_backup_summary_object_disk_size() {
        let summary = BackupSummary {
            name: "test-s3".to_string(),
            timestamp: None,
            size: 2000,
            compressed_size: 1000,
            table_count: 1,
            metadata_size: 0,
            rbac_size: 0,
            config_size: 0,
            object_disk_size: 512,
            required: String::new(),
            is_broken: false,
            broken_reason: None,
        };
        assert_eq!(summary.object_disk_size, 512);
    }

    #[test]
    fn test_compute_object_disk_size_sums_s3_objects() {
        use crate::manifest::{PartInfo, S3ObjectInfo, TableManifest};

        let mut tables = BTreeMap::new();
        let mut parts = BTreeMap::new();
        parts.insert(
            "default".to_string(),
            vec![PartInfo::new("all_0_0_0", 1000, 0).with_s3_objects(vec![
                S3ObjectInfo {
                    path: "obj1".to_string(),
                    size: 200,
                    backup_key: String::new(),
                },
                S3ObjectInfo {
                    path: "obj2".to_string(),
                    size: 300,
                    backup_key: String::new(),
                },
            ])],
        );
        parts.insert(
            "s3disk".to_string(),
            vec![PartInfo::new("all_1_1_0", 500, 0)], // local disk part, no s3_objects
        );
        tables.insert(
            "db.table".to_string(),
            TableManifest::test_new("")
                .with_total_bytes(1500)
                .with_parts(parts),
        );

        let manifest = BackupManifest::test_new("test").with_tables(tables);

        assert_eq!(compute_object_disk_size(&manifest), 500); // 200 + 300
    }

    #[test]
    fn test_extract_required_from_manifest() {
        use crate::manifest::{PartInfo, TableManifest};

        let mut tables = BTreeMap::new();
        let mut parts = BTreeMap::new();
        parts.insert(
            "default".to_string(),
            vec![PartInfo::new("all_0_0_0", 100, 0), {
                let mut p = PartInfo::new("all_1_1_0", 100, 0);
                p.source = "carried:base-backup".to_string();
                p
            }],
        );
        tables.insert(
            "db.table".to_string(),
            TableManifest::test_new("")
                .with_total_bytes(200)
                .with_parts(parts),
        );

        let manifest = BackupManifest::test_new("incr-backup").with_tables(tables);

        assert_eq!(extract_required_backup(&manifest), "base-backup");
    }

    #[test]
    fn test_extract_required_empty_for_full_backup() {
        let manifest = BackupManifest::test_new("full-backup");

        assert_eq!(extract_required_backup(&manifest), "");
    }

    #[test]
    fn test_parse_backup_summary_populates_metadata_size() {
        // Create a backup directory with a manifest that has metadata_size
        let dir = tempfile::tempdir().unwrap();
        let backup_dir = dir.path().join("test-backup");
        std::fs::create_dir_all(&backup_dir).unwrap();

        let manifest = BackupManifest::test_new("test-backup")
            .with_compressed_size(500)
            .with_metadata_size(1024);

        let metadata_path = backup_dir.join("metadata.json");
        manifest.save_to_file(&metadata_path).unwrap();

        let summary = parse_backup_summary("test-backup", &metadata_path);
        assert!(!summary.is_broken);
        assert_eq!(summary.metadata_size, 1024);
    }

    // -- Format output tests --

    #[test]
    fn test_format_list_output_json() {
        let summaries = vec![BackupSummary {
            name: "test-backup".to_string(),
            timestamp: None,
            size: 1000,
            compressed_size: 500,
            table_count: 2,
            metadata_size: 256,
            rbac_size: 0,
            config_size: 0,
            object_disk_size: 0,
            required: String::new(),
            is_broken: false,
            broken_reason: None,
        }];

        let output = format_list_output(&summaries, &ListFormat::Json).unwrap();
        assert!(output.contains("\"name\": \"test-backup\""));
        assert!(output.contains("\"size\": 1000"));

        // Should be valid JSON
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["name"], "test-backup");
    }

    #[test]
    fn test_format_list_output_yaml() {
        let summaries = vec![BackupSummary {
            name: "test-backup".to_string(),
            timestamp: None,
            size: 1000,
            compressed_size: 500,
            table_count: 2,
            metadata_size: 256,
            rbac_size: 0,
            config_size: 0,
            object_disk_size: 0,
            required: String::new(),
            is_broken: false,
            broken_reason: None,
        }];

        let output = format_list_output(&summaries, &ListFormat::Yaml).unwrap();
        assert!(output.contains("name: test-backup"));
        assert!(output.contains("size: 1000"));
    }

    #[test]
    fn test_format_list_output_csv() {
        let summaries = vec![BackupSummary {
            name: "backup-1".to_string(),
            timestamp: None,
            size: 2000,
            compressed_size: 1000,
            table_count: 5,
            metadata_size: 128,
            rbac_size: 0,
            config_size: 0,
            object_disk_size: 0,
            required: String::new(),
            is_broken: false,
            broken_reason: None,
        }];

        let output = format_list_output(&summaries, &ListFormat::Csv).unwrap();
        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(lines.len(), 2); // header + 1 data row
        assert!(lines[0].contains("name,timestamp,size"));
        assert!(lines[1].starts_with("backup-1,"));
        assert!(lines[1].contains("2000"));
    }

    #[test]
    fn test_format_list_output_tsv() {
        let summaries = vec![BackupSummary {
            name: "backup-1".to_string(),
            timestamp: None,
            size: 2000,
            compressed_size: 1000,
            table_count: 5,
            metadata_size: 128,
            rbac_size: 0,
            config_size: 0,
            object_disk_size: 0,
            required: String::new(),
            is_broken: false,
            broken_reason: None,
        }];

        let output = format_list_output(&summaries, &ListFormat::Tsv).unwrap();
        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("name\ttimestamp\tsize"));
        assert!(lines[1].starts_with("backup-1\t"));
    }

    #[test]
    fn test_format_list_output_default() {
        let summaries = vec![BackupSummary {
            name: "my-backup".to_string(),
            timestamp: None,
            size: 1_048_576,
            compressed_size: 524_288,
            table_count: 3,
            metadata_size: 0,
            rbac_size: 0,
            config_size: 0,
            object_disk_size: 0,
            required: String::new(),
            is_broken: false,
            broken_reason: None,
        }];

        let output = format_list_output(&summaries, &ListFormat::Default).unwrap();
        assert!(output.contains("my-backup"));
        assert!(output.contains("1.00 MB"));
        assert!(output.contains("3 tables"));
    }

    #[test]
    fn test_format_list_output_empty() {
        let summaries: Vec<BackupSummary> = vec![];

        let json = format_list_output(&summaries, &ListFormat::Json).unwrap();
        assert_eq!(json, "[]");

        let csv = format_list_output(&summaries, &ListFormat::Csv).unwrap();
        // CSV with empty data should just have header
        assert!(csv.contains("name"));
        assert_eq!(csv.lines().count(), 1);
    }

    // -- Backup shortcut tests --

    #[test]
    fn test_resolve_backup_shortcut_latest() {
        let backups = vec![
            BackupSummary {
                name: "backup-a".to_string(),
                timestamp: None,
                size: 0,
                compressed_size: 0,
                table_count: 0,
                metadata_size: 0,
                rbac_size: 0,
                config_size: 0,
                object_disk_size: 0,
                required: String::new(),
                is_broken: false,
                broken_reason: None,
            },
            BackupSummary {
                name: "backup-b".to_string(),
                timestamp: None,
                size: 0,
                compressed_size: 0,
                table_count: 0,
                metadata_size: 0,
                rbac_size: 0,
                config_size: 0,
                object_disk_size: 0,
                required: String::new(),
                is_broken: false,
                broken_reason: None,
            },
            BackupSummary {
                name: "backup-c".to_string(),
                timestamp: None,
                size: 0,
                compressed_size: 0,
                table_count: 0,
                metadata_size: 0,
                rbac_size: 0,
                config_size: 0,
                object_disk_size: 0,
                required: String::new(),
                is_broken: false,
                broken_reason: None,
            },
        ];

        let resolved = resolve_backup_shortcut("latest", &backups).unwrap();
        assert_eq!(resolved, "backup-c");
    }

    #[test]
    fn test_resolve_backup_shortcut_previous() {
        let backups = vec![
            BackupSummary {
                name: "backup-a".to_string(),
                timestamp: None,
                size: 0,
                compressed_size: 0,
                table_count: 0,
                metadata_size: 0,
                rbac_size: 0,
                config_size: 0,
                object_disk_size: 0,
                required: String::new(),
                is_broken: false,
                broken_reason: None,
            },
            BackupSummary {
                name: "backup-b".to_string(),
                timestamp: None,
                size: 0,
                compressed_size: 0,
                table_count: 0,
                metadata_size: 0,
                rbac_size: 0,
                config_size: 0,
                object_disk_size: 0,
                required: String::new(),
                is_broken: false,
                broken_reason: None,
            },
            BackupSummary {
                name: "backup-c".to_string(),
                timestamp: None,
                size: 0,
                compressed_size: 0,
                table_count: 0,
                metadata_size: 0,
                rbac_size: 0,
                config_size: 0,
                object_disk_size: 0,
                required: String::new(),
                is_broken: false,
                broken_reason: None,
            },
        ];

        let resolved = resolve_backup_shortcut("previous", &backups).unwrap();
        assert_eq!(resolved, "backup-b");
    }

    #[test]
    fn test_resolve_backup_shortcut_passthrough() {
        let backups = vec![];
        let resolved = resolve_backup_shortcut("my-specific-backup", &backups).unwrap();
        assert_eq!(resolved, "my-specific-backup");
    }

    #[test]
    fn test_resolve_backup_shortcut_latest_no_backups() {
        let backups: Vec<BackupSummary> = vec![];
        let result = resolve_backup_shortcut("latest", &backups);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No backups found"));
    }

    #[test]
    fn test_resolve_backup_shortcut_previous_not_enough() {
        let backups = vec![BackupSummary {
            name: "only-one".to_string(),
            timestamp: None,
            size: 0,
            compressed_size: 0,
            table_count: 0,
            metadata_size: 0,
            rbac_size: 0,
            config_size: 0,
            object_disk_size: 0,
            required: String::new(),
            is_broken: false,
            broken_reason: None,
        }];

        let result = resolve_backup_shortcut("previous", &backups);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Not enough backups"));
    }

    #[test]
    fn test_resolve_backup_shortcut_skips_broken() {
        let backups = vec![
            BackupSummary {
                name: "backup-a".to_string(),
                timestamp: None,
                size: 0,
                compressed_size: 0,
                table_count: 0,
                metadata_size: 0,
                rbac_size: 0,
                config_size: 0,
                object_disk_size: 0,
                required: String::new(),
                is_broken: false,
                broken_reason: None,
            },
            BackupSummary {
                name: "backup-b-broken".to_string(),
                timestamp: None,
                size: 0,
                compressed_size: 0,
                table_count: 0,
                metadata_size: 0,
                rbac_size: 0,
                config_size: 0,
                object_disk_size: 0,
                required: String::new(),
                is_broken: true,
                broken_reason: Some("corrupt".to_string()),
            },
            BackupSummary {
                name: "backup-c".to_string(),
                timestamp: None,
                size: 0,
                compressed_size: 0,
                table_count: 0,
                metadata_size: 0,
                rbac_size: 0,
                config_size: 0,
                object_disk_size: 0,
                required: String::new(),
                is_broken: false,
                broken_reason: None,
            },
        ];

        // latest should skip broken and return backup-c
        let resolved = resolve_backup_shortcut("latest", &backups).unwrap();
        assert_eq!(resolved, "backup-c");

        // previous should skip broken and return backup-a
        let resolved = resolve_backup_shortcut("previous", &backups).unwrap();
        assert_eq!(resolved, "backup-a");
    }

    #[test]
    fn test_resolve_backup_shortcut_sorts_by_timestamp() {
        use chrono::TimeZone;

        // Names in alphabetical order but timestamps in reverse order.
        // alpha has the newest timestamp, beta the oldest, gamma in the middle.
        let backups = vec![
            BackupSummary {
                name: "alpha".to_string(),
                timestamp: Some(chrono::Utc.with_ymd_and_hms(2024, 3, 1, 0, 0, 0).unwrap()),
                size: 0,
                compressed_size: 0,
                table_count: 0,
                metadata_size: 0,
                rbac_size: 0,
                config_size: 0,
                object_disk_size: 0,
                required: String::new(),
                is_broken: false,
                broken_reason: None,
            },
            BackupSummary {
                name: "beta".to_string(),
                timestamp: Some(chrono::Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap()),
                size: 0,
                compressed_size: 0,
                table_count: 0,
                metadata_size: 0,
                rbac_size: 0,
                config_size: 0,
                object_disk_size: 0,
                required: String::new(),
                is_broken: false,
                broken_reason: None,
            },
            BackupSummary {
                name: "gamma".to_string(),
                timestamp: Some(chrono::Utc.with_ymd_and_hms(2024, 2, 1, 0, 0, 0).unwrap()),
                size: 0,
                compressed_size: 0,
                table_count: 0,
                metadata_size: 0,
                rbac_size: 0,
                config_size: 0,
                object_disk_size: 0,
                required: String::new(),
                is_broken: false,
                broken_reason: None,
            },
        ];

        // "latest" should resolve to "alpha" (most recent timestamp: 2024-03-01),
        // NOT "gamma" (last by name).
        let resolved = resolve_backup_shortcut("latest", &backups).unwrap();
        assert_eq!(
            resolved, "alpha",
            "latest should resolve to backup with newest timestamp"
        );

        // "previous" should resolve to "gamma" (second-most-recent: 2024-02-01)
        let resolved = resolve_backup_shortcut("previous", &backups).unwrap();
        assert_eq!(
            resolved, "gamma",
            "previous should resolve to backup with second-newest timestamp"
        );
    }

    #[test]
    fn test_resolve_backup_shortcut_none_timestamps_sort_first() {
        use chrono::TimeZone;

        // Backups with None timestamp should sort before those with Some timestamp.
        let backups = vec![
            BackupSummary {
                name: "no-ts".to_string(),
                timestamp: None,
                size: 0,
                compressed_size: 0,
                table_count: 0,
                metadata_size: 0,
                rbac_size: 0,
                config_size: 0,
                object_disk_size: 0,
                required: String::new(),
                is_broken: false,
                broken_reason: None,
            },
            BackupSummary {
                name: "has-ts".to_string(),
                timestamp: Some(chrono::Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap()),
                size: 0,
                compressed_size: 0,
                table_count: 0,
                metadata_size: 0,
                rbac_size: 0,
                config_size: 0,
                object_disk_size: 0,
                required: String::new(),
                is_broken: false,
                broken_reason: None,
            },
        ];

        // "latest" should resolve to "has-ts" (Some > None in sort order)
        let resolved = resolve_backup_shortcut("latest", &backups).unwrap();
        assert_eq!(resolved, "has-ts");

        // "previous" should resolve to "no-ts" (None sorts first)
        let resolved = resolve_backup_shortcut("previous", &backups).unwrap();
        assert_eq!(resolved, "no-ts");
    }

    #[test]
    fn test_backup_summary_deserialize_roundtrip() {
        let summary = BackupSummary {
            name: "roundtrip-test".to_string(),
            timestamp: Some(chrono::Utc::now()),
            size: 12345,
            compressed_size: 6789,
            table_count: 4,
            metadata_size: 512,
            rbac_size: 0,
            config_size: 0,
            object_disk_size: 0,
            required: String::new(),
            is_broken: false,
            broken_reason: None,
        };

        let json = serde_json::to_string(&summary).unwrap();
        let deserialized: BackupSummary = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.name, summary.name);
        assert_eq!(deserialized.size, summary.size);
        assert_eq!(deserialized.compressed_size, summary.compressed_size);
        assert_eq!(deserialized.table_count, summary.table_count);
        assert_eq!(deserialized.metadata_size, summary.metadata_size);
        assert_eq!(deserialized.is_broken, summary.is_broken);
    }

    #[test]
    fn test_manifest_cache_basic() {
        let mut cache = ManifestCache::new(Duration::from_secs(300));

        // Initially empty
        assert!(cache.get().is_none());

        // Set some summaries
        let summaries = vec![BackupSummary {
            name: "test-backup".to_string(),
            timestamp: None,
            size: 1024,
            compressed_size: 512,
            table_count: 1,
            metadata_size: 128,
            rbac_size: 0,
            config_size: 0,
            object_disk_size: 0,
            required: String::new(),
            is_broken: false,
            broken_reason: None,
        }];
        cache.set(summaries.clone());

        // Should return cached data
        let cached = cache.get();
        assert!(cached.is_some());
        assert_eq!(cached.unwrap().len(), 1);
        assert_eq!(cached.unwrap()[0].name, "test-backup");

        // Invalidate
        cache.invalidate();
        assert!(cache.get().is_none());
    }

    #[test]
    fn test_manifest_cache_ttl_expiry() {
        // TTL of 0 means immediate expiry
        let mut cache = ManifestCache::new(Duration::from_millis(0));

        let summaries = vec![BackupSummary {
            name: "expired-backup".to_string(),
            timestamp: None,
            size: 0,
            compressed_size: 0,
            table_count: 0,
            metadata_size: 0,
            rbac_size: 0,
            config_size: 0,
            object_disk_size: 0,
            required: String::new(),
            is_broken: false,
            broken_reason: None,
        }];
        cache.set(summaries);

        // Even after set, TTL=0 means it should be expired immediately
        // (Instant::now().elapsed() >= Duration::ZERO is always true)
        std::thread::sleep(Duration::from_millis(1));
        assert!(cache.get().is_none());
    }

    // -- Per-disk delete_local tests --

    #[test]
    fn test_delete_local_cleans_per_disk_dirs() {
        // Create a tempdir simulating a multi-disk setup with metadata.json
        // containing a disks map and per-disk backup dirs.
        let tmp = tempfile::tempdir().unwrap();
        let data_path = tmp.path().join("clickhouse");
        let disk2_path = tmp.path().join("nvme1");

        // Create default backup dir with metadata.json
        let backup_dir = data_path.join("backup").join("test-del");
        std::fs::create_dir_all(&backup_dir).unwrap();

        // Create per-disk backup dir on disk2
        let per_disk_dir = disk2_path.join("backup").join("test-del");
        std::fs::create_dir_all(per_disk_dir.join("shadow")).unwrap();
        std::fs::write(per_disk_dir.join("shadow").join("data.bin"), b"data").unwrap();

        // Write a manifest with disks pointing to both paths
        let manifest = BackupManifest::test_new("test-del").with_disks(BTreeMap::from([
            (
                "default".to_string(),
                data_path.to_string_lossy().to_string(),
            ),
            (
                "nvme1".to_string(),
                disk2_path.to_string_lossy().to_string(),
            ),
        ]));
        manifest
            .save_to_file(&backup_dir.join("metadata.json"))
            .unwrap();

        assert!(per_disk_dir.exists());
        assert!(backup_dir.exists());

        delete_local(data_path.to_str().unwrap(), "test-del").unwrap();

        assert!(
            !per_disk_dir.exists(),
            "Per-disk backup dir should be removed"
        );
        assert!(!backup_dir.exists(), "Default backup dir should be removed");
    }

    #[test]
    fn test_delete_local_no_manifest_uses_download_state() {
        // When metadata.json is missing but download.state.json has disk_map,
        // per-disk dirs should still be cleaned.
        let tmp = tempfile::tempdir().unwrap();
        let data_path = tmp.path().join("clickhouse");
        let disk2_path = tmp.path().join("nvme1");

        // Create default backup dir (no metadata.json)
        let backup_dir = data_path.join("backup").join("test-state");
        std::fs::create_dir_all(&backup_dir).unwrap();

        // Create per-disk backup dir
        let per_disk_dir = disk2_path.join("backup").join("test-state");
        std::fs::create_dir_all(per_disk_dir.join("shadow")).unwrap();
        std::fs::write(per_disk_dir.join("shadow").join("data.bin"), b"data").unwrap();

        // Write a download state file with disk_map
        let state = crate::resume::DownloadState {
            completed_keys: std::collections::HashSet::new(),
            backup_name: "test-state".to_string(),
            params_hash: "abc".to_string(),
            disk_map: HashMap::from([(
                "nvme1".to_string(),
                disk2_path.to_string_lossy().to_string(),
            )]),
        };
        crate::resume::save_state_file(&backup_dir.join("download.state.json"), &state).unwrap();

        assert!(per_disk_dir.exists());
        assert!(backup_dir.exists());

        delete_local(data_path.to_str().unwrap(), "test-state").unwrap();

        assert!(
            !per_disk_dir.exists(),
            "Per-disk dir should be removed via state file fallback"
        );
        assert!(!backup_dir.exists(), "Default backup dir should be removed");
    }

    #[test]
    fn test_delete_local_no_manifest_no_state_fallback() {
        // When neither manifest nor state file exists (broken backup),
        // only the default dir is removed.
        let tmp = tempfile::tempdir().unwrap();
        let data_path = tmp.path().join("clickhouse");
        let backup_dir = data_path.join("backup").join("test-broken");
        std::fs::create_dir_all(&backup_dir).unwrap();
        std::fs::write(backup_dir.join("something.txt"), b"data").unwrap();

        assert!(backup_dir.exists());

        delete_local(data_path.to_str().unwrap(), "test-broken").unwrap();

        assert!(!backup_dir.exists(), "Default backup dir should be removed");
    }

    #[test]
    fn test_delete_local_symlink_dedup() {
        // When two disk paths resolve to the same canonical path,
        // the directory should only be deleted once.
        let tmp = tempfile::tempdir().unwrap();
        let data_path = tmp.path().join("clickhouse");
        let real_disk = tmp.path().join("real_disk");

        // Create real disk directory
        std::fs::create_dir_all(&real_disk).unwrap();

        // Create a symlink from another name to the real disk
        let symlink_disk = tmp.path().join("symlink_disk");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real_disk, &symlink_disk).unwrap();
        #[cfg(not(unix))]
        {
            // On non-Unix, just create a regular directory as fallback
            std::fs::create_dir_all(&symlink_disk).unwrap();
        }

        // Create default backup dir with manifest
        let backup_dir = data_path.join("backup").join("test-sym");
        std::fs::create_dir_all(&backup_dir).unwrap();

        // Create per-disk backup dir on the real disk
        let per_disk_real = real_disk.join("backup").join("test-sym");
        std::fs::create_dir_all(per_disk_real.join("shadow")).unwrap();

        let manifest = BackupManifest::test_new("test-sym").with_disks(BTreeMap::from([
            (
                "default".to_string(),
                data_path.to_string_lossy().to_string(),
            ),
            // Both point to the same canonical path
            (
                "disk_a".to_string(),
                real_disk.to_string_lossy().to_string(),
            ),
            (
                "disk_b".to_string(),
                symlink_disk.to_string_lossy().to_string(),
            ),
        ]));
        manifest
            .save_to_file(&backup_dir.join("metadata.json"))
            .unwrap();

        // Should succeed without double-delete errors
        delete_local(data_path.to_str().unwrap(), "test-sym").unwrap();

        assert!(!backup_dir.exists(), "Default backup dir should be removed");
        assert!(
            !per_disk_real.exists(),
            "Per-disk real dir should be removed"
        );
    }

    // -----------------------------------------------------------------------
    // csv_quote tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_csv_quote_no_quoting_needed() {
        assert_eq!(csv_quote("hello", ','), "hello");
        assert_eq!(csv_quote("simple-name", ','), "simple-name");
    }

    #[test]
    fn test_csv_quote_with_comma() {
        assert_eq!(csv_quote("hello,world", ','), "\"hello,world\"");
    }

    #[test]
    fn test_csv_quote_with_double_quote() {
        assert_eq!(csv_quote("say \"hi\"", ','), "\"say \"\"hi\"\"\"");
    }

    #[test]
    fn test_csv_quote_with_newline() {
        assert_eq!(csv_quote("line1\nline2", ','), "\"line1\nline2\"");
    }

    #[test]
    fn test_csv_quote_with_carriage_return() {
        assert_eq!(csv_quote("line1\rline2", ','), "\"line1\rline2\"");
    }

    #[test]
    fn test_csv_quote_empty_string() {
        assert_eq!(csv_quote("", ','), "");
    }

    #[test]
    fn test_csv_quote_tab_delimiter() {
        // With tab delimiter, comma should not trigger quoting
        assert_eq!(csv_quote("hello,world", '\t'), "hello,world");
        // But tab should
        assert_eq!(csv_quote("hello\tworld", '\t'), "\"hello\tworld\"");
    }

    // -----------------------------------------------------------------------
    // broken_summary tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_broken_summary_structure() {
        let summary = broken_summary(
            "bad-backup".to_string(),
            "metadata.json not found".to_string(),
        );
        assert_eq!(summary.name, "bad-backup");
        assert!(summary.is_broken);
        assert_eq!(
            summary.broken_reason,
            Some("metadata.json not found".to_string())
        );
        assert!(summary.timestamp.is_none());
        assert_eq!(summary.size, 0);
        assert_eq!(summary.compressed_size, 0);
        assert_eq!(summary.table_count, 0);
    }

    #[test]
    fn test_broken_summary_has_zero_sizes() {
        let summary = broken_summary("b".to_string(), "err".to_string());
        assert_eq!(summary.metadata_size, 0);
        assert_eq!(summary.rbac_size, 0);
        assert_eq!(summary.config_size, 0);
        assert_eq!(summary.object_disk_size, 0);
    }

    // -----------------------------------------------------------------------
    // effective_retention_local / effective_retention_remote tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_effective_retention_local_override_precedence() {
        let mut config = Config::default();
        config.general.backups_to_keep_local = 5;
        config.retention.backups_to_keep_local = 10;
        // retention.* overrides general.* when non-zero
        assert_eq!(effective_retention_local(&config), 10);
    }

    #[test]
    fn test_effective_retention_local_fallback() {
        let mut config = Config::default();
        config.general.backups_to_keep_local = 5;
        config.retention.backups_to_keep_local = 0;
        // Falls back to general when retention is 0
        assert_eq!(effective_retention_local(&config), 5);
    }

    #[test]
    fn test_effective_retention_remote_override_precedence() {
        let mut config = Config::default();
        config.general.backups_to_keep_remote = 3;
        config.retention.backups_to_keep_remote = 7;
        assert_eq!(effective_retention_remote(&config), 7);
    }

    #[test]
    fn test_effective_retention_remote_fallback() {
        let mut config = Config::default();
        config.general.backups_to_keep_remote = 3;
        config.retention.backups_to_keep_remote = 0;
        assert_eq!(effective_retention_remote(&config), 3);
    }

    // -----------------------------------------------------------------------
    // summary_from_manifest tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_summary_from_manifest_basic() {
        let manifest = BackupManifest::test_new("test-summary")
            .with_compressed_size(2048)
            .with_metadata_size(512);
        let summary = summary_from_manifest(&manifest, "test-summary");

        assert_eq!(summary.name, "test-summary");
        assert!(!summary.is_broken);
        assert!(summary.timestamp.is_some());
        assert_eq!(summary.compressed_size, 2048);
        assert_eq!(summary.metadata_size, 512);
        assert_eq!(summary.table_count, 0);
        assert_eq!(summary.size, 0);
        assert_eq!(summary.object_disk_size, 0);
        assert!(summary.required.is_empty());
        assert!(summary.broken_reason.is_none());
    }

    #[test]
    fn test_summary_from_manifest_with_s3_objects() {
        use crate::manifest::{PartInfo, S3ObjectInfo, TableManifest};

        let mut parts = BTreeMap::new();
        parts.insert(
            "s3disk".to_string(),
            vec![PartInfo::new("all_0_0_0", 1000, 123).with_s3_objects(vec![
                S3ObjectInfo {
                    path: "store/abc/data.bin".to_string(),
                    size: 800,
                    backup_key: "backup/objects/data.bin".to_string(),
                },
                S3ObjectInfo {
                    path: "store/abc/idx.bin".to_string(),
                    size: 200,
                    backup_key: "backup/objects/idx.bin".to_string(),
                },
            ])],
        );

        let mut tables = BTreeMap::new();
        tables.insert(
            "db.table".to_string(),
            TableManifest::test_new("MergeTree")
                .with_total_bytes(5000)
                .with_parts(parts),
        );

        let manifest = BackupManifest::test_new("s3-summary")
            .with_tables(tables)
            .with_compressed_size(3000);

        let summary = summary_from_manifest(&manifest, "s3-summary");
        assert_eq!(summary.object_disk_size, 1000); // 800 + 200
        assert_eq!(summary.size, 5000);
        assert_eq!(summary.table_count, 1);
    }

    #[test]
    fn test_summary_from_manifest_incremental() {
        use crate::manifest::{PartInfo, TableManifest};

        let mut parts = BTreeMap::new();
        parts.insert(
            "default".to_string(),
            vec![{
                let mut p = PartInfo::new("all_0_0_0", 100, 0);
                p.source = "carried:base-full-backup".to_string();
                p
            }],
        );

        let mut tables = BTreeMap::new();
        tables.insert(
            "db.table".to_string(),
            TableManifest::test_new("MergeTree")
                .with_total_bytes(100)
                .with_parts(parts),
        );

        let manifest = BackupManifest::test_new("incr-summary").with_tables(tables);
        let summary = summary_from_manifest(&manifest, "incr-summary");
        assert_eq!(summary.required, "base-full-backup");
    }

    #[test]
    fn test_parse_backup_summary_uses_directory_name_not_manifest_name() {
        let dir = tempfile::tempdir().unwrap();
        let metadata_path = dir.path().join("metadata.json");

        let manifest = BackupManifest::test_new("../evil-name");
        manifest.save_to_file(&metadata_path).unwrap();

        let summary = parse_backup_summary("safe-backup", &metadata_path);
        assert_eq!(summary.name, "safe-backup");
    }

    // -----------------------------------------------------------------------
    // format_list_output with broken backup in Default format
    // -----------------------------------------------------------------------

    #[test]
    fn test_format_list_output_default_broken() {
        let summaries = vec![BackupSummary {
            name: "broken-backup".to_string(),
            timestamp: None,
            size: 0,
            compressed_size: 0,
            table_count: 0,
            metadata_size: 0,
            rbac_size: 0,
            config_size: 0,
            object_disk_size: 0,
            required: String::new(),
            is_broken: true,
            broken_reason: Some("metadata.json not found".to_string()),
        }];

        let output = format_list_output(&summaries, &ListFormat::Default).unwrap();
        assert!(
            output.contains("[BROKEN: metadata.json not found]"),
            "Default format should show BROKEN reason, got: {}",
            output
        );
        assert!(output.contains("broken-backup"));
    }

    #[test]
    fn test_format_list_output_default_broken_no_reason() {
        let summaries = vec![BackupSummary {
            name: "broken-no-reason".to_string(),
            timestamp: None,
            size: 0,
            compressed_size: 0,
            table_count: 0,
            metadata_size: 0,
            rbac_size: 0,
            config_size: 0,
            object_disk_size: 0,
            required: String::new(),
            is_broken: true,
            broken_reason: None,
        }];

        let output = format_list_output(&summaries, &ListFormat::Default).unwrap();
        assert!(
            output.contains("[BROKEN]"),
            "Default format should show [BROKEN] when no reason, got: {}",
            output
        );
        // Should NOT show "[BROKEN: ]"
        assert!(!output.contains("[BROKEN: ]"));
    }

    // -----------------------------------------------------------------------
    // format_delimited with special characters (RFC 4180 quoting)
    // -----------------------------------------------------------------------

    #[test]
    fn test_format_delimited_csv_with_special_chars() {
        let summaries = vec![BackupSummary {
            name: "backup,with,commas".to_string(),
            timestamp: None,
            size: 100,
            compressed_size: 50,
            table_count: 1,
            metadata_size: 0,
            rbac_size: 0,
            config_size: 0,
            object_disk_size: 0,
            required: String::new(),
            is_broken: true,
            broken_reason: Some("has \"quotes\" and,comma".to_string()),
        }];

        let output = format_delimited(&summaries, ',');
        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(lines.len(), 2);
        // Name with commas should be quoted
        assert!(
            lines[1].starts_with("\"backup,with,commas\""),
            "Name with commas should be quoted, got: {}",
            lines[1]
        );
        // Broken reason with quotes and comma should be double-quoted
        assert!(
            lines[1].contains("\"has \"\"quotes\"\" and,comma\""),
            "Broken reason should have escaped quotes, got: {}",
            lines[1]
        );
    }

    #[test]
    fn test_format_delimited_tsv_no_comma_quoting() {
        let summaries = vec![BackupSummary {
            name: "backup,with,commas".to_string(),
            timestamp: None,
            size: 100,
            compressed_size: 50,
            table_count: 1,
            metadata_size: 0,
            rbac_size: 0,
            config_size: 0,
            object_disk_size: 0,
            required: String::new(),
            is_broken: false,
            broken_reason: None,
        }];

        let output = format_delimited(&summaries, '\t');
        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(lines.len(), 2);
        // Commas in name should NOT be quoted with tab delimiter
        assert!(
            lines[1].starts_with("backup,with,commas\t"),
            "Commas in name should not be quoted with tab delimiter, got: {}",
            lines[1]
        );
    }

    // -----------------------------------------------------------------------
    // retention_local edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn test_retention_local_exact_keep_count() {
        let dir = tempfile::tempdir().unwrap();
        let backup_base = dir.path().join("backup");
        std::fs::create_dir_all(&backup_base).unwrap();

        let base_ts = chrono::Utc::now();

        // Create exactly 3 backups
        for i in 0..3 {
            let ts = base_ts - chrono::Duration::days(2 - i);
            create_backup_with_timestamp(&backup_base, &format!("backup-{}", i), ts);
        }

        // Keep 3 => should delete nothing since count == keep
        let deleted = retention_local(dir.path().to_str().unwrap(), 3).unwrap();
        assert_eq!(deleted, 0, "Should not delete when count == keep");

        // All 3 should still exist
        for i in 0..3 {
            assert!(backup_base.join(format!("backup-{}", i)).exists());
        }
    }

    #[test]
    fn test_retention_local_fewer_than_keep() {
        let dir = tempfile::tempdir().unwrap();
        let backup_base = dir.path().join("backup");
        std::fs::create_dir_all(&backup_base).unwrap();

        let base_ts = chrono::Utc::now();
        create_backup_with_timestamp(&backup_base, "only-one", base_ts);

        // Keep 5 but only 1 exists
        let deleted = retention_local(dir.path().to_str().unwrap(), 5).unwrap();
        assert_eq!(deleted, 0, "Should not delete when count < keep");
        assert!(backup_base.join("only-one").exists());
    }

    #[test]
    fn test_retention_local_no_backup_dir() {
        let dir = tempfile::tempdir().unwrap();
        // No backup directory exists at all
        let deleted = retention_local(dir.path().to_str().unwrap(), 2).unwrap();
        assert_eq!(deleted, 0);
    }

    // -----------------------------------------------------------------------
    // collect_key_prefixes_from_manifest edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn test_collect_keys_skips_empty_backup_key() {
        use crate::manifest::{PartInfo, TableManifest};

        let mut parts = BTreeMap::new();
        parts.insert(
            "default".to_string(),
            vec![
                // Part with empty backup_key (should be skipped)
                PartInfo::new("all_0_0_0", 100, 0),
                // Part with a real backup_key
                {
                    let mut p = PartInfo::new("all_1_1_0", 200, 0);
                    p.backup_key = "backup/data/part.tar.lz4".to_string();
                    p
                },
            ],
        );

        let mut tables = BTreeMap::new();
        tables.insert(
            "db.table".to_string(),
            TableManifest::test_new("MergeTree")
                .with_total_bytes(300)
                .with_parts(parts),
        );

        let manifest = BackupManifest::test_new("test").with_tables(tables);
        let keys = collect_key_prefixes_from_manifest(&manifest);

        assert_eq!(keys.len(), 1);
        assert!(keys.contains("backup/data/part.tar.lz4"));
        // Empty string should not be in the set
        assert!(!keys.contains(""));
    }

    // -----------------------------------------------------------------------
    // list_local sorting verification
    // -----------------------------------------------------------------------

    #[test]
    fn test_list_local_sorted_by_timestamp() {
        use chrono::TimeZone;

        let dir = tempfile::tempdir().unwrap();
        let backup_base = dir.path().join("backup");
        std::fs::create_dir_all(&backup_base).unwrap();

        // Create backups with explicit timestamps where name order differs from timestamp order
        let cases = [
            (
                "zebra",
                chrono::Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap(),
            ),
            (
                "alpha",
                chrono::Utc.with_ymd_and_hms(2024, 6, 1, 0, 0, 0).unwrap(),
            ),
            (
                "middle",
                chrono::Utc.with_ymd_and_hms(2024, 3, 1, 0, 0, 0).unwrap(),
            ),
        ];
        for (name, ts) in &cases {
            let backup_dir = backup_base.join(name);
            std::fs::create_dir_all(&backup_dir).unwrap();
            let mut manifest = BackupManifest::test_new(*name);
            manifest.timestamp = *ts;
            manifest
                .save_to_file(&backup_dir.join("metadata.json"))
                .unwrap();
        }

        let summaries = list_local(dir.path().to_str().unwrap()).unwrap();
        assert_eq!(summaries.len(), 3);
        // Should be sorted by timestamp, not name
        assert_eq!(summaries[0].name, "zebra"); // oldest: 2024-01-01
        assert_eq!(summaries[1].name, "middle"); // middle: 2024-03-01
        assert_eq!(summaries[2].name, "alpha"); // newest: 2024-06-01
    }

    // -----------------------------------------------------------------------
    // list_local ignores non-directory entries
    // -----------------------------------------------------------------------

    #[test]
    fn test_list_local_ignores_files() {
        let dir = tempfile::tempdir().unwrap();
        let backup_base = dir.path().join("backup");
        std::fs::create_dir_all(&backup_base).unwrap();

        // Create a regular file (not a directory) in the backup base
        std::fs::write(backup_base.join("not-a-dir.txt"), "hello").unwrap();

        // Create one valid backup
        let backup_dir = backup_base.join("real-backup");
        std::fs::create_dir_all(&backup_dir).unwrap();
        let manifest = BackupManifest::test_new("real-backup");
        manifest
            .save_to_file(&backup_dir.join("metadata.json"))
            .unwrap();

        let summaries = list_local(dir.path().to_str().unwrap()).unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].name, "real-backup");
    }

    // -----------------------------------------------------------------------
    // extract_backup_name_from_prefix edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn test_extract_backup_name_empty_prefix() {
        assert_eq!(extract_backup_name_from_prefix("/", ""), "");
    }

    #[test]
    fn test_extract_backup_name_nested_prefix() {
        assert_eq!(
            extract_backup_name_from_prefix("prefix/my-backup/", "prefix"),
            "my-backup"
        );
    }

    // -----------------------------------------------------------------------
    // format_size additional boundary values
    // -----------------------------------------------------------------------

    #[test]
    fn test_format_size_boundary_values() {
        assert_eq!(format_size(1023), "1023 B");
        assert_eq!(format_size(1025), "1.00 KB");
        // Just below MB threshold
        assert_eq!(format_size(1_048_575), "1024.00 KB");
        // Exactly 1.5 GB
        assert_eq!(format_size(1_610_612_736), "1.50 GB");
        // Large TB value
        assert_eq!(format_size(5_497_558_138_880), "5.00 TB");
    }

    // -----------------------------------------------------------------------
    // ManifestCache set_ttl test
    // -----------------------------------------------------------------------

    #[test]
    fn test_manifest_cache_set_ttl() {
        let mut cache = ManifestCache::new(Duration::from_secs(300));

        let summaries = vec![BackupSummary {
            name: "ttl-test".to_string(),
            timestamp: None,
            size: 0,
            compressed_size: 0,
            table_count: 0,
            metadata_size: 0,
            rbac_size: 0,
            config_size: 0,
            object_disk_size: 0,
            required: String::new(),
            is_broken: false,
            broken_reason: None,
        }];
        cache.set(summaries);

        // Should be cached with TTL=300s
        assert!(cache.get().is_some());

        // Change TTL to 0 (immediate expiry)
        cache.set_ttl(Duration::from_millis(0));
        std::thread::sleep(Duration::from_millis(1));

        // Now get() should return None (expired with new TTL)
        assert!(
            cache.get().is_none(),
            "Cache should expire after TTL change to 0"
        );
    }

    // -----------------------------------------------------------------------
    // ManifestCache repeated set overwrites
    // -----------------------------------------------------------------------

    #[test]
    fn test_manifest_cache_overwrite() {
        let mut cache = ManifestCache::new(Duration::from_secs(300));

        let summaries_v1 = vec![BackupSummary {
            name: "v1".to_string(),
            timestamp: None,
            size: 0,
            compressed_size: 0,
            table_count: 0,
            metadata_size: 0,
            rbac_size: 0,
            config_size: 0,
            object_disk_size: 0,
            required: String::new(),
            is_broken: false,
            broken_reason: None,
        }];

        let summaries_v2 = vec![
            BackupSummary {
                name: "v2-a".to_string(),
                timestamp: None,
                size: 0,
                compressed_size: 0,
                table_count: 0,
                metadata_size: 0,
                rbac_size: 0,
                config_size: 0,
                object_disk_size: 0,
                required: String::new(),
                is_broken: false,
                broken_reason: None,
            },
            BackupSummary {
                name: "v2-b".to_string(),
                timestamp: None,
                size: 0,
                compressed_size: 0,
                table_count: 0,
                metadata_size: 0,
                rbac_size: 0,
                config_size: 0,
                object_disk_size: 0,
                required: String::new(),
                is_broken: false,
                broken_reason: None,
            },
        ];

        cache.set(summaries_v1);
        assert_eq!(cache.get().unwrap().len(), 1);

        cache.set(summaries_v2);
        assert_eq!(cache.get().unwrap().len(), 2);
        assert_eq!(cache.get().unwrap()[0].name, "v2-a");
    }

    // -----------------------------------------------------------------------
    // total_uncompressed_size tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_total_uncompressed_size_multi_table() {
        use crate::manifest::TableManifest;

        let mut tables = BTreeMap::new();
        tables.insert(
            "db.t1".to_string(),
            TableManifest::test_new("MergeTree").with_total_bytes(1000),
        );
        tables.insert(
            "db.t2".to_string(),
            TableManifest::test_new("MergeTree").with_total_bytes(2000),
        );
        tables.insert(
            "db.t3".to_string(),
            TableManifest::test_new("MergeTree").with_total_bytes(3000),
        );

        let manifest = BackupManifest::test_new("multi-table").with_tables(tables);
        assert_eq!(total_uncompressed_size(&manifest), 6000);
    }

    #[test]
    fn test_total_uncompressed_size_empty() {
        let manifest = BackupManifest::test_new("empty");
        assert_eq!(total_uncompressed_size(&manifest), 0);
    }

    // -----------------------------------------------------------------------
    // strip_s3_prefix edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn test_strip_s3_prefix_with_trailing_slash() {
        assert_eq!(strip_s3_prefix("prefix/key.json", "prefix/"), "key.json");
    }

    #[test]
    fn test_strip_s3_prefix_no_match() {
        assert_eq!(
            strip_s3_prefix("other/key.json", "myprefix"),
            "other/key.json"
        );
    }

    // -----------------------------------------------------------------------
    // clean_broken_local with no broken backups
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_clean_broken_local_none_broken() {
        let dir = tempfile::tempdir().unwrap();
        let backup_base = dir.path().join("backup");
        std::fs::create_dir_all(&backup_base).unwrap();

        // Create only valid backups
        let valid_dir = backup_base.join("valid-1");
        std::fs::create_dir_all(&valid_dir).unwrap();
        let manifest = BackupManifest::test_new("valid-1");
        manifest
            .save_to_file(&valid_dir.join("metadata.json"))
            .unwrap();

        let count = clean_broken_local(dir.path().to_str().unwrap(), None)
            .await
            .unwrap();
        assert_eq!(count, 0, "No broken backups to clean");
        assert!(valid_dir.exists(), "Valid backup should remain");
    }

    // -----------------------------------------------------------------------
    // compute_object_disk_size with no s3_objects
    // -----------------------------------------------------------------------

    #[test]
    fn test_compute_object_disk_size_no_s3_parts() {
        use crate::manifest::{PartInfo, TableManifest};

        let mut parts = BTreeMap::new();
        parts.insert(
            "default".to_string(),
            vec![
                PartInfo::new("all_0_0_0", 1000, 0),
                PartInfo::new("all_1_1_0", 2000, 0),
            ],
        );
        let mut tables = BTreeMap::new();
        tables.insert(
            "db.t".to_string(),
            TableManifest::test_new("MergeTree")
                .with_total_bytes(3000)
                .with_parts(parts),
        );

        let manifest = BackupManifest::test_new("no-s3").with_tables(tables);
        assert_eq!(compute_object_disk_size(&manifest), 0);
    }

    // -----------------------------------------------------------------------
    // Location enum coverage
    // -----------------------------------------------------------------------

    #[test]
    fn test_location_enum_equality() {
        assert_eq!(Location::Local, Location::Local);
        assert_eq!(Location::Remote, Location::Remote);
        assert_ne!(Location::Local, Location::Remote);
    }

    // -----------------------------------------------------------------------
    // ListFormat enum coverage
    // -----------------------------------------------------------------------

    #[test]
    fn test_list_format_enum_equality() {
        assert_eq!(ListFormat::Default, ListFormat::Default);
        assert_eq!(ListFormat::Json, ListFormat::Json);
        assert_eq!(ListFormat::Yaml, ListFormat::Yaml);
        assert_eq!(ListFormat::Csv, ListFormat::Csv);
        assert_eq!(ListFormat::Tsv, ListFormat::Tsv);
        assert_ne!(ListFormat::Json, ListFormat::Yaml);
    }

    // -----------------------------------------------------------------------
    // BackupSummary serde with all fields
    // -----------------------------------------------------------------------

    #[test]
    fn test_backup_summary_serde_all_fields() {
        use chrono::TimeZone;

        let summary = BackupSummary {
            name: "full-test".to_string(),
            timestamp: Some(chrono::Utc.with_ymd_and_hms(2025, 6, 15, 12, 0, 0).unwrap()),
            size: 99999,
            compressed_size: 55555,
            table_count: 10,
            metadata_size: 1024,
            rbac_size: 512,
            config_size: 256,
            object_disk_size: 8000,
            required: "base-backup-name".to_string(),
            is_broken: false,
            broken_reason: None,
        };

        let json = serde_json::to_string(&summary).unwrap();
        let deser: BackupSummary = serde_json::from_str(&json).unwrap();

        assert_eq!(deser.name, "full-test");
        assert_eq!(deser.size, 99999);
        assert_eq!(deser.compressed_size, 55555);
        assert_eq!(deser.table_count, 10);
        assert_eq!(deser.metadata_size, 1024);
        assert_eq!(deser.rbac_size, 512);
        assert_eq!(deser.config_size, 256);
        assert_eq!(deser.object_disk_size, 8000);
        assert_eq!(deser.required, "base-backup-name");
        assert!(!deser.is_broken);
        assert!(deser.broken_reason.is_none());
    }

    #[test]
    fn test_backup_summary_serde_broken_with_reason() {
        let summary = BackupSummary {
            name: "broken-test".to_string(),
            timestamp: None,
            size: 0,
            compressed_size: 0,
            table_count: 0,
            metadata_size: 0,
            rbac_size: 0,
            config_size: 0,
            object_disk_size: 0,
            required: String::new(),
            is_broken: true,
            broken_reason: Some("corrupt metadata".to_string()),
        };

        let json = serde_json::to_string(&summary).unwrap();
        let deser: BackupSummary = serde_json::from_str(&json).unwrap();

        assert!(deser.is_broken);
        assert_eq!(deser.broken_reason, Some("corrupt metadata".to_string()));
    }

    // -----------------------------------------------------------------------
    // format_list_output with multiple backups
    // -----------------------------------------------------------------------

    #[test]
    fn test_format_list_output_json_multiple() {
        use chrono::TimeZone;

        let summaries = vec![
            BackupSummary {
                name: "backup-1".to_string(),
                timestamp: Some(chrono::Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap()),
                size: 1000,
                compressed_size: 500,
                table_count: 2,
                metadata_size: 128,
                rbac_size: 0,
                config_size: 0,
                object_disk_size: 0,
                required: String::new(),
                is_broken: false,
                broken_reason: None,
            },
            BackupSummary {
                name: "backup-2".to_string(),
                timestamp: Some(chrono::Utc.with_ymd_and_hms(2025, 1, 2, 0, 0, 0).unwrap()),
                size: 2000,
                compressed_size: 1000,
                table_count: 5,
                metadata_size: 256,
                rbac_size: 64,
                config_size: 32,
                object_disk_size: 512,
                required: "backup-1".to_string(),
                is_broken: false,
                broken_reason: None,
            },
        ];

        let output = format_list_output(&summaries, &ListFormat::Json).unwrap();
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0]["name"], "backup-1");
        assert_eq!(parsed[1]["name"], "backup-2");
        assert_eq!(parsed[1]["required"], "backup-1");
        assert_eq!(parsed[1]["object_disk_size"], 512);
    }

    // -----------------------------------------------------------------------
    // format_delimited column count matches header
    // -----------------------------------------------------------------------

    #[test]
    fn test_format_delimited_column_count_matches() {
        use chrono::TimeZone;

        let summaries = vec![BackupSummary {
            name: "col-test".to_string(),
            timestamp: Some(chrono::Utc.with_ymd_and_hms(2025, 3, 1, 0, 0, 0).unwrap()),
            size: 100,
            compressed_size: 50,
            table_count: 1,
            metadata_size: 10,
            rbac_size: 5,
            config_size: 3,
            object_disk_size: 20,
            required: "base".to_string(),
            is_broken: false,
            broken_reason: None,
        }];

        let csv_output = format_delimited(&summaries, ',');
        let lines: Vec<&str> = csv_output.lines().collect();
        assert_eq!(lines.len(), 2);

        let header_cols = lines[0].split(',').count();
        let data_cols = lines[1].split(',').count();
        assert_eq!(
            header_cols, data_cols,
            "Header and data columns should match: header={}, data={}",
            header_cols, data_cols
        );
        assert_eq!(header_cols, 12, "Should have 12 columns");
    }

    #[test]
    fn test_list_sort_by_timestamp_not_name() {
        use chrono::TimeZone;

        // Build summaries where name order differs from timestamp order
        let mut summaries = [
            BackupSummary {
                name: "z-old".to_string(),
                timestamp: Some(chrono::Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap()),
                size: 0,
                compressed_size: 0,
                table_count: 0,
                metadata_size: 0,
                rbac_size: 0,
                config_size: 0,
                object_disk_size: 0,
                required: String::new(),
                is_broken: false,
                broken_reason: None,
            },
            BackupSummary {
                name: "a-new".to_string(),
                timestamp: Some(chrono::Utc.with_ymd_and_hms(2024, 6, 1, 0, 0, 0).unwrap()),
                size: 0,
                compressed_size: 0,
                table_count: 0,
                metadata_size: 0,
                rbac_size: 0,
                config_size: 0,
                object_disk_size: 0,
                required: String::new(),
                is_broken: false,
                broken_reason: None,
            },
            // Broken backup with None timestamp — should sort first (None < Some)
            BackupSummary {
                name: "m-broken".to_string(),
                timestamp: None,
                size: 0,
                compressed_size: 0,
                table_count: 0,
                metadata_size: 0,
                rbac_size: 0,
                config_size: 0,
                object_disk_size: 0,
                required: String::new(),
                is_broken: true,
                broken_reason: Some("missing metadata".to_string()),
            },
        ];

        // Apply the same sort used by list_local / list_remote
        summaries.sort_by(|a, b| {
            a.timestamp
                .cmp(&b.timestamp)
                .then_with(|| a.name.cmp(&b.name))
        });

        // None timestamps sort first, then ascending by timestamp
        assert_eq!(
            summaries[0].name, "m-broken",
            "Broken (None ts) should be first"
        );
        assert_eq!(
            summaries[1].name, "z-old",
            "Oldest timestamp should be second"
        );
        assert_eq!(
            summaries[2].name, "a-new",
            "Newest timestamp should be last"
        );
    }
}
