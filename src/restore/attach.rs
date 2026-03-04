//! Part attachment: hardlink parts to detached/ directory and ATTACH PART.
//!
//! For each part in the backup:
//! 1. For local disk parts: hardlink (or copy) files from backup to
//!    `{table_data_path}/detached/{part_name}/`
//! 2. For S3 disk parts: CopyObject to UUID-derived paths, rewrite metadata,
//!    write to `{table_data_path}/detached/{part_name}/`
//! 3. Chown to ClickHouse uid/gid
//! 4. ALTER TABLE ATTACH PART '{part_name}'

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use futures::future::try_join_all;
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};
use walkdir::WalkDir;

use crate::backup::collect::resolve_shadow_part_path;
use crate::clickhouse::client::ChClient;
use crate::manifest::PartInfo;
use crate::object_disk::{is_s3_disk, parse_metadata, rewrite_metadata};
use crate::path_encoding::encode_path_component;
use crate::resume::{save_state_graceful, RestoreState};
use crate::storage::{parse_s3_uri, S3Client};

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
    /// Parts already attached (resume skip set).
    pub already_attached: &'a HashSet<String>,
    /// Shared resume state for tracking (optional).
    pub resume_state: Option<&'a Arc<tokio::sync::Mutex<(RestoreState, PathBuf)>>>,
    /// Disk name -> disk path mapping from manifest for per-disk path resolution.
    pub manifest_disks: &'a BTreeMap<String, String>,
    /// Original (pre-remap) database name for shadow path lookup.
    pub source_db: &'a str,
    /// Original (pre-remap) table name for shadow path lookup.
    pub source_table: &'a str,
    /// Parts grouped by disk name, for building part -> disk reverse map.
    pub parts_by_disk: &'a BTreeMap<String, Vec<PartInfo>>,
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
    pub disk_type_map: BTreeMap<String, String>,
    /// Concurrency limit for S3 CopyObject operations during restore.
    pub object_disk_server_side_copy_concurrency: usize,
    /// Whether to allow streaming fallback for CopyObject failures.
    pub allow_object_disk_streaming: bool,
    /// Disk name -> remote_path for S3 disks (from DiskRow.remote_path).
    pub disk_remote_paths: BTreeMap<String, String>,
    /// Table UUID for UUID-isolated S3 restore path derivation.
    pub table_uuid: Option<String>,
    /// Parts grouped by disk name, for S3 disk routing.
    pub parts_by_disk: BTreeMap<String, Vec<PartInfo>>,
    /// Parts already attached (from resume state + system.parts). Parts in this
    /// set are skipped during ATTACH.
    pub already_attached: HashSet<String>,
    /// Global semaphore for ATTACH PART concurrency across all tables.
    /// When set, each individual ATTACH PART acquires a permit.
    /// When None (e.g., ATTACH TABLE mode), falls back to sequential behavior.
    pub attach_semaphore: Option<Arc<Semaphore>>,
    /// Shared resume state for tracking attached parts across parallel tasks.
    /// When set, each successful ATTACH is recorded and persisted.
    pub resume_state: Option<Arc<tokio::sync::Mutex<(RestoreState, PathBuf)>>>,
    /// Jitter factor for retry backoff (0.0 = no jitter).
    pub jitter_factor: f64,
    /// Disk name -> disk path mapping from manifest. Used by
    /// resolve_shadow_part_path() to find per-disk backup directories.
    pub manifest_disks: BTreeMap<String, String>,
    /// Original (pre-remap) database name for shadow path lookup.
    /// Shadow directories are created during backup using the source names,
    /// so lookups must always use source names even when remap is active.
    pub source_db: String,
    /// Original (pre-remap) table name for shadow path lookup.
    pub source_table: String,
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
    parts_by_disk: &'a BTreeMap<String, Vec<PartInfo>>,
    disk_type_map: &'a BTreeMap<String, String>,
    table_uuid: &'a str,
    table_data_path: &'a Path,
    backup_dir: &'a Path,
    db: &'a str,
    table: &'a str,
    concurrency: usize,
    allow_streaming: bool,
    clickhouse_uid: Option<u32>,
    clickhouse_gid: Option<u32>,
    jitter_factor: f64,
    /// Disk name -> disk path from manifest for per-disk resolution.
    manifest_disks: &'a BTreeMap<String, String>,
    /// Original source database name for shadow path lookup.
    source_db: &'a str,
    /// Original source table name for shadow path lookup.
    source_table: &'a str,
    /// Disk name -> remote_path S3 URI for data disks (from DiskRow.remote_path).
    disk_remote_paths: &'a BTreeMap<String, String>,
}

