//! List and delete commands for local and remote backups.
//!
//! The `list` function scans local backup directories and/or queries S3 to
//! produce a summary of available backups. The `delete` function removes
//! backups from local disk or S3.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use tracing::{debug, info, warn};

use crate::manifest::BackupManifest;
use crate::storage::S3Client;

/// Location specifier matching the CLI `Location` enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    Local,
    Remote,
}

/// Summary of a single backup for display in list output.
#[derive(Debug, Clone)]
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
    /// Whether the backup manifest is missing or corrupt.
    pub is_broken: bool,
}

/// List backups based on the requested location.
///
/// If `location` is `None`, shows both local and remote backups.
/// If `Some(Local)`, shows only local backups.
/// If `Some(Remote)`, shows only remote backups.
pub async fn list(
    data_path: &str,
    s3: &S3Client,
    location: Option<&Location>,
) -> Result<()> {
    let show_local = location.is_none() || location == Some(&Location::Local);
    let show_remote = location.is_none() || location == Some(&Location::Remote);

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

    Ok(())
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
                        is_broken: false,
                    });
                }
                Err(e) => {
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
                        is_broken: true,
                    });
                }
            },
            Err(e) => {
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
                    is_broken: true,
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
            is_broken: true,
        };
    }

    match BackupManifest::load_from_file(metadata_path) {
        Ok(manifest) => BackupSummary {
            name: manifest.name.clone(),
            timestamp: Some(manifest.timestamp),
            size: total_uncompressed_size(&manifest),
            compressed_size: manifest.compressed_size,
            table_count: manifest.tables.len(),
            is_broken: false,
        },
        Err(e) => {
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
                is_broken: true,
            }
        }
    }
}

/// Compute total uncompressed size from all table parts.
fn total_uncompressed_size(manifest: &BackupManifest) -> u64 {
    manifest
        .tables
        .values()
        .map(|t| t.total_bytes)
        .sum()
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
fn format_size(bytes: u64) -> String {
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
        let status = if s.is_broken { " [BROKEN]" } else { "" };
        let ts = match &s.timestamp {
            Some(t) => t.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
            None => "unknown".to_string(),
        };
        let size_str = format_size(s.size);
        println!(
            "  {}{}\t{}\t{}\t{} tables",
            s.name, status, ts, size_str, s.table_count
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
        let broken = summaries.iter().find(|s| s.name == "broken-backup").unwrap();
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
            extract_backup_name_from_prefix("prod/region1/chbackup/daily/", "prod/region1/chbackup"),
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
        assert_eq!(
            strip_s3_prefix("other/key", "chbackup"),
            "other/key"
        );
    }
}
