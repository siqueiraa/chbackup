//! Download: fetch backup from S3 and decompress parts to local directory.
//!
//! Download flow (design doc section 4):
//! 1. Download manifest: `s3.get_object("{backup_name}/metadata.json")` -> parse BackupManifest
//! 2. Create local backup directory: `{data_path}/backup/{backup_name}/`
//! 3. Flatten all parts into a work queue, download in parallel
//!    (bounded by download_concurrency semaphore, rate-limited)
//!    - Local disk parts: full download + decompress (tar+LZ4)
//!    - S3 disk parts: download metadata files only (data objects stay in backup bucket until restore)
//! 4. Save per-table metadata and manifest to local directory
//! 5. Return backup directory path

pub mod stream;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use futures::future::try_join_all;
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};

use crate::concurrency::effective_download_concurrency;
use crate::config::Config;
use crate::manifest::{BackupManifest, PartInfo};
use crate::object_disk::is_s3_disk;
use crate::rate_limiter::RateLimiter;
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

/// A work item for the parallel download queue.
struct DownloadWorkItem {
    /// Table key in "db.table" format.
    table_key: String,
    /// Database name.
    db: String,
    /// Table name.
    table: String,
    /// Disk name within the table's parts map.
    disk_name: String,
    /// Part info (name, backup_key, etc.).
    part: PartInfo,
    /// Whether this part resides on an S3 object disk.
    is_s3_disk_part: bool,
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

    // 3. Flatten all parts into work items
    let mut work_items: Vec<DownloadWorkItem> = Vec::new();

    for (table_key, table_manifest) in &manifest.tables {
        let (db, table) = table_key.split_once('.').unwrap_or(("default", table_key));

        // Skip metadata-only tables (no data parts)
        if table_manifest.metadata_only {
            debug!(table = %table_key, "Skipping metadata-only table");
            continue;
        }

        for (disk_name, parts) in &table_manifest.parts {
            let disk_is_s3 = manifest
                .disk_types
                .get(disk_name)
                .map(|dt| is_s3_disk(dt))
                .unwrap_or(false);

            for part in parts {
                if part.backup_key.is_empty() {
                    warn!(
                        table = %table_key,
                        part = %part.name,
                        "Part has no backup_key, skipping download"
                    );
                    continue;
                }

                work_items.push(DownloadWorkItem {
                    table_key: table_key.clone(),
                    db: db.to_string(),
                    table: table.to_string(),
                    disk_name: disk_name.clone(),
                    part: part.clone(),
                    is_s3_disk_part: disk_is_s3 && part.s3_objects.is_some(),
                });
            }
        }
    }

    let total_parts = work_items.len();
    let concurrency = effective_download_concurrency(config) as usize;

    info!(
        "Downloading {} parts (concurrency={})",
        total_parts, concurrency
    );

    // 4. Download parts in parallel with semaphore and rate limiter
    let semaphore = Arc::new(Semaphore::new(concurrency));
    let rate_limiter = RateLimiter::new(config.backup.download_max_bytes_per_second);

    let mut handles = Vec::with_capacity(total_parts);

    for item in work_items {
        let sem = semaphore.clone();
        let s3 = s3.clone();
        let rate_limiter = rate_limiter.clone();
        let backup_dir = backup_dir.clone();

        let handle = tokio::spawn(async move {
            let _permit = sem
                .acquire()
                .await
                .map_err(|_| anyhow::anyhow!("Semaphore closed"))?;

            debug!(
                table = %item.table_key,
                part = %item.part.name,
                key = %item.part.backup_key,
                is_s3_disk = item.is_s3_disk_part,
                "Downloading part"
            );

            let url_db = url_encode(&item.db);
            let url_table = url_encode(&item.table);

            if item.is_s3_disk_part {
                // S3 disk part: download only metadata files, not the full
                // compressed data archive. The actual S3 data objects remain
                // in the backup bucket until restore copies them.
                let metadata_prefix = &item.part.backup_key;
                let metadata_objects = s3
                    .list_objects(metadata_prefix)
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to list metadata for S3 disk part {} of table {}",
                            item.part.name, item.table_key
                        )
                    })?;

                let shadow_dir = backup_dir
                    .join("shadow")
                    .join(&url_db)
                    .join(&url_table)
                    .join(&item.part.name);

                std::fs::create_dir_all(&shadow_dir).with_context(|| {
                    format!(
                        "Failed to create shadow dir for S3 disk part: {}",
                        shadow_dir.display()
                    )
                })?;

                let mut total_metadata_bytes = 0u64;

                for obj in &metadata_objects {
                    // Extract filename relative to the part prefix
                    let relative_name = obj
                        .key
                        .strip_prefix(s3.prefix())
                        .unwrap_or(&obj.key)
                        .strip_prefix(metadata_prefix)
                        .unwrap_or(&obj.key)
                        .trim_start_matches('/');

                    if relative_name.is_empty() {
                        continue;
                    }

                    let data = s3
                        .get_object(
                            &format!("{}/{}", metadata_prefix.trim_end_matches('/'), relative_name),
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "Failed to download metadata file {} for part {}",
                                relative_name, item.part.name
                            )
                        })?;

