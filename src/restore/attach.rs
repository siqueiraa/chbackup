//! Part attachment: hardlink parts to detached/ directory and ATTACH PART.
//!
//! For each part in the backup:
//! 1. For local disk parts: hardlink (or copy) files from backup to
//!    `{table_data_path}/detached/{part_name}/`
//! 2. For S3 disk parts: CopyObject to UUID-derived paths, rewrite metadata,
//!    write to `{table_data_path}/detached/{part_name}/`
//! 3. Chown to ClickHouse uid/gid
//! 4. ALTER TABLE ATTACH PART '{part_name}'

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use futures::future::try_join_all;
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};
use walkdir::WalkDir;

use crate::clickhouse::client::ChClient;
use crate::manifest::PartInfo;
use crate::object_disk::{is_s3_disk, parse_metadata, rewrite_metadata};
use crate::storage::S3Client;

use super::sort::{needs_sequential_attach, sort_parts_by_min_block};

/// Parameters for attaching parts to a table (borrowed references).
///
/// Used for the internal sequential attach path where lifetimes are known.
pub struct AttachParams<'a> {
    /// ClickHouse client for ATTACH PART queries.
    pub ch: &'a ChClient,
    /// Database name.
    pub db: &'a str,
    /// Table name.
    pub table: &'a str,
    /// List of parts to attach (from all disks).
    pub parts: &'a [PartInfo],
    /// Base backup directory.
    pub backup_dir: &'a Path,
    /// Path to the table's data directory (from system.tables data_paths).
    pub table_data_path: &'a Path,
    /// ClickHouse process UID (for chown).
    pub clickhouse_uid: Option<u32>,
    /// ClickHouse process GID (for chown).
    pub clickhouse_gid: Option<u32>,
}

/// Owned parameters for attaching parts to a table.
///
/// All fields use owned types (String, PathBuf, Vec) so this struct can be
/// sent across `tokio::spawn` boundaries without lifetime constraints.
pub struct OwnedAttachParams {
    /// ClickHouse client for ATTACH PART queries.
    pub ch: ChClient,
    /// Database name.
    pub db: String,
    /// Table name.
    pub table: String,
    /// List of parts to attach (from all disks).
    pub parts: Vec<PartInfo>,
    /// Base backup directory.
    pub backup_dir: PathBuf,
    /// Path to the table's data directory (from system.tables data_paths).
    pub table_data_path: PathBuf,
    /// ClickHouse process UID (for chown).
    pub clickhouse_uid: Option<u32>,
    /// ClickHouse process GID (for chown).
    pub clickhouse_gid: Option<u32>,
    /// Engine name for determining sequential vs parallel ATTACH.
    pub engine: String,
    /// S3 client for CopyObject during S3 disk part restore.
    pub s3_client: Option<S3Client>,
    /// Disk name -> disk type mapping for routing parts by disk type.
    pub disk_type_map: HashMap<String, String>,
    /// Concurrency limit for S3 CopyObject operations during restore.
    pub object_disk_server_side_copy_concurrency: usize,
    /// Whether to allow streaming fallback for CopyObject failures.
    pub allow_object_disk_streaming: bool,
    /// Disk name -> remote_path for S3 disks (from DiskRow.remote_path).
    pub disk_remote_paths: HashMap<String, String>,
    /// Table UUID for UUID-isolated S3 restore path derivation.
    pub table_uuid: Option<String>,
    /// Parts grouped by disk name, for S3 disk routing.
    pub parts_by_disk: HashMap<String, Vec<PartInfo>>,
}

/// Derive the UUID-based S3 path prefix for restore.
///
/// Per design doc section 5.4, the UUID path format is:
/// `store/{uuid_hex[0..3]}/{uuid_with_dashes}/`
///
/// The first 3 hex characters of the UUID (without dashes) form the
/// directory prefix, followed by the full UUID with dashes.
pub fn uuid_s3_prefix(uuid: &str) -> String {
    // Strip dashes to get a hex-only string for the prefix directory
    let hex_only: String = uuid.chars().filter(|c| *c != '-').collect();
    let prefix_dir = if hex_only.len() >= 3 {
        &hex_only[..3]
    } else {
        &hex_only
    };
    format!("store/{}/{}", prefix_dir, uuid)
}

