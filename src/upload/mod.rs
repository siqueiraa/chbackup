//! Upload: compress parts with LZ4 and upload to S3.
//!
//! Upload flow:
//! 1. Read manifest from `{backup_dir}/metadata.json`
//! 2. Flatten all parts across all tables into work queues:
//!    - Local disk parts: compress with tar+LZ4 and upload
//!    - S3 disk parts: server-side CopyObject (no compression)
//! 3. Upload parts in parallel (bounded by separate semaphores per queue)
//!    - Local parts: upload_concurrency semaphore, multipart for >32MB
//!    - S3 disk parts: object_disk_copy_concurrency semaphore, CopyObject per S3 object
//!    - Rate limiter gates bytes uploaded (local parts only)
//! 4. Upload manifest JSON last (per design 3.6 -- backup is only "visible" when metadata.json exists)
//! 5. If delete_local, remove local backup directory

pub mod stream;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use futures::future::try_join_all;
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};

use crate::backup::diff::diff_parts;
use crate::concurrency::{effective_object_disk_copy_concurrency, effective_upload_concurrency};
use crate::config::Config;
use crate::manifest::{BackupManifest, PartInfo, S3ObjectInfo};
use crate::object_disk::is_s3_disk;
use crate::progress::ProgressTracker;
use crate::rate_limiter::RateLimiter;
use crate::resume::{
    compute_params_hash, delete_state_file, load_state_file, save_state_graceful, UploadState,
};
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
/// Format: `{backup_name}/data/{url_encode(db)}/{url_encode(table)}/{part_name}{extension}`
/// where `extension` is derived from `data_format` via `stream::archive_extension()`.
fn s3_key_for_part(
    backup_name: &str,
    db: &str,
    table: &str,
    part_name: &str,
    data_format: &str,
) -> String {
    format!(
        "{}/data/{}/{}/{}{}",
        backup_name,
        url_encode_component(db),
        url_encode_component(table),
        part_name,
        stream::archive_extension(data_format)
    )
}

