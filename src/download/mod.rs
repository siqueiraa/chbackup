//! Download: fetch backup from S3 and decompress parts to local directory.
//!
//! Download flow (design doc section 4):
//! 1. Download manifest: `s3.get_object("{backup_name}/metadata.json")` -> parse BackupManifest
//! 2. Create local backup directory: `{data_path}/backup/{backup_name}/`
//! 3. For each table in manifest, for each part:
//!    - Download compressed archive from S3
//!    - Decompress (LZ4) and untar to local directory
//! 4. Save manifest to local directory
//! 5. Return backup directory path

pub mod stream;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

use crate::config::Config;
use crate::manifest::BackupManifest;
use crate::storage::S3Client;

/// URL-encode a component for use in S3 key paths and local directory names.
///
/// Replaces special characters with percent-encoded equivalents, but keeps
/// alphanumeric chars, `/`, `-`, `_`, and `.` as-is.
fn url_encode(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '/' || c == '-' || c == '_' || c == '.' {
                c.to_string()
            } else {
                format!("%{:02X}", c as u32)
            }
        })
        .collect()
}

/// Download a backup from S3 to the local filesystem.
///
/// Returns the path to the local backup directory containing the downloaded
/// manifest and part data.
///
/// # Arguments
///
/// * `config` - Application configuration (for data_path)
/// * `s3` - S3 client for downloading objects
/// * `backup_name` - Name of the backup to download
pub async fn download(
    config: &Config,
    s3: &S3Client,
    backup_name: &str,
) -> Result<PathBuf> {
    let data_path = &config.clickhouse.data_path;
    let backup_dir = Path::new(data_path)
        .join("backup")
        .join(backup_name);

    info!(
        backup_name = %backup_name,
        backup_dir = %backup_dir.display(),
        "Starting download"
    );

    // 1. Download manifest from S3
    let manifest_key = format!("{}/metadata.json", backup_name);
    let manifest_bytes = s3
        .get_object(&manifest_key)
        .await
        .with_context(|| format!("Failed to download manifest for backup '{}'", backup_name))?;

    let manifest = BackupManifest::from_json_bytes(&manifest_bytes)
        .with_context(|| format!("Failed to parse manifest for backup '{}'", backup_name))?;

    info!(
        backup_name = %backup_name,
        tables = manifest.tables.len(),
        "Downloaded manifest"
    );

    // 2. Create local backup directory
    std::fs::create_dir_all(&backup_dir)
        .with_context(|| format!("Failed to create backup dir: {}", backup_dir.display()))?;

    // 3. Download and decompress each part
    let mut total_parts = 0u64;
    let mut total_compressed_bytes = 0u64;

    for (table_key, table_manifest) in &manifest.tables {
        // Parse db.table from the key
        let (db, table) = table_key.split_once('.').unwrap_or(("default", table_key));

        // Skip metadata-only tables (no data parts)
        if table_manifest.metadata_only {
            debug!(table = %table_key, "Skipping metadata-only table");
            continue;
        }

        for parts in table_manifest.parts.values() {
            for part in parts {
                if part.backup_key.is_empty() {
                    warn!(
                        table = %table_key,
                        part = %part.name,
                        "Part has no backup_key, skipping download"
                    );
                    continue;
                }

                debug!(
                    table = %table_key,
                    part = %part.name,
                    key = %part.backup_key,
                    "Downloading part"
                );

                // Download compressed part from S3
                let compressed_data = s3
                    .get_object(&part.backup_key)
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to download part {} for table {}",
                            part.name, table_key
                        )
                    })?;

                let compressed_size = compressed_data.len() as u64;

                // Decompress and extract to local directory
                // Target: {backup_dir}/shadow/{db}/{table}/
                let url_db = url_encode(db);
                let url_table = url_encode(table);
                let shadow_dir = backup_dir
                    .join("shadow")
                    .join(&url_db)
                    .join(&url_table);

                // Run decompression in a blocking task (sync I/O)
                let shadow_dir_clone = shadow_dir.clone();
                let part_name = part.name.clone();
                tokio::task::spawn_blocking(move || {
                    stream::decompress_part(&compressed_data, &shadow_dir_clone)
                })
                .await
                .context("Decompress task panicked")?
                .with_context(|| {
                    format!(
                        "Failed to decompress part {} to {}",
                        part_name,
                        shadow_dir.display()
                    )
                })?;

                total_parts += 1;
                total_compressed_bytes += compressed_size;

                debug!(
                    table = %table_key,
                    part = %part.name,
                    compressed_size = compressed_size,
                    "Part downloaded and decompressed"
                );
            }
        }

        // Save per-table metadata
        let metadata_dir = backup_dir
            .join("metadata")
            .join(url_encode(db));
        std::fs::create_dir_all(&metadata_dir).with_context(|| {
            format!(
                "Failed to create metadata dir: {}",
                metadata_dir.display()
            )
        })?;

        let table_metadata_path = metadata_dir.join(format!("{}.json", url_encode(table)));
        let table_json = serde_json::to_string_pretty(table_manifest)
            .context("Failed to serialize table manifest")?;
        std::fs::write(&table_metadata_path, &table_json).with_context(|| {
            format!(
                "Failed to write table metadata: {}",
                table_metadata_path.display()
            )
        })?;
    }

    // 4. Save manifest to local backup directory
    let manifest_path = backup_dir.join("metadata.json");
    manifest
        .save_to_file(&manifest_path)
        .context("Failed to save manifest to local backup directory")?;

    info!(
        backup_name = %backup_name,
        parts = total_parts,
        compressed_bytes = total_compressed_bytes,
        backup_dir = %backup_dir.display(),
        "Download complete"
    );

    Ok(backup_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_url_encode_simple() {
        assert_eq!(url_encode("default"), "default");
        assert_eq!(url_encode("my_table"), "my_table");
        assert_eq!(url_encode("my-table"), "my-table");
    }

    #[test]
    fn test_url_encode_special_chars() {
        assert_eq!(url_encode("my table"), "my%20table");
        assert_eq!(url_encode("db:name"), "db%3Aname");
        assert_eq!(url_encode("test+data"), "test%2Bdata");
    }

    #[test]
    fn test_url_encode_preserves_dots() {
        assert_eq!(url_encode("db.table"), "db.table");
    }

    #[test]
    fn test_url_encode_preserves_slashes() {
        assert_eq!(url_encode("path/to/file"), "path/to/file");
    }
}
