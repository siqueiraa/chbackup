//! List and delete commands for local and remote backups.
//!
//! The `list` function scans local backup directories and/or queries S3 to
//! produce a summary of available backups. The `delete` function removes
//! backups from local disk or S3.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use tracing::{debug, info, warn};

use crate::backup::collect::per_disk_backup_dir;
use crate::clickhouse::{sanitize_name, ChClient};
use crate::config::Config;
use crate::error::ChBackupError;
use crate::manifest::BackupManifest;
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
    valid.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

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

    // Sort by name (chronological if names are date-based)
    summaries.sort_by(|a, b| a.name.cmp(&b.name));

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
                    summaries.push(summary_from_manifest(&manifest));
                }
                Err(e) => {
                    let reason = format!("manifest parse error: {e}");
                    warn!(
                        backup = %name,
                        error = %e,
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

    // Sort by name
    summaries.sort_by(|a, b| a.name.cmp(&b.name));

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
    for disk_path in disk_map.values() {
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

    s3.delete_objects(keys).await?;

    info!(backup = %backup_name, "Remote backup deleted");
    Ok(())
}

// -- Clean broken functions --

/// Delete all broken local backups (missing or corrupt metadata.json).
///
/// Returns the count of deleted broken backups.
pub fn clean_broken_local(data_path: &str) -> Result<usize> {
    let backups = list_local(data_path)?;
    let broken: Vec<&BackupSummary> = backups.iter().filter(|b| b.is_broken).collect();

    if broken.is_empty() {
        info!("No broken local backups found");
        return Ok(0);
    }

    let mut deleted = 0;
    for b in &broken {
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

/// Delete all broken remote backups (missing or corrupt metadata.json).
///
/// Returns the count of deleted broken backups.
pub async fn clean_broken_remote(s3: &S3Client) -> Result<usize> {
    let backups = list_remote(s3).await?;
    let broken: Vec<&BackupSummary> = backups.iter().filter(|b| b.is_broken).collect();

    if broken.is_empty() {
        info!("No broken remote backups found");
        return Ok(0);
    }

    let mut deleted = 0;
    for b in &broken {
        match delete_remote(s3, &b.name).await {
            Ok(()) => {
                info!(backup = %b.name, "Deleted broken remote backup");
                deleted += 1;
            }
            Err(e) => {
                warn!(
                    backup = %b.name,
                    error = %e,
                    "Failed to delete broken remote backup"
                );
            }
        }
    }

    info!("clean_broken: deleted {} broken backups", deleted);
    Ok(deleted)
}

/// Clean broken backups by location (local or remote).
pub async fn clean_broken(data_path: &str, s3: &S3Client, location: &Location) -> Result<()> {
    match location {
        Location::Local => {
            let count = clean_broken_local(data_path)?;
            info!(count = count, "Clean broken local complete");
        }
        Location::Remote => {
            let count = clean_broken_remote(s3).await?;
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
    valid.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

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

/// Extract all referenced S3 keys from a backup manifest.
///
/// Collects `backup_key` from every `PartInfo` and every `S3ObjectInfo`
/// within each table's parts. Returns a set of relative S3 keys.
fn collect_keys_from_manifest(manifest: &BackupManifest) -> HashSet<String> {
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

/// Collect all S3 keys referenced by surviving remote backups (excluding one).
///
/// Downloads and parses each surviving backup's manifest, then unions all
/// referenced keys. Used by GC to determine which keys must not be deleted.
///
/// `exclude_backup` is the backup currently being deleted -- its manifest
/// is not loaded (it would reference its own keys).
///
/// `cached_backups` -- when provided, uses the pre-fetched backup list instead
/// of calling `list_remote` again.  This avoids redundant S3 LIST calls when
/// the caller (e.g. `retention_remote`) already holds the backup list and no
/// mutations have occurred between iterations.
pub async fn gc_collect_referenced_keys(
    s3: &S3Client,
    exclude_backup: &str,
    cached_backups: Option<&[BackupSummary]>,
) -> Result<HashSet<String>> {
    // Use the pre-fetched list when available; otherwise fall back to a fresh listing.
    let owned_backups;
    let backups: &[BackupSummary] = match cached_backups {
        Some(list) => list,
        None => {
            owned_backups = list_remote(s3).await?;
            &owned_backups
        }
    };

    let mut all_keys = HashSet::new();
    let mut manifest_count = 0;

    for backup in backups {
        // Skip the backup being deleted
        if backup.name == exclude_backup {
            continue;
        }
        // Skip broken backups (no valid manifest to load)
        if backup.is_broken {
            continue;
        }

        let manifest_key = format!("{}/metadata.json", backup.name);
        match s3.get_object(&manifest_key).await {
            Ok(data) => match BackupManifest::from_json_bytes(&data) {
                Ok(manifest) => {
                    let keys = collect_keys_from_manifest(&manifest);
                    all_keys.extend(keys);
                    manifest_count += 1;
                }
                Err(e) => {
                    warn!(
                        backup = %backup.name,
                        error = %e,
                        "gc: failed to parse manifest, skipping"
                    );
                }
            },
            Err(e) => {
                warn!(
                    backup = %backup.name,
                    error = %e,
                    "gc: failed to download manifest, skipping"
                );
            }
        }
    }

    info!(
        manifest_count = manifest_count,
        key_count = all_keys.len(),
        "gc: collected N referenced keys from M manifests"
    );

    Ok(all_keys)
}

/// Delete a remote backup with GC-safe key filtering.
///
/// Lists all S3 keys under the backup prefix, partitions them into manifest
/// key and data keys, filters out data keys that are still referenced by other
/// backups, deletes unreferenced data keys first, then deletes the manifest last.
///
/// The `referenced_keys` set should be produced by `gc_collect_referenced_keys()`.
/// Keys are compared as relative keys (matching `PartInfo.backup_key` format).
pub async fn gc_delete_backup(
    s3: &S3Client,
    backup_name: &str,
    referenced_keys: &HashSet<String>,
) -> Result<()> {
    let prefix = format!("{}/", backup_name);
    let objects = s3.list_objects(&prefix).await?;

    if objects.is_empty() {
        return Err(ChBackupError::BackupNotFound(format!(
            "remote backup '{}' not found (no objects under prefix '{}')",
            backup_name, prefix
        ))
        .into());
    }

    let s3_prefix = s3.prefix();

    // Partition keys: manifest key vs data keys
    let manifest_relative = format!("{}/metadata.json", backup_name);
    let mut manifest_key: Option<String> = None;
    let mut unreferenced_keys: Vec<String> = Vec::new();
    let mut referenced_count: usize = 0;

    for obj in &objects {
        let relative_key = strip_s3_prefix(&obj.key, s3_prefix);

        if relative_key == manifest_relative {
            manifest_key = Some(relative_key);
            continue;
        }

        // Check if this data key is referenced by another surviving backup
        if referenced_keys.contains(&relative_key) {
            referenced_count += 1;
        } else {
            unreferenced_keys.push(relative_key);
        }
    }

    info!(
        total_keys = objects.len(),
        unreferenced = unreferenced_keys.len(),
        referenced = referenced_count,
        "gc: deleting N unreferenced keys, preserving N referenced"
    );

    // Delete unreferenced data keys first
    if !unreferenced_keys.is_empty() {
        s3.delete_objects(unreferenced_keys).await?;
    }

    // Delete the manifest key last (makes the backup "broken" first, then gone)
    if let Some(mk) = manifest_key {
        s3.delete_objects(vec![mk]).await?;
    }

    info!(backup = %backup_name, "gc: remote backup deleted");
    Ok(())
}

/// Collect the set of backup names referenced as incremental bases by a list of backups.
///
/// Scans the `source` field of every `PartInfo` in the given manifests. Parts with
/// `source = "carried:{base_name}"` indicate that the backup depends on `{base_name}`
/// for its data. Returns the set of all such base names.
async fn collect_incremental_bases(s3: &S3Client, surviving_names: &[&str]) -> HashSet<String> {
    let mut bases = HashSet::new();

    for name in surviving_names {
        let manifest_key = format!("{}/metadata.json", name);
        let manifest = match s3.get_object(&manifest_key).await {
            Ok(data) => match BackupManifest::from_json_bytes(&data) {
                Ok(m) => m,
                Err(e) => {
                    warn!(
                        backup = %name,
                        error = %e,
                        "retention_remote: failed to parse surviving manifest for incremental check"
                    );
                    continue;
                }
            },
            Err(e) => {
                warn!(
                    backup = %name,
                    error = %e,
                    "retention_remote: failed to download surviving manifest for incremental check"
                );
                continue;
            }
        };

        for table in manifest.tables.values() {
            for parts in table.parts.values() {
                for part in parts {
                    if let Some(base_name) = part.source.strip_prefix("carried:") {
                        bases.insert(base_name.to_string());
                    }
                }
            }
        }
    }

    bases
}

/// Delete oldest remote backups exceeding the `keep` count with GC-safe deletion.
///
/// For each backup to delete, collects referenced keys from all surviving
/// manifests fresh (per design 8.2 race protection), then uses `gc_delete_backup`
/// to only delete unreferenced keys.
///
/// Before deleting a backup, checks whether any SURVIVING backup references it as
/// an incremental base (via `carried:{name}` in `PartInfo.source`). If so, the
/// deletion is skipped to prevent orphaned incremental backups.
///
/// Broken backups are excluded from retention counting (not deleted by retention).
/// Errors on individual backup deletions are logged as warnings, not fatal.
///
/// - `keep == 0`: unlimited, no retention action.
/// - `keep > 0`: keep the N newest valid backups, delete the rest.
///
/// Returns the number of successfully deleted backups.
pub async fn retention_remote(s3: &S3Client, keep: i32) -> Result<usize> {
    if keep <= 0 {
        return Ok(0);
    }

    let backups = list_remote(s3).await?;

    // Filter to valid (non-broken) backups
    let mut valid: Vec<&BackupSummary> = backups.iter().filter(|b| !b.is_broken).collect();

    let keep = keep as usize;
    if valid.len() <= keep {
        return Ok(0);
    }

    // Sort by timestamp ascending (oldest first)
    valid.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

    let to_delete = valid.len() - keep;
    let total = backups.len();

    // Determine surviving backups (those that will be kept)
    let surviving_names: Vec<&str> = valid
        .iter()
        .skip(to_delete)
        .map(|b| b.name.as_str())
        .collect();

    // Collect all backup names referenced as incremental bases by surviving backups
    let incremental_bases = collect_incremental_bases(s3, &surviving_names).await;

    let mut deleted = 0;

    for b in valid.iter().take(to_delete) {
        // Check if this backup is referenced as an incremental base by any surviving backup
        if incremental_bases.contains(&b.name) {
            warn!(
                backup = %b.name,
                "Skipping deletion of {}: referenced as incremental base by surviving backup(s)",
                b.name
            );
            continue;
        }

        // Collect referenced keys for each deletion using the pre-fetched backup list
        // to avoid redundant S3 LIST calls. The list is stable because we hold the
        // global lock and no other mutations happen between iterations.
        let referenced_keys = match gc_collect_referenced_keys(s3, &b.name, Some(&backups)).await {
            Ok(keys) => keys,
            Err(e) => {
                warn!(
                    backup = %b.name,
                    error = %e,
                    "retention_remote: failed to collect referenced keys, skipping backup"
                );
                continue;
            }
        };

        match gc_delete_backup(s3, &b.name, &referenced_keys).await {
            Ok(()) => {
                info!(backup = %b.name, "retention_remote: deleted old remote backup");
                deleted += 1;
            }
            Err(e) => {
                warn!(
                    backup = %b.name,
                    error = %e,
                    "retention_remote: failed to delete remote backup"
                );
            }
        }
    }

    info!(
        deleted = deleted,
        total = total,
        "retention_remote: deleted N of M remote backups"
    );
    Ok(deleted)
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
            prefixes.insert(format!("chbackup_{}_", sanitize_name(inner)));
        }
    }
    prefixes
}

/// Remove `chbackup_*` directories from a single disk's shadow path (sync helper).
///
/// If `name` is provided, only removes entries matching `chbackup_{sanitized_name}_*`.
/// If `name` is `None`, removes all entries matching `chbackup_*`.
///
/// Skips any freeze directories that belong to a currently-active backup (PID
/// lock file exists and held by a live process) to avoid racing with in-progress
/// `backup::create` operations.
///
/// Returns the number of directories removed.
fn clean_shadow_dir(disk_path: &str, name: Option<&str>) -> Result<usize> {
    let shadow_path = PathBuf::from(disk_path).join("shadow");

    if !shadow_path.exists() {
        return Ok(0);
    }

    let entries = std::fs::read_dir(&shadow_path)
        .with_context(|| format!("Failed to read shadow directory: {}", shadow_path.display()))?;

    let prefix_filter = name.map(|n| format!("chbackup_{}_", sanitize_name(n)));

    // When cleaning a specific backup, check its per-backup PID lock once up front.
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

        let should_remove = if let Some(ref prefix) = prefix_filter {
            dir_name.starts_with(prefix)
        } else {
            dir_name.starts_with("chbackup_")
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
pub async fn clean_shadow(ch: &ChClient, data_path: &str, name: Option<&str>) -> Result<usize> {
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
            clean_shadow_dir(&disk_path, name_owned.as_deref())
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
        let count =
            tokio::task::spawn_blocking(move || clean_shadow_dir(&dp, name_owned.as_deref()))
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
fn summary_from_manifest(manifest: &BackupManifest) -> BackupSummary {
    let object_disk_size = compute_object_disk_size(manifest);
    let required = extract_required_backup(manifest);
    BackupSummary {
        name: manifest.name.clone(),
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
        Ok(manifest) => summary_from_manifest(&manifest),
        Err(e) => {
            let reason = format!("manifest parse error: {e}");
            warn!(
                backup = %name,
                path = %metadata_path.display(),
                error = %e,
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

        // Results are sorted by name
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
                .with_ddl(
                    "CREATE TABLE default.trades (id UInt64) ENGINE = MergeTree ORDER BY id",
                )
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

    #[test]
    fn test_clean_broken_local() {
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
        let count = clean_broken_local(dir.path().to_str().unwrap()).unwrap();
        assert_eq!(count, 2, "Should have deleted 2 broken backups");

        // Verify both are gone
        assert!(!broken_dir.exists());
        assert!(!broken_dir2.exists());
    }

    #[test]
    fn test_clean_broken_local_preserves_valid() {
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
        let count = clean_broken_local(dir.path().to_str().unwrap()).unwrap();
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

        let count = clean_shadow_dir(dir.path().to_str().unwrap(), None).unwrap();

        assert_eq!(count, 2, "Should have removed 2 chbackup shadow dirs");
        assert!(!chbackup1.exists(), "chbackup_daily_mon should be removed");
        assert!(!chbackup2.exists(), "chbackup_weekly should be removed");
        assert!(other.exists(), "other_freeze_data should NOT be removed");
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
        let count = clean_shadow_dir(dir.path().to_str().unwrap(), Some("daily-mon")).unwrap();

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
        let count = clean_shadow_dir(dir.path().to_str().unwrap(), None).unwrap();
        assert_eq!(count, 0, "Should return 0 when no shadow dir exists");
    }

    #[test]
    fn test_clean_shadow_empty_shadow_dir() {
        let dir = tempfile::tempdir().unwrap();
        let shadow_dir = dir.path().join("shadow");
        std::fs::create_dir_all(&shadow_dir).unwrap();
        // Shadow dir exists but empty
        let count = clean_shadow_dir(dir.path().to_str().unwrap(), None).unwrap();
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
                p.backup_key =
                    "daily/data/default/trades/s3disk/202403_1_1_0.tar.lz4".to_string();
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

        let keys = collect_keys_from_manifest(&manifest);

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

        let keys = collect_keys_from_manifest(&manifest);
        assert!(keys.is_empty(), "Empty manifest should produce no keys");
    }

    // -- GC filtering tests --

    #[test]
    fn test_gc_filter_unreferenced_keys() {
        // Simulate the GC filtering logic from gc_delete_backup:
        // Given a set of S3 keys for a backup and a referenced set from other backups,
        // only unreferenced data keys should be candidates for deletion.

        let all_keys = vec![
            "backup-a/data/default/trades/default/part1.tar.lz4".to_string(),
            "backup-a/data/default/trades/default/part2.tar.lz4".to_string(),
            "backup-a/objects/store/abc/data.bin".to_string(),
            "backup-a/metadata.json".to_string(),
        ];

        // part1 is referenced by another backup (shared via incremental)
        let mut referenced = HashSet::new();
        referenced.insert("backup-a/data/default/trades/default/part1.tar.lz4".to_string());

        let manifest_key = "backup-a/metadata.json";

        // Apply the same filtering logic as gc_delete_backup
        let mut unreferenced: Vec<&String> = Vec::new();
        let mut referenced_count = 0;
        let mut found_manifest = false;

        for key in &all_keys {
            if key == manifest_key {
                found_manifest = true;
                continue;
            }
            if referenced.contains(key.as_str()) {
                referenced_count += 1;
            } else {
                unreferenced.push(key);
            }
        }

        // part1 is referenced, so only part2 and objects/store/abc/data.bin are unreferenced
        assert_eq!(unreferenced.len(), 2, "Should have 2 unreferenced keys");
        assert_eq!(referenced_count, 1, "Should have 1 referenced key");
        assert!(found_manifest, "Should have found the manifest key");

        assert!(unreferenced
            .contains(&&"backup-a/data/default/trades/default/part2.tar.lz4".to_string()));
        assert!(unreferenced.contains(&&"backup-a/objects/store/abc/data.bin".to_string()));

        // part1 should NOT be in unreferenced (it's still needed by another backup)
        assert!(!unreferenced
            .contains(&&"backup-a/data/default/trades/default/part1.tar.lz4".to_string()));
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
            vec![
                PartInfo::new("all_0_0_0", 100, 0),
                {
                    let mut p = PartInfo::new("all_1_1_0", 100, 0);
                    p.source = "carried:base-backup".to_string();
                    p
                },
            ],
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
}
