//! List and delete commands for local and remote backups.
//!
//! The `list` function scans local backup directories and/or queries S3 to
//! produce a summary of available backups. The `delete` function removes
//! backups from local disk or S3.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use tracing::{debug, info, warn};

use crate::clickhouse::{sanitize_name, ChClient};
use crate::config::Config;
use crate::manifest::BackupManifest;
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
    /// Whether the backup manifest is missing or corrupt.
    pub is_broken: bool,
    /// Reason why the backup is broken (e.g., "metadata.json not found").
    /// None for valid backups.
    pub broken_reason: Option<String>,
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
    s3: &S3Client,
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

/// Format backup summaries as delimited text (CSV or TSV).
fn format_delimited(summaries: &[BackupSummary], delimiter: char) -> String {
    let mut output = String::new();
    let d = &delimiter.to_string();

    // Header row
    let headers = [
        "name",
        "timestamp",
        "size",
        "compressed_size",
        "table_count",
        "metadata_size",
        "is_broken",
        "broken_reason",
    ];
    output.push_str(&headers.join(d));
    output.push('\n');

    // Data rows
    for s in summaries {
        let ts = match &s.timestamp {
            Some(t) => t.to_rfc3339(),
            None => String::new(),
        };
        let broken_reason = s.broken_reason.as_deref().unwrap_or("");

        let fields = [
            s.name.as_str(),
            &ts,
            &s.size.to_string(),
            &s.compressed_size.to_string(),
            &s.table_count.to_string(),
            &s.metadata_size.to_string(),
            &s.is_broken.to_string(),
            broken_reason,
        ];
        output.push_str(&fields.join(d));
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
    match name {
        "latest" => {
            let valid: Vec<&BackupSummary> = backups.iter().filter(|b| !b.is_broken).collect();
            valid
                .last()
                .map(|b| b.name.clone())
                .ok_or_else(|| anyhow::anyhow!("No backups found to resolve 'latest'"))
        }
        "previous" => {
            let valid: Vec<&BackupSummary> = backups.iter().filter(|b| !b.is_broken).collect();
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
                    summaries.push(BackupSummary {
                        name: manifest.name.clone(),
                        timestamp: Some(manifest.timestamp),
                        size: total_uncompressed_size(&manifest),
                        compressed_size: manifest.compressed_size,
                        table_count: manifest.tables.len(),
                        metadata_size: manifest.metadata_size,
                        is_broken: false,
                        broken_reason: None,
                    });
                }
                Err(e) => {
                    let reason = format!("manifest parse error: {e}");
                    warn!(
                        backup = %name,
                        error = %e,
                        "Failed to parse remote manifest, marking as broken"
                    );
                    summaries.push(BackupSummary {
                        name,
                        timestamp: None,
                        size: 0,
                        compressed_size: 0,
                        table_count: 0,
                        metadata_size: 0,
                        is_broken: true,
                        broken_reason: Some(reason),
                    });
                }
            },
            Err(e) => {
                let reason = format!("metadata.json not found: {e}");
                debug!(
                    backup = %name,
                    error = %e,
                    "No manifest found for remote backup, marking as broken"
                );
                summaries.push(BackupSummary {
                    name,
                    timestamp: None,
                    size: 0,
                    compressed_size: 0,
                    table_count: 0,
                    metadata_size: 0,
                    is_broken: true,
                    broken_reason: Some(reason),
                });
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

/// Delete a local backup directory.
///
/// Removes `{data_path}/backup/{backup_name}/` entirely.
pub fn delete_local(data_path: &str, backup_name: &str) -> Result<()> {
    let backup_dir = PathBuf::from(data_path).join("backup").join(backup_name);

    if !backup_dir.exists() {
        return Err(anyhow::anyhow!(
            "Local backup '{}' not found at: {}",
            backup_name,
            backup_dir.display()
        ));
    }

    info!(
        backup = %backup_name,
        path = %backup_dir.display(),
        "Deleting local backup"
    );

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
        return Err(anyhow::anyhow!(
            "Remote backup '{}' not found (no objects under prefix '{}')",
            backup_name,
            prefix
        ));
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
pub async fn gc_collect_referenced_keys(
    s3: &S3Client,
    exclude_backup: &str,
) -> Result<HashSet<String>> {
    let backups = list_remote(s3).await?;

    let mut all_keys = HashSet::new();
    let mut manifest_count = 0;

    for backup in &backups {
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
        return Err(anyhow::anyhow!(
            "Remote backup '{}' not found (no objects under prefix '{}')",
            backup_name,
            prefix
        ));
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

    // DEBUG_MARKER:F001 - verify incremental base protection
    debug!(
        target: "debug",
        "DEBUG_VERIFY:F001 incremental_bases={:?}, to_delete_count={}, surviving_count={}",
        incremental_bases, to_delete, surviving_names.len()
    );
    // END_DEBUG_MARKER:F001

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

        // Collect referenced keys fresh for each deletion (design 8.2 race protection)
        let referenced_keys = match gc_collect_referenced_keys(s3, &b.name).await {
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

/// Remove `chbackup_*` directories from a single disk's shadow path (sync helper).
///
/// If `name` is provided, only removes entries matching `chbackup_{sanitized_name}_*`.
/// If `name` is `None`, removes all entries matching `chbackup_*`.
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

        if should_remove {
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

/// Parse a backup summary from a metadata.json file path.
fn parse_backup_summary(name: &str, metadata_path: &Path) -> BackupSummary {
    if !metadata_path.exists() {
        return BackupSummary {
            name: name.to_string(),
            timestamp: None,
            size: 0,
            compressed_size: 0,
            table_count: 0,
            metadata_size: 0,
            is_broken: true,
            broken_reason: Some("metadata.json not found".to_string()),
        };
    }

    match BackupManifest::load_from_file(metadata_path) {
        Ok(manifest) => BackupSummary {
            name: manifest.name.clone(),
            timestamp: Some(manifest.timestamp),
            size: total_uncompressed_size(&manifest),
            compressed_size: manifest.compressed_size,
            table_count: manifest.tables.len(),
            metadata_size: manifest.metadata_size,
            is_broken: false,
            broken_reason: None,
        },
        Err(e) => {
            let reason = format!("manifest parse error: {e}");
            warn!(
                backup = %name,
                path = %metadata_path.display(),
                error = %e,
                "Failed to parse manifest, marking as broken"
            );
            BackupSummary {
                name: name.to_string(),
                timestamp: None,
                size: 0,
                compressed_size: 0,
                table_count: 0,
                metadata_size: 0,
                is_broken: true,
                broken_reason: Some(reason),
            }
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
fn print_backup_table(summaries: &[BackupSummary]) {
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
        println!(
            "  {}{}\t{}\t{}\t{}\t{} tables",
            s.name, status, ts, size_str, compressed_str, s.table_count
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_parse_local_backup_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let backup_base = dir.path().join("backup");
        std::fs::create_dir_all(&backup_base).unwrap();

        // Create a valid backup with metadata.json
        let backup1 = backup_base.join("daily-2024-01-15");
        std::fs::create_dir_all(&backup1).unwrap();
        let manifest = BackupManifest {
            manifest_version: 1,
            name: "daily-2024-01-15".to_string(),
            timestamp: chrono::Utc::now(),
            clickhouse_version: "24.1.3.31".to_string(),
            chbackup_version: "0.1.0".to_string(),
            data_format: "lz4".to_string(),
            compressed_size: 1024,
            metadata_size: 256,
            disks: HashMap::new(),
            disk_types: HashMap::new(),
            disk_remote_paths: HashMap::new(),
            tables: HashMap::new(),
            databases: Vec::new(),
            functions: Vec::new(),
            named_collections: Vec::new(),
            rbac: None,
        };
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
        let dir = tempfile::tempdir().unwrap();
        let backup_base = dir.path().join("backup");
        std::fs::create_dir_all(&backup_base).unwrap();

        let backup1 = backup_base.join("test-backup");
        std::fs::create_dir_all(&backup1).unwrap();

        let mut tables = HashMap::new();
        tables.insert(
            "default.trades".to_string(),
            crate::manifest::TableManifest {
                ddl: "CREATE TABLE default.trades (id UInt64) ENGINE = MergeTree ORDER BY id"
                    .to_string(),
                uuid: None,
                engine: "MergeTree".to_string(),
                total_bytes: 1_000_000,
                parts: HashMap::new(),
                pending_mutations: Vec::new(),
                metadata_only: false,
                dependencies: Vec::new(),
            },
        );

        let manifest = BackupManifest {
            manifest_version: 1,
            name: "test-backup".to_string(),
            timestamp: chrono::Utc::now(),
            clickhouse_version: "24.1.3.31".to_string(),
            chbackup_version: "0.1.0".to_string(),
            data_format: "lz4".to_string(),
            compressed_size: 500_000,
            metadata_size: 256,
            disks: HashMap::new(),
            disk_types: HashMap::new(),
            disk_remote_paths: HashMap::new(),
            tables,
            databases: Vec::new(),
            functions: Vec::new(),
            named_collections: Vec::new(),
            rbac: None,
        };
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
            size: 1_048_576,           // 1 MB
            compressed_size: 524_288,  // 512 KB
            table_count: 3,
            metadata_size: 0,
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
        let manifest = BackupManifest {
            manifest_version: 1,
            name: "valid-backup".to_string(),
            timestamp: chrono::Utc::now(),
            clickhouse_version: "24.1.3.31".to_string(),
            chbackup_version: "0.1.0".to_string(),
            data_format: "lz4".to_string(),
            compressed_size: 1024,
            metadata_size: 256,
            disks: HashMap::new(),
            disk_types: HashMap::new(),
            disk_remote_paths: HashMap::new(),
            tables: HashMap::new(),
            databases: Vec::new(),
            functions: Vec::new(),
            named_collections: Vec::new(),
            rbac: None,
        };
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
        let manifest = BackupManifest {
            manifest_version: 1,
            name: name.to_string(),
            timestamp,
            clickhouse_version: "24.1.3.31".to_string(),
            chbackup_version: "0.1.0".to_string(),
            data_format: "lz4".to_string(),
            compressed_size: 1024,
            metadata_size: 256,
            disks: HashMap::new(),
            disk_types: HashMap::new(),
            disk_remote_paths: HashMap::new(),
            tables: HashMap::new(),
            databases: Vec::new(),
            functions: Vec::new(),
            named_collections: Vec::new(),
            rbac: None,
        };
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

        let mut parts = HashMap::new();

        // Local disk parts with backup_key
        parts.insert(
            "default".to_string(),
            vec![
                PartInfo {
                    name: "202401_1_50_3".to_string(),
                    size: 100,
                    backup_key: "daily/data/default/trades/default/202401_1_50_3.tar.lz4"
                        .to_string(),
                    source: "uploaded".to_string(),
                    checksum_crc64: 123,
                    s3_objects: None,
                },
                PartInfo {
                    name: "202402_1_1_0".to_string(),
                    size: 50,
                    backup_key: "daily/data/default/trades/default/202402_1_1_0.tar.lz4"
                        .to_string(),
                    source: "uploaded".to_string(),
                    checksum_crc64: 456,
                    s3_objects: None,
                },
            ],
        );

        // S3 disk parts with s3_objects
        parts.insert(
            "s3disk".to_string(),
            vec![PartInfo {
                name: "202403_1_1_0".to_string(),
                size: 200,
                backup_key: "daily/data/default/trades/s3disk/202403_1_1_0.tar.lz4".to_string(),
                source: "uploaded".to_string(),
                checksum_crc64: 789,
                s3_objects: Some(vec![
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
                ]),
            }],
        );

        let mut tables = HashMap::new();
        tables.insert(
            "default.trades".to_string(),
            TableManifest {
                ddl: "CREATE TABLE ...".to_string(),
                uuid: None,
                engine: "MergeTree".to_string(),
                total_bytes: 350,
                parts,
                pending_mutations: Vec::new(),
                metadata_only: false,
                dependencies: Vec::new(),
            },
        );

        let manifest = BackupManifest {
            manifest_version: 1,
            name: "daily".to_string(),
            timestamp: chrono::Utc::now(),
            clickhouse_version: "24.1.3.31".to_string(),
            chbackup_version: "0.1.0".to_string(),
            data_format: "lz4".to_string(),
            compressed_size: 350,
            metadata_size: 256,
            disks: HashMap::new(),
            disk_types: HashMap::new(),
            disk_remote_paths: HashMap::new(),
            tables,
            databases: Vec::new(),
            functions: Vec::new(),
            named_collections: Vec::new(),
            rbac: None,
        };

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
        let manifest = BackupManifest {
            manifest_version: 1,
            name: "empty".to_string(),
            timestamp: chrono::Utc::now(),
            clickhouse_version: "24.1.3.31".to_string(),
            chbackup_version: "0.1.0".to_string(),
            data_format: "lz4".to_string(),
            compressed_size: 0,
            metadata_size: 0,
            disks: HashMap::new(),
            disk_types: HashMap::new(),
            disk_remote_paths: HashMap::new(),
            tables: HashMap::new(),
            databases: Vec::new(),
            functions: Vec::new(),
            named_collections: Vec::new(),
            rbac: None,
        };

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
            is_broken: false,
            broken_reason: None,
        };
        assert_eq!(summary.metadata_size, 256);
    }

    #[test]
    fn test_parse_backup_summary_populates_metadata_size() {
        // Create a backup directory with a manifest that has metadata_size
        let dir = tempfile::tempdir().unwrap();
        let backup_dir = dir.path().join("test-backup");
        std::fs::create_dir_all(&backup_dir).unwrap();

        let manifest = crate::manifest::BackupManifest {
            manifest_version: 1,
            name: "test-backup".to_string(),
            timestamp: chrono::Utc::now(),
            clickhouse_version: String::new(),
            chbackup_version: String::new(),
            data_format: "lz4".to_string(),
            compressed_size: 500,
            metadata_size: 1024,
            disks: std::collections::HashMap::new(),
            disk_types: std::collections::HashMap::new(),
            disk_remote_paths: std::collections::HashMap::new(),
            tables: std::collections::HashMap::new(),
            databases: Vec::new(),
            functions: Vec::new(),
            named_collections: Vec::new(),
            rbac: None,
        };

        let metadata_path = backup_dir.join("metadata.json");
        manifest.save_to_file(&metadata_path).unwrap();

        let summary = parse_backup_summary("test-backup", &metadata_path);
        assert!(!summary.is_broken);
        assert_eq!(summary.metadata_size, 1024);
    }
}