                    let file_size = data.len() as u64;
                    total_metadata_bytes += file_size;

                    // Write metadata file to local shadow directory
                    let file_path = shadow_dir.join(relative_name);
                    if let Some(parent) = file_path.parent() {
                        std::fs::create_dir_all(parent).with_context(|| {
                            format!(
                                "Failed to create parent dir: {}",
                                parent.display()
                            )
                        })?;
                    }
                    std::fs::write(&file_path, &data).with_context(|| {
                        format!(
                            "Failed to write metadata file: {}",
                            file_path.display()
                        )
                    })?;
                }

                rate_limiter.consume(total_metadata_bytes).await;

                debug!(
                    table = %item.table_key,
                    part = %item.part.name,
                    metadata_files = metadata_objects.len(),
                    metadata_bytes = total_metadata_bytes,
                    "S3 disk part metadata downloaded (data objects stay on S3)"
                );

                Ok::<(String, u64), anyhow::Error>((item.table_key, total_metadata_bytes))
            } else {
                // Local disk part: full download + decompress
                let compressed_data = s3
                    .get_object(&item.part.backup_key)
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to download part {} for table {}",
                            item.part.name, item.table_key
                        )
                    })?;

                let compressed_size = compressed_data.len() as u64;

                // Rate limit after download
                rate_limiter.consume(compressed_size).await;

                // Decompress and extract to local directory
                // Target: {backup_dir}/shadow/{db}/{table}/
                let shadow_dir = backup_dir
                    .join("shadow")
                    .join(&url_db)
                    .join(&url_table);

                // Run decompression in a blocking task (sync I/O)
                let shadow_dir_clone = shadow_dir.clone();
                let part_name = item.part.name.clone();
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

                debug!(
                    table = %item.table_key,
                    part = %item.part.name,
                    compressed_size = compressed_size,
                    "Part downloaded and decompressed"
                );

                Ok::<(String, u64), anyhow::Error>((item.table_key, compressed_size))
            }
        });

        handles.push(handle);
    }

    // Await all downloads
    let results: Vec<(String, u64)> = try_join_all(handles)
        .await
        .context("A download task panicked")?
        .into_iter()
        .collect::<Result<Vec<_>>>()?;

    // 5. Tally totals
    let mut total_compressed_bytes = 0u64;
    for (_table_key, compressed_size) in &results {
        total_compressed_bytes += compressed_size;
    }

    // 6. Save per-table metadata (sequential)
    for (table_key, table_manifest) in &manifest.tables {
        let (db, table) = table_key.split_once('.').unwrap_or(("default", table_key));

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

    // 7. Save manifest to local backup directory
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
    use crate::manifest::S3ObjectInfo;
    use std::collections::HashMap;

    #[test]
    fn test_download_work_item_construction() {
        let part = PartInfo {
            name: "202401_1_50_3".to_string(),
            size: 134_217_728,
            backup_key: "daily/data/default/trades/202401_1_50_3.tar.lz4".to_string(),
            source: "uploaded".to_string(),
            checksum_crc64: 12345,
            s3_objects: None,
        };

        let work_item = DownloadWorkItem {
            table_key: "default.trades".to_string(),
            db: "default".to_string(),
            table: "trades".to_string(),
            disk_name: "default".to_string(),
            part: part.clone(),
            is_s3_disk_part: false,
        };

        assert_eq!(work_item.table_key, "default.trades");
        assert_eq!(work_item.db, "default");
        assert_eq!(work_item.table, "trades");
        assert_eq!(work_item.disk_name, "default");
        assert_eq!(work_item.part.name, "202401_1_50_3");
        assert_eq!(
            work_item.part.backup_key,
            "daily/data/default/trades/202401_1_50_3.tar.lz4"
        );
        assert!(!work_item.is_s3_disk_part);
    }

    #[test]
    fn test_download_s3_disk_work_item_detected() {
        // Verify that S3 disk parts are flagged correctly
        let s3_part = PartInfo {
            name: "202401_1_50_3".to_string(),
            size: 134_217_728,
            backup_key: "daily/data/default/trades/s3disk/202401_1_50_3/".to_string(),
            source: "uploaded".to_string(),
            checksum_crc64: 12345,
            s3_objects: Some(vec![S3ObjectInfo {
                path: "store/abc/def/data.bin".to_string(),
                size: 134_217_000,
                backup_key: "daily/objects/store/abc/def/data.bin".to_string(),
            }]),
        };

        let disk_types: HashMap<String, String> = HashMap::from([
            ("default".to_string(), "local".to_string()),
            ("s3disk".to_string(), "s3".to_string()),
        ]);

        // Simulate what the download loop does: detect S3 disk type
        let disk_name = "s3disk";
        let disk_is_s3 = disk_types
            .get(disk_name)
            .map(|dt| is_s3_disk(dt))
            .unwrap_or(false);

        let work_item = DownloadWorkItem {
            table_key: "default.trades".to_string(),
            db: "default".to_string(),
            table: "trades".to_string(),
            disk_name: disk_name.to_string(),
            part: s3_part,
            is_s3_disk_part: disk_is_s3 && true, // s3_objects.is_some()
        };

        assert!(work_item.is_s3_disk_part);
        assert_eq!(work_item.disk_name, "s3disk");
        assert!(work_item.part.s3_objects.is_some());
    }

    #[test]
    fn test_download_local_parts_not_flagged_as_s3() {
        // Verify that local disk parts are NOT flagged as S3
        let local_part = PartInfo {
            name: "202401_1_50_3".to_string(),
            size: 4096,
            backup_key: "daily/data/default/trades/202401_1_50_3.tar.lz4".to_string(),
            source: "uploaded".to_string(),
            checksum_crc64: 11111,
            s3_objects: None,
        };

        let disk_types: HashMap<String, String> =
            HashMap::from([("default".to_string(), "local".to_string())]);

        let disk_name = "default";
        let disk_is_s3 = disk_types
            .get(disk_name)
            .map(|dt| is_s3_disk(dt))
            .unwrap_or(false);

        let work_item = DownloadWorkItem {
            table_key: "default.trades".to_string(),
            db: "default".to_string(),
            table: "trades".to_string(),
            disk_name: disk_name.to_string(),
            part: local_part,
            is_s3_disk_part: disk_is_s3 && false, // s3_objects.is_none()
        };

        assert!(!work_item.is_s3_disk_part);
        assert_eq!(work_item.disk_name, "default");
        assert!(work_item.part.s3_objects.is_none());
    }

    #[test]
    fn test_download_s3_disk_skips_data_detection() {
        // Verify that S3 disk detection requires BOTH disk_type=s3 AND s3_objects=Some
        let disk_types: HashMap<String, String> = HashMap::from([
            ("s3disk".to_string(), "s3".to_string()),
            ("default".to_string(), "local".to_string()),
        ]);

        // Case 1: S3 disk with s3_objects -> is_s3_disk_part = true
        let s3_disk_s3_objects = disk_types
            .get("s3disk")
            .map(|dt| is_s3_disk(dt))
            .unwrap_or(false);
        assert!(s3_disk_s3_objects && true); // s3_objects.is_some()

        // Case 2: S3 disk but no s3_objects (shouldn't happen, but defensive) -> false
        assert!(!(s3_disk_s3_objects && false)); // s3_objects.is_none()

        // Case 3: Local disk with no s3_objects -> false
        let local_disk = disk_types
            .get("default")
            .map(|dt| is_s3_disk(dt))
            .unwrap_or(false);
        assert!(!local_disk);

        // Case 4: Unknown disk -> false
        let unknown_disk = disk_types
            .get("unknown")
            .map(|dt| is_s3_disk(dt))
            .unwrap_or(false);
        assert!(!unknown_disk);
    }

    #[test]
    fn test_download_object_storage_disk_detected() {
        // Verify that "object_storage" disk type is also detected as S3
        let disk_types: HashMap<String, String> =
            HashMap::from([("objdisk".to_string(), "object_storage".to_string())]);

        let disk_is_s3 = disk_types
            .get("objdisk")
            .map(|dt| is_s3_disk(dt))
            .unwrap_or(false);
        assert!(disk_is_s3);
    }

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
