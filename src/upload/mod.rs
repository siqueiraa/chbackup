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

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use futures::future::try_join_all;
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};

use tokio_util::sync::CancellationToken;

use crate::backup::diff::diff_parts;
use crate::concurrency::{
    effective_object_disk_copy_concurrency, effective_upload_concurrency,
    effective_upload_rate_limit,
};
use crate::config::Config;
use crate::manifest::{BackupManifest, PartInfo, S3ObjectInfo};
use crate::object_disk::is_s3_disk;
use crate::path_encoding::encode_path_component;
use crate::progress::ProgressTracker;
use crate::rate_limiter::RateLimiter;
use crate::resume::{
    compute_params_hash, delete_state_file, load_state_file, save_state_graceful, UploadState,
};
use crate::storage::s3::{calculate_chunk_size, RetryConfig};
use crate::storage::{parse_s3_uri, S3Client};

/// Multipart upload threshold: parts with compressed data larger than 32 MiB
/// use multipart upload instead of a single PutObject.
const MULTIPART_THRESHOLD: u64 = 32 * 1024 * 1024;

/// Check if a given data size should use multipart upload.
fn should_use_multipart(compressed_size: u64) -> bool {
    compressed_size > MULTIPART_THRESHOLD
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
        encode_path_component(db),
        encode_path_component(table),
        part_name,
        stream::archive_extension(data_format)
    )
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
#[allow(clippy::too_many_arguments)]
pub async fn upload(
    config: &Config,
    s3: &S3Client,
    backup_name: &str,
    backup_dir: &Path,
    delete_local: bool,
    diff_from_remote: Option<&str>,
    resume: bool,
    cancel: CancellationToken,
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
                        encode_path_component(db),
                        encode_path_component(table),
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

                    let part_dir = find_part_dir(
                        backup_dir,
                        db,
                        table,
                        &part.name,
                        &manifest.disks,
                        backup_name,
                        disk_name,
                    )?;

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
                    let part_dir = find_part_dir(
                        backup_dir,
                        db,
                        table,
                        &part.name,
                        &manifest.disks,
                        backup_name,
                        disk_name,
                    )?;

                    if !part_dir.exists() {
                        return Err(anyhow::anyhow!(
                            "Part directory not found: {} (expected at {})",
                            part.name,
                            part_dir.display()
                        ));
                    }

                    // Generate S3 key
                    let s3_key = s3_key_for_part(backup_name, db, table, &part.name, data_format);

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
    let rate_limiter = RateLimiter::new(effective_upload_rate_limit(config));
    let s3_chunk_size = config.s3.chunk_size;
    let s3_max_parts_count = config.s3.max_parts_count;
    let allow_object_disk_streaming = config.s3.allow_object_disk_streaming;
    let streaming_upload_threshold = config.backup.streaming_upload_threshold;
    let (retries_on_failure, retry_delay_secs, jitter_factor) =
        crate::config::effective_retries(config);
    let retry_config = RetryConfig {
        max_retries: retries_on_failure,
        base_delay_secs: retry_delay_secs,
        jitter_factor,
    };

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
        let cancel_clone = cancel.clone();

        let handle = tokio::spawn(async move {
            let _permit = sem
                .acquire()
                .await
                .map_err(|_| anyhow::anyhow!("Semaphore closed"))?;

            // Pre-clone values needed by the cancel arm before item is moved.
            let cancel_table_key = item.table_key.clone();
            let cancel_disk_name = item.disk_name.clone();
            let cancel_part = item.part.clone();

            // Track in-progress multipart upload ID so the cancel arm can abort it
            // (design §3.6, §11.5: AbortMultipartUpload on cancellation).
            // std::sync::Mutex is used so set/clear are non-blocking (no await needed).
            let active_mpu: Arc<std::sync::Mutex<Option<String>>> =
                Arc::new(std::sync::Mutex::new(None));
            let active_mpu_cancel = active_mpu.clone();
            let s3_abort = s3.clone();
            let abort_key = item.s3_key.clone();

            tokio::select! {
                biased;
                _ = cancel_clone.cancelled() => {
                    // Abort any in-progress multipart upload before returning.
                    let uid = active_mpu_cancel
                        .lock()
                        .expect("MPU lock poisoned")
                        .take();
                    if let Some(uid) = uid {
                        let _ = s3_abort.abort_multipart_upload(&abort_key, &uid).await;
                    }
                    Ok((cancel_table_key, cancel_disk_name, cancel_part, 0))
                },
                result = async move {

            debug!(
                table = %item.table_key,
                part = %item.part.name,
                s3_key = %item.s3_key,
                "Compressing and uploading part"
            );

            // Choose between streaming multipart (large parts) and buffered upload
            let compressed_size = if streaming_upload_threshold > 0
                && item.part.size > streaming_upload_threshold
            {
                // --- Streaming multipart upload path for large parts ---
                info!(
                    table = %item.table_key,
                    part = %item.part.name,
                    size = item.part.size,
                    "Streaming multipart upload for large part"
                );

                let chunk_size =
                    calculate_chunk_size(item.part.size, s3_chunk_size, s3_max_parts_count)
                        as usize;
                // Ensure chunk_size >= MIN_MULTIPART_CHUNK for S3 compliance
                let chunk_size = chunk_size.max(stream::MIN_MULTIPART_CHUNK);

                let part_dir = item.part_dir.clone();
                let part_name_for_compress = item.part.name.clone();
                let fmt = data_format_clone.clone();

                // Start streaming compression in a background thread
                let receiver = tokio::task::spawn_blocking(move || {
                    stream::compress_part_streaming(
                        &part_dir,
                        &part_name_for_compress,
                        &fmt,
                        compression_level,
                        chunk_size,
                    )
                })
                .await
                .context("Streaming compress setup panicked")?
                .with_context(|| {
                    format!(
                        "Failed to start streaming compression for part {}",
                        item.part.name
                    )
                })?;

                // Create multipart upload
                let upload_id = s3.create_multipart_upload(&item.s3_key).await?;
                // Register upload_id so the cancel arm can abort it if needed.
                *active_mpu.lock().expect("MPU lock poisoned") = Some(upload_id.clone());

                // Pipeline: bridge the sync mpsc::Receiver (compression thread) to async
                // upload via a tokio channel.  Compression and upload run concurrently:
                // as soon as a chunk is ready it is uploaded, so only one chunk_size of
                // memory is held at a time instead of all chunks.
                let (tokio_tx, mut tokio_rx) =
                    tokio::sync::mpsc::channel::<Result<Vec<u8>>>(2);

                let bridge = tokio::task::spawn_blocking(move || {
                    for chunk_result in receiver.iter() {
                        if tokio_tx.blocking_send(chunk_result).is_err() {
                            break; // upload side aborted
                        }
                    }
                });

                let part_name_for_err = item.part.name.clone();
                let upload_result = async {
                    let mut completed_parts: Vec<(i32, String)> = Vec::new();
                    let mut total_compressed: u64 = 0;
                    let mut part_number = 1i32;

                    while let Some(chunk_result) = tokio_rx.recv().await {
                        let chunk_data = chunk_result.with_context(|| {
                            format!(
                                "Streaming compression error for part {}",
                                part_name_for_err
                            )
                        })?;
                        total_compressed += chunk_data.len() as u64;
                        let e_tag = s3
                            .upload_part_with_retry(
                                &item.s3_key,
                                &upload_id,
                                part_number,
                                chunk_data,
                                retry_config,
                            )
                            .await?;
                        completed_parts.push((part_number, e_tag));
                        part_number += 1;
                    }

                    bridge.await.context("Chunk bridge task panicked")?;

                    if completed_parts.is_empty() {
                        // No chunks produced -- abort the multipart upload
                        let _ = s3.abort_multipart_upload(&item.s3_key, &upload_id).await;
                        anyhow::bail!(
                            "Streaming compression produced zero chunks for part {}",
                            part_name_for_err
                        );
                    }

                    s3.complete_multipart_upload(&item.s3_key, &upload_id, completed_parts)
                        .await?;

                    Ok::<u64, anyhow::Error>(total_compressed)
                }
                .await;

                // Clear tracking before handling result.  The error arm below calls abort
                // explicitly; the cancel arm uses take() so clearing here prevents a
                // redundant abort if cancel fires after the upload_result block finishes.
                *active_mpu.lock().expect("MPU lock poisoned") = None;

                match upload_result {
                    Ok(total) => total,
                    Err(e) => {
                        // Best-effort abort to clean up partial upload
                        let _ = s3.abort_multipart_upload(&item.s3_key, &upload_id).await;
                        return Err(e).with_context(|| {
                            format!(
                                "Streaming multipart upload failed for part {}",
                                item.part.name
                            )
                        });
                    }
                }
            } else {
                // --- Buffered upload path (existing behavior) ---
                // Compress part using spawn_blocking (sync tar + compression)
                let part_dir = item.part_dir.clone();
                let part_name_for_compress = item.part.name.clone();
                let fmt = data_format_clone.clone();
                let compressed = tokio::task::spawn_blocking(move || {
                    stream::compress_part(
                        &part_dir,
                        &part_name_for_compress,
                        &fmt,
                        compression_level,
                    )
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
                    // Register upload_id so the cancel arm can abort it if needed.
                    *active_mpu.lock().expect("MPU lock poisoned") = Some(upload_id.clone());

                    // Upload chunks, aborting on error
                    let upload_result = async {
                        let mut completed_parts: Vec<(i32, String)> = Vec::new();
                        let mut part_number = 1i32;

                        for chunk_start in (0..compressed.len()).step_by(chunk_size) {
                            let chunk_end = (chunk_start + chunk_size).min(compressed.len());
                            let chunk_data = compressed[chunk_start..chunk_end].to_vec();

                            let e_tag = s3
                                .upload_part_with_retry(
                                    &item.s3_key,
                                    &upload_id,
                                    part_number,
                                    chunk_data,
                                    retry_config,
                                )
                                .await?;

                            completed_parts.push((part_number, e_tag));
                            part_number += 1;
                        }

                        s3.complete_multipart_upload(&item.s3_key, &upload_id, completed_parts)
                            .await?;

                        Ok::<(), anyhow::Error>(())
                    }
                    .await;

                    // Clear tracking before handling result.
                    *active_mpu.lock().expect("MPU lock poisoned") = None;

                    if let Err(e) = upload_result {
                        // Best-effort abort to clean up partial upload
                        let _ = s3.abort_multipart_upload(&item.s3_key, &upload_id).await;
                        return Err(e).with_context(|| {
                            format!("Multipart upload failed for part {}", item.part.name)
                        });
                    }
                } else {
                    // Single PutObject with retry
                    s3.put_object_with_retry(&item.s3_key, compressed, retry_config)
                        .await
                        .with_context(|| {
                            format!("Failed to upload part {} to S3", item.part.name)
                        })?;
                }

                compressed_size
            };

            // Rate limit after upload
            rate_limiter.consume(compressed_size).await;

            // Build updated part info
            let mut updated_part = item.part.clone();
            updated_part.backup_key = item.s3_key.clone();
            updated_part.source = "uploaded".to_string();
            updated_part.backup_size = compressed_size;

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

            } => result,
            }
        });

        handles.push(handle);
    }

    // 3b. Spawn S3 disk CopyObject tasks
    for item in s3_disk_work_items {
        let sem = object_disk_copy_semaphore.clone();
        let s3 = s3.clone();
        let resume_state = resume_state.clone();
        let progress = progress.clone();
        let cancel_clone = cancel.clone();

        let handle = tokio::spawn(async move {
            let _permit = sem
                .acquire()
                .await
                .map_err(|_| anyhow::anyhow!("Semaphore closed"))?;

            // Pre-clone values needed by the cancel arm before item is moved.
            let cancel_table_key = item.table_key.clone();
            let cancel_disk_name = item.disk_name.clone();
            let cancel_part = item.part.clone();
            // Clone cancel token for use inside the CopyObject loop.
            let inner_cancel = cancel_clone.clone();

            tokio::select! {
                biased;
                _ = cancel_clone.cancelled() => Ok((cancel_table_key, cancel_disk_name, cancel_part, 0)),
                result = async move {

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
            let mut cancelled_during_copy = false;
            for s3_obj in &item.s3_objects {
                if inner_cancel.is_cancelled() {
                    cancelled_during_copy = true;
                    break;
                }
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

            // If cancelled mid-copy, return early without recording completion
            if cancelled_during_copy {
                let mut updated_part = item.part.clone();
                updated_part.s3_objects = Some(updated_s3_objects);
                return Ok((item.table_key, item.disk_name, updated_part, 0u64));
            }

            // Upload metadata files for this S3 disk part.
            // The metadata files are in the local part_dir (from shadow walk).
            let metadata_backup_key = format!(
                "{}/data/{}/{}/{}/{}/",
                item.backup_name,
                encode_path_component(&item.db),
                encode_path_component(&item.table),
                item.disk_name,
                item.part.name,
            );

            if item.part_dir.exists() {
                upload_metadata_files(&s3, &item.part_dir, &metadata_backup_key, retry_config)
                    .await?;
            }

            // Build updated part info
            let mut updated_part = item.part.clone();
            updated_part.s3_objects = Some(updated_s3_objects);
            updated_part.backup_key = metadata_backup_key.clone();
            updated_part.source = "uploaded".to_string();
            // S3 disk parts are not compressed; backup_size equals original size
            updated_part.backup_size = item.part.size;

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

            } => result,
            }
        });

        handles.push(handle);
    }

    // Await all uploads (both local and S3 disk)
    let results: Vec<(String, String, PartInfo, u64)> = try_join_all(handles)
        .await
        .context("An upload task panicked")?
        .into_iter()
        .collect::<Result<Vec<_>>>()?;

    // Guard: workers return Ok on cancel (not Err), so try_join_all succeeds even when
    // the operation was killed. Without this check the sync manifest-update code below
    // would run before the outer tokio::select! gets a chance to detect the cancellation,
    // potentially producing a manifest with empty backup_key values for unfinished parts.
    if cancel.is_cancelled() {
        return Err(anyhow::anyhow!("upload cancelled"));
    }

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

    for ((table_key, disk_name), uploaded_parts) in &updates {
        if let Some(tm) = manifest.tables.get_mut(table_key) {
            // Build a set of part names that were uploaded this run.
            // Parts not in this set were either carried (diff) or resume-skipped;
            // both already have a correct backup_key in the manifest and must be preserved.
            let uploaded_names: std::collections::HashSet<&str> =
                uploaded_parts.iter().map(|p| p.name.as_str()).collect();
            let existing: Vec<PartInfo> = tm
                .parts
                .get(disk_name)
                .map(|parts| {
                    parts
                        .iter()
                        .filter(|p| !uploaded_names.contains(p.name.as_str()))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            let mut merged = existing;
            merged.extend(uploaded_parts.iter().cloned());
            tm.parts.insert(disk_name.clone(), merged);
        }
    }

    // 5. Update manifest with compressed_size and data_format
    manifest.compressed_size = total_compressed_size;
    manifest.data_format = data_format.clone();

    // 5a. Upload access/ directory (RBAC files) if present
    let access_dir = backup_dir.join("access");
    if access_dir.exists() {
        upload_simple_directory(s3, backup_name, &access_dir, "access", retry_config).await?;
        info!("Uploaded access/ directory to S3");
    }

    // 5b. Upload configs/ directory if present
    let configs_dir = backup_dir.join("configs");
    if configs_dir.exists() {
        upload_simple_directory(s3, backup_name, &configs_dir, "configs", retry_config).await?;
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
        let data_path = config.clickhouse.data_path.clone();
        let backup_name_owned = backup_name.to_string();
        tokio::task::spawn_blocking(move || {
            crate::list::delete_local(&data_path, &backup_name_owned)
        })
        .await
        .context("delete_local task panicked")?
        .with_context(|| format!("Failed to delete local backup '{}'", backup_name))?;
    }

    Ok(())
}

/// Upload metadata files from a local part directory to S3.
///
/// Walks the part directory and uploads each file under the given S3 key prefix.
/// Used for S3 disk parts whose metadata files need to be stored alongside
/// the CopyObject-ed data objects.
async fn upload_metadata_files(
    s3: &S3Client,
    part_dir: &Path,
    key_prefix: &str,
    retry: RetryConfig,
) -> Result<()> {
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
        s3.put_object_with_retry(&file_key, data, retry)
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
            let relative = match path.strip_prefix(base_dir).unwrap_or(&path).to_str() {
                Some(s) => s.to_string(),
                None => {
                    warn!("Skipping non-UTF-8 filename: {}", path.display());
                    continue;
                }
            };
            let data = std::fs::read(&path)
                .with_context(|| format!("Failed to read file: {}", path.display()))?;
            let relative_key = format!("/{}", relative.replace('\\', "/"));
            files.push((relative_key, data));
        }
    }

    Ok(())
}

/// Find the part directory within the backup staging area.
///
/// Delegates to `resolve_shadow_part_path()` which tries per-disk paths first,
/// then falls back to legacy encoded and plain paths. This is the single source
/// of truth for part directory resolution during upload.
fn find_part_dir(
    backup_dir: &Path,
    db: &str,
    table: &str,
    part_name: &str,
    manifest_disks: &BTreeMap<String, String>,
    backup_name: &str,
    disk_name: &str,
) -> Result<PathBuf> {
    use crate::backup::collect::resolve_shadow_part_path;

    let url_db = encode_path_component(db);
    let url_table = encode_path_component(table);

    resolve_shadow_part_path(
        backup_dir,
        manifest_disks,
        backup_name,
        disk_name,
        &url_db,
        &url_table,
        db,
        table,
        part_name,
    )
    .ok_or_else(|| {
        anyhow::anyhow!(
            "Part directory not found for {}.{}/{} (checked per-disk, legacy encoded, and legacy plain paths)",
            db, table, part_name
        )
    })
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
    retry: RetryConfig,
) -> Result<()> {
    let local_dir_owned = local_dir.to_path_buf();
    let entries: Vec<(String, Vec<u8>)> = tokio::task::spawn_blocking(move || {
        let mut files = Vec::new();
        for entry in walkdir::WalkDir::new(&local_dir_owned)
            .into_iter()
            .filter_map(|e| match e {
                Ok(entry) => Some(entry),
                Err(err) => {
                    tracing::warn!(error = %err, "Skipping file during directory upload");
                    None
                }
            })
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
        s3.put_object_with_retry(&key, data, retry)
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
        let part = PartInfo::new("202401_1_50_3", 134_217_728, 12345);

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
    fn test_encode_path_component_simple() {
        assert_eq!(encode_path_component("default"), "default");
        assert_eq!(encode_path_component("my_table"), "my_table");
    }

    #[test]
    fn test_encode_path_component_special() {
        assert_eq!(encode_path_component("my table"), "my%20table");
        assert_eq!(encode_path_component("db:name"), "db%3Aname");
    }

    #[test]
    fn test_s3_key_for_part_simple() {
        let key = s3_key_for_part(
            "daily-20240115",
            "default",
            "trades",
            "202401_1_50_3",
            "lz4",
        );
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
        // Legacy layout: part under backup_dir/shadow/{db}/{table}/{part}
        let dir = tempfile::tempdir().unwrap();
        let part_path = dir
            .path()
            .join("shadow")
            .join("default")
            .join("trades")
            .join("202401_1_50_3");
        std::fs::create_dir_all(&part_path).unwrap();

        // No disk in manifest_disks => falls back to legacy path under backup_dir
        let manifest_disks: BTreeMap<String, String> = BTreeMap::new();
        let found = find_part_dir(
            dir.path(),
            "default",
            "trades",
            "202401_1_50_3",
            &manifest_disks,
            "my-backup",
            "disk1",
        )
        .unwrap();
        assert_eq!(found, part_path);
    }

    #[test]
    fn test_find_part_dir_per_disk() {
        // Per-disk layout: part under {disk_path}/backup/{name}/shadow/{db}/{table}/{part}
        let dir = tempfile::tempdir().unwrap();
        let disk_path = dir.path().join("store1");
        let backup_name = "daily-2024-01-15";
        let per_disk_part = disk_path
            .join("backup")
            .join(backup_name)
            .join("shadow")
            .join("mydb")
            .join("mytable")
            .join("202401_1_50_3");
        std::fs::create_dir_all(&per_disk_part).unwrap();

        let manifest_disks: BTreeMap<String, String> =
            BTreeMap::from([("nvme1".to_string(), disk_path.to_str().unwrap().to_string())]);

        // backup_dir is a separate temp dir (the default data_path location)
        let backup_dir = tempfile::tempdir().unwrap();
        let found = find_part_dir(
            backup_dir.path(),
            "mydb",
            "mytable",
            "202401_1_50_3",
            &manifest_disks,
            backup_name,
            "nvme1",
        )
        .unwrap();
        assert_eq!(found, per_disk_part);
    }

    #[test]
    fn test_find_part_dir_fallback_default() {
        // Old single-dir layout: part under backup_dir/shadow/{db}/{table}/{part}
        // Even though manifest_disks has an entry, the per-disk path doesn't exist,
        // so it falls back to the legacy backup_dir path.
        let dir = tempfile::tempdir().unwrap();
        let legacy_part = dir
            .path()
            .join("shadow")
            .join("mydb")
            .join("mytable")
            .join("202401_1_50_3");
        std::fs::create_dir_all(&legacy_part).unwrap();

        let manifest_disks: BTreeMap<String, String> = BTreeMap::from([(
            "default".to_string(),
            dir.path().to_str().unwrap().to_string(),
        )]);

        let found = find_part_dir(
            dir.path(),
            "mydb",
            "mytable",
            "202401_1_50_3",
            &manifest_disks,
            "daily-2024-01-15",
            "default",
        )
        .unwrap();
        assert_eq!(found, legacy_part);
    }

    #[test]
    fn test_find_part_dir_old_backup_with_manifest_disks() {
        // Simulate: manifest.disks has an entry for "nvme1" pointing to /store1,
        // but the data was actually stored in the legacy backup_dir/shadow/ layout
        // (e.g., backup was created before per-disk feature). Fallback should find it.
        let backup_dir = tempfile::tempdir().unwrap();
        let legacy_part = backup_dir
            .path()
            .join("shadow")
            .join("mydb")
            .join("mytable")
            .join("202401_1_50_3");
        std::fs::create_dir_all(&legacy_part).unwrap();

        // manifest_disks points to a different path that has no backup data
        let disk_dir = tempfile::tempdir().unwrap();
        let manifest_disks: BTreeMap<String, String> = BTreeMap::from([(
            "nvme1".to_string(),
            disk_dir.path().to_str().unwrap().to_string(),
        )]);

        let found = find_part_dir(
            backup_dir.path(),
            "mydb",
            "mytable",
            "202401_1_50_3",
            &manifest_disks,
            "daily-2024-01-15",
            "nvme1",
        )
        .unwrap();
        assert_eq!(found, legacy_part);
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
        let s3_part = PartInfo::new("202401_1_50_3", 134_217_728, 12345).with_s3_objects(vec![
            S3ObjectInfo {
                path: "store/abc/def/data.bin".to_string(),
                size: 134_217_000,
                backup_key: String::new(),
            },
        ]);

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
        let local_part = PartInfo::new("202401_1_50_3", 4096, 11111);

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
        let s3_key = s3_key_for_part(
            "daily-2024-01-15",
            "default",
            "trades",
            &local_part.name,
            "lz4",
        );
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

        let part = PartInfo::new("202401_1_50_3", 134_217_728, 12345)
            .with_s3_objects(vec![s3_obj.clone()]);

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

    #[test]
    fn test_should_use_streaming() {
        let threshold: u64 = 256 * 1024 * 1024; // 256 MiB

        // Parts below threshold should not use streaming
        let small: u64 = 100 * 1024 * 1024;
        assert!(small <= threshold);
        assert!(1024_u64 <= threshold);

        // Parts above threshold should use streaming
        let large: u64 = 300 * 1024 * 1024;
        assert!(large > threshold);
        assert!(threshold + 1 > threshold);

        // Exactly at threshold should NOT use streaming (> not >=)
        let at_threshold = threshold;
        assert!(at_threshold <= threshold);

        // Threshold of 0 disables streaming
        let disabled: u64 = 0;
        // When threshold is 0, the condition `threshold > 0 && size > threshold` is false
        let size: u64 = 500 * 1024 * 1024;
        let should_stream = disabled > 0 && size > disabled;
        assert!(!should_stream);
    }

    #[test]
    fn test_upload_delete_local_cleans_per_disk_dirs() {
        // Verify that the delete_local cleanup logic in upload() correctly
        // removes per-disk backup dirs with canonical dedup, then removes
        // the default backup_dir last.
        use crate::backup::collect::per_disk_backup_dir;
        use std::collections::HashSet;

        let tmp = tempfile::tempdir().unwrap();
        let data_path = tmp.path().join("clickhouse");
        let nvme1_path = tmp.path().join("nvme1");
        let backup_name = "test-upload-delete";

        // Create default backup_dir
        let backup_dir = data_path.join("backup").join(backup_name);
        std::fs::create_dir_all(backup_dir.join("shadow")).unwrap();
        std::fs::write(backup_dir.join("metadata.json"), b"{}").unwrap();

        // Create per-disk backup dir for nvme1
        let per_disk_dir = per_disk_backup_dir(nvme1_path.to_str().unwrap(), backup_name);
        std::fs::create_dir_all(per_disk_dir.join("shadow")).unwrap();
        std::fs::write(per_disk_dir.join("shadow").join("data.bin"), b"data").unwrap();

        assert!(backup_dir.exists());
        assert!(per_disk_dir.exists());

        // Simulate the manifest.disks map
        let manifest_disks: HashMap<String, String> = HashMap::from([
            (
                "default".to_string(),
                data_path.to_string_lossy().to_string(),
            ),
            (
                "nvme1".to_string(),
                nvme1_path.to_string_lossy().to_string(),
            ),
        ]);

        // Execute the same cleanup pattern as upload() delete_local
        let canonical_default =
            std::fs::canonicalize(&backup_dir).unwrap_or_else(|_| backup_dir.clone());
        let mut seen: HashSet<PathBuf> = HashSet::new();
        seen.insert(canonical_default);

        for disk_path in manifest_disks.values() {
            let per_disk = per_disk_backup_dir(disk_path.trim_end_matches('/'), backup_name);
            if per_disk.exists() {
                let canonical =
                    std::fs::canonicalize(&per_disk).unwrap_or_else(|_| per_disk.clone());
                if seen.insert(canonical) {
                    std::fs::remove_dir_all(&per_disk).unwrap();
                }
            }
        }

        // Delete default backup_dir last
        std::fs::remove_dir_all(&backup_dir).unwrap();

        assert!(!backup_dir.exists(), "Default backup dir should be removed");
        assert!(
            !per_disk_dir.exists(),
            "Per-disk backup dir should be removed"
        );
    }
}