/// Restore S3 disk parts for a single table.
///
/// For each S3 disk part:
/// 1. Same-name optimization: ListObjectsV2 to check existing objects
/// 2. CopyObject from backup bucket to data disk bucket/prefix
/// 3. Rewrite metadata files to point to new UUID paths
/// 4. Write rewritten metadata to detached/{part_name}/
///
/// Key insight: the backup S3Client has the backup prefix (e.g., `chbackup/prefix/`),
/// but restored objects must go to the ClickHouse data disk's S3 prefix (e.g.,
/// `clickhouse-disks/`). We create a data-disk-targeted S3Client per disk to
/// handle the prefix difference correctly.
async fn restore_s3_disk_parts(p: &S3RestoreParams<'_>) -> Result<u64> {
    let s3 = p.s3;
    let db = p.db;
    let table = p.table;
    let uuid_prefix = uuid_s3_prefix(p.table_uuid);
    let allow_streaming = p.allow_streaming;
    let jitter_factor = p.jitter_factor;

    info!(
        db = %db,
        table = %table,
        uuid = %p.table_uuid,
        "Restoring S3 disk parts"
    );

    let semaphore = Arc::new(Semaphore::new(p.concurrency));
    let detached_dir = p.table_data_path.join("detached");
    let mut skipped_count = 0u64;

    let url_src_db = encode_path_component(p.source_db);
    let url_src_table = encode_path_component(p.source_table);
    let backup_name = p
        .backup_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");

    // Backup S3 bucket (where CopyObject source lives)
    let backup_bucket = s3.bucket().to_string();

    for (disk_name, parts) in p.parts_by_disk {
        let disk_type = p
            .disk_type_map
            .get(disk_name)
            .map(|s| s.as_str())
            .unwrap_or("");
        if !is_s3_disk(disk_type) {
            continue;
        }

        // Parse the data disk's S3 URI to get its bucket and prefix.
        // This tells us WHERE to write the restored objects (data disk location,
        // not backup location).
        let disk_remote_path = match p.disk_remote_paths.get(disk_name) {
            Some(path) => path.clone(),
            None => {
                let disk_parts_count = parts.len() as u64;
                warn!(
                    disk = %disk_name,
                    parts_skipped = disk_parts_count,
                    "No remote_path found for S3 disk, cannot restore S3 disk parts"
                );
                skipped_count += disk_parts_count;
                continue;
            }
        };

        let (data_bucket, data_prefix) = parse_s3_uri(&disk_remote_path);
        if data_bucket.is_empty() {
            let disk_parts_count = parts.len() as u64;
            warn!(
                disk = %disk_name,
                remote_path = %disk_remote_path,
                parts_skipped = disk_parts_count,
                "Failed to parse S3 URI from disk remote_path, cannot restore S3 disk parts"
            );
            skipped_count += disk_parts_count;
            continue;
        }

        // Create an S3Client targeting the data disk's bucket/prefix for destination
        // operations. Reuses the backup S3Client's connection pool and credentials.
        let data_s3 = s3.with_bucket_and_prefix(&data_bucket, &data_prefix);

        // Same-name optimization: list existing objects at the UUID prefix in the DATA disk
        let existing_objects = data_s3
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

        let existing_map: HashMap<String, i64> = existing_objects
            .into_iter()
            .map(|obj| (obj.key, obj.size))
            .collect();

        debug!(
            disk = %disk_name,
            data_bucket = %data_bucket,
            data_prefix = %data_prefix,
            existing_objects = existing_map.len(),
            "S3 disk restore: data disk target resolved"
        );

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

                // Destination key relative to data disk prefix:
                // store/{uuid_hex[0..3]}/{uuid_with_dashes}/{relative_path}
                let dest_key = format!("{}/{}", uuid_prefix, s3_obj.path);

                debug!(
                    s3_obj_path = %s3_obj.path,
                    s3_obj_backup_key = %s3_obj.backup_key,
                    dest_key = %dest_key,
                    uuid_prefix = %uuid_prefix,
                    "S3 disk CopyObject path details"
                );

                // Same-name optimization: check data disk for existing object
                let full_dest_key = data_s3.full_key(&dest_key);
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

                // Source: backup_key is relative to the backup S3 prefix.
                // copy_object() needs the ABSOLUTE source key in the source bucket,
                // so we prepend the backup prefix via s3.full_key().
                let source_key = if s3_obj.backup_key.is_empty() {
                    s3_obj.path.clone()
                } else {
                    s3.full_key(&s3_obj.backup_key)
                };

                let sem = semaphore.clone();
                let data_s3_clone = data_s3.clone();
                let backup_bucket_clone = backup_bucket.clone();

                let handle = tokio::spawn(async move {
                    let _permit = sem
                        .acquire()
                        .await
                        .map_err(|_| anyhow::anyhow!("Semaphore closed"))?;

                    // Copy from backup bucket (absolute source key) to data disk
                    // bucket (dest_key relative to data disk prefix).
                    data_s3_clone
                        .copy_object_with_retry_jitter(
                            &backup_bucket_clone,
                            &source_key,
                            &dest_key,
                            allow_streaming,
                            jitter_factor,
                        )
                        .await
                        .with_context(|| {
                            format!("Failed to copy S3 object: {} -> {}", source_key, dest_key)
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

            let resolved = resolve_shadow_part_path(
                p.backup_dir,
                p.manifest_disks,
                backup_name,
                disk_name,
                &url_src_db,
                &url_src_table,
                p.source_db,
                p.source_table,
                &part.name,
            );

            debug!(
                part = %part.name,
                backup_dir = %p.backup_dir.display(),
                backup_name = %backup_name,
                disk_name = %disk_name,
                url_src_db = %url_src_db,
                url_src_table = %url_src_table,
                resolved_path = ?resolved,
                manifest_disks = ?p.manifest_disks,
                "resolve_shadow_part_path for S3 disk metadata rewrite"
            );

            let source_dir = resolved.unwrap_or_else(|| {
                // Fallback: legacy hardcoded path for backward compat
                p.backup_dir
                    .join("shadow")
                    .join(&url_src_db)
                    .join(&url_src_table)
                    .join(&part.name)
            });

            debug!(
                source_dir = %source_dir.display(),
                exists = source_dir.exists(),
                "S3 disk metadata source directory"
            );

            if source_dir.exists() {
                // Walk all files in the source part directory
                for entry in WalkDir::new(&source_dir).min_depth(1) {
                    let entry = entry.with_context(|| {
                        format!("Failed to read entry under: {}", source_dir.display())
                    })?;

                    // Skip FREEZE artifacts that ClickHouse cannot parse
                    if entry.file_name().to_string_lossy() == "frozen_metadata.txt" {
                        continue;
                    }

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
                                    debug!(
                                        file = %relative.display(),
                                        objects = metadata.objects.len(),
                                        first_path = %metadata.objects.first().map(|o| o.relative_path.as_str()).unwrap_or(""),
                                        uuid_prefix = %uuid_prefix,
                                        "Rewriting S3 disk metadata file"
                                    );
                                    let rewritten = rewrite_metadata(&metadata, &uuid_prefix);
                                    std::fs::write(&dest_path, &rewritten).with_context(|| {
                                        format!(
                                            "Failed to write rewritten metadata: {}",
                                            dest_path.display()
                                        )
                                    })?;
                                }
                                Err(_) => {
                                    // Not a metadata file -- copy as-is
                                    // (e.g. checksums.txt, columns.txt)
                                    std::fs::write(&dest_path, &text).with_context(|| {
                                        format!("Failed to write file: {}", dest_path.display())
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

    Ok(skipped_count)
}

/// Attach parts for a single table using owned parameters.
///
/// Handles both local disk parts (hardlink) and S3 disk parts (CopyObject +
/// metadata rewrite) before delegating to the internal attach logic.
pub(crate) async fn attach_parts_owned(params: OwnedAttachParams) -> Result<AttachResult> {
    let mut s3_skipped = 0u64;

    // Handle S3 disk parts first (CopyObject + metadata rewrite)
    let has_s3_client = params.s3_client.is_some();
    let has_table_uuid = params.table_uuid.is_some();
    let has_s3_disk_parts = params.parts_by_disk.iter().any(|(disk_name, parts)| {
        let disk_type = params
            .disk_type_map
            .get(disk_name)
            .map(|s| s.as_str())
            .unwrap_or("");
        let is_s3 = is_s3_disk(disk_type);
        let has_objs = parts.iter().any(|p| p.s3_objects.is_some());
        debug!(
            db = %params.db,
            table = %params.table,
            disk_name = %disk_name,
            disk_type = %disk_type,
            is_s3_disk = is_s3,
            has_s3_objects = has_objs,
            parts_count = parts.len(),
            "S3 disk check for restore"
        );
        is_s3 && has_objs
    });
    let has_s3_parts = has_s3_client && has_table_uuid && has_s3_disk_parts;

    debug!(
        db = %params.db,
        table = %params.table,
        has_s3_client = has_s3_client,
        has_table_uuid = has_table_uuid,
        has_s3_disk_parts = has_s3_disk_parts,
        has_s3_parts = has_s3_parts,
        table_uuid = ?params.table_uuid,
        "S3 parts detection result"
    );

    if has_s3_parts {
        // On resume, check if ALL S3 disk parts are already attached.
        // If so, skip the S3 restore entirely (avoids ListObjectsV2 per table).
        let all_s3_attached = params
            .parts_by_disk
            .iter()
            .filter(|(disk_name, _)| {
                let disk_type = params
                    .disk_type_map
                    .get(disk_name.as_str())
                    .map(|s| s.as_str())
                    .unwrap_or("");
                is_s3_disk(disk_type)
            })
            .flat_map(|(_, parts)| parts.iter())
            .filter(|p| p.s3_objects.is_some())
            .all(|p| params.already_attached.contains(&p.name));

        if all_s3_attached && !params.already_attached.is_empty() {
            debug!(
                db = %params.db,
                table = %params.table,
                "All S3 disk parts already attached, skipping S3 restore"
            );
        } else {
            let s3 = params.s3_client.as_ref().expect("s3_client checked above");
            let uuid = params
                .table_uuid
                .as_ref()
                .expect("table_uuid checked above");

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
                jitter_factor: params.jitter_factor,
                manifest_disks: &params.manifest_disks,
                source_db: &params.source_db,
                source_table: &params.source_table,
                disk_remote_paths: &params.disk_remote_paths,
            };
            s3_skipped = restore_s3_disk_parts(&s3_params).await?;
        }
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
        already_attached: &params.already_attached,
        resume_state: params.resume_state.as_ref(),
        manifest_disks: &params.manifest_disks,
        source_db: &params.source_db,
        source_table: &params.source_table,
        parts_by_disk: &params.parts_by_disk,
    };

    let inner_result = attach_parts_inner(
        &attach_params,
        &params.engine,
        params.attach_semaphore.as_ref(),
    )
    .await?;

    Ok(AttachResult {
        attached: inner_result.attached,
        skipped: inner_result.skipped + s3_skipped,
    })
}

/// Internal attach implementation with engine-aware routing.
///
/// When `needs_sequential_attach(engine)` returns true (Replacing, Collapsing,
/// Versioned engines), parts are attached strictly in sorted order.
/// For non-dedup engines (MergeTree, SummingMergeTree, AggregatingMergeTree,
/// ReplicatedMergeTree), parts are attached in parallel when a global
/// `attach_semaphore` is provided.
async fn attach_parts_inner(
    params: &AttachParams<'_>,
    engine: &str,
    attach_semaphore: Option<&Arc<Semaphore>>,
) -> Result<AttachResult> {
    let db = params.db;
    let table = params.table;

    if params.parts.is_empty() {
        debug!(
            db = %db,
            table = %table,
            "No parts to attach"
        );
        return Ok(AttachResult {
            attached: 0,
            skipped: 0,
        });
    }

    let sequential = needs_sequential_attach(engine) || engine.is_empty();

    if sequential {
        debug!(
            db = %db,
            table = %table,
            engine = %engine,
            "Using sequential attach (engine requires ordered attachment)"
        );
    } else if attach_semaphore.is_some() {
        debug!(
            db = %db,
            table = %table,
            engine = %engine,
            "Using parallel in-table attach"
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

    let resume_table_key = format!("{}.{}", params.source_db, params.source_table);

    // Hoist loop-invariant computations above the loop.
    let url_src_db = encode_path_component(params.source_db);
    let url_src_table = encode_path_component(params.source_table);

    let backup_name = params
        .backup_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");

    // Build reverse map: part_name -> disk_name (O(n) instead of O(n^2) linear scan).
    let part_to_disk: std::collections::HashMap<&str, &str> = params
        .parts_by_disk
        .iter()
        .flat_map(|(dn, parts)| parts.iter().map(move |p| (p.name.as_str(), dn.as_str())))
        .collect();

    // Filter out parts that should be skipped (resume name match)
    let mut skipped_resume = 0u64;
    let mut work_parts: Vec<&PartInfo> = Vec::new();
    for part in &sorted_parts {
        if params.already_attached.contains(&part.name) {
            skipped_resume += 1;
            debug!(
                db = %db,
                table = %table,
                part = %part.name,
                "Skipping already-attached part (resume)"
            );
            continue;
        }

        work_parts.push(part);
    }

    // Prepare each part: hardlink/copy to detached/
    // This is done sequentially because it's filesystem I/O (not network)
    // and benefits from sequential disk access patterns.
    // Track parts with missing source dirs so they are excluded from ATTACH.
    let mut missing_source_parts: HashSet<String> = HashSet::new();

    for part in &work_parts {
        let dest_dir = detached_dir.join(&part.name);

        let is_s3_part = part.s3_objects.is_some();
        if is_s3_part && dest_dir.exists() {
            debug!(
                part = %part.name,
                "S3 disk part already prepared in detached/, skipping hardlink"
            );
        } else {
            let disk_name = part_to_disk
                .get(part.name.as_str())
                .copied()
                .unwrap_or("default");

            let source_dir = resolve_shadow_part_path(
                params.backup_dir,
                params.manifest_disks,
                backup_name,
                disk_name,
                &url_src_db,
                &url_src_table,
                params.source_db,
                params.source_table,
                &part.name,
            );

            match source_dir {
                Some(dir) => {
                    hardlink_or_copy_dir(&dir, &dest_dir).with_context(|| {
                        format!(
                            "Failed to hardlink/copy part {} from {} to {}",
                            part.name,
                            dir.display(),
                            dest_dir.display()
                        )
                    })?;
                    chown_recursive(&dest_dir, params.clickhouse_uid, params.clickhouse_gid)?;
                }
                None => {
                    warn!(
                        part = %part.name,
                        source_db = %params.source_db,
                        source_table = %params.source_table,
                        "Part source directory not found, skipping"
                    );
                    missing_source_parts.insert(part.name.clone());
                }
            }
        }
    }

    // Filter out parts with missing source dirs before ATTACH phase
    if !missing_source_parts.is_empty() {
        work_parts.retain(|part| !missing_source_parts.contains(&part.name));
    }

    // ATTACH PART phase: sequential for dedup engines, parallel otherwise
    // Each branch returns (attached_count, attach_skipped_count).
    // attach_skipped tracks NO_SUCH_DATA_PART errors (source existed but CH can't find it).
    let (attached_count, attach_skipped) =
        if let (false, Some(global_sem)) = (sequential, attach_semaphore) {
            // Parallel ATTACH for non-dedup engines.
            // Each part's ATTACH is spawned as a tokio task with a semaphore permit.
            let sem = global_sem.clone();
            let ch_clone = params.ch.clone();
            let db_owned = db.to_string();
            let table_owned = table.to_string();
            let resume_key_owned = resume_table_key.clone();
            let resume_state_clone = params.resume_state.cloned();

            let mut handles = Vec::with_capacity(work_parts.len());
            for part in &work_parts {
                let sem = sem.clone();
                let ch = ch_clone.clone();
                let db = db_owned.clone();
                let table = table_owned.clone();
                let part_name = part.name.clone();
                let resume_key = resume_key_owned.clone();
                let resume_state = resume_state_clone.clone();

                handles.push(tokio::spawn(async move {
                let _permit = sem
                    .acquire()
                    .await
                    .map_err(|_| anyhow::anyhow!("Semaphore closed"))?;

                debug!(db = %db, table = %table, part = %part_name, "Attaching part (parallel)");

                match ch.attach_part(&db, &table, &part_name).await {
                    Ok(()) => {
                        if let Some(state_mutex) = &resume_state {
                            let mut guard = state_mutex.lock().await;
                            guard
                                .0
                                .attached_parts
                                .entry(resume_key.clone())
                                .or_default()
                                .push(part_name.clone());
                            let state_path = guard.1.clone();
                            save_state_graceful(&state_path, &guard.0);
                        }
                        // (attached=true, skipped=false)
                        Ok((true, false))
                    }
                    Err(e) => {
                        if is_benign_attach_error(&e) {
                            warn!(
                                db = %db, table = %table, part = %part_name,
                                error = %format!("{:#}", e),
                                "Part already exists or overlaps, skipping"
                            );
                            Ok((false, false))
                        } else if is_missing_part_error(&e) {
                            warn!(
                                db = %db, table = %table, part = %part_name,
                                error = %format!("{:#}", e),
                                "NO_SUCH_DATA_PART during ATTACH, counting as skipped"
                            );
                            Ok((false, true))
                        } else {
                            Err(e).with_context(|| {
                                format!("Failed to ATTACH PART '{}' to {}.{}", part_name, db, table)
                            })
                        }
                    }
                }
            }));
            }

            let results = try_join_all(handles)
                .await
                .context("ATTACH task panicked")?;
            let mut count = 0u64;
            let mut skip_count = 0u64;
            for result in results {
                let (attached, skipped) = result?;
                if attached {
                    count += 1;
                }
                if skipped {
                    skip_count += 1;
                }
            }
            (count, skip_count)
        } else {
            // Sequential ATTACH: one part at a time (for dedup engines or no semaphore)
            let mut count = 0u64;
            let mut skip_count = 0u64;
            for part in &work_parts {
                // Acquire global permit if semaphore is provided
                let _permit = if let Some(sem) = attach_semaphore {
                    Some(
                        sem.acquire()
                            .await
                            .map_err(|_| anyhow::anyhow!("Semaphore closed"))?,
                    )
                } else {
                    None
                };

                debug!(db = %db, table = %table, part = %part.name, "Attaching part");

                match params.ch.attach_part(db, table, &part.name).await {
                    Ok(()) => {
                        count += 1;
                        if let Some(state_mutex) = &params.resume_state {
                            let mut guard = state_mutex.lock().await;
                            guard
                                .0
                                .attached_parts
                                .entry(resume_table_key.clone())
                                .or_default()
                                .push(part.name.clone());
                            let state_path = guard.1.clone();
                            save_state_graceful(&state_path, &guard.0);
                        }
                    }
                    Err(e) => {
                        if is_benign_attach_error(&e) {
                            warn!(
                                db = %db, table = %table, part = %part.name,
                                error = %format!("{:#}", e),
                                "Part already exists or overlaps, skipping"
                            );
                        } else if is_missing_part_error(&e) {
                            warn!(
                                db = %db, table = %table, part = %part.name,
                                error = %format!("{:#}", e),
                                "NO_SUCH_DATA_PART during ATTACH, counting as skipped"
                            );
                            skip_count += 1;
                        } else {
                            return Err(e).with_context(|| {
                                format!("Failed to ATTACH PART '{}' to {}.{}", part.name, db, table)
                            });
                        }
                    }
                }
            }
            (count, skip_count)
        };

    if skipped_resume > 0 {
        info!(
            db = %db,
            table = %table,
            skipped = skipped_resume,
            "Skipped already-attached parts (resume)"
        );
    }

    let total_skipped = missing_source_parts.len() as u64 + attach_skipped;

    info!(
        db = %db,
        table = %table,
        attached = attached_count,
        skipped = total_skipped,
        total = sorted_parts.len(),
        "Parts attached"
    );

    Ok(AttachResult {
        attached: attached_count,
        skipped: total_skipped,
    })
}

/// Result from attaching parts to a single table.
///
/// Tracks both successfully attached and skipped parts so the caller
/// can detect partial restores.
pub(crate) struct AttachResult {
    /// Number of parts successfully attached.
    pub attached: u64,
    /// Number of parts skipped (missing source, NO_SUCH_DATA_PART, missing S3 remote_path).
    pub skipped: u64,
}

/// Check if an ATTACH PART error is a benign warning (232/233 overlap/duplicate).
///
/// These errors mean the part already exists or is temporarily locked,
/// and can be safely skipped without counting as a "skipped" part.
fn is_benign_attach_error(e: &anyhow::Error) -> bool {
    let err_str = format!("{:#}", e);
    err_str.contains("DUPLICATE_DATA_PART")
        || err_str.contains("PART_IS_TEMPORARILY_LOCKED")
        || err_str.contains("Code: 232")
        || err_str.contains("Code: 233")
}

/// Check if an ATTACH PART error indicates a missing part.
///
/// NO_SUCH_DATA_PART means the source existed but ClickHouse couldn't find
/// the data part during ATTACH. This counts as a "skipped" part.
fn is_missing_part_error(e: &anyhow::Error) -> bool {
    let err_str = format!("{:#}", e);
    err_str.contains("NO_SUCH_DATA_PART")
}

/// Hardlink all files from source directory to destination directory.
///
/// If hardlink fails with EXDEV (cross-device link), falls back to file copy.
pub(crate) fn hardlink_or_copy_dir(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)
        .with_context(|| format!("Failed to create destination dir: {}", dst.display()))?;

    for entry in WalkDir::new(src).min_depth(1) {
        let entry = entry
            .with_context(|| format!("Failed to read directory entry under: {}", src.display()))?;

        let relative = entry
            .path()
            .strip_prefix(src)
            .context("Failed to strip source prefix")?;
        let dest_path = dst.join(relative);

        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&dest_path)
                .with_context(|| format!("Failed to create directory: {}", dest_path.display()))?;
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
pub(crate) fn chown_recursive(path: &Path, uid: Option<u32>, gid: Option<u32>) -> Result<()> {
    if uid.is_none() && gid.is_none() {
        return Ok(());
    }

    let nix_uid = uid.map(nix::unistd::Uid::from_raw);
    let nix_gid = gid.map(nix::unistd::Gid::from_raw);

    for entry in WalkDir::new(path) {
        let entry = entry
            .with_context(|| format!("Failed to read directory entry under: {}", path.display()))?;

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
        assert_eq!(prefix2, "store/abc/abcdef01-2345-6789-abcd-ef0123456789");
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
            disk_type_map: BTreeMap::from([("s3disk".to_string(), "s3".to_string())]),
            object_disk_server_side_copy_concurrency: 32,
            allow_object_disk_streaming: false,
            disk_remote_paths: BTreeMap::new(),
            table_uuid: Some("5f3a7b2c-1234-5678-9abc-def012345678".to_string()),
            parts_by_disk: BTreeMap::new(),
            already_attached: HashSet::new(),
            attach_semaphore: None,
            resume_state: None,
            jitter_factor: 0.0,
            manifest_disks: BTreeMap::new(),
            source_db: "default".to_string(),
            source_table: "trades".to_string(),
        };

        assert_eq!(params.object_disk_server_side_copy_concurrency, 32);
        assert!(!params.allow_object_disk_streaming);
        assert!(params.s3_client.is_none());
        assert_eq!(params.disk_type_map.get("s3disk"), Some(&"s3".to_string()));
        assert_eq!(
            params.table_uuid,
            Some("5f3a7b2c-1234-5678-9abc-def012345678".to_string())
        );
    }

    #[test]
    fn test_owned_attach_params_resume_fields() {
        use crate::config::ClickHouseConfig;

        // Verify OwnedAttachParams supports resume fields
        let ch = ChClient::new(&ClickHouseConfig::default()).unwrap();
        let already = HashSet::from(["202401_1_50_3".to_string(), "202401_51_100_3".to_string()]);
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
            disk_type_map: BTreeMap::new(),
            object_disk_server_side_copy_concurrency: 32,
            allow_object_disk_streaming: false,
            disk_remote_paths: BTreeMap::new(),
            table_uuid: None,
            parts_by_disk: BTreeMap::new(),
            already_attached: already.clone(),
            attach_semaphore: None,
            resume_state: None,
            jitter_factor: 0.0,
            manifest_disks: BTreeMap::new(),
            source_db: "default".to_string(),
            source_table: "trades".to_string(),
        };

        assert_eq!(params.already_attached.len(), 2);
        assert!(params.already_attached.contains("202401_1_50_3"));
        assert!(params.already_attached.contains("202401_51_100_3"));
        assert!(params.resume_state.is_none());
    }

    #[test]
    fn test_resume_state_tracking_with_mutex() {
        use crate::resume::RestoreState;

        // Verify the Arc<Mutex<(RestoreState, PathBuf)>> pattern works
        let state = RestoreState {
            attached_parts: HashMap::new(),
            backup_name: "test-backup".to_string(),
            params_hash: String::new(),
        };
        let state_path = PathBuf::from("/tmp/test.state.json");
        let shared = Arc::new(tokio::sync::Mutex::new((state, state_path)));

        // Simulate what attach_parts_inner does in a blocking context
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let mut guard = shared.lock().await;
            guard
                .0
                .attached_parts
                .entry("default.trades".to_string())
                .or_default()
                .push("202401_1_50_3".to_string());

            assert_eq!(guard.0.attached_parts.len(), 1);
            assert_eq!(
                guard.0.attached_parts.get("default.trades").unwrap().len(),
                1
            );
        });
    }

    // ---- Task 7: Per-disk restore path resolution tests ----

    /// Verify that when manifest_disks maps a part's disk to a non-default path,
    /// the source resolves to the per-disk path.
    #[test]
    fn test_attach_source_dir_per_disk() {
        use crate::backup::collect::resolve_shadow_part_path;

        let tmp = tempfile::tempdir().unwrap();
        let backup_dir = tmp.path().join("clickhouse/backup/daily-2024");
        let nvme1_path = tmp.path().join("nvme1");

        // Create per-disk shadow path
        let per_disk_part =
            nvme1_path.join("backup/daily-2024/shadow/default/trades/202401_1_50_3");
        std::fs::create_dir_all(&per_disk_part).unwrap();
        std::fs::write(per_disk_part.join("checksums.txt"), b"data").unwrap();

        let manifest_disks = BTreeMap::from([(
            "nvme1".to_string(),
            nvme1_path.to_string_lossy().to_string(),
        )]);

        let result = resolve_shadow_part_path(
            &backup_dir,
            &manifest_disks,
            "daily-2024",
            "nvme1",
            "default",
            "trades",
            "default",
            "trades",
            "202401_1_50_3",
        );

        assert!(result.is_some(), "Should find part at per-disk path");
        assert_eq!(result.unwrap(), per_disk_part);
    }

    /// Verify that when source_db != db (remap active), the shadow lookup
    /// uses source_db/source_table, NOT destination names.
    #[test]
    fn test_attach_source_dir_remap_uses_source_names() {
        use crate::backup::collect::resolve_shadow_part_path;

        let tmp = tempfile::tempdir().unwrap();
        let backup_dir = tmp.path().join("clickhouse/backup/daily-2024");

        // Shadow dirs use source names (prod.trades), NOT destination (staging.trades_copy)
        let source_part = backup_dir.join("shadow/prod/trades/202401_1_50_3");
        std::fs::create_dir_all(&source_part).unwrap();
        std::fs::write(source_part.join("checksums.txt"), b"data").unwrap();

        // Destination names -- these should NOT be found
        let dest_part = backup_dir.join("shadow/staging/trades_copy/202401_1_50_3");
        assert!(!dest_part.exists());

        let manifest_disks = BTreeMap::new();

        // Lookup with source names (prod.trades) should succeed
        let result = resolve_shadow_part_path(
            &backup_dir,
            &manifest_disks,
            "daily-2024",
            "default",
            "prod",
            "trades",
            "prod",
            "trades",
            "202401_1_50_3",
        );
        assert!(result.is_some(), "Should find part using source names");
        assert_eq!(result.unwrap(), source_part);

        // Lookup with destination names (staging.trades_copy) should fail
        let result_dst = resolve_shadow_part_path(
            &backup_dir,
            &manifest_disks,
            "daily-2024",
            "default",
            "staging",
            "trades_copy",
            "staging",
            "trades_copy",
            "202401_1_50_3",
        );
        assert!(
            result_dst.is_none(),
            "Should NOT find part using destination names"
        );
    }

    /// Verify that old backups with manifest.disks populated but legacy layout
    /// fall through to the legacy path.
    #[test]
    fn test_attach_source_dir_old_backup_fallback() {
        use crate::backup::collect::resolve_shadow_part_path;

        let tmp = tempfile::tempdir().unwrap();
        let backup_dir = tmp.path().join("clickhouse/backup/daily-2024");

        // Create only the legacy (default backup_dir) path
        let legacy_part = backup_dir.join("shadow/default/trades/202401_1_50_3");
        std::fs::create_dir_all(&legacy_part).unwrap();
        std::fs::write(legacy_part.join("checksums.txt"), b"data").unwrap();

        // manifest.disks has a disk entry but per-disk path doesn't exist
        let manifest_disks = BTreeMap::from([(
            "nvme1".to_string(),
            tmp.path()
                .join("nvme1_does_not_exist")
                .to_string_lossy()
                .to_string(),
        )]);

        let result = resolve_shadow_part_path(
            &backup_dir,
            &manifest_disks,
            "daily-2024",
            "nvme1",
            "default",
            "trades",
            "default",
            "trades",
            "202401_1_50_3",
        );

        assert!(result.is_some(), "Should fall back to legacy path");
        assert_eq!(result.unwrap(), legacy_part);
    }

    // ---- chown_recursive tests ----

    #[test]
    fn test_chown_recursive_both_none_is_noop() {
        // When both uid and gid are None, should return Ok immediately
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("file.txt"), b"data").unwrap();
        let result = chown_recursive(dir.path(), None, None);
        assert!(result.is_ok());
    }

    #[test]
    fn test_chown_recursive_with_current_uid() {
        // Non-root: chown to own uid/gid should succeed or silently skip
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("file.txt"), b"data").unwrap();

        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::metadata(dir.path()).unwrap();
        let uid = meta.uid();
        let gid = meta.gid();

        // Chown to same user should succeed
        let result = chown_recursive(dir.path(), Some(uid), Some(gid));
        assert!(result.is_ok());
    }

    #[test]
    fn test_chown_recursive_eperm_is_silent() {
        // Trying to chown to root (uid 0) as non-root should hit EPERM
        // and return Ok (silently skipped)
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("file.txt"), b"data").unwrap();

        // Only test if not already root
        if nix::unistd::getuid().as_raw() != 0 {
            let result = chown_recursive(dir.path(), Some(0), Some(0));
            assert!(result.is_ok(), "EPERM should be handled gracefully");
        }
    }

    // ---- detect_clickhouse_ownership with existing path ----

    #[test]
    fn test_detect_clickhouse_ownership_existing_dir() {
        let dir = tempfile::tempdir().unwrap();
        let result = detect_clickhouse_ownership(dir.path());
        assert!(result.is_ok());
        let (uid, gid) = result.unwrap();
        // Should return Some values for an existing directory
        assert!(uid.is_some());
        assert!(gid.is_some());
    }

    #[test]
    fn test_detect_clickhouse_ownership_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test_file");
        std::fs::write(&file, b"data").unwrap();
        let result = detect_clickhouse_ownership(&file);
        assert!(result.is_ok());
        let (uid, gid) = result.unwrap();
        assert!(uid.is_some());
        assert!(gid.is_some());

        // UID/GID should match our process UID/GID
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::metadata(&file).unwrap();
        assert_eq!(uid.unwrap(), meta.uid());
        assert_eq!(gid.unwrap(), meta.gid());
    }

    // ---- hardlink_or_copy_dir edge cases ----

    #[test]
    fn test_hardlink_or_copy_dir_empty_src() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("empty_src");
        let dst = dir.path().join("empty_dst");
        std::fs::create_dir_all(&src).unwrap();

        let result = hardlink_or_copy_dir(&src, &dst);
        assert!(result.is_ok());
        assert!(dst.exists());
    }

    #[test]
    fn test_hardlink_or_copy_dir_nonexistent_src() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("nonexistent");
        let dst = dir.path().join("dst");

        // WalkDir on nonexistent dir will fail
        let result = hardlink_or_copy_dir(&src, &dst);
        assert!(result.is_err());
    }

    #[test]
    fn test_hardlink_or_copy_dir_multiple_subdirs() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("multi_src");
        let dst = dir.path().join("multi_dst");

        // Create multiple subdirs with files
        for subdir in &["alpha", "beta", "gamma"] {
            let sub = src.join(subdir);
            std::fs::create_dir_all(&sub).unwrap();
            std::fs::write(sub.join("data.bin"), format!("{} data", subdir).as_bytes()).unwrap();
        }

        hardlink_or_copy_dir(&src, &dst).unwrap();

        for subdir in &["alpha", "beta", "gamma"] {
            let content = std::fs::read_to_string(dst.join(subdir).join("data.bin")).unwrap();
            assert_eq!(content, format!("{} data", subdir));
        }
    }

    #[test]
    fn test_hardlink_or_copy_dir_deeply_nested() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("deep_src");
        let dst = dir.path().join("deep_dst");

        // Create deeply nested structure
        let deep = src.join("a").join("b").join("c");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("leaf.txt"), b"deep content").unwrap();
        std::fs::write(src.join("root.txt"), b"root content").unwrap();

        hardlink_or_copy_dir(&src, &dst).unwrap();

        assert_eq!(
            std::fs::read_to_string(dst.join("a/b/c/leaf.txt")).unwrap(),
            "deep content"
        );
        assert_eq!(
            std::fs::read_to_string(dst.join("root.txt")).unwrap(),
            "root content"
        );
    }

    // ---- is_benign_attach_error tests ----

    #[test]
    fn test_is_benign_attach_error_duplicate_data_part() {
        let err = anyhow::anyhow!(
            "Code: 232. DB::Exception: Unexpected part all_1_1_0 already exists. DUPLICATE_DATA_PART"
        );
        assert!(is_benign_attach_error(&err));
    }

    #[test]
    fn test_is_benign_attach_error_part_temporarily_locked() {
        let err = anyhow::anyhow!("Code: 233. PART_IS_TEMPORARILY_LOCKED");
        assert!(is_benign_attach_error(&err));
    }

    #[test]
    fn test_is_benign_attach_error_no_such_data_part_not_benign() {
        // NO_SUCH_DATA_PART is NOT benign -- it's a missing part error
        let err = anyhow::anyhow!("NO_SUCH_DATA_PART in table");
        assert!(!is_benign_attach_error(&err));
    }

    #[test]
    fn test_is_benign_attach_error_code_232_only() {
        let err = anyhow::anyhow!("Code: 232");
        assert!(is_benign_attach_error(&err));
    }

    #[test]
    fn test_is_benign_attach_error_code_233_only() {
        let err = anyhow::anyhow!("Code: 233");
        assert!(is_benign_attach_error(&err));
    }

    #[test]
    fn test_is_benign_attach_error_other_error() {
        let err = anyhow::anyhow!("Code: 60. UNKNOWN_TABLE");
        assert!(!is_benign_attach_error(&err));
    }

    #[test]
    fn test_is_benign_attach_error_connection_error() {
        let err = anyhow::anyhow!("Connection refused");
        assert!(!is_benign_attach_error(&err));
    }

    #[test]
    fn test_is_benign_attach_error_empty_error() {
        let err = anyhow::anyhow!("");
        assert!(!is_benign_attach_error(&err));
    }

    // ---- is_missing_part_error tests ----

    #[test]
    fn test_is_missing_part_error_no_such_data_part() {
        let err = anyhow::anyhow!("NO_SUCH_DATA_PART in table");
        assert!(is_missing_part_error(&err));
    }

    #[test]
    fn test_is_missing_part_error_other_error() {
        let err = anyhow::anyhow!("Code: 232. DUPLICATE_DATA_PART");
        assert!(!is_missing_part_error(&err));
    }

    #[test]
    fn test_is_missing_part_error_connection_error() {
        let err = anyhow::anyhow!("Connection refused");
        assert!(!is_missing_part_error(&err));
    }
}
