//! Upload: compress parts with LZ4 and upload to S3.
//!
//! Upload flow:
//! 1. Read manifest from `{backup_dir}/metadata.json`
//! 2. For each table in manifest, for each part:
//!    - Read part directory from backup staging
//!    - Compress (tar + LZ4) to in-memory buffer
//!    - Upload to S3 with key `{backup_name}/data/{url_db}/{url_table}/{part_name}.tar.lz4`
//! 3. Upload manifest JSON last (per design 3.6 -- backup is only "visible" when metadata.json exists)
//! 4. If delete_local, remove local backup directory

pub mod stream;

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use tracing::{debug, info};

use crate::config::Config;
use crate::manifest::BackupManifest;
use crate::storage::S3Client;

/// URL-encode a component for use in S3 key paths.
///
/// Replaces special characters with percent-encoded equivalents, but keeps
/// alphanumeric chars, `-`, `_`, and `.` as-is.
/// Note: does NOT preserve `/` (unlike download::url_encode) since we
/// encode individual path components.
fn url_encode_component(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c.to_string()
            } else {
                format!("%{:02X}", c as u32)
            }
        })
        .collect()
}

/// Generate the S3 key for a compressed part archive.
///
/// Format: `{backup_name}/data/{url_encode(db)}/{url_encode(table)}/{part_name}.tar.lz4`
fn s3_key_for_part(backup_name: &str, db: &str, table: &str, part_name: &str) -> String {
    format!(
        "{}/data/{}/{}/{}.tar.lz4",
        backup_name,
        url_encode_component(db),
        url_encode_component(table),
        part_name
    )
}

/// Upload a local backup to S3.
///
/// Reads the manifest from the local backup directory, compresses each part
/// with tar+LZ4, uploads to S3, and uploads the manifest last.
///
/// # Arguments
///
/// * `config` - Application configuration
/// * `s3` - S3 client for uploading
/// * `backup_name` - Name of the backup to upload
/// * `backup_dir` - Path to the local backup directory
/// * `delete_local` - If true, remove local backup directory after successful upload
pub async fn upload(
    config: &Config,
    s3: &S3Client,
    backup_name: &str,
    backup_dir: &Path,
    delete_local: bool,
) -> Result<()> {
    info!(
        backup_name = %backup_name,
        backup_dir = %backup_dir.display(),
        delete_local = delete_local,
        "Starting upload"
    );

    // 1. Read manifest from local backup directory
    let manifest_path = backup_dir.join("metadata.json");
    if !manifest_path.exists() {
        bail!(
            "Manifest not found at {}. Run 'create' before 'upload'.",
            manifest_path.display()
        );
    }

    let mut manifest = BackupManifest::load_from_file(&manifest_path)
        .with_context(|| format!("Failed to load manifest from: {}", manifest_path.display()))?;

    let data_format = &config.backup.compression;

    info!(
        tables = manifest.tables.len(),
        data_format = %data_format,
        "Loaded manifest"
    );

    // 2. Upload each part
    let mut total_parts = 0u64;
    let mut total_compressed_size = 0u64;

    // We need to iterate over table keys and mutate their parts, so we collect keys first
    let table_keys: Vec<String> = manifest.tables.keys().cloned().collect();

    for table_key in &table_keys {
        let (db, table) = table_key
            .split_once('.')
            .unwrap_or(("default", table_key));

        let table_manifest = manifest
            .tables
            .get(table_key)
            .expect("table key exists")
            .clone();

        // Skip metadata-only tables (no data parts)
        if table_manifest.metadata_only {
            debug!(table = %table_key, "Skipping metadata-only table");
            continue;
        }

        // Collect all parts with their disk names for mutation
        let mut updated_parts: std::collections::HashMap<String, Vec<crate::manifest::PartInfo>> =
            std::collections::HashMap::new();

        for (disk_name, parts) in &table_manifest.parts {
            let mut updated_disk_parts = Vec::new();

            for part in parts {
                // Locate part directory in the backup staging area
                let part_dir = find_part_dir(backup_dir, db, table, &part.name)?;

                if !part_dir.exists() {
                    return Err(anyhow::anyhow!(
                        "Part directory not found: {} (expected at {})",
                        part.name,
                        part_dir.display()
                    ));
                }

                // Generate S3 key
                let s3_key = s3_key_for_part(backup_name, db, table, &part.name);

                debug!(
                    table = %table_key,
                    part = %part.name,
                    s3_key = %s3_key,
                    "Compressing and uploading part"
                );

                // Compress part using spawn_blocking (sync tar + LZ4)
                let part_dir_clone = part_dir.clone();
                let part_name = part.name.clone();
                let compressed = tokio::task::spawn_blocking(move || {
                    stream::compress_part(&part_dir_clone, &part_name)
                })
                .await
                .context("Compress task panicked")?
                .with_context(|| {
                    format!("Failed to compress part {}", part.name)
                })?;

                let compressed_size = compressed.len() as u64;

                // Upload to S3
                s3.put_object(&s3_key, compressed)
                    .await
                    .with_context(|| {
                        format!("Failed to upload part {} to S3", part.name)
                    })?;

                total_parts += 1;
                total_compressed_size += compressed_size;

                // Update part info with S3 key
                let mut updated_part = part.clone();
                updated_part.backup_key = s3_key;
                updated_part.source = "uploaded".to_string();
                updated_disk_parts.push(updated_part);

                debug!(
                    table = %table_key,
                    part = %part.name,
                    compressed_size = compressed_size,
                    "Part uploaded"
                );
            }

            updated_parts.insert(disk_name.clone(), updated_disk_parts);
        }

        // Update manifest with S3 keys
        if let Some(tm) = manifest.tables.get_mut(table_key) {
            tm.parts = updated_parts;
        }
    }

    // 3. Update manifest with compressed_size and data_format
    manifest.compressed_size = total_compressed_size;
    manifest.data_format = data_format.clone();

    // 4. Upload manifest LAST (per design 3.6)
    let manifest_key = format!("{}/metadata.json", backup_name);
    let manifest_bytes = manifest.to_json_bytes()?;

    info!(
        key = %manifest_key,
        size = manifest_bytes.len(),
        "Uploading manifest"
    );

    s3.put_object_with_options(&manifest_key, manifest_bytes, Some("application/json"))
        .await
        .context("Failed to upload manifest to S3")?;

    // 5. Update local manifest with S3 keys
    manifest
        .save_to_file(&manifest_path)
        .context("Failed to update local manifest with S3 keys")?;

    info!(
        backup_name = %backup_name,
        parts = total_parts,
        compressed_size = total_compressed_size,
        "Upload complete"
    );

    // 6. Delete local backup if requested
    if delete_local {
        info!(
            backup_dir = %backup_dir.display(),
            "Deleting local backup after upload"
        );
        std::fs::remove_dir_all(backup_dir).with_context(|| {
            format!(
                "Failed to delete local backup directory: {}",
                backup_dir.display()
            )
        })?;
    }

    // DEBUG_MARKER:F007 - verify upload completion
    info!(
        target: "debug",
        "DEBUG_VERIFY:F007 parts={} compressed_size={} backup_name={}",
        total_parts, total_compressed_size, backup_name
    );
    // END_DEBUG_MARKER:F007

    Ok(())
}

