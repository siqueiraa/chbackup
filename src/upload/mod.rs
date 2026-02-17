//! Upload: compress parts with LZ4 and upload to S3.
//!
//! Upload flow:
//! 1. Read manifest from `{backup_dir}/metadata.json`
//! 2. Flatten all parts across all tables into a single work queue
//! 3. Upload parts in parallel (bounded by upload_concurrency semaphore)
//!    - Parts with compressed size > 32MB use multipart upload
//!    - Rate limiter gates bytes uploaded
//! 4. Upload manifest JSON last (per design 3.6 -- backup is only "visible" when metadata.json exists)
//! 5. If delete_local, remove local backup directory

pub mod stream;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use futures::future::try_join_all;
use tokio::sync::Semaphore;
use tracing::{debug, info};

use crate::backup::diff::diff_parts;
use crate::concurrency::effective_upload_concurrency;
use crate::config::Config;
use crate::manifest::{BackupManifest, PartInfo};
use crate::rate_limiter::RateLimiter;
use crate::storage::s3::calculate_chunk_size;
use crate::storage::S3Client;

/// Multipart upload threshold: parts with compressed data larger than 32 MiB
/// use multipart upload instead of a single PutObject.
const MULTIPART_THRESHOLD: u64 = 32 * 1024 * 1024;

/// Check if a given data size should use multipart upload.
fn should_use_multipart(compressed_size: u64) -> bool {
    compressed_size > MULTIPART_THRESHOLD
}

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

/// A work item for the parallel upload queue.
struct UploadWorkItem {
    /// Table key in "db.table" format.
    table_key: String,
    /// Disk name within the table's parts map.
    disk_name: String,
    /// Part info (name, size, etc.).
    part: PartInfo,
    /// Local directory containing the part's data files.
    part_dir: PathBuf,
    /// S3 key where the compressed archive will be uploaded.
    s3_key: String,
}

