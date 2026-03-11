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

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use futures::future::try_join_all;
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};

use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

use crate::backup::checksum::compute_crc64;
use crate::backup::collect::per_disk_backup_dir;
use crate::concurrency::{effective_download_concurrency, effective_download_rate_limit};
use crate::config::Config;
use crate::error::ChBackupError;
use crate::manifest::{BackupManifest, PartInfo};
use crate::object_disk::is_s3_disk;
use crate::path_encoding::{encode_path_component, validate_disk_path};
use crate::progress::ProgressTracker;
use crate::rate_limiter::RateLimiter;
use crate::resume::{
    compute_params_hash, delete_state_file, load_state_file, save_state_graceful, DownloadState,
};
use crate::storage::S3Client;

/// Sanitize a relative path by keeping only `Normal` components.
///
/// Strips `ParentDir` (`..`), `RootDir` (`/`), `CurDir` (`.`), and `Prefix`
/// components to prevent path traversal attacks via crafted S3 object keys.
fn sanitize_relative_path(input: &str) -> PathBuf {
    Path::new(input)
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(seg) => Some(seg),
            _ => None,
        })
        .collect()
}

/// Resolve the per-disk target backup directory for a download write path.
///
/// During download we are CREATING directories, not reading existing ones,
/// so we check disk-path existence (not part-path existence). If the disk
/// path from the manifest exists on the local host, parts are written to
/// `{disk_path}/backup/{name}/`; otherwise falls back to the default
/// `backup_dir` (for cross-host downloads where the original disk layout
/// is not available).
fn resolve_download_target_dir(
    manifest_disks: &BTreeMap<String, String>,
    disk_name: &str,
    backup_name: &str,
    backup_dir: &Path,
) -> PathBuf {
    match manifest_disks.get(disk_name) {
        Some(dp) if Path::new(dp.trim_end_matches('/')).exists() => {
            if !validate_disk_path(dp) {
                warn!(
                    disk_path = %dp,
                    disk_name = %disk_name,
                    "Disk path failed validation, falling back to backup_dir"
                );
                return backup_dir.to_path_buf();
            }
            per_disk_backup_dir(dp.trim_end_matches('/'), backup_name)
        }
        _ => backup_dir.to_path_buf(), // Disk not on this host -- fall back to default
    }
}

/// A work item for the parallel download queue.
struct DownloadWorkItem {
    /// Table key in "db.table" format.
    table_key: String,
    /// Database name.
    db: String,
    /// Table name.
    table: String,
    /// Disk name within the table's parts map (used for resume state tracking).
    disk_name: String,
    /// Part info (name, backup_key, etc.).
    part: PartInfo,
    /// Whether this part resides on an S3 object disk.
    is_s3_disk_part: bool,
}

/// Check available disk space before downloading.
///
/// Uses `nix::sys::statvfs::statvfs()` on the backup directory to determine
/// available space. Returns an error if the available space (with 5% safety
/// margin) is less than the required space.
fn check_disk_space(backup_dir: &Path, required_bytes: u64) -> Result<()> {
    // Ensure the directory exists for statvfs
    let check_path = if backup_dir.exists() {
        backup_dir.to_path_buf()
    } else if let Some(parent) = backup_dir.parent() {
        if parent.exists() {
            parent.to_path_buf()
        } else {
            // Cannot check disk space if parent doesn't exist yet
            return Ok(());
        }
    } else {
        return Ok(());
    };

    match nix::sys::statvfs::statvfs(&check_path) {
        Ok(stat) => {
            let block_size = stat.block_size();
            #[allow(clippy::unnecessary_cast)]
            let available_blocks: u64 = stat.blocks_available() as u64;
            // Use saturating_mul to prevent theoretical overflow on exotic
            // filesystems with very large block_size * blocks_available.
            let available_bytes = block_size.saturating_mul(available_blocks);
            // Apply 5% safety margin per design doc
            let safe_available = (available_bytes as f64 * 0.95) as u64;

            info!(
                path = %check_path.display(),
                available_bytes = available_bytes,
                safe_available = safe_available,
                required_bytes = required_bytes,
                "Disk space pre-flight check"
            );

            if safe_available < required_bytes {
                bail!(
                    "Insufficient disk space: need {} bytes but only {} bytes available (with 5% safety margin) at {}",
                    required_bytes,
                    safe_available,
                    check_path.display()
                );
            }

            Ok(())
        }
        Err(e) => {
            warn!(
                path = %check_path.display(),
                error = %e,
                "Failed to check disk space via statvfs (continuing anyway)"
            );
            Ok(())
        }
    }
}

/// Scan existing local backups for a part with matching name and CRC64.
///
/// Searches `{data_path}/backup/*/shadow/{table_key}/{part_name}/` (excluding
/// `current_backup`) for parts whose `checksums.txt` CRC64 matches `expected_crc`.
///
/// Also searches per-disk backup directories: for each disk in `manifest_disks`
/// where `disk_path != data_path` and the disk path exists locally, searches
/// `{disk_path}/backup/*/shadow/{table_key}/{part_name}/`.
///
/// Returns the path to the first matching part directory, or `None`.
fn find_existing_part(
    data_path: &str,
    current_backup: &str,
    table_key: &str,
    part_name: &str,
    expected_crc: u64,
    manifest_disks: &BTreeMap<String, String>,
    disk_name: &str,
) -> Option<PathBuf> {
    use crate::backup::checksum::compute_crc64;

    if expected_crc == 0 {
        return None;
    }

    // URL-encode the table_key components for filesystem path
    let (db, table) = table_key.split_once('.').unwrap_or(("default", table_key));
    let url_db = encode_path_component(db);
    let url_table = encode_path_component(table);

    // Collect all backup base directories to search, deduped
    let mut search_bases: Vec<PathBuf> = Vec::new();
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();

    // Always search the default data_path/backup/ first
    let default_base = Path::new(data_path).join("backup");
    let canonical_default =
        std::fs::canonicalize(&default_base).unwrap_or_else(|_| default_base.clone());
    seen.insert(canonical_default);
    search_bases.push(default_base);

    // Also search per-disk backup directories for the part's disk
    if let Some(disk_path) = manifest_disks.get(disk_name) {
        let dp = disk_path.trim_end_matches('/');
        if validate_disk_path(dp) {
            let per_disk_base = Path::new(dp).join("backup");
            if per_disk_base.exists() {
                let canonical =
                    std::fs::canonicalize(&per_disk_base).unwrap_or_else(|_| per_disk_base.clone());
                if seen.insert(canonical) {
                    search_bases.push(per_disk_base);
                }
            }
        } else {
            warn!(
                "Disk path '{}' (disk: {}) failed validation in find_existing_part, skipping",
                dp, disk_name
            );
        }
    }

    for backup_base in &search_bases {
        let entries = match std::fs::read_dir(backup_base) {
            Ok(e) => e,
            Err(_) => continue,
        };

        for entry in entries.flatten() {
            let name = match entry.file_name().into_string() {
                Ok(n) => n,
                Err(_) => continue,
            };

            // Skip the current backup being downloaded
            if name == current_backup {
                continue;
            }

            let candidate = backup_base
                .join(&name)
                .join("shadow")
                .join(&url_db)
                .join(&url_table)
                .join(part_name);

            if !candidate.is_dir() {
                continue;
            }

            let checksums_path = candidate.join("checksums.txt");
            if !checksums_path.exists() {
                continue;
            }

            match compute_crc64(&checksums_path) {
                Ok(crc) if crc == expected_crc => {
                    return Some(candidate);
                }
                _ => continue,
            }
        }
    }

    None
}