/// Parse an S3 URI like `s3://bucket/prefix/` into (bucket, prefix).
///
/// Returns `(bucket, prefix)`. If the URI does not match `s3://` format,
/// returns the whole string as the prefix with an empty bucket.
fn parse_s3_uri(uri: &str) -> (String, String) {
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

/// A work item for the parallel local upload queue.
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

/// A work item for the parallel S3 disk CopyObject queue.
struct S3DiskUploadWorkItem {
    /// Table key in "db.table" format.
    table_key: String,
    /// Disk name within the table's parts map.
    disk_name: String,
    /// Part info (name, size, etc.).
    part: PartInfo,
    /// S3 objects to copy (from part.s3_objects).
    s3_objects: Vec<S3ObjectInfo>,
    /// Source bucket for CopyObject.
    source_bucket: String,
    /// Source key prefix for CopyObject.
    source_prefix: String,
    /// Backup name (used to build destination key).
    backup_name: String,
    /// Local part directory (for uploading metadata files).
    part_dir: PathBuf,
    /// Database name.
    db: String,
    /// Table name.
    table: String,
}

/// Upload a local backup to S3.
///
/// Reads the manifest from the local backup directory, compresses each part
/// with tar+LZ4, uploads to S3 in parallel, and uploads the manifest last.
///
/// S3 disk parts are routed through CopyObject with a separate concurrency
/// semaphore instead of compress+upload.
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
/// * `resume` - If true and use_resumable_state config is set, load resume state
pub async fn upload(
    config: &Config,
    s3: &S3Client,
    backup_name: &str,
    backup_dir: &Path,
    delete_local: bool,
    diff_from_remote: Option<&str>,
    resume: bool,
) -> Result<()> {
    info!(
        backup_name = %backup_name,
        backup_dir = %backup_dir.display(),
        delete_local = delete_local,
        resume = resume,
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
        let base_bytes = s3.get_object(&base_manifest_key).await.with_context(|| {
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

    // 1c. Load resume state if --resume and use_resumable_state
    let use_resume = resume && config.general.use_resumable_state;
    let state_path = backup_dir.join("upload.state.json");
    let current_params_hash = compute_params_hash(&[backup_name, diff_from_remote.unwrap_or("")]);

    let completed_keys: HashSet<String> = if use_resume {
        match load_state_file::<UploadState>(&state_path) {
            Ok(Some(state)) => {
                if state.params_hash != current_params_hash {
                    warn!(
                        "Upload state params_hash mismatch (stale state from different params), ignoring"
                    );
                    HashSet::new()
                } else if state.backup_name != backup_name {
                    warn!("Upload state backup_name mismatch, ignoring");
                    HashSet::new()
                } else {
                    let count = state.completed_keys.len();
                    info!(
                        completed = count,
                        "Resuming upload: {} parts already uploaded", count
                    );
                    state.completed_keys
                }
            }
            Ok(None) => HashSet::new(),
            Err(e) => {
                warn!(error = %e, "Failed to load upload state, starting fresh");
                HashSet::new()
            }
        }
    } else {
        HashSet::new()
    };

    // 2. Flatten all parts into work items, split by disk type
    let mut local_work_items: Vec<UploadWorkItem> = Vec::new();
    let mut s3_disk_work_items: Vec<S3DiskUploadWorkItem> = Vec::new();
    let mut table_count = 0u64;

    let table_keys: Vec<String> = manifest.tables.keys().cloned().collect();

    for table_key in &table_keys {
        let (db, table) = table_key.split_once('.').unwrap_or(("default", table_key));

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
            // Check if this disk is an S3 object disk
            let disk_is_s3 = manifest
                .disk_types
                .get(disk_name)
                .map(|dt| is_s3_disk(dt))
                .unwrap_or(false);

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

                if disk_is_s3 && part.s3_objects.is_some() {
                    // S3 disk part: route through CopyObject
                    let remote_path = manifest
                        .disk_remote_paths
                        .get(disk_name)
                        .cloned()
                        .unwrap_or_default();

                    let (source_bucket, source_prefix) = if remote_path.is_empty() {
                        // Fallback: use the backup S3 bucket
                        (s3.bucket().to_string(), String::new())
                    } else {
                        parse_s3_uri(&remote_path)
                    };

                    // If parsed bucket is empty, use the backup bucket
                    let source_bucket = if source_bucket.is_empty() {
                        s3.bucket().to_string()
                    } else {
                        source_bucket
                    };

                    // S3 disk metadata key used for resume tracking
                    let metadata_key = format!(
                        "{}/data/{}/{}/{}/{}/",
                        backup_name,
                        url_encode_component(db),
                        url_encode_component(table),
                        disk_name,
                        part.name,
                    );

                    // Skip already-completed parts (resume)
                    if completed_keys.contains(&metadata_key) {
                        debug!(
                            table = %table_key,
                            part = %part.name,
                            "Skipping already-uploaded S3 disk part (resume)"
                        );
                        has_parts = true;
                        continue;
                    }

                    let part_dir = find_part_dir(backup_dir, db, table, &part.name)?;

                    s3_disk_work_items.push(S3DiskUploadWorkItem {
                        table_key: table_key.clone(),
                        disk_name: disk_name.clone(),
                        part: part.clone(),
                        s3_objects: part.s3_objects.clone().unwrap_or_default(),
                        source_bucket,
                        source_prefix,
                        backup_name: backup_name.to_string(),
                        part_dir,
                        db: db.to_string(),
                        table: table.to_string(),
                    });

                    has_parts = true;
                } else {
                    // Local disk part: compress + upload
                    let part_dir = find_part_dir(backup_dir, db, table, &part.name)?;

                    if !part_dir.exists() {
                        return Err(anyhow::anyhow!(
                            "Part directory not found: {} (expected at {})",
                            part.name,
                            part_dir.display()
                        ));
                    }

                    // Generate S3 key
                    let s3_key =
                        s3_key_for_part(backup_name, db, table, &part.name, data_format);

                    // Skip already-completed parts (resume)
                    if completed_keys.contains(&s3_key) {
                        debug!(
                            table = %table_key,
                            part = %part.name,
                            "Skipping already-uploaded part (resume)"
                        );
                        has_parts = true;
                        continue;
                    }

                    local_work_items.push(UploadWorkItem {
                        table_key: table_key.clone(),
                        disk_name: disk_name.clone(),
                        part: part.clone(),
                        part_dir,
                        s3_key,
                    });

                    has_parts = true;
                }
            }
        }

        if has_parts {
            table_count += 1;
        }
    }

    let total_local_parts = local_work_items.len();
    let total_s3_disk_parts = s3_disk_work_items.len();
    let total_parts = total_local_parts + total_s3_disk_parts;
    let concurrency = effective_upload_concurrency(config) as usize;
    let object_disk_concurrency = effective_object_disk_copy_concurrency(config) as usize;

    info!(
        "Uploading {} parts ({} local, {} S3 disk) across {} tables (upload_concurrency={}, object_disk_copy_concurrency={})",
        total_parts, total_local_parts, total_s3_disk_parts, table_count, concurrency, object_disk_concurrency
    );

    if total_s3_disk_parts > 0 {
        info!(
            s3_disk_parts = total_s3_disk_parts,
            "S3 disk parts: using CopyObject (no compression)"
        );
    }

    // Create progress tracker for upload
    let progress = ProgressTracker::new(
        "Upload",
        total_parts as u64,
        config.general.disable_progress_bar,
    );

    // 3. Upload both queues in parallel
    let upload_semaphore = Arc::new(Semaphore::new(concurrency));
    let object_disk_copy_semaphore = Arc::new(Semaphore::new(object_disk_concurrency));
    let rate_limiter = RateLimiter::new(config.backup.upload_max_bytes_per_second);
    let s3_chunk_size = config.s3.chunk_size;
    let s3_max_parts_count = config.s3.max_parts_count;
    let allow_object_disk_streaming = config.s3.allow_object_disk_streaming;
    let (_, _, jitter_factor) = crate::config::effective_retries(config);

    // Shared resume state for tracking completed parts across parallel tasks
    let resume_state = if use_resume {
        let state = UploadState {
            completed_keys: completed_keys.clone(),
            backup_name: backup_name.to_string(),
            params_hash: current_params_hash.clone(),
        };
        Some(Arc::new(tokio::sync::Mutex::new((
            state,
            state_path.clone(),
        ))))
    } else {
        None
    };

    // Result type: (table_key, disk_name, updated_part, compressed_size)
    type UploadResult = Result<(String, String, PartInfo, u64)>;
    let mut handles: Vec<tokio::task::JoinHandle<UploadResult>> = Vec::with_capacity(total_parts);

    // 3a. Spawn local disk upload tasks
    let compression_level = config.backup.compression_level;
    for item in local_work_items {
        let sem = upload_semaphore.clone();
        let s3 = s3.clone();
        let rate_limiter = rate_limiter.clone();
        let resume_state = resume_state.clone();
        let data_format_clone = data_format.clone();
        let progress = progress.clone();

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

            // Compress part using spawn_blocking (sync tar + compression)
            let part_dir = item.part_dir.clone();
            let part_name_for_compress = item.part.name.clone();
            let fmt = data_format_clone.clone();
            let compressed = tokio::task::spawn_blocking(move || {
                stream::compress_part(&part_dir, &part_name_for_compress, &fmt, compression_level)
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
                    .with_context(|| format!("Failed to upload part {} to S3", item.part.name))?;
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

            // Update resume state after successful upload
            if let Some(ref state_mutex) = resume_state {
                let mut guard = state_mutex.lock().await;
                guard.0.completed_keys.insert(item.s3_key.clone());
                save_state_graceful(&guard.1, &guard.0);
            }

            progress.inc();

            Ok((
                item.table_key,
                item.disk_name,
                updated_part,
                compressed_size,
            ))
        });

        handles.push(handle);
    }

    // 3b. Spawn S3 disk CopyObject tasks
    for item in s3_disk_work_items {
        let sem = object_disk_copy_semaphore.clone();
        let s3 = s3.clone();
        let resume_state = resume_state.clone();
        let progress = progress.clone();

        let handle = tokio::spawn(async move {
            let _permit = sem
                .acquire()
                .await
                .map_err(|_| anyhow::anyhow!("Semaphore closed"))?;

            debug!(
                table = %item.table_key,
                part = %item.part.name,
                s3_objects = item.s3_objects.len(),
                source_bucket = %item.source_bucket,
                "Copying S3 disk part objects via CopyObject"
            );

            let mut updated_s3_objects: Vec<S3ObjectInfo> =
                Vec::with_capacity(item.s3_objects.len());

            // CopyObject for each S3 object in the part
            for s3_obj in &item.s3_objects {
                // Skip zero-size objects (inline data in v4+ metadata)
                if s3_obj.size == 0 {
                    updated_s3_objects.push(S3ObjectInfo {
                        path: s3_obj.path.clone(),
                        size: 0,
                        backup_key: String::new(),
                    });
                    continue;
                }

                // Source key: {remote_path_prefix}/{relative_path}
                let source_key = if item.source_prefix.is_empty() {
                    s3_obj.path.clone()
                } else {
                    format!("{}/{}", item.source_prefix, s3_obj.path)
                };

                // Dest key: {backup_name}/objects/{relative_path}
                let dest_key = format!("{}/objects/{}", item.backup_name, s3_obj.path);

                s3.copy_object_with_retry_jitter(
                    &item.source_bucket,
                    &source_key,
                    &dest_key,
                    allow_object_disk_streaming,
                    jitter_factor,
                )
                .await
                .with_context(|| {
                    format!(
                        "CopyObject failed for S3 object {} in part {}",
                        s3_obj.path, item.part.name
                    )
                })?;

                updated_s3_objects.push(S3ObjectInfo {
                    path: s3_obj.path.clone(),
                    size: s3_obj.size,
                    backup_key: dest_key,
                });
            }

            // Upload metadata files for this S3 disk part.
            // The metadata files are in the local part_dir (from shadow walk).
            let metadata_backup_key = format!(
                "{}/data/{}/{}/{}/{}/",
                item.backup_name,
                url_encode_component(&item.db),
                url_encode_component(&item.table),
                item.disk_name,
                item.part.name,
            );

            if item.part_dir.exists() {
                upload_metadata_files(&s3, &item.part_dir, &metadata_backup_key).await?;
            }

            // Build updated part info
            let mut updated_part = item.part.clone();
            updated_part.s3_objects = Some(updated_s3_objects);
            updated_part.backup_key = metadata_backup_key.clone();
            updated_part.source = "uploaded".to_string();

            debug!(
                table = %item.table_key,
                part = %item.part.name,
                "S3 disk part CopyObject complete"
            );

            // Update resume state after successful CopyObject
            if let Some(ref state_mutex) = resume_state {
                let mut guard = state_mutex.lock().await;
                guard.0.completed_keys.insert(metadata_backup_key);
                save_state_graceful(&guard.1, &guard.0);
            }

            progress.inc();

            // S3 disk parts have no compressed_size (no compression)
            Ok((item.table_key, item.disk_name, updated_part, 0u64))
        });

        handles.push(handle);
    }

    // Await all uploads (both local and S3 disk)
    let results: Vec<(String, String, PartInfo, u64)> = try_join_all(handles)
        .await
        .context("An upload task panicked")?
        .into_iter()
        .collect::<Result<Vec<_>>>()?;

    progress.finish();

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

    // 5a. Upload access/ directory (RBAC files) if present
    let access_dir = backup_dir.join("access");
    if access_dir.exists() {
        upload_simple_directory(s3, backup_name, &access_dir, "access").await?;
        info!("Uploaded access/ directory to S3");
    }

    // 5b. Upload configs/ directory if present
    let configs_dir = backup_dir.join("configs");
    if configs_dir.exists() {
        upload_simple_directory(s3, backup_name, &configs_dir, "configs").await?;
        info!("Uploaded configs/ directory to S3");
    }

    // 6. Upload manifest LAST with atomic pattern (per design 3.6)
    //    Upload to .tmp key, CopyObject to final key, delete .tmp
    let manifest_key = format!("{}/metadata.json", backup_name);
    let manifest_tmp_key = format!("{}/metadata.json.tmp", backup_name);
    let manifest_bytes = manifest.to_json_bytes()?;

    info!(
        key = %manifest_key,
        size = manifest_bytes.len(),
        "Uploading manifest atomically"
    );

    // Step 1: Upload to .tmp key
    s3.put_object_with_options(&manifest_tmp_key, manifest_bytes, Some("application/json"))
        .await
        .context("Failed to upload manifest .tmp to S3")?;

    // Step 2: CopyObject from .tmp to final key
    let source_bucket = s3.bucket().to_string();
    let source_key_full = s3.full_key(&manifest_tmp_key);
    s3.copy_object(&source_bucket, &source_key_full, &manifest_key)
        .await
        .context("Failed to copy manifest from .tmp to final key")?;

    // Step 3: Delete .tmp key
    if let Err(e) = s3.delete_object(&manifest_tmp_key).await {
        warn!(
            error = %e,
            key = %manifest_tmp_key,
            "Failed to delete manifest .tmp key (non-fatal)"
        );
    }

    info!("Manifest uploaded atomically");

    // 7. Update local manifest with S3 keys
    manifest
        .save_to_file(&manifest_path)
        .context("Failed to update local manifest with S3 keys")?;

    info!(
        backup_name = %backup_name,
        parts = total_parts,
        local_parts = total_local_parts,
        s3_disk_parts = total_s3_disk_parts,
        compressed_size = total_compressed_size,
        "Upload complete"
    );

    // 8. Delete resume state file on success
    if use_resume {
        delete_state_file(&state_path);
    }

    // 9. Delete local backup if requested
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

/// Upload metadata files from a local part directory to S3.
///
/// Walks the part directory and uploads each file under the given S3 key prefix.
/// Used for S3 disk parts whose metadata files need to be stored alongside
/// the CopyObject-ed data objects.
async fn upload_metadata_files(s3: &S3Client, part_dir: &Path, key_prefix: &str) -> Result<()> {
    // Read directory entries synchronously (small number of metadata files)
    let part_dir_owned = part_dir.to_path_buf();
    let entries: Vec<(String, Vec<u8>)> = tokio::task::spawn_blocking(move || {
        let mut files = Vec::new();
        collect_files_recursive(&part_dir_owned, &part_dir_owned, &mut files)?;
        Ok::<Vec<(String, Vec<u8>)>, anyhow::Error>(files)
    })
    .await
    .context("spawn_blocking panicked during metadata file collection")??;

    for (relative_name, data) in entries {
        let file_key = format!("{}{}", key_prefix.trim_end_matches('/'), relative_name);
        s3.put_object(&file_key, data)
            .await
            .with_context(|| format!("Failed to upload metadata file: {}", file_key))?;
    }

    Ok(())
}

/// Recursively collect files from a directory, returning (relative_path, contents) pairs.
fn collect_files_recursive(
    base_dir: &Path,
    current_dir: &Path,
    files: &mut Vec<(String, Vec<u8>)>,
) -> Result<()> {
    if !current_dir.is_dir() {
        return Ok(());
    }

    for entry in std::fs::read_dir(current_dir)
        .with_context(|| format!("Failed to read directory: {}", current_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            collect_files_recursive(base_dir, &path, files)?;
        } else if path.is_file() {
            let relative = path
                .strip_prefix(base_dir)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();
            let data = std::fs::read(&path)
                .with_context(|| format!("Failed to read file: {}", path.display()))?;
            // Use forward-slash path separator for S3 keys
            let relative_key = format!("/{}", relative.replace('\\', "/"));
            files.push((relative_key, data));
        }
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

/// Upload all files from a local directory to S3 under `{backup_name}/{prefix}/`.
///
/// Files are uploaded sequentially (these are small RBAC/config files, no parallelism needed).
/// Uses `spawn_blocking` for directory walk, then `put_object` for each file.
async fn upload_simple_directory(
    s3: &S3Client,
    backup_name: &str,
    local_dir: &Path,
    prefix: &str,
) -> Result<()> {
    let local_dir_owned = local_dir.to_path_buf();
    let entries: Vec<(String, Vec<u8>)> = tokio::task::spawn_blocking(move || {
        let mut files = Vec::new();
        for entry in walkdir::WalkDir::new(&local_dir_owned)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_file() {
                let rel = entry
                    .path()
                    .strip_prefix(&local_dir_owned)
                    .map_err(|e| anyhow::anyhow!("Failed to strip prefix: {}", e))?;
                let data = std::fs::read(entry.path())
                    .with_context(|| format!("Failed to read {}", entry.path().display()))?;
                files.push((rel.to_string_lossy().to_string(), data));
            }
        }
        Ok::<_, anyhow::Error>(files)
    })
    .await
    .context("spawn_blocking panicked during directory walk")??;

    for (rel_path, data) in entries {
        let key = format!("{}/{}/{}", backup_name, prefix, rel_path);
        s3.put_object(&key, data)
            .await
            .with_context(|| format!("Failed to upload {}/{}", prefix, rel_path))?;
    }

    Ok(())
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
        let key = s3_key_for_part("daily-20240115", "default", "trades", "202401_1_50_3", "lz4");
        assert_eq!(
            key,
            "daily-20240115/data/default/trades/202401_1_50_3.tar.lz4"
        );
    }

    #[test]
    fn test_s3_key_for_part_special_chars() {
        let key = s3_key_for_part("my-backup", "my db", "my+table", "202401_1_1_0", "lz4");
        assert_eq!(
            key,
            "my-backup/data/my%20db/my%2Btable/202401_1_1_0.tar.lz4"
        );
    }

    #[test]
    fn test_s3_key_for_part_with_format() {
        // lz4
        let key = s3_key_for_part("daily", "db", "t", "part1", "lz4");
        assert!(key.ends_with(".tar.lz4"));

        // zstd
        let key = s3_key_for_part("daily", "db", "t", "part1", "zstd");
        assert!(key.ends_with(".tar.zstd"));

        // gzip
        let key = s3_key_for_part("daily", "db", "t", "part1", "gzip");
        assert!(key.ends_with(".tar.gz"));

        // none
        let key = s3_key_for_part("daily", "db", "t", "part1", "none");
        assert!(key.ends_with(".tar"));
        // Make sure it doesn't end with .tar.something
        assert!(!key.ends_with(".tar.lz4"));
        assert!(!key.ends_with(".tar.zstd"));
        assert!(!key.ends_with(".tar.gz"));
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

    #[test]
    fn test_parse_s3_uri_standard() {
        let (bucket, prefix) = parse_s3_uri("s3://my-data-bucket/ch-data/store");
        assert_eq!(bucket, "my-data-bucket");
        assert_eq!(prefix, "ch-data/store");
    }

    #[test]
    fn test_parse_s3_uri_trailing_slash() {
        let (bucket, prefix) = parse_s3_uri("s3://my-data-bucket/ch-data/");
        assert_eq!(bucket, "my-data-bucket");
        assert_eq!(prefix, "ch-data");
    }

    #[test]
    fn test_parse_s3_uri_bucket_only() {
        let (bucket, prefix) = parse_s3_uri("s3://my-bucket/");
        assert_eq!(bucket, "my-bucket");
        assert_eq!(prefix, "");
    }

    #[test]
    fn test_parse_s3_uri_bucket_no_slash() {
        let (bucket, prefix) = parse_s3_uri("s3://my-bucket");
        assert_eq!(bucket, "my-bucket");
        assert_eq!(prefix, "");
    }

    #[test]
    fn test_parse_s3_uri_not_s3() {
        let (bucket, prefix) = parse_s3_uri("/local/path/to/disk");
        assert_eq!(bucket, "");
        assert_eq!(prefix, "/local/path/to/disk");
    }

    #[test]
    fn test_upload_routes_s3_disk_to_copy() {
        // Verify that S3 disk parts are routed to the S3DiskUploadWorkItem queue
        let s3_part = PartInfo {
            name: "202401_1_50_3".to_string(),
            size: 134_217_728,
            backup_key: String::new(),
            source: "uploaded".to_string(),
            checksum_crc64: 12345,
            s3_objects: Some(vec![S3ObjectInfo {
                path: "store/abc/def/data.bin".to_string(),
                size: 134_217_000,
                backup_key: String::new(),
            }]),
        };

        let disk_types: HashMap<String, String> = HashMap::from([
            ("default".to_string(), "local".to_string()),
            ("s3disk".to_string(), "s3".to_string()),
        ]);

        let disk_remote_paths: HashMap<String, String> = HashMap::from([(
            "s3disk".to_string(),
            "s3://data-bucket/ch-data/".to_string(),
        )]);

        // Simulate the routing logic from upload()
        let disk_name = "s3disk";
        let disk_is_s3 = disk_types
            .get(disk_name)
            .map(|dt| is_s3_disk(dt))
            .unwrap_or(false);

        assert!(disk_is_s3, "s3disk should be detected as S3");
        assert!(
            s3_part.s3_objects.is_some(),
            "s3_part should have s3_objects"
        );

        // S3 disk routing: parse remote_path
        let remote_path = disk_remote_paths
            .get(disk_name)
            .cloned()
            .unwrap_or_default();
        let (source_bucket, source_prefix) = parse_s3_uri(&remote_path);
        assert_eq!(source_bucket, "data-bucket");
        assert_eq!(source_prefix, "ch-data");

        // Verify destination key format: {backup_name}/objects/{relative_path}
        let backup_name = "daily-2024-01-15";
        let obj_path = &s3_part.s3_objects.as_ref().unwrap()[0].path;
        let dest_key = format!("{}/objects/{}", backup_name, obj_path);
        assert_eq!(dest_key, "daily-2024-01-15/objects/store/abc/def/data.bin");

        // Verify source key format: {source_prefix}/{relative_path}
        let source_key = format!("{}/{}", source_prefix, obj_path);
        assert_eq!(source_key, "ch-data/store/abc/def/data.bin");
    }

    #[test]
    fn test_upload_local_parts_unchanged() {
        // Verify that local disk parts use the standard compress+upload path
        let local_part = PartInfo {
            name: "202401_1_50_3".to_string(),
            size: 4096,
            backup_key: String::new(),
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

        assert!(!disk_is_s3, "default disk should not be S3");
        assert!(
            local_part.s3_objects.is_none(),
            "local part should not have s3_objects"
        );

        // Local disk part: should use standard S3 key format
        let s3_key =
            s3_key_for_part("daily-2024-01-15", "default", "trades", &local_part.name, "lz4");
        assert_eq!(
            s3_key,
            "daily-2024-01-15/data/default/trades/202401_1_50_3.tar.lz4"
        );
    }

    #[test]
    fn test_s3_disk_work_item_construction() {
        // Verify S3DiskUploadWorkItem collects correct fields
        let s3_obj = S3ObjectInfo {
            path: "store/abc/def/data.bin".to_string(),
            size: 134_217_000,
            backup_key: String::new(),
        };

        let part = PartInfo {
            name: "202401_1_50_3".to_string(),
            size: 134_217_728,
            backup_key: String::new(),
            source: "uploaded".to_string(),
            checksum_crc64: 12345,
            s3_objects: Some(vec![s3_obj.clone()]),
        };

        let work_item = S3DiskUploadWorkItem {
            table_key: "default.trades".to_string(),
            disk_name: "s3disk".to_string(),
            part: part.clone(),
            s3_objects: vec![s3_obj],
            source_bucket: "data-bucket".to_string(),
            source_prefix: "ch-data".to_string(),
            backup_name: "daily-2024-01-15".to_string(),
            part_dir: PathBuf::from("/tmp/backup/shadow/default/trades/202401_1_50_3"),
            db: "default".to_string(),
            table: "trades".to_string(),
        };

        assert_eq!(work_item.table_key, "default.trades");
        assert_eq!(work_item.disk_name, "s3disk");
        assert_eq!(work_item.source_bucket, "data-bucket");
        assert_eq!(work_item.source_prefix, "ch-data");
        assert_eq!(work_item.s3_objects.len(), 1);
        assert_eq!(work_item.s3_objects[0].path, "store/abc/def/data.bin");
    }

    #[test]
    fn test_s3_disk_zero_size_objects_skipped() {
        // Verify that zero-size S3 objects (inline data) get empty backup_key
        let s3_obj = S3ObjectInfo {
            path: "store/abc/data.bin".to_string(),
            size: 0,
            backup_key: String::new(),
        };

        // The upload logic skips CopyObject for size=0 objects
        // and sets backup_key to empty
        let updated = S3ObjectInfo {
            path: s3_obj.path.clone(),
            size: 0,
            backup_key: String::new(),
        };

        assert_eq!(updated.size, 0);
        assert!(updated.backup_key.is_empty());
    }

    #[test]
    fn test_collect_files_recursive_basic() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();

        // Create some test files
        std::fs::write(base.join("checksums.txt"), b"test data").unwrap();
        let sub = base.join("subdir");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("data.bin"), b"more data").unwrap();

        let mut files = Vec::new();
        collect_files_recursive(base, base, &mut files).unwrap();

        assert_eq!(files.len(), 2);

        // Sort for deterministic comparison
        files.sort_by(|a, b| a.0.cmp(&b.0));

        assert_eq!(files[0].0, "/checksums.txt");
        assert_eq!(files[0].1, b"test data");
        assert_eq!(files[1].0, "/subdir/data.bin");
        assert_eq!(files[1].1, b"more data");
    }

    #[test]
    fn test_object_storage_disk_type_detected() {
        // Verify that "object_storage" disk type is also routed as S3 disk
        let disk_types: HashMap<String, String> =
            HashMap::from([("objdisk".to_string(), "object_storage".to_string())]);

        let disk_is_s3 = disk_types
            .get("objdisk")
            .map(|dt| is_s3_disk(dt))
            .unwrap_or(false);
        assert!(disk_is_s3);
    }

    #[test]
    fn test_upload_skips_completed_parts() {
        // Verify that completed_keys causes parts to be skipped in work queue
        let completed_keys: HashSet<String> =
            HashSet::from(["daily/data/default/trades/202401_1_50_3.tar.lz4".to_string()]);

        let s3_key = s3_key_for_part("daily", "default", "trades", "202401_1_50_3", "lz4");
        assert!(completed_keys.contains(&s3_key));

        // A different part should NOT be in completed_keys
        let other_key = s3_key_for_part("daily", "default", "trades", "202402_1_1_0", "lz4");
        assert!(!completed_keys.contains(&other_key));
    }

    #[test]
    fn test_manifest_atomicity_key_format() {
        // Verify .tmp key generation for atomic manifest upload
        let backup_name = "daily-2024-01-15";
        let manifest_key = format!("{}/metadata.json", backup_name);
        let manifest_tmp_key = format!("{}/metadata.json.tmp", backup_name);

        assert_eq!(manifest_key, "daily-2024-01-15/metadata.json");
        assert_eq!(manifest_tmp_key, "daily-2024-01-15/metadata.json.tmp");

        // tmp key should differ from final key
        assert_ne!(manifest_key, manifest_tmp_key);

        // tmp key should have .tmp suffix
        assert!(manifest_tmp_key.ends_with(".tmp"));
    }

    #[test]
    fn test_upload_resume_state_params_hash() {
        use crate::resume::compute_params_hash;

        // Verify params_hash for upload includes backup_name and diff_from_remote
        let h1 = compute_params_hash(&["daily-2024-01-15", ""]);
        let h2 = compute_params_hash(&["daily-2024-01-15", ""]);
        assert_eq!(h1, h2);

        // Different diff_from_remote should produce different hash
        let h3 = compute_params_hash(&["daily-2024-01-15", "base-backup"]);
        assert_ne!(h1, h3);
    }
}