/// Upload a local backup to S3.
///
/// Reads the manifest from the local backup directory, compresses each part
/// with tar+LZ4, uploads to S3 in parallel, and uploads the manifest last.
///
/// If `diff_from_remote` is provided, the specified remote backup's manifest
/// is loaded from S3 and used as a base for incremental comparison. Parts
/// matching by name+CRC64 are carried forward (skipped in the upload queue),
/// referencing the base backup's S3 key.
///
/// # Arguments
///
/// * `config` - Application configuration
/// * `s3` - S3 client for uploading
/// * `backup_name` - Name of the backup to upload
/// * `backup_dir` - Path to the local backup directory
/// * `delete_local` - If true, remove local backup directory after successful upload
/// * `diff_from_remote` - Optional remote base backup name for incremental upload
pub async fn upload(
    config: &Config,
    s3: &S3Client,
    backup_name: &str,
    backup_dir: &Path,
    delete_local: bool,
    diff_from_remote: Option<&str>,
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

    // 1b. If --diff-from-remote is specified, load remote base manifest and apply diff
    if let Some(base_name) = diff_from_remote {
        info!(base = %base_name, "Loading remote base manifest for diff-from-remote");
        let base_manifest_key = format!("{}/metadata.json", base_name);
        let base_bytes = s3
            .get_object(&base_manifest_key)
            .await
            .with_context(|| {
                format!(
                    "Failed to download base manifest for --diff-from-remote '{}'",
                    base_name
                )
            })?;
        let base = BackupManifest::from_json_bytes(&base_bytes).with_context(|| {
            format!(
                "Failed to parse base manifest for --diff-from-remote '{}'",
                base_name
            )
        })?;
        let result = diff_parts(&mut manifest, &base);
        info!(
            carried = result.carried,
            uploaded = result.uploaded,
            crc_mismatches = result.crc_mismatches,
            "Incremental diff applied to manifest (diff-from-remote)"
        );
        // Save updated manifest locally so carried parts are recorded
        manifest
            .save_to_file(&manifest_path)
            .context("Failed to save updated manifest after diff-from-remote")?;
    }

    let data_format = &config.backup.compression;

    info!(
        tables = manifest.tables.len(),
        data_format = %data_format,
        "Loaded manifest"
    );

    // 2. Flatten all parts into work items
    let mut work_items: Vec<UploadWorkItem> = Vec::new();
    let mut table_count = 0u64;

    let table_keys: Vec<String> = manifest.tables.keys().cloned().collect();

    for table_key in &table_keys {
        let (db, table) = table_key
            .split_once('.')
            .unwrap_or(("default", table_key));

        let table_manifest = match manifest.tables.get(table_key) {
            Some(tm) => tm,
            None => continue,
        };

        // Skip metadata-only tables (no data parts)
        if table_manifest.metadata_only {
            debug!(table = %table_key, "Skipping metadata-only table");
            continue;
        }

        let mut has_parts = false;

        for (disk_name, parts) in &table_manifest.parts {
            for part in parts {
                // Skip carried parts -- their data is already on S3 from the base backup
                if part.source.starts_with("carried:") {
                    debug!(
                        table = %table_key,
                        part = %part.name,
                        source = %part.source,
                        "Skipping carried part (already on S3)"
                    );
                    continue;
                }

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

                work_items.push(UploadWorkItem {
                    table_key: table_key.clone(),
                    disk_name: disk_name.clone(),
                    part: part.clone(),
                    part_dir,
                    s3_key,
                });

                has_parts = true;
            }
        }

        if has_parts {
            table_count += 1;
        }
    }

    let total_parts = work_items.len();
    let concurrency = effective_upload_concurrency(config) as usize;

    info!(
        "Uploading {} parts across {} tables (concurrency={})",
        total_parts, table_count, concurrency
    );

    // 3. Upload parts in parallel with semaphore and rate limiter
    let semaphore = Arc::new(Semaphore::new(concurrency));
    let rate_limiter = RateLimiter::new(config.backup.upload_max_bytes_per_second);
    let s3_chunk_size = config.s3.chunk_size;
    let s3_max_parts_count = config.s3.max_parts_count;

    let mut handles = Vec::with_capacity(total_parts);

    for item in work_items {
        let sem = semaphore.clone();
        let s3 = s3.clone();
        let rate_limiter = rate_limiter.clone();

        let handle = tokio::spawn(async move {
            let _permit = sem
                .acquire()
                .await
                .map_err(|_| anyhow::anyhow!("Semaphore closed"))?;

            debug!(
                table = %item.table_key,
                part = %item.part.name,
                s3_key = %item.s3_key,
                "Compressing and uploading part"
            );

            // Compress part using spawn_blocking (sync tar + LZ4)
            let part_dir = item.part_dir.clone();
            let part_name_for_compress = item.part.name.clone();
            let compressed = tokio::task::spawn_blocking(move || {
                stream::compress_part(&part_dir, &part_name_for_compress)
            })
            .await
            .context("Compress task panicked")?
            .with_context(|| format!("Failed to compress part {}", item.part.name))?;

            let compressed_size = compressed.len() as u64;

            // Decide between single PutObject and multipart upload
            if should_use_multipart(compressed_size) {
                debug!(
                    table = %item.table_key,
                    part = %item.part.name,
                    compressed_size = compressed_size,
                    "Part using multipart upload"
                );

                let chunk_size =
                    calculate_chunk_size(compressed_size, s3_chunk_size, s3_max_parts_count)
                        as usize;

                // Create multipart upload
                let upload_id = s3.create_multipart_upload(&item.s3_key).await?;

                // Upload chunks, aborting on error
                let upload_result = async {
                    let mut completed_parts: Vec<(i32, String)> = Vec::new();
                    let mut part_number = 1i32;

                    for chunk_start in (0..compressed.len()).step_by(chunk_size) {
                        let chunk_end = (chunk_start + chunk_size).min(compressed.len());
                        let chunk_data = compressed[chunk_start..chunk_end].to_vec();

                        let e_tag = s3
                            .upload_part(&item.s3_key, &upload_id, part_number, chunk_data)
                            .await?;

                        completed_parts.push((part_number, e_tag));
                        part_number += 1;
                    }

                    s3.complete_multipart_upload(&item.s3_key, &upload_id, completed_parts)
                        .await?;

                    Ok::<(), anyhow::Error>(())
                }
                .await;

                if let Err(e) = upload_result {
                    // Best-effort abort to clean up partial upload
                    let _ = s3.abort_multipart_upload(&item.s3_key, &upload_id).await;
                    return Err(e).with_context(|| {
                        format!("Multipart upload failed for part {}", item.part.name)
                    });
                }
            } else {
                // Single PutObject
                s3.put_object(&item.s3_key, compressed)
                    .await
                    .with_context(|| {
                        format!("Failed to upload part {} to S3", item.part.name)
                    })?;
            }

            // Rate limit after upload
            rate_limiter.consume(compressed_size).await;

            // Build updated part info
            let mut updated_part = item.part.clone();
            updated_part.backup_key = item.s3_key.clone();
            updated_part.source = "uploaded".to_string();

            debug!(
                table = %item.table_key,
                part = %item.part.name,
                compressed_size = compressed_size,
                "Part uploaded"
            );

            Ok((item.table_key, item.disk_name, updated_part, compressed_size))
        });

        handles.push(handle);
    }

    // Await all uploads
    let results: Vec<(String, String, PartInfo, u64)> = try_join_all(handles)
        .await
        .context("An upload task panicked")?
        .into_iter()
        .collect::<Result<Vec<_>>>()?;

    // 4. Apply results to manifest sequentially
    let mut total_compressed_size = 0u64;

    // Group results by (table_key, disk_name)
    let mut updates: HashMap<(String, String), Vec<PartInfo>> = HashMap::new();
    for (table_key, disk_name, updated_part, compressed) in results {
        total_compressed_size += compressed;
        updates
            .entry((table_key, disk_name))
            .or_default()
            .push(updated_part);
    }

    for ((table_key, disk_name), uploaded_parts) in updates {
        if let Some(tm) = manifest.tables.get_mut(&table_key) {
            // Preserve carried parts (already have correct backup_key from diff),
            // then append the newly uploaded parts with updated S3 keys.
            let carried: Vec<PartInfo> = tm
                .parts
                .get(&disk_name)
                .map(|parts| {
                    parts
                        .iter()
                        .filter(|p| p.source.starts_with("carried:"))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            let mut merged = carried;
            merged.extend(uploaded_parts);
            tm.parts.insert(disk_name, merged);
        }
    }

    // 5. Update manifest with compressed_size and data_format
    manifest.compressed_size = total_compressed_size;
    manifest.data_format = data_format.clone();

    // 6. Upload manifest LAST (per design 3.6)
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

    // 7. Update local manifest with S3 keys
    manifest
        .save_to_file(&manifest_path)
        .context("Failed to update local manifest with S3 keys")?;

    info!(
        backup_name = %backup_name,
        parts = total_parts,
        compressed_size = total_compressed_size,
        "Upload complete"
    );

    // 8. Delete local backup if requested
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
    fn test_should_use_multipart() {
        // Exactly at threshold: should NOT use multipart (must be strictly greater)
        assert!(!should_use_multipart(32 * 1024 * 1024));
        // Below threshold
        assert!(!should_use_multipart(1024));
        assert!(!should_use_multipart(0));
        // Above threshold
        assert!(should_use_multipart(32 * 1024 * 1024 + 1));
        assert!(should_use_multipart(100 * 1024 * 1024));
    }

    #[test]
    fn test_upload_work_item_construction() {
        // Verify work items collect correct fields from manifest data
        let part = PartInfo {
            name: "202401_1_50_3".to_string(),
            size: 134_217_728,
            backup_key: String::new(),
            source: "uploaded".to_string(),
            checksum_crc64: 12345,
            s3_objects: None,
        };

        let work_item = UploadWorkItem {
            table_key: "default.trades".to_string(),
            disk_name: "default".to_string(),
            part: part.clone(),
            part_dir: PathBuf::from("/tmp/backup/shadow/default/trades/202401_1_50_3"),
            s3_key: "daily/data/default/trades/202401_1_50_3.tar.lz4".to_string(),
        };

        assert_eq!(work_item.table_key, "default.trades");
        assert_eq!(work_item.disk_name, "default");
        assert_eq!(work_item.part.name, "202401_1_50_3");
        assert_eq!(work_item.part.size, 134_217_728);
        assert_eq!(
            work_item.s3_key,
            "daily/data/default/trades/202401_1_50_3.tar.lz4"
        );
    }

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