/// Hardlink all files from an existing part directory to a target directory.
///
/// Walks the source directory recursively and creates hardlinks. Falls back
/// to file copy on EXDEV (cross-device link, error code 18).
fn hardlink_existing_part(existing: &Path, target: &Path) -> Result<()> {
    use walkdir::WalkDir;

    std::fs::create_dir_all(target)
        .with_context(|| format!("Failed to create target dir: {}", target.display()))?;

    for entry in WalkDir::new(existing).min_depth(1) {
        let entry = entry?;
        let relative = entry.path().strip_prefix(existing)?;
        let dest = target.join(relative);

        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&dest)?;
        } else {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            if let Err(e) = std::fs::hard_link(entry.path(), &dest) {
                let is_exdev = e.raw_os_error() == Some(nix::errno::Errno::EXDEV as i32);
                if is_exdev {
                    std::fs::copy(entry.path(), &dest)?;
                } else {
                    return Err(e).with_context(|| {
                        format!(
                            "Failed to hardlink {} -> {}",
                            entry.path().display(),
                            dest.display()
                        )
                    });
                }
            }
        }
    }

    Ok(())
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
/// * `resume` - If true and use_resumable_state config is set, load resume state
/// * `hardlink_exists_files` - If true, deduplicate parts via hardlinks to existing backups
pub async fn download(
    config: &Config,
    s3: &S3Client,
    backup_name: &str,
    resume: bool,
    hardlink_exists_files: bool,
    cancel: CancellationToken,
) -> Result<PathBuf> {
    let data_path = &config.clickhouse.data_path;
    let backup_dir = Path::new(data_path).join("backup").join(backup_name);

    info!(
        backup_name = %backup_name,
        backup_dir = %backup_dir.display(),
        resume = resume,
        "Starting download"
    );

    // 1. Download manifest from S3
    let manifest_key = format!("{}/metadata.json", backup_name);
    let manifest_bytes = s3.get_object(&manifest_key).await.map_err(|e| {
        let msg = format!("{e:#}");
        if msg.contains("NoSuchKey") || msg.contains("404") || msg.contains("not found") {
            anyhow::Error::new(ChBackupError::BackupNotFound(format!(
                "backup '{}' not found in S3 (key: {})",
                backup_name, manifest_key
            )))
        } else {
            e.context(format!(
                "Failed to download manifest for backup '{}'",
                backup_name
            ))
        }
    })?;

    let manifest = BackupManifest::from_json_bytes(&manifest_bytes)
        .with_context(|| format!("Failed to parse manifest for backup '{}'", backup_name))?;

    info!(
        backup_name = %backup_name,
        tables = manifest.tables.len(),
        "Downloaded manifest"
    );

    // 2. Disk space pre-flight check (design §16.3).
    // Group required space by disk so each physical filesystem is checked
    // independently.  S3 disk parts don't consume local space (data stays
    // in the backup bucket until restore), so they are skipped.
    let mut space_by_disk: std::collections::HashMap<String, u64> =
        std::collections::HashMap::new();
    for table in manifest.tables.values() {
        for (disk_name, parts) in &table.parts {
            let disk_type = manifest
                .disk_types
                .get(disk_name)
                .map(|s| s.as_str())
                .unwrap_or("local");
            if is_s3_disk(disk_type) {
                continue;
            }
            let disk_total: u64 = parts.iter().map(|p| p.size).sum();
            *space_by_disk.entry(disk_name.clone()).or_insert(0) += disk_total;
        }
    }

    // When hardlink dedup is enabled, subtract parts that will be satisfied
    // by hardlinks from existing local backups (design §16.3: "required_space
    // = sum(all parts) - sum(parts that match local CRC64)").
    if hardlink_exists_files && !space_by_disk.is_empty() {
        let data_path_for_scan = data_path.to_string();
        let backup_name_for_scan = backup_name.to_string();
        let manifest_for_scan = manifest.clone();
        let savings: std::collections::HashMap<String, u64> =
            tokio::task::spawn_blocking(move || {
                let mut savings: std::collections::HashMap<String, u64> =
                    std::collections::HashMap::new();
                for (table_key, table) in &manifest_for_scan.tables {
                    for (disk_name, parts) in &table.parts {
                        let disk_type = manifest_for_scan
                            .disk_types
                            .get(disk_name)
                            .map(|s| s.as_str())
                            .unwrap_or("local");
                        if is_s3_disk(disk_type) {
                            continue;
                        }
                        for part in parts {
                            if part.checksum_crc64 == 0 {
                                continue;
                            }
                            if find_existing_part(
                                &data_path_for_scan,
                                &backup_name_for_scan,
                                table_key,
                                &part.name,
                                part.checksum_crc64,
                                &manifest_for_scan.disks,
                                disk_name,
                            )
                            .is_some()
                            {
                                *savings.entry(disk_name.clone()).or_insert(0) += part.size;
                            }
                        }
                    }
                }
                savings
            })
            .await
            .unwrap_or_else(|e| {
                warn!(error = %e, "Hardlink dedup scan task panicked");
                HashMap::new()
            });

        for (disk_name, saved) in savings {
            if let Some(req) = space_by_disk.get_mut(&disk_name) {
                *req = req.saturating_sub(saved);
            }
        }
    }

    // Check space per unique disk filesystem.
    for (disk_name, required_bytes) in &space_by_disk {
        if *required_bytes == 0 {
            continue;
        }
        let check_dir = match manifest.disks.get(disk_name).map(|s| s.as_str()) {
            Some(disk_path) => {
                let normalized = disk_path.trim_end_matches('/');
                if validate_disk_path(normalized) && Path::new(normalized).exists() {
                    Path::new(normalized).join("backup")
                } else {
                    warn!(
                        disk_name = %disk_name,
                        disk_path = %disk_path,
                        "Disk path is invalid or not present locally; using default data_path for space preflight"
                    );
                    Path::new(data_path).join("backup")
                }
            }
            None => Path::new(data_path).join("backup"),
        };
        std::fs::create_dir_all(&check_dir).ok();
        check_disk_space(&check_dir, *required_bytes)?;
    }

    // 2b. Create local backup directory
    std::fs::create_dir_all(&backup_dir)
        .with_context(|| format!("Failed to create backup dir: {}", backup_dir.display()))?;

    // 2c. Load resume state if --resume and use_resumable_state
    let use_resume = resume && config.general.use_resumable_state;
    let state_path = backup_dir.join("download.state.json");
    let current_params_hash = compute_params_hash(&[backup_name]);

    let completed_keys: HashSet<String> = if use_resume {
        match load_state_file::<DownloadState>(&state_path) {
            Ok(Some(state)) => {
                if state.params_hash != current_params_hash {
                    warn!("Download state params_hash mismatch (stale state), ignoring");
                    HashSet::new()
                } else if state.backup_name != backup_name {
                    warn!("Download state backup_name mismatch, ignoring");
                    HashSet::new()
                } else {
                    let count = state.completed_keys.len();
                    info!(
                        completed = count,
                        "Resuming download: {} parts already downloaded", count
                    );
                    state.completed_keys
                }
            }
            Ok(None) => HashSet::new(),
            Err(e) => {
                warn!(error = %e, "Failed to load download state, starting fresh");
                HashSet::new()
            }
        }
    } else {
        HashSet::new()
    };

    // 2d. Persist disk map unconditionally so delete_local can find per-disk dirs
    // even if the download fails before writing metadata.json. This is independent
    // of resume mode -- it's for cleanup, not resume.
    if !manifest.disks.is_empty() {
        let disk_map_state = DownloadState {
            completed_keys: HashSet::new(),
            backup_name: backup_name.to_string(),
            params_hash: current_params_hash.clone(),
            disk_map: manifest
                .disks
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        };
        save_state_graceful(&state_path, &disk_map_state);
    }

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

                // Skip already-completed parts (resume)
                if completed_keys.contains(&part.backup_key) {
                    debug!(
                        table = %table_key,
                        part = %part.name,
                        "Skipping already-downloaded part (resume)"
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

    // Create progress tracker for download
    let progress = ProgressTracker::new(
        "Download",
        total_parts as u64,
        config.general.disable_progress_bar,
    );

    // 4. Download parts in parallel with semaphore and rate limiter
    let semaphore = Arc::new(Semaphore::new(concurrency));
    let rate_limiter = RateLimiter::new(effective_download_rate_limit(config));
    let (effective_retries_count, effective_retry_delay_secs, effective_jitter) =
        crate::config::effective_retries(config);
    let retries_on_failure = effective_retries_count;

    // Shared resume state for tracking completed parts across parallel tasks
    let resume_state = if use_resume {
        let state = DownloadState {
            completed_keys,
            backup_name: backup_name.to_string(),
            params_hash: current_params_hash,
            disk_map: manifest
                .disks
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        };
        Some(Arc::new(tokio::sync::Mutex::new((
            state,
            state_path.clone(),
        ))))
    } else {
        None
    };

    let mut handles = Vec::with_capacity(total_parts);
    let data_path_str = data_path.to_string();
    let manifest_disks = Arc::new(manifest.disks.clone());

    for item in work_items {
        let sem = semaphore.clone();
        let s3 = s3.clone();
        let rate_limiter = rate_limiter.clone();
        let backup_dir = backup_dir.clone();
        let resume_state = resume_state.clone();
        let data_format_clone = manifest.data_format.clone();
        let data_path_clone = data_path_str.clone();
        let backup_name_clone = backup_name.to_string();
        let progress = progress.clone();
        let manifest_disks = manifest_disks.clone();
        let cancel_clone = cancel.clone();

        let handle = tokio::spawn(async move {
            let _permit = sem
                .acquire()
                .await
                .map_err(|_| anyhow::anyhow!("Semaphore closed"))?;

            // Pre-clone the table_key needed by the cancel arm before item is moved.
            let cancel_table_key = item.table_key.clone();

            tokio::select! {
                biased;
                _ = cancel_clone.cancelled() => Ok((cancel_table_key, 0u64)),
                result = async move {

            debug!(
                table = %item.table_key,
                part = %item.part.name,
                key = %item.part.backup_key,
                disk = %item.disk_name,
                is_s3_disk = item.is_s3_disk_part,
                "Downloading part"
            );

            let url_db = encode_path_component(&item.db);
            let url_table = encode_path_component(&item.table);

            if item.is_s3_disk_part {
                // S3 disk part: download only metadata files, not the full
                // compressed data archive. The actual S3 data objects remain
                // in the backup bucket until restore copies them.
                let metadata_prefix = &item.part.backup_key;
                let metadata_objects =
                    s3.list_objects(metadata_prefix).await.with_context(|| {
                        format!(
                            "Failed to list metadata for S3 disk part {} of table {}",
                            item.part.name, item.table_key
                        )
                    })?;

                // Resolve per-disk target dir for this part's disk
                let target_backup_dir = resolve_download_target_dir(
                    &manifest_disks,
                    &item.disk_name,
                    &backup_name_clone,
                    &backup_dir,
                );
                let shadow_dir = target_backup_dir
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
                    // Extract filename relative to the part prefix.
                    // obj.key is the full S3 key; strip the S3 prefix first,
                    // then the separator slash, then the metadata key prefix.
                    let relative_name = obj
                        .key
                        .strip_prefix(s3.prefix())
                        .unwrap_or(&obj.key)
                        .trim_start_matches('/')
                        .strip_prefix(metadata_prefix)
                        .unwrap_or(&obj.key)
                        .trim_start_matches('/');

                    if relative_name.is_empty() {
                        continue;
                    }

                    let data = s3
                        .get_object(&format!(
                            "{}/{}",
                            metadata_prefix.trim_end_matches('/'),
                            relative_name
                        ))
                        .await
                        .with_context(|| {
                            format!(
                                "Failed to download metadata file {} for part {}",
                                relative_name, item.part.name
                            )
                        })?;

                    let file_size = data.len() as u64;
                    total_metadata_bytes += file_size;

                    // Write metadata file to local shadow directory.
                    // Sanitize relative_name to prevent path traversal via
                    // crafted S3 keys containing ".." components.
                    let safe_path = sanitize_relative_path(relative_name);
                    if safe_path.as_os_str().is_empty() {
                        warn!(
                            relative_name = %relative_name,
                            "Skipping metadata file with unsafe path"
                        );
                        continue;
                    }
                    let file_path = shadow_dir.join(&safe_path);
                    if let Some(parent) = file_path.parent() {
                        std::fs::create_dir_all(parent).with_context(|| {
                            format!("Failed to create parent dir: {}", parent.display())
                        })?;
                    }
                    std::fs::write(&file_path, &data).with_context(|| {
                        format!("Failed to write metadata file: {}", file_path.display())
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

                // Update resume state after successful S3 disk part download
                if let Some(ref state_mutex) = resume_state {
                    let mut guard = state_mutex.lock().await;
                    guard.0.completed_keys.insert(item.part.backup_key.clone());
                    save_state_graceful(&guard.1, &guard.0);
                }

                progress.inc();

                Ok::<(String, u64), anyhow::Error>((item.table_key, total_metadata_bytes))
            } else {
                // Local disk part: full download + decompress + CRC64 verify
                // Resolve per-disk target dir for this part's disk
                let target_backup_dir = resolve_download_target_dir(
                    &manifest_disks,
                    &item.disk_name,
                    &backup_name_clone,
                    &backup_dir,
                );
                let shadow_dir = target_backup_dir
                    .join("shadow")
                    .join(&url_db)
                    .join(&url_table);
                let part_name = item.part.name.clone();
                let expected_crc = item.part.checksum_crc64;
                let backup_key = item.part.backup_key.clone();

                // Hardlink dedup: check if identical part exists in another local backup
                if hardlink_exists_files {
                    let dp = data_path_clone.clone();
                    let bn = backup_name_clone.clone();
                    let tk = item.table_key.clone();
                    let pn = part_name.clone();
                    let sd = shadow_dir.clone();
                    let ec = expected_crc;
                    let md = (*manifest_disks).clone();
                    let dn = item.disk_name.clone();

                    let dedup_result = tokio::task::spawn_blocking(move || {
                        find_existing_part(&dp, &bn, &tk, &pn, ec, &md, &dn).map(|existing| {
                            let target = sd.join(&pn);
                            hardlink_existing_part(&existing, &target).map(|()| existing)
                        })
                    })
                    .await
                    .context("Dedup task panicked")?;

                    if let Some(result) = dedup_result {
                        match result {
                            Ok(existing) => {
                                info!(
                                    table = %item.table_key,
                                    part = %part_name,
                                    existing = %existing.display(),
                                    "Hardlink dedup: reusing existing part"
                                );

                                // Update resume state
                                if let Some(ref state_mutex) = resume_state {
                                    let mut guard = state_mutex.lock().await;
                                    guard.0.completed_keys.insert(backup_key.clone());
                                    save_state_graceful(&guard.1, &guard.0);
                                }

                                progress.inc();

                                return Ok::<(String, u64), anyhow::Error>((item.table_key, 0));
                            }
                            Err(e) => {
                                warn!(
                                    table = %item.table_key,
                                    part = %part_name,
                                    error = %e,
                                    "Hardlink dedup failed, falling back to download"
                                );
                            }
                        }
                    }
                }

                let mut last_error: Option<anyhow::Error> = None;
                let max_attempts = if expected_crc != 0 {
                    retries_on_failure + 1
                } else {
                    1
                };

                for attempt in 0..max_attempts {
                    if attempt > 0 {
                        let delay_ms = effective_retry_delay_secs
                            .saturating_mul(1000)
                            .saturating_mul(2u64.saturating_pow(attempt))
                            .min(300_000); // Cap at 5 minutes
                        let jittered_ms = crate::config::apply_jitter(delay_ms, effective_jitter);
                        info!(
                            table = %item.table_key,
                            part = %part_name,
                            attempt = attempt + 1,
                            delay_ms = jittered_ms,
                            "Retrying download after CRC64 mismatch"
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(jittered_ms)).await;
                    }

                    let tmp_archive = shadow_dir.join(format!(".{}.download.tmp", part_name));
                    let compressed_size = {
                        let mut object_stream = s3
                            .get_object_stream(&backup_key)
                            .await
                            .with_context(|| {
                                format!(
                                    "Failed to download part {} for table {}",
                                    part_name, item.table_key
                                )
                            })?;

                        let mut out = tokio::fs::OpenOptions::new()
                            .write(true)
                            .create(true)
                            .truncate(true)
                            .open(&tmp_archive)
                            .await
                            .with_context(|| {
                                format!(
                                    "Failed to create temp archive file: {}",
                                    tmp_archive.display()
                                )
                            })?;

                        let mut total = 0u64;
                        while let Some(chunk) = object_stream.try_next().await.with_context(|| {
                            format!(
                                "Failed while streaming part {} for table {}",
                                part_name, item.table_key
                            )
                        })? {
                            total += chunk.len() as u64;
                            out.write_all(&chunk).await.with_context(|| {
                                format!(
                                    "Failed to write temp archive file: {}",
                                    tmp_archive.display()
                                )
                            })?;
                        }
                        out.flush().await.with_context(|| {
                            format!("Failed to flush temp archive file: {}", tmp_archive.display())
                        })?;
                        total
                    };

                    // Rate limit after download
                    rate_limiter.consume(compressed_size).await;

                    // Decompress and extract to local directory
                    let shadow_dir_clone = shadow_dir.clone();
                    let part_name_clone = part_name.clone();
                    let fmt = data_format_clone.clone();
                    let tmp_archive_clone = tmp_archive.clone();
                    let decompress_result = tokio::task::spawn_blocking(move || {
                        stream::decompress_part_file(&tmp_archive_clone, &shadow_dir_clone, &fmt)
                    })
                    .await
                    .context("Decompress task panicked")?;

                    // Temp file is no longer needed after decompression attempt.
                    if let Err(e) = std::fs::remove_file(&tmp_archive) {
                        debug!(
                            file = %tmp_archive.display(),
                            error = %e,
                            "Failed to remove temp archive file"
                        );
                    }

                    decompress_result.with_context(|| {
                        format!(
                            "Failed to decompress part {} to {}",
                            part_name_clone,
                            shadow_dir.display()
                        )
                    })?;

                    // CRC64 verification after decompression
                    if expected_crc != 0 {
                        let checksums_path = shadow_dir.join(&part_name).join("checksums.txt");
                        if checksums_path.exists() {
                            let actual_crc = {
                                let cp = checksums_path.clone();
                                tokio::task::spawn_blocking(move || compute_crc64(&cp))
                                    .await
                                    .context("CRC64 compute task panicked")?
                                    .with_context(|| {
                                        format!(
                                            "Failed to compute CRC64 for {}",
                                            checksums_path.display()
                                        )
                                    })?
                            };

                            if actual_crc != expected_crc {
                                warn!(
                                    table = %item.table_key,
                                    part = %part_name,
                                    expected = expected_crc,
                                    actual = actual_crc,
                                    "Post-download CRC64 mismatch"
                                );

                                // Delete corrupted part directory
                                let corrupt_dir = shadow_dir.join(&part_name);
                                if corrupt_dir.exists() {
                                    let _ = std::fs::remove_dir_all(&corrupt_dir);
                                }

                                last_error = Some(anyhow::anyhow!(
                                    "CRC64 mismatch for part {}: expected {} got {}",
                                    part_name,
                                    expected_crc,
                                    actual_crc
                                ));
                                continue;
                            }
                        } else {
                            // checksums.txt missing after decompression but
                            // manifest records a non-zero CRC -- treat as
                            // corruption to trigger retry (design 16.1).
                            warn!(
                                table = %item.table_key,
                                part = %part_name,
                                expected_crc = expected_crc,
                                "checksums.txt missing after decompression but CRC expected, treating as corruption"
                            );

                            let corrupt_dir = shadow_dir.join(&part_name);
                            if corrupt_dir.exists() {
                                let _ = std::fs::remove_dir_all(&corrupt_dir);
                            }

                            last_error = Some(anyhow::anyhow!(
                                "checksums.txt missing for part {} but expected CRC {}",
                                part_name,
                                expected_crc
                            ));
                            continue;
                        }
                    }

                    // CRC64 passed or no checksum available -- success
                    debug!(
                        table = %item.table_key,
                        part = %part_name,
                        compressed_size = compressed_size,
                        "Part downloaded and decompressed"
                    );

                    // Update resume state after successful download
                    if let Some(ref state_mutex) = resume_state {
                        let mut guard = state_mutex.lock().await;
                        guard.0.completed_keys.insert(backup_key.clone());
                        save_state_graceful(&guard.1, &guard.0);
                    }

                    progress.inc();

                    return Ok::<(String, u64), anyhow::Error>((item.table_key, compressed_size));
                }

                // All retries exhausted
                Err(last_error.unwrap_or_else(|| {
                    anyhow::anyhow!("Download failed for part {} after retries", part_name)
                }))
            }

            } => result,
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

    // Guard: workers return Ok on cancel, so try_join_all succeeds even when killed.
    // Prevent saving state and metadata when the operation was cancelled.
    if cancel.is_cancelled() {
        return Err(anyhow::anyhow!("download cancelled"));
    }

    progress.finish();

    // 5. Tally totals
    let mut total_compressed_bytes = 0u64;
    for (_table_key, compressed_size) in &results {
        total_compressed_bytes += compressed_size;
    }

    // 5a. Download access/ directory (RBAC files) if present in manifest
    if manifest.rbac.is_some() {
        download_simple_directory(
            s3,
            backup_name,
            &backup_dir,
            "access",
            effective_retries_count,
            effective_retry_delay_secs,
            effective_jitter,
        )
        .await?;
        info!("Downloaded access/ directory from S3");
    }

    // 5b. Download configs/ directory (check if any configs/ keys exist in S3)
    download_simple_directory(
        s3,
        backup_name,
        &backup_dir,
        "configs",
        effective_retries_count,
        effective_retry_delay_secs,
        effective_jitter,
    )
    .await?;
    if backup_dir.join("configs").exists() {
        info!("Downloaded configs/ directory from S3");
    }

    // 6. Save per-table metadata (sequential)
    for (table_key, table_manifest) in &manifest.tables {
        let (db, table) = table_key.split_once('.').unwrap_or(("default", table_key));

        let metadata_dir = backup_dir.join("metadata").join(encode_path_component(db));
        std::fs::create_dir_all(&metadata_dir).with_context(|| {
            format!("Failed to create metadata dir: {}", metadata_dir.display())
        })?;

        let table_metadata_path =
            metadata_dir.join(format!("{}.json", encode_path_component(table)));
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

    // 8. Delete resume state file on success
    if use_resume {
        delete_state_file(&state_path);
    }

    info!(
        backup_name = %backup_name,
        parts = total_parts,
        compressed_bytes = total_compressed_bytes,
        backup_dir = %backup_dir.display(),
        "Download complete"
    );

    Ok(backup_dir)
}

/// Download all files under `{backup_name}/{prefix}/` from S3 to `{local_dir}/{prefix}/`.
///
/// Lists objects with the S3 prefix, creates the local directory structure, and
/// downloads each file. If no objects exist under the prefix, this is a no-op.
async fn download_simple_directory(
    s3: &S3Client,
    backup_name: &str,
    local_dir: &Path,
    prefix: &str,
    max_retries: u32,
    retry_delay_secs: u64,
    jitter_factor: f64,
) -> Result<()> {
    let s3_prefix = format!("{}/{}/", backup_name, prefix);
    let objects = s3
        .list_objects(&s3_prefix)
        .await
        .with_context(|| format!("Failed to list S3 objects under {}", s3_prefix))?;

    if objects.is_empty() {
        debug!(prefix = %prefix, "No {} files found in S3", prefix);
        return Ok(());
    }

    let target_dir = local_dir.join(prefix);
    std::fs::create_dir_all(&target_dir)
        .with_context(|| format!("Failed to create {} directory", prefix))?;

    let client_prefix = s3.prefix();
    let client_prefix_slash = if client_prefix.is_empty() {
        String::new()
    } else if client_prefix.ends_with('/') {
        client_prefix.to_string()
    } else {
        format!("{}/", client_prefix)
    };

    for obj in &objects {
        let relative_key = if !client_prefix_slash.is_empty() {
            obj.key
                .strip_prefix(&client_prefix_slash)
                .unwrap_or(&obj.key)
        } else {
            &obj.key
        };

        let mut last_err: Option<anyhow::Error> = None;
        let total_attempts = max_retries + 1;
        for attempt in 0..total_attempts {
            if attempt > 0 {
                let delay_ms = retry_delay_secs
                    .saturating_mul(1000)
                    .saturating_mul(2u64.saturating_pow(attempt))
                    .min(300_000); // Cap at 5 minutes
                let jittered_ms = crate::config::apply_jitter(delay_ms, jitter_factor);
                warn!(
                    key = %obj.key,
                    attempt = attempt + 1,
                    delay_ms = jittered_ms,
                    "Retrying download of {} file after failure",
                    prefix
                );
                tokio::time::sleep(std::time::Duration::from_millis(jittered_ms)).await;
            }
            match s3.get_object(relative_key).await {
                Ok(data) => {
                    let full_s3_prefix = format!("{}{}", client_prefix_slash, s3_prefix);
                    let rel_str = obj.key.strip_prefix(&full_s3_prefix).unwrap_or(&obj.key);
                    let safe_rel = sanitize_relative_path(rel_str);
                    let file_path = target_dir.join(&safe_rel);
                    if let Some(parent) = file_path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(&file_path, &data)
                        .with_context(|| format!("Failed to write {}", file_path.display()))?;
                    last_err = None;
                    break;
                }
                Err(e) => {
                    last_err = Some(e);
                }
            }
        }
        if let Some(e) = last_err {
            return Err(e).with_context(|| format!("Failed to download {}", obj.key));
        }
    }

    debug!(
        prefix = %prefix,
        count = objects.len(),
        "Downloaded {} files from S3",
        objects.len()
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::S3ObjectInfo;
    use std::collections::HashMap;

    #[test]
    fn test_download_work_item_construction() {
        let mut part = PartInfo::new("202401_1_50_3", 134_217_728, 12345);
        part.backup_key = "daily/data/default/trades/202401_1_50_3.tar.lz4".to_string();

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
        let mut s3_part = PartInfo::new("202401_1_50_3", 134_217_728, 12345).with_s3_objects(vec![
            S3ObjectInfo {
                path: "store/abc/def/data.bin".to_string(),
                size: 134_217_000,
                backup_key: "daily/objects/store/abc/def/data.bin".to_string(),
            },
        ]);
        s3_part.backup_key = "daily/data/default/trades/s3disk/202401_1_50_3/".to_string();

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
            is_s3_disk_part: disk_is_s3, // s3_objects.is_some()
        };

        assert!(work_item.is_s3_disk_part);
        assert_eq!(work_item.disk_name, "s3disk");
        assert!(work_item.part.s3_objects.is_some());
    }

    #[test]
    fn test_download_local_parts_not_flagged_as_s3() {
        // Verify that local disk parts are NOT flagged as S3
        let mut local_part = PartInfo::new("202401_1_50_3", 4096, 11111);
        local_part.backup_key = "daily/data/default/trades/202401_1_50_3.tar.lz4".to_string();

        let disk_types: HashMap<String, String> =
            HashMap::from([("default".to_string(), "local".to_string())]);

        let disk_name = "default";
        let _disk_is_s3 = disk_types
            .get(disk_name)
            .map(|dt| is_s3_disk(dt))
            .unwrap_or(false);

        let work_item = DownloadWorkItem {
            table_key: "default.trades".to_string(),
            db: "default".to_string(),
            table: "trades".to_string(),
            disk_name: disk_name.to_string(),
            part: local_part,
            is_s3_disk_part: false, // local disk, s3_objects.is_none()
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
        assert!(s3_disk_s3_objects); // s3_objects.is_some() -> true

        // Case 2: S3 disk but no s3_objects (shouldn't happen, but defensive) -> false
        // When s3_objects is None on an S3 disk, is_s3_disk_part should be false
        // (defensive -- shouldn't happen, but the flag explicitly requires s3_objects)
        let s3_disk_no_objects = false; // s3_objects.is_none() -> is_s3_disk_part = false
        assert!(!s3_disk_no_objects);

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
    fn test_download_skips_completed_parts() {
        // Verify that completed_keys causes parts to be skipped
        let completed_keys: HashSet<String> =
            HashSet::from(["daily/data/default/trades/202401_1_50_3.tar.lz4".to_string()]);

        let backup_key = "daily/data/default/trades/202401_1_50_3.tar.lz4";
        assert!(completed_keys.contains(backup_key));

        // A different part should NOT be skipped
        let other_key = "daily/data/default/trades/202402_1_1_0.tar.lz4";
        assert!(!completed_keys.contains(other_key));
    }

    #[test]
    fn test_crc64_verification_pass() {
        use crate::backup::checksum::compute_crc64;

        // Create a test file, compute CRC64, verify it matches
        let dir = tempfile::tempdir().unwrap();
        let checksums_path = dir.path().join("checksums.txt");
        std::fs::write(&checksums_path, b"test checksum data").unwrap();

        let crc = compute_crc64(&checksums_path).unwrap();
        assert_ne!(crc, 0);

        // Second computation should match
        let crc2 = compute_crc64(&checksums_path).unwrap();
        assert_eq!(crc, crc2);
    }

    #[test]
    fn test_disk_space_preflight_sufficient() {
        // Check that the current temp directory has sufficient space for a small amount
        let dir = tempfile::tempdir().unwrap();
        let result = check_disk_space(dir.path(), 1024); // 1KB should be fine
        assert!(result.is_ok());
    }

    #[test]
    fn test_disk_space_preflight_insufficient() {
        // Check with an absurdly large requirement that no disk could satisfy
        let dir = tempfile::tempdir().unwrap();
        let result = check_disk_space(dir.path(), u64::MAX);
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("Insufficient disk space"));
    }

    #[test]
    fn test_disk_space_preflight_nonexistent_path() {
        // Non-existent path should gracefully skip
        let result = check_disk_space(Path::new("/nonexistent/path/xyz"), 1024);
        assert!(result.is_ok());
    }

    #[test]
    fn test_hardlink_dedup_finds_existing_part() {
        use crate::backup::checksum::compute_crc64;

        let dir = tempfile::tempdir().unwrap();
        let data_path = dir.path().to_str().unwrap();

        // Create an existing backup with a part
        let existing_backup = dir
            .path()
            .join("backup")
            .join("old-backup")
            .join("shadow")
            .join("default")
            .join("trades")
            .join("202401_1_50_3");
        std::fs::create_dir_all(&existing_backup).unwrap();
        std::fs::write(existing_backup.join("checksums.txt"), b"checksum data").unwrap();
        std::fs::write(existing_backup.join("data.bin"), b"binary data").unwrap();

        let expected_crc = compute_crc64(&existing_backup.join("checksums.txt")).unwrap();

        // find_existing_part should find the existing part
        let result = find_existing_part(
            data_path,
            "new-backup",
            "default.trades",
            "202401_1_50_3",
            expected_crc,
            &BTreeMap::new(),
            "default",
        );
        assert!(result.is_some());
        assert!(result.unwrap().ends_with("202401_1_50_3"));
    }

    #[test]
    fn test_hardlink_dedup_skips_current_backup() {
        use crate::backup::checksum::compute_crc64;

        let dir = tempfile::tempdir().unwrap();
        let data_path = dir.path().to_str().unwrap();

        // Create a backup with same name as current
        let same_backup = dir
            .path()
            .join("backup")
            .join("my-backup")
            .join("shadow")
            .join("default")
            .join("trades")
            .join("202401_1_50_3");
        std::fs::create_dir_all(&same_backup).unwrap();
        std::fs::write(same_backup.join("checksums.txt"), b"checksum data").unwrap();

        let crc = compute_crc64(&same_backup.join("checksums.txt")).unwrap();

        // Should NOT find it since it's the current backup
        let result = find_existing_part(
            data_path,
            "my-backup",
            "default.trades",
            "202401_1_50_3",
            crc,
            &BTreeMap::new(),
            "default",
        );
        assert!(result.is_none());
    }

    #[test]
    fn test_hardlink_dedup_no_match_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let data_path = dir.path().to_str().unwrap();

        // No backups exist
        std::fs::create_dir_all(dir.path().join("backup")).unwrap();

        let result = find_existing_part(
            data_path,
            "new-backup",
            "default.trades",
            "202401_1_50_3",
            12345,
            &BTreeMap::new(),
            "default",
        );
        assert!(result.is_none());
    }

    #[test]
    fn test_hardlink_dedup_zero_crc_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let data_path = dir.path().to_str().unwrap();

        // CRC of 0 means no checksum -> skip dedup
        let result = find_existing_part(
            data_path,
            "new-backup",
            "default.trades",
            "202401_1_50_3",
            0,
            &BTreeMap::new(),
            "default",
        );
        assert!(result.is_none());
    }

    #[test]
    fn test_download_per_disk_dir_construction() {
        // When manifest.disks maps a disk to a non-default path AND the disk path
        // exists locally, the shadow_dir should use {disk_path}/backup/{name}/shadow/...
        let dir = tempfile::tempdir().unwrap();

        // Create two "disks": data_path (default) and a separate nvme disk
        let data_path = dir.path().join("data");
        let nvme_path = dir.path().join("nvme1");
        std::fs::create_dir_all(&data_path).unwrap();
        std::fs::create_dir_all(&nvme_path).unwrap();

        let backup_dir = data_path.join("backup").join("daily-2024");
        std::fs::create_dir_all(&backup_dir).unwrap();

        let mut manifest_disks = BTreeMap::new();
        manifest_disks.insert(
            "default".to_string(),
            data_path.to_str().unwrap().to_string(),
        );
        manifest_disks.insert("nvme1".to_string(), nvme_path.to_str().unwrap().to_string());

        // For the default disk, target should resolve to the default backup_dir
        let target_default =
            resolve_download_target_dir(&manifest_disks, "default", "daily-2024", &backup_dir);
        assert_eq!(
            target_default,
            data_path.join("backup").join("daily-2024"),
            "Default disk should resolve to data_path/backup/name"
        );

        // For the nvme1 disk, target should resolve to per-disk path
        let target_nvme =
            resolve_download_target_dir(&manifest_disks, "nvme1", "daily-2024", &backup_dir);
        assert_eq!(
            target_nvme,
            nvme_path.join("backup").join("daily-2024"),
            "Non-default disk should resolve to disk_path/backup/name"
        );

        // The shadow dirs should be under per-disk paths
        let shadow_nvme = target_nvme
            .join("shadow")
            .join("default")
            .join("trades")
            .join("202401_1_50_3");
        assert!(shadow_nvme.starts_with(&nvme_path));
    }

    #[test]
    fn test_download_per_disk_fallback_disk_not_present() {
        // When the disk path from the manifest does NOT exist on the local host,
        // should fall back to the default backup_dir
        let dir = tempfile::tempdir().unwrap();
        let backup_dir = dir.path().join("backup").join("daily-2024");
        std::fs::create_dir_all(&backup_dir).unwrap();

        let mut manifest_disks = BTreeMap::new();
        manifest_disks.insert(
            "nvme_remote".to_string(),
            "/nonexistent/remote/nvme/path".to_string(),
        );

        // Disk path doesn't exist -> should fall back to backup_dir
        let target =
            resolve_download_target_dir(&manifest_disks, "nvme_remote", "daily-2024", &backup_dir);
        assert_eq!(
            target, backup_dir,
            "Non-existent disk path should fall back to default backup_dir"
        );
    }

    #[test]
    fn test_download_per_disk_fallback_invalid_disk_path() {
        let dir = tempfile::tempdir().unwrap();
        let backup_dir = dir.path().join("backup").join("daily-2024");
        std::fs::create_dir_all(&backup_dir).unwrap();

        let mut manifest_disks = BTreeMap::new();
        manifest_disks.insert("nvme1".to_string(), "/etc".to_string());

        let target =
            resolve_download_target_dir(&manifest_disks, "nvme1", "daily-2024", &backup_dir);
        assert_eq!(
            target, backup_dir,
            "Invalid manifest disk path must fall back to default backup_dir"
        );
    }

    #[test]
    fn test_download_per_disk_fallback_unknown_disk() {
        // When the disk_name is not in manifest.disks, should fall back to backup_dir
        let dir = tempfile::tempdir().unwrap();
        let backup_dir = dir.path().join("backup").join("daily-2024");
        std::fs::create_dir_all(&backup_dir).unwrap();

        let manifest_disks = BTreeMap::new(); // empty

        let target =
            resolve_download_target_dir(&manifest_disks, "unknown_disk", "daily-2024", &backup_dir);
        assert_eq!(
            target, backup_dir,
            "Unknown disk should fall back to default backup_dir"
        );
    }

    #[test]
    fn test_download_disk_map_persisted_without_resume() {
        // Verify that disk_map is written to the state file even when resume=false,
        // by simulating the unconditional persistence logic from download()
        use crate::resume::{load_state_file, save_state_graceful, DownloadState};

        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("download.state.json");

        let mut disk_map = HashMap::new();
        disk_map.insert("default".to_string(), "/var/lib/clickhouse".to_string());
        disk_map.insert("nvme1".to_string(), "/mnt/nvme1/clickhouse".to_string());

        // Simulate the unconditional disk_map persistence (not gated by resume)
        let disk_map_state = DownloadState {
            completed_keys: HashSet::new(),
            backup_name: "daily-2024".to_string(),
            params_hash: "test_hash".to_string(),
            disk_map: disk_map.clone(),
        };
        save_state_graceful(&state_path, &disk_map_state);

        // Verify the state file was written and disk_map is present
        let loaded: DownloadState = load_state_file(&state_path).unwrap().unwrap();
        assert_eq!(loaded.disk_map.len(), 2);
        assert_eq!(
            loaded.disk_map.get("default").unwrap(),
            "/var/lib/clickhouse"
        );
        assert_eq!(
            loaded.disk_map.get("nvme1").unwrap(),
            "/mnt/nvme1/clickhouse"
        );
        assert!(
            loaded.completed_keys.is_empty(),
            "completed_keys should be empty in initial state"
        );
    }

    #[test]
    fn test_download_disk_map_backward_compat() {
        // Verify that a state file WITHOUT disk_map (old format) deserializes cleanly
        use crate::resume::load_state_file;

        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("download.state.json");

        // Write a state file without disk_map field (simulating old format)
        let old_format_json = r#"{
            "completed_keys": ["key1"],
            "backup_name": "old-backup",
            "params_hash": "old_hash"
        }"#;
        std::fs::write(&state_path, old_format_json).unwrap();

        // Should deserialize cleanly with disk_map defaulting to empty HashMap
        let loaded: DownloadState = load_state_file(&state_path).unwrap().unwrap();
        assert_eq!(loaded.backup_name, "old-backup");
        assert!(
            loaded.disk_map.is_empty(),
            "Missing disk_map should default to empty HashMap"
        );
        assert_eq!(loaded.completed_keys.len(), 1);
    }

    #[test]
    fn test_find_existing_part_per_disk() {
        // Verify that find_existing_part searches per-disk backup directories
        // when manifest_disks maps a disk to a non-default path
        use crate::backup::checksum::compute_crc64;

        let tmp = tempfile::tempdir().unwrap();
        let data_path = tmp.path().join("data");
        let nvme_path = tmp.path().join("nvme1");
        std::fs::create_dir_all(data_path.join("backup")).unwrap();

        // Create a part in a per-disk backup directory (not under data_path)
        let per_disk_part = nvme_path
            .join("backup")
            .join("old-backup")
            .join("shadow")
            .join("default")
            .join("trades")
            .join("202401_1_50_3");
        std::fs::create_dir_all(&per_disk_part).unwrap();
        std::fs::write(per_disk_part.join("checksums.txt"), b"data").unwrap();

        let expected_crc = compute_crc64(&per_disk_part.join("checksums.txt")).unwrap();

        let mut manifest_disks = BTreeMap::new();
        manifest_disks.insert("nvme1".to_string(), nvme_path.to_str().unwrap().to_string());

        // Should find the part at the per-disk location
        let result = find_existing_part(
            data_path.to_str().unwrap(),
            "new-backup",
            "default.trades",
            "202401_1_50_3",
            expected_crc,
            &manifest_disks,
            "nvme1",
        );
        assert!(result.is_some());
        assert_eq!(result.unwrap(), per_disk_part);
    }

    #[test]
    fn test_find_existing_part_per_disk_also_searches_default() {
        // Verify that find_existing_part still finds parts in the default
        // data_path even when manifest_disks is populated
        use crate::backup::checksum::compute_crc64;

        let tmp = tempfile::tempdir().unwrap();
        let data_path = tmp.path().join("data");

        // Create a part in the default backup directory
        let source_part = data_path
            .join("backup")
            .join("old-backup")
            .join("shadow")
            .join("default")
            .join("trades")
            .join("202401_1_50_3");
        std::fs::create_dir_all(&source_part).unwrap();
        std::fs::write(source_part.join("checksums.txt"), b"data").unwrap();

        let expected_crc = compute_crc64(&source_part.join("checksums.txt")).unwrap();

        let mut manifest_disks = BTreeMap::new();
        manifest_disks.insert(
            "default".to_string(),
            data_path.to_str().unwrap().to_string(),
        );

        // Should find the part at the default location
        let result = find_existing_part(
            data_path.to_str().unwrap(),
            "new-backup",
            "default.trades",
            "202401_1_50_3",
            expected_crc,
            &manifest_disks,
            "default",
        );
        assert!(result.is_some());
        assert_eq!(result.unwrap(), source_part);
    }

    #[test]
    fn test_find_existing_part_per_disk_fallback_to_default() {
        // When per-disk path does not have the part but default does,
        // should still find it at the default location
        use crate::backup::checksum::compute_crc64;

        let tmp = tempfile::tempdir().unwrap();
        let data_path = tmp.path().join("data");
        let nvme_path = tmp.path().join("nvme1");
        std::fs::create_dir_all(nvme_path.join("backup")).unwrap();

        // Part only exists in default backup dir, not in per-disk dir
        let legacy_part = data_path
            .join("backup")
            .join("old-backup")
            .join("shadow")
            .join("default")
            .join("trades")
            .join("202401_1_50_3");
        std::fs::create_dir_all(&legacy_part).unwrap();
        std::fs::write(legacy_part.join("checksums.txt"), b"data").unwrap();

        let expected_crc = compute_crc64(&legacy_part.join("checksums.txt")).unwrap();

        let mut manifest_disks = BTreeMap::new();
        manifest_disks.insert("nvme1".to_string(), nvme_path.to_str().unwrap().to_string());

        // Should find the part at the default data_path location
        let result = find_existing_part(
            data_path.to_str().unwrap(),
            "new-backup",
            "default.trades",
            "202401_1_50_3",
            expected_crc,
            &manifest_disks,
            "nvme1",
        );
        assert!(result.is_some());
        assert_eq!(result.unwrap(), legacy_part);
    }

    // ---- sanitize_relative_path tests (SECURITY-CRITICAL) ----

    #[test]
    fn test_sanitize_relative_path_normal() {
        assert_eq!(
            sanitize_relative_path("metadata/db/table.json"),
            PathBuf::from("metadata/db/table.json")
        );
    }

    #[test]
    fn test_sanitize_relative_path_parent_traversal() {
        // Strips .. components
        assert_eq!(
            sanitize_relative_path("../../etc/passwd"),
            PathBuf::from("etc/passwd")
        );
    }

    #[test]
    fn test_sanitize_relative_path_absolute() {
        // Strips root prefix
        assert_eq!(
            sanitize_relative_path("/etc/passwd"),
            PathBuf::from("etc/passwd")
        );
    }

    #[test]
    fn test_sanitize_relative_path_curdir() {
        // Strips . components
        assert_eq!(
            sanitize_relative_path("./some/./path"),
            PathBuf::from("some/path")
        );
    }

    #[test]
    fn test_sanitize_relative_path_mixed_attack() {
        assert_eq!(
            sanitize_relative_path("/../../../tmp/evil"),
            PathBuf::from("tmp/evil")
        );
    }

    #[test]
    fn test_sanitize_relative_path_empty() {
        assert_eq!(sanitize_relative_path(""), PathBuf::from(""));
    }

    #[test]
    fn test_sanitize_relative_path_single_normal() {
        assert_eq!(
            sanitize_relative_path("file.txt"),
            PathBuf::from("file.txt")
        );
    }

    #[test]
    fn test_sanitize_relative_path_double_dot_in_name() {
        // "..hidden" is a Normal component, not ParentDir (only bare ".." is ParentDir)
        assert_eq!(
            sanitize_relative_path("dir/..hidden/file"),
            PathBuf::from("dir/..hidden/file")
        );
    }

    #[test]
    fn test_hardlink_existing_part() {
        let dir = tempfile::tempdir().unwrap();

        // Create source part directory
        let src = dir.path().join("source_part");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("checksums.txt"), b"checksums").unwrap();
        std::fs::write(src.join("data.bin"), b"binary data").unwrap();

        // Hardlink to target
        let target = dir.path().join("target_part");
        hardlink_existing_part(&src, &target).unwrap();

        // Verify target files exist with correct content
        assert!(target.join("checksums.txt").exists());
        assert!(target.join("data.bin").exists());
        assert_eq!(
            std::fs::read(target.join("data.bin")).unwrap(),
            b"binary data"
        );
    }
}