/// Find the part directory within the backup staging area.
///
/// Looks for the part at `{backup_dir}/shadow/{db}/{table}/{part_name}/`
/// Uses URL-encoded paths to match what collect_parts creates.
fn find_part_dir(backup_dir: &Path, db: &str, table: &str, part_name: &str) -> Result<PathBuf> {
    // Try URL-encoded path first (as created by backup::collect)
    let url_db = url_encode_component(db);
    let url_table = url_encode_component(table);

    let path = backup_dir
        .join("shadow")
        .join(&url_db)
        .join(&url_table)
        .join(part_name);

    if path.exists() {
        return Ok(path);
    }

    // Try plain path as fallback
    let plain_path = backup_dir
        .join("shadow")
        .join(db)
        .join(table)
        .join(part_name);

    if plain_path.exists() {
        return Ok(plain_path);
    }

    // Return the URL-encoded path (caller will check existence)
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_url_encode_component_simple() {
        assert_eq!(url_encode_component("default"), "default");
        assert_eq!(url_encode_component("my_table"), "my_table");
    }

    #[test]
    fn test_url_encode_component_special() {
        assert_eq!(url_encode_component("my table"), "my%20table");
        assert_eq!(url_encode_component("db:name"), "db%3Aname");
    }

    #[test]
    fn test_s3_key_for_part_simple() {
        let key = s3_key_for_part("daily-20240115", "default", "trades", "202401_1_50_3");
        assert_eq!(
            key,
            "daily-20240115/data/default/trades/202401_1_50_3.tar.lz4"
        );
    }

    #[test]
    fn test_s3_key_for_part_special_chars() {
        let key = s3_key_for_part("my-backup", "my db", "my+table", "202401_1_1_0");
        assert_eq!(
            key,
            "my-backup/data/my%20db/my%2Btable/202401_1_1_0.tar.lz4"
        );
    }

    #[test]
    fn test_find_part_dir_url_encoded() {
        let dir = tempfile::tempdir().unwrap();
        let part_path = dir
            .path()
            .join("shadow")
            .join("default")
            .join("trades")
            .join("202401_1_50_3");
        std::fs::create_dir_all(&part_path).unwrap();

        let found = find_part_dir(dir.path(), "default", "trades", "202401_1_50_3").unwrap();
        assert_eq!(found, part_path);
    }
}