/// Parameters for restoring S3 disk parts of a single table.
struct S3RestoreParams<'a> {
    s3: &'a S3Client,
    parts_by_disk: &'a HashMap<String, Vec<PartInfo>>,
    disk_type_map: &'a HashMap<String, String>,
    table_uuid: &'a str,
    table_data_path: &'a Path,
    backup_dir: &'a Path,
    db: &'a str,
    table: &'a str,
    concurrency: usize,
    allow_streaming: bool,
    clickhouse_uid: Option<u32>,
    clickhouse_gid: Option<u32>,
}

/// Restore S3 disk parts for a single table.
///
/// For each S3 disk part:
/// 1. Same-name optimization: ListObjectsV2 to check existing objects
/// 2. CopyObject non-matching objects to UUID-derived paths
/// 3. Rewrite metadata files to point to new UUID paths
/// 4. Write rewritten metadata to detached/{part_name}/
async fn restore_s3_disk_parts(p: &S3RestoreParams<'_>) -> Result<()> {
    let s3 = p.s3;
    let db = p.db;
    let table = p.table;
    let uuid_prefix = uuid_s3_prefix(p.table_uuid);
    let allow_streaming = p.allow_streaming;

    // Same-name optimization: list existing objects at the UUID prefix
    // This is a single ListObjectsV2 per table, not per-object HeadObject
    let existing_objects = s3
        .list_objects(&uuid_prefix)
        .await
        .unwrap_or_else(|e| {
            debug!(
                error = %e,
                prefix = %uuid_prefix,
                "Failed to list existing S3 objects for same-name optimization, will copy all"
            );
            Vec::new()
        });

    // Build lookup map: relative_key -> size for same-name optimization
    let existing_map: HashMap<String, i64> = existing_objects
        .into_iter()
        .map(|obj| (obj.key, obj.size))
        .collect();

    info!(
        db = %db,
        table = %table,
        uuid = %p.table_uuid,
        existing_objects = existing_map.len(),
        "Restoring S3 disk parts"
    );

    let semaphore = Arc::new(Semaphore::new(p.concurrency));
    let detached_dir = p.table_data_path.join("detached");

    for (disk_name, parts) in p.parts_by_disk {
        let disk_type = p.disk_type_map.get(disk_name).map(|s| s.as_str()).unwrap_or("");
        if !is_s3_disk(disk_type) {
            continue;
        }

        for part in parts {
            let s3_objects = match &part.s3_objects {
                Some(objs) if !objs.is_empty() => objs,
                _ => continue,
            };

            info!(
                db = %db,
                table = %table,
                part = %part.name,
                objects = s3_objects.len(),
                "S3 disk parts: CopyObject to UUID-isolated paths"
            );

            // Collect copy tasks for this part
            let mut copy_handles = Vec::new();

            for s3_obj in s3_objects {
                // Skip inline data objects (size=0 with no backup_key)
                if s3_obj.size == 0 && s3_obj.backup_key.is_empty() {
                    debug!(
                        path = %s3_obj.path,
                        "Skipping inline data object (size=0)"
                    );
                    continue;
                }

                // Destination key: store/{uuid_hex[0..3]}/{uuid_with_dashes}/{relative_path}
                let dest_key = format!("{}/{}", uuid_prefix, s3_obj.path);

                // Same-name optimization: check if object already exists with matching size
                let full_dest_key = s3.full_key(&dest_key);
                if let Some(&existing_size) = existing_map.get(&full_dest_key) {
                    if existing_size as u64 == s3_obj.size {
                        info!(
                            path = %s3_obj.path,
                            size = s3_obj.size,
                            "Skipping existing S3 object (same-name optimization)"
                        );
                        continue;
                    }
                }

                // Source: the backup_key where the object was stored during upload
                let source_bucket = s3.bucket().to_string();
                let source_key = if s3_obj.backup_key.is_empty() {
                    // Fallback: use the original object path
                    s3_obj.path.clone()
                } else {
                    // Use the backup key (includes the backup prefix)
                    s3_obj.backup_key.clone()
                };

                let sem = semaphore.clone();
                let s3_clone = s3.clone();

                let handle = tokio::spawn(async move {
                    let _permit = sem
                        .acquire()
                        .await
                        .map_err(|_| anyhow::anyhow!("Semaphore closed"))?;

                    s3_clone
                        .copy_object_with_retry(
                            &source_bucket,
                            &source_key,
                            &dest_key,
                            allow_streaming,
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "Failed to copy S3 object: {} -> {}",
                                source_key, dest_key
                            )
                        })?;

                    Ok::<(), anyhow::Error>(())
                });

                copy_handles.push(handle);
            }

            // Await all copy tasks for this part
            if !copy_handles.is_empty() {
                let results = try_join_all(copy_handles)
                    .await
                    .context("S3 copy task panicked")?;
                for result in results {
                    result?;
                }
            }

            // Rewrite metadata files and write to detached/{part_name}/
            let part_detached_dir = detached_dir.join(&part.name);
            std::fs::create_dir_all(&part_detached_dir).with_context(|| {
                format!(
                    "Failed to create detached dir: {}",
                    part_detached_dir.display()
                )
            })?;

            // Read metadata files from the backup shadow directory
            let url_db = url_encode(db);
            let url_table = url_encode(table);
            let source_dir = p.backup_dir
                .join("shadow")
                .join(&url_db)
                .join(&url_table)
                .join(&part.name);

            if source_dir.exists() {
                // Walk all files in the source part directory
                for entry in WalkDir::new(&source_dir).min_depth(1) {
                    let entry = entry.with_context(|| {
                        format!("Failed to read entry under: {}", source_dir.display())
                    })?;

                    let relative = entry
                        .path()
                        .strip_prefix(&source_dir)
                        .context("Failed to strip source prefix")?;
                    let dest_path = part_detached_dir.join(relative);

                    if entry.file_type().is_dir() {
                        std::fs::create_dir_all(&dest_path)?;
                        continue;
                    }

                    // Read the file content
                    let content = std::fs::read_to_string(entry.path());

                    match content {
                        Ok(text) => {
                            // Try to parse as object disk metadata and rewrite paths
                            match parse_metadata(&text) {
                                Ok(metadata) => {
                                    let rewritten = rewrite_metadata(&metadata, &uuid_prefix);
                                    std::fs::write(&dest_path, &rewritten).with_context(
                                        || {
                                            format!(
                                                "Failed to write rewritten metadata: {}",
                                                dest_path.display()
                                            )
                                        },
                                    )?;
                                }
                                Err(_) => {
                                    // Not a metadata file -- copy as-is
                                    // (e.g. checksums.txt, columns.txt)
                                    std::fs::write(&dest_path, &text).with_context(|| {
                                        format!(
                                            "Failed to write file: {}",
                                            dest_path.display()
                                        )
                                    })?;
                                }
                            }
                        }
                        Err(_) => {
                            // Binary file -- hardlink or copy
                            match std::fs::hard_link(entry.path(), &dest_path) {
                                Ok(()) => {}
                                Err(e) => {
                                    if e.raw_os_error() == Some(18) {
                                        std::fs::copy(entry.path(), &dest_path)?;
                                    } else {
                                        return Err(e).with_context(|| {
                                            format!(
                                                "Failed to hardlink {} to {}",
                                                entry.path().display(),
                                                dest_path.display()
                                            )
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Chown the rewritten metadata files
            chown_recursive(&part_detached_dir, p.clickhouse_uid, p.clickhouse_gid)?;
        }
    }

    Ok(())
}

/// Attach parts for a single table using owned parameters.
///
/// Handles both local disk parts (hardlink) and S3 disk parts (CopyObject +
/// metadata rewrite) before delegating to the internal attach logic.
pub async fn attach_parts_owned(params: OwnedAttachParams) -> Result<u64> {
    // Handle S3 disk parts first (CopyObject + metadata rewrite)
    let has_s3_parts = params.s3_client.is_some()
        && params.table_uuid.is_some()
        && params.parts_by_disk.iter().any(|(disk_name, parts)| {
            let disk_type = params.disk_type_map.get(disk_name).map(|s| s.as_str()).unwrap_or("");
            is_s3_disk(disk_type) && parts.iter().any(|p| p.s3_objects.is_some())
        });

    if has_s3_parts {
        let s3 = params.s3_client.as_ref().expect("s3_client checked above");
        let uuid = params.table_uuid.as_ref().expect("table_uuid checked above");

        let s3_params = S3RestoreParams {
            s3,
            parts_by_disk: &params.parts_by_disk,
            disk_type_map: &params.disk_type_map,
            table_uuid: uuid,
            table_data_path: &params.table_data_path,
            backup_dir: &params.backup_dir,
            db: &params.db,
            table: &params.table,
            concurrency: params.object_disk_server_side_copy_concurrency,
            allow_streaming: params.allow_object_disk_streaming,
            clickhouse_uid: params.clickhouse_uid,
            clickhouse_gid: params.clickhouse_gid,
        };
        restore_s3_disk_parts(&s3_params).await?;
    }

    let attach_params = AttachParams {
        ch: &params.ch,
        db: &params.db,
        table: &params.table,
        parts: &params.parts,
        backup_dir: &params.backup_dir,
        table_data_path: &params.table_data_path,
        clickhouse_uid: params.clickhouse_uid,
        clickhouse_gid: params.clickhouse_gid,
    };

    attach_parts_inner(&attach_params, &params.engine).await
}

/// Attach parts for a single table using borrowed parameters.
///
/// Hardlinks backup part files to the table's detached/ directory, then
/// executes ALTER TABLE ATTACH PART for each part.
pub async fn attach_parts(params: &AttachParams<'_>) -> Result<u64> {
    // When called from the borrowed API, we don't have engine info,
    // so default to sequential (safe for all engines).
    attach_parts_inner(params, "").await
}

/// Internal attach implementation with engine-aware routing.
///
/// When `needs_sequential_attach(engine)` returns true (Replacing, Collapsing,
/// Versioned engines), parts are attached strictly in sorted order.
/// For plain MergeTree engines, parts are also attached sequentially within
/// the table (Phase 2a keeps per-table ATTACH sequential; parallelism is
/// across tables, not within a single table).
async fn attach_parts_inner(params: &AttachParams<'_>, engine: &str) -> Result<u64> {
    let db = params.db;
    let table = params.table;

    if params.parts.is_empty() {
        debug!(
            db = %db,
            table = %table,
            "No parts to attach"
        );
        return Ok(0);
    }

    let sequential = needs_sequential_attach(engine) || engine.is_empty();

    if sequential {
        debug!(
            db = %db,
            table = %table,
            engine = %engine,
            "Using sequential attach (engine requires ordered attachment)"
        );
    }

    // Sort parts by (partition, min_block) for correct attach order
    let sorted_parts = sort_parts_by_min_block(params.parts);

    let detached_dir = params.table_data_path.join("detached");
    std::fs::create_dir_all(&detached_dir).with_context(|| {
        format!(
            "Failed to create detached directory: {}",
            detached_dir.display()
        )
    })?;

    let mut attached_count = 0u64;

    for part in &sorted_parts {
        // Destination: {table_data_path}/detached/{part_name}/
        let dest_dir = detached_dir.join(&part.name);

        // S3 disk parts: detached/ was already populated by restore_s3_disk_parts
        let is_s3_part = part.s3_objects.is_some();
        if is_s3_part && dest_dir.exists() {
            debug!(
                part = %part.name,
                "S3 disk part already prepared in detached/, skipping hardlink"
            );
        } else {
            // URL-encode db and table names for the backup shadow path
            let url_db = url_encode(db);
            let url_table = url_encode(table);

            // Source: {backup_dir}/shadow/{db}/{table}/{part_name}/
            let source_dir = params.backup_dir
                .join("shadow")
                .join(&url_db)
                .join(&url_table)
                .join(&part.name);

            if !source_dir.exists() {
                warn!(
                    part = %part.name,
                    source = %source_dir.display(),
                    "Part source directory not found, skipping"
                );
                continue;
            }

            // Hardlink or copy all files from source to dest
            hardlink_or_copy_dir(&source_dir, &dest_dir)
                .with_context(|| {
                    format!(
                        "Failed to hardlink/copy part {} from {} to {}",
                        part.name,
                        source_dir.display(),
                        dest_dir.display()
                    )
                })?;

            // Chown to ClickHouse uid/gid
            chown_recursive(&dest_dir, params.clickhouse_uid, params.clickhouse_gid)?;
        }

        // ATTACH PART
        debug!(
            db = %db,
            table = %table,
            part = %part.name,
            "Attaching part"
        );

        match params.ch.attach_part(db, table, &part.name).await {
            Ok(()) => {
                attached_count += 1;
            }
            Err(e) => {
                let err_str = format!("{:#}", e);
                // ClickHouse errors 232/233: overlapping block range or part already exists
                if err_str.contains("DUPLICATE_DATA_PART")
                    || err_str.contains("PART_IS_TEMPORARILY_LOCKED")
                    || err_str.contains("NO_SUCH_DATA_PART")
                    || err_str.contains("232")
                    || err_str.contains("233")
                {
                    warn!(
                        db = %db,
                        table = %table,
                        part = %part.name,
                        error = %err_str,
                        "Part already exists or overlaps, skipping"
                    );
                } else {
                    return Err(e).with_context(|| {
                        format!(
                            "Failed to ATTACH PART '{}' to {}.{}",
                            part.name, db, table
                        )
                    });
                }
            }
        }
    }

    info!(
        db = %db,
        table = %table,
        attached = attached_count,
        total = sorted_parts.len(),
        "Parts attached"
    );

    Ok(attached_count)
}

/// Hardlink all files from source directory to destination directory.
///
/// If hardlink fails with EXDEV (cross-device link), falls back to file copy.
fn hardlink_or_copy_dir(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)
        .with_context(|| format!("Failed to create destination dir: {}", dst.display()))?;

    for entry in WalkDir::new(src).min_depth(1) {
        let entry = entry.with_context(|| {
            format!("Failed to read directory entry under: {}", src.display())
        })?;

        let relative = entry
            .path()
            .strip_prefix(src)
            .context("Failed to strip source prefix")?;
        let dest_path = dst.join(relative);

        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&dest_path).with_context(|| {
                format!("Failed to create directory: {}", dest_path.display())
            })?;
        } else {
            // Try hardlink first
            match std::fs::hard_link(entry.path(), &dest_path) {
                Ok(()) => {}
                Err(e) => {
                    // Check for EXDEV (cross-device link, error code 18)
                    if e.raw_os_error() == Some(18) {
                        debug!(
                            src = %entry.path().display(),
                            dst = %dest_path.display(),
                            "Cross-device link, falling back to copy"
                        );
                        std::fs::copy(entry.path(), &dest_path).with_context(|| {
                            format!(
                                "Failed to copy {} to {}",
                                entry.path().display(),
                                dest_path.display()
                            )
                        })?;
                    } else {
                        return Err(e).with_context(|| {
                            format!(
                                "Failed to hardlink {} to {}",
                                entry.path().display(),
                                dest_path.display()
                            )
                        });
                    }
                }
            }
        }
    }

    Ok(())
}

/// Recursively chown files and directories to the given uid/gid.
///
/// If uid and gid are both None, this is a no-op.
/// If the process is not running as root, chown may fail with EPERM --
/// in that case we log a warning and continue.
fn chown_recursive(path: &Path, uid: Option<u32>, gid: Option<u32>) -> Result<()> {
    if uid.is_none() && gid.is_none() {
        return Ok(());
    }

    let nix_uid = uid.map(nix::unistd::Uid::from_raw);
    let nix_gid = gid.map(nix::unistd::Gid::from_raw);

    for entry in WalkDir::new(path) {
        let entry = entry.with_context(|| {
            format!("Failed to read directory entry under: {}", path.display())
        })?;

        match nix::unistd::chown(entry.path(), nix_uid, nix_gid) {
            Ok(()) => {}
            Err(nix::errno::Errno::EPERM) => {
                // Not running as root -- skip chown silently
                debug!(
                    path = %entry.path().display(),
                    "Cannot chown (not root), skipping"
                );
                return Ok(()); // Skip remaining files too
            }
            Err(e) => {
                warn!(
                    path = %entry.path().display(),
                    error = %e,
                    "Failed to chown"
                );
            }
        }
    }

    Ok(())
}

/// Detect ClickHouse uid/gid by stat-ing the data directory.
///
/// Returns (uid, gid) of the data directory owner, which should be the
/// ClickHouse process user.
pub fn detect_clickhouse_ownership(data_path: &Path) -> Result<(Option<u32>, Option<u32>)> {
    use std::os::unix::fs::MetadataExt;

    if !data_path.exists() {
        debug!(
            path = %data_path.display(),
            "Data path does not exist, cannot detect ClickHouse ownership"
        );
        return Ok((None, None));
    }

    let metadata = std::fs::metadata(data_path).with_context(|| {
        format!(
            "Failed to stat ClickHouse data path: {}",
            data_path.display()
        )
    })?;

    let uid = metadata.uid();
    let gid = metadata.gid();

    debug!(
        path = %data_path.display(),
        uid = uid,
        gid = gid,
        "Detected ClickHouse ownership"
    );

    Ok((Some(uid), Some(gid)))
}

/// Get the data path for a specific table by querying system.tables.
///
/// Falls back to constructing a default path if the query fails.
pub fn get_table_data_path(
    data_paths: &[String],
    data_path_config: &str,
    db: &str,
    table: &str,
) -> PathBuf {
    // data_paths from system.tables contains the actual storage paths
    // The first element is the primary data path for the table
    if let Some(first) = data_paths.first() {
        if !first.is_empty() {
            return PathBuf::from(first);
        }
    }

    // Fallback: construct path from config
    PathBuf::from(data_path_config)
        .join("data")
        .join(db)
        .join(table)
}

/// URL-encode a component for directory paths (same as download module).
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_owned_attach_params_send() {
        fn assert_send<T: Send>() {}
        assert_send::<OwnedAttachParams>();
    }

    #[test]
    fn test_needs_sequential_attach_wired() {
        // Verify needs_sequential_attach is called from attach flow
        // by testing the function directly with the same engines
        // that the attach_parts_inner logic would use
        assert!(needs_sequential_attach("ReplacingMergeTree"));
        assert!(needs_sequential_attach("CollapsingMergeTree"));
        assert!(needs_sequential_attach("VersionedCollapsingMergeTree"));
        assert!(!needs_sequential_attach("MergeTree"));
        assert!(!needs_sequential_attach("AggregatingMergeTree"));
    }

    #[test]
    fn test_hardlink_or_copy_dir() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");

        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("file1.txt"), b"content1").unwrap();
        std::fs::write(src.join("file2.txt"), b"content2").unwrap();

        let sub = src.join("subdir");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("nested.txt"), b"nested").unwrap();

        hardlink_or_copy_dir(&src, &dst).unwrap();

        assert_eq!(
            std::fs::read_to_string(dst.join("file1.txt")).unwrap(),
            "content1"
        );
        assert_eq!(
            std::fs::read_to_string(dst.join("file2.txt")).unwrap(),
            "content2"
        );
        assert_eq!(
            std::fs::read_to_string(dst.join("subdir/nested.txt")).unwrap(),
            "nested"
        );
    }

    #[test]
    fn test_get_table_data_path_from_data_paths() {
        let data_paths = vec!["/var/lib/clickhouse/store/abc/abcdef123456/".to_string()];
        let result = get_table_data_path(&data_paths, "/var/lib/clickhouse", "default", "trades");
        assert_eq!(
            result,
            PathBuf::from("/var/lib/clickhouse/store/abc/abcdef123456/")
        );
    }

    #[test]
    fn test_get_table_data_path_fallback() {
        let data_paths: Vec<String> = vec![];
        let result = get_table_data_path(&data_paths, "/var/lib/clickhouse", "default", "trades");
        assert_eq!(
            result,
            PathBuf::from("/var/lib/clickhouse/data/default/trades")
        );
    }

    #[test]
    fn test_get_table_data_path_empty_first() {
        let data_paths = vec!["".to_string()];
        let result = get_table_data_path(&data_paths, "/var/lib/clickhouse", "default", "trades");
        assert_eq!(
            result,
            PathBuf::from("/var/lib/clickhouse/data/default/trades")
        );
    }

    #[test]
    fn test_detect_clickhouse_ownership_nonexistent() {
        let result = detect_clickhouse_ownership(Path::new("/nonexistent/path/123456789"));
        assert!(result.is_ok());
        let (uid, gid) = result.unwrap();
        assert!(uid.is_none());
        assert!(gid.is_none());
    }

    #[test]
    fn test_url_encode() {
        assert_eq!(url_encode("default"), "default");
        assert_eq!(url_encode("my table"), "my%20table");
    }

    // ---- Task 8: UUID-isolated S3 restore tests ----

    #[test]
    fn test_restore_s3_uuid_path_derivation() {
        // Standard UUID with dashes
        let uuid = "5f3a7b2c-1234-5678-9abc-def012345678";
        let prefix = uuid_s3_prefix(uuid);
        // hex-only: 5f3a7b2c123456789abcdef012345678
        // First 3 hex chars: "5f3"
        assert_eq!(prefix, "store/5f3/5f3a7b2c-1234-5678-9abc-def012345678");

        // Another UUID
        let uuid2 = "abcdef01-2345-6789-abcd-ef0123456789";
        let prefix2 = uuid_s3_prefix(uuid2);
        // hex-only: abcdef0123456789abcdef0123456789
        // First 3 hex chars: "abc"
        assert_eq!(
            prefix2,
            "store/abc/abcdef01-2345-6789-abcd-ef0123456789"
        );
    }

    #[test]
    fn test_restore_s3_uuid_path_short_uuid() {
        // Edge case: UUID shorter than expected
        let uuid = "ab";
        let prefix = uuid_s3_prefix(uuid);
        assert_eq!(prefix, "store/ab/ab");
    }

    #[test]
    fn test_restore_s3_uuid_path_full_path() {
        // Verify full path construction with relative path
        let uuid = "5f3a7b2c-1234-5678-9abc-def012345678";
        let prefix = uuid_s3_prefix(uuid);
        let full_path = format!("{}/{}", prefix, "data.bin");
        assert_eq!(
            full_path,
            "store/5f3/5f3a7b2c-1234-5678-9abc-def012345678/data.bin"
        );
    }

    #[test]
    fn test_restore_rewrite_metadata() {
        // Verify metadata rewrite updates paths, sets RefCount=0, ReadOnly=false
        let content = "3\n\
                        2\t700\n\
                        500\tstore/old/abc/data.bin\n\
                        200\tstore/old/abc/index.mrk\n\
                        5\n\
                        1\n";
        let metadata = parse_metadata(content).unwrap();
        assert!(metadata.read_only);
        assert_eq!(metadata.ref_count, 5);

        let uuid = "5f3a7b2c-1234-5678-9abc-def012345678";
        let new_prefix = uuid_s3_prefix(uuid);
        let rewritten = rewrite_metadata(&metadata, &new_prefix);

        let reparsed = parse_metadata(&rewritten).unwrap();
        assert_eq!(reparsed.version, 3);
        assert_eq!(reparsed.objects.len(), 2);
        assert!(!reparsed.read_only); // ReadOnly reset to false
        assert_eq!(reparsed.ref_count, 0); // RefCount reset to 0

        // Verify paths include the UUID prefix
        assert!(reparsed.objects[0]
            .relative_path
            .contains("5f3a7b2c-1234-5678-9abc-def012345678"));
        assert!(reparsed.objects[1]
            .relative_path
            .contains("5f3a7b2c-1234-5678-9abc-def012345678"));
    }

    #[test]
    fn test_owned_attach_params_has_s3_fields() {
        use crate::config::ClickHouseConfig;

        // Verify OwnedAttachParams has all the new S3 disk fields
        let ch = ChClient::new(&ClickHouseConfig::default()).unwrap();
        let params = OwnedAttachParams {
            ch,
            db: "default".to_string(),
            table: "trades".to_string(),
            parts: Vec::new(),
            backup_dir: PathBuf::from("/tmp"),
            table_data_path: PathBuf::from("/tmp"),
            clickhouse_uid: None,
            clickhouse_gid: None,
            engine: "MergeTree".to_string(),
            s3_client: None,
            disk_type_map: HashMap::from([("s3disk".to_string(), "s3".to_string())]),
            object_disk_server_side_copy_concurrency: 32,
            allow_object_disk_streaming: false,
            disk_remote_paths: HashMap::new(),
            table_uuid: Some("5f3a7b2c-1234-5678-9abc-def012345678".to_string()),
            parts_by_disk: HashMap::new(),
        };

        assert_eq!(params.object_disk_server_side_copy_concurrency, 32);
        assert!(!params.allow_object_disk_streaming);
        assert!(params.s3_client.is_none());
        assert_eq!(
            params.disk_type_map.get("s3disk"),
            Some(&"s3".to_string())
        );
        assert_eq!(
            params.table_uuid,
            Some("5f3a7b2c-1234-5678-9abc-def012345678".to_string())
        );
    }
}
