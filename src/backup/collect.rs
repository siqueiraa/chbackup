//! Shadow directory walk and part collection.
//!
//! After FREEZE, ClickHouse places hardlinks in:
//!   `{data_path}/shadow/{freeze_name}/store/{shard_hex_prefix}/{table_hex_uuid}/{part_name}/...`
//!
//! This module walks those directories, hardlinks files to the backup staging area,
//! and computes CRC64 checksums.
//!
//! For S3 disk parts, metadata files are parsed to extract S3 object references
//! instead of hardlinking data (the data lives on S3, not the local filesystem).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::{debug, info, warn};
use walkdir::WalkDir;

use super::checksum::compute_crc64;
use crate::clickhouse::client::TableRow;
use crate::manifest::{PartInfo, S3ObjectInfo};
use crate::object_disk;
use crate::path_encoding::encode_path_component;
use crate::table_filter::is_disk_excluded;

/// Compute the per-disk backup directory for a given disk.
///
/// Returns `{disk_path}/backup/{backup_name}`. For the default disk where
/// `disk_path == data_path`, this produces the same path as the existing
/// `{data_path}/backup/{backup_name}` layout (zero behavior change).
pub fn per_disk_backup_dir(disk_path: &str, backup_name: &str) -> PathBuf {
    PathBuf::from(disk_path).join("backup").join(backup_name)
}

/// Resolve the shadow part path with strict fallback order:
/// 1. Per-disk candidate (encoded):  {disk_path}/backup/{name}/shadow/{db}/{table}/{part}/
/// 2. Legacy default (encoded):      {backup_dir}/shadow/{db}/{table}/{part}/
/// 3. Legacy default (plain):        {backup_dir}/shadow/{plain_db}/{plain_table}/{part}/
/// 4. None (part not found at any location)
///
/// `encoded_db` and `encoded_table` are URL-encoded (as created by backup::collect).
/// `plain_db` and `plain_table` are the original unencoded names (for very old backups
/// that stored shadow dirs without URL encoding).
///
/// This is the SINGLE source of truth for shadow path resolution across
/// upload, download, and restore. Consumers must not implement their own
/// fallback logic.
#[allow(clippy::too_many_arguments)]
pub fn resolve_shadow_part_path(
    backup_dir: &Path,
    manifest_disks: &BTreeMap<String, String>,
    backup_name: &str,
    disk_name: &str,
    encoded_db: &str,
    encoded_table: &str,
    plain_db: &str,
    plain_table: &str,
    part_name: &str,
) -> Option<PathBuf> {
    let encoded_suffix = PathBuf::from("shadow")
        .join(encoded_db)
        .join(encoded_table)
        .join(part_name);

    // 1. Try per-disk candidate (encoded path)
    if let Some(disk_path) = manifest_disks.get(disk_name) {
        let per_disk = per_disk_backup_dir(disk_path.trim_end_matches('/'), backup_name);
        let candidate = per_disk.join(&encoded_suffix);
        if candidate.exists() {
            return Some(candidate);
        }
    }

    // 2. Fallback to legacy encoded path (covers old backups and single-disk setups)
    let legacy_encoded = backup_dir.join(&encoded_suffix);
    if legacy_encoded.exists() {
        return Some(legacy_encoded);
    }

    // 3. Fallback to legacy plain path (very old backups without URL encoding)
    if plain_db != encoded_db || plain_table != encoded_table {
        let plain_suffix = PathBuf::from("shadow")
            .join(plain_db)
            .join(plain_table)
            .join(part_name);
        let legacy_plain = backup_dir.join(&plain_suffix);
        if legacy_plain.exists() {
            return Some(legacy_plain);
        }
    }

    // 4. Not found
    None
}

/// Parse a part name like "202401_1_50_3" into (partition, min_block, max_block, level).
///
/// The partition can contain underscores, so we split from the right.
/// Format: `{partition}_{min}_{max}_{level}`
pub fn parse_part_name(name: &str) -> Option<(String, u64, u64, u64)> {
    let parts: Vec<&str> = name.rsplitn(4, '_').collect();
    if parts.len() < 4 {
        return None;
    }
    // rsplitn reverses the order: [level, max, min, partition]
    let level: u64 = parts[0].parse().ok()?;
    let max_block: u64 = parts[1].parse().ok()?;
    let min_block: u64 = parts[2].parse().ok()?;
    let partition = parts[3].to_string();
    Some((partition, min_block, max_block, level))
}

/// Map from table UUID to (database, table_name) using data_paths.
///
/// Each table's `data_paths` typically contains something like:
///   `/var/lib/clickhouse/store/abc/abcdef12-3456-7890-abcd-ef1234567890/`
///
/// We extract the UUID hex portion from the path to create the mapping.
fn build_uuid_map(tables: &[TableRow]) -> HashMap<String, (String, String)> {
    let mut map = HashMap::new();
    for table in tables {
        for data_path in &table.data_paths {
            // Extract the UUID directory from the path
            // Format: .../store/{3char_prefix}/{uuid_with_dashes}/
            let normalized = data_path.trim_end_matches('/');
            if let Some(uuid_dir) = normalized.rsplit('/').next() {
                // The directory name is the UUID with dashes
                map.insert(
                    uuid_dir.to_string(),
                    (table.database.clone(), table.name.clone()),
                );
            }
        }
        // Also map by the `uuid` column from system.tables
        if !table.uuid.is_empty() && table.uuid != "00000000-0000-0000-0000-000000000000" {
            map.insert(
                table.uuid.clone(),
                (table.database.clone(), table.name.clone()),
            );
        }
    }
    map
}

/// Represents a collected part from the shadow directory.
#[derive(Clone)]
pub struct CollectedPart {
    pub database: String,
    pub table: String,
    pub part_info: PartInfo,
    /// Disk name this part belongs to (e.g. "default", "s3disk").
    pub disk_name: String,
}

/// Walk the shadow directories for a given freeze name and collect parts.
///
/// For each part found:
/// 1. Identifies the owning table via UUID mapping
/// 2. Determines the disk type (local vs S3) from `disk_type_map`
/// 3. For local disk parts: hardlinks (or copies) files to the backup staging directory
/// 4. For S3 disk parts: parses metadata files to extract S3 object references
/// 5. Computes CRC64 checksum of the checksums.txt file
///
/// Walks ALL disk paths (not just `data_path`) to discover parts on every disk.
///
/// Returns a mapping of "db.table" -> Vec<CollectedPart> with disk_name populated.
#[allow(clippy::too_many_arguments)]
pub fn collect_parts(
    data_path: &str,
    freeze_name: &str,
    backup_name: &str,
    tables: &[TableRow],
    disk_type_map: &BTreeMap<String, String>,
    disk_paths: &BTreeMap<String, String>,
    skip_disks: &[String],
    skip_disk_types: &[String],
    skip_projections: &[String],
) -> Result<HashMap<String, Vec<CollectedPart>>> {
    let uuid_map = build_uuid_map(tables);
    let mut result: HashMap<String, Vec<CollectedPart>> = HashMap::new();
    // Track which disks have already been logged to avoid per-part log spam.
    let mut logged_disks: HashSet<String> = HashSet::new();

    // Collect all disk paths to walk. Always include the default data_path
    // (for the "default" disk even if not explicitly in disk_paths).
    let mut paths_to_walk: Vec<(String, String)> = Vec::new(); // (disk_name, disk_path)

    for (disk_name, disk_path) in disk_paths {
        paths_to_walk.push((disk_name.clone(), disk_path.clone()));
    }

    // Ensure we always walk the default data_path even if disk_paths is empty
    let data_path_normalized = data_path.trim_end_matches('/');
    let has_data_path = paths_to_walk
        .iter()
        .any(|(_, p)| p.trim_end_matches('/') == data_path_normalized);
    if !has_data_path {
        paths_to_walk.push(("default".to_string(), data_path.to_string()));
    }

    for (disk_name, disk_path) in &paths_to_walk {
        // Skip disks that are excluded by name or type
        let disk_type = disk_type_map
            .get(disk_name)
            .map(|s| s.as_str())
            .unwrap_or("");
        if is_disk_excluded(disk_name, disk_type, skip_disks, skip_disk_types) {
            info!(
                disk = %disk_name,
                disk_type = %disk_type,
                "Skipping disk (excluded by skip_disks or skip_disk_types)"
            );
            continue;
        }

        let shadow_dir = PathBuf::from(disk_path).join("shadow").join(freeze_name);

        if !shadow_dir.exists() {
            debug!(
                shadow_dir = %shadow_dir.display(),
                disk = %disk_name,
                "Shadow directory does not exist for disk (table may have no data on this disk)"
            );
            continue;
        }

        let is_s3 = object_disk::is_s3_disk(disk_type);

        if is_s3 {
            info!(
                disk = %disk_name,
                shadow_dir = %shadow_dir.display(),
                "Walking S3 disk shadow directory"
            );
        }

        // Walk the shadow directory looking for part directories
        // Structure: shadow/{freeze_name}/store/{3char_prefix}/{uuid_dir}/{part_name}/...
        let store_dir = shadow_dir.join("store");
        if !store_dir.exists() {
            debug!(
                store_dir = %store_dir.display(),
                "Store directory does not exist in shadow"
            );
            continue;
        }

        // Iterate: store/{prefix_3}/{uuid_dir}/{part_name}
        for prefix_entry in std::fs::read_dir(&store_dir)
            .with_context(|| format!("Failed to read shadow store dir: {}", store_dir.display()))?
        {
            let prefix_entry = prefix_entry?;
            if !prefix_entry.file_type()?.is_dir() {
                continue;
            }

            for uuid_entry in std::fs::read_dir(prefix_entry.path())? {
                let uuid_entry = uuid_entry?;
                if !uuid_entry.file_type()?.is_dir() {
                    continue;
                }

                let uuid_dir_name = match uuid_entry.file_name().to_str() {
                    Some(name) => name.to_string(),
                    None => {
                        warn!(path = %uuid_entry.path().display(), "Non-UTF-8 directory name, skipping");
                        continue;
                    }
                };

                // Look up which table this UUID belongs to
                let (db, table) = match uuid_map.get(&uuid_dir_name) {
                    Some(pair) => pair.clone(),
                    None => {
                        warn!(
                            uuid = %uuid_dir_name,
                            "Could not map shadow UUID to a table, skipping"
                        );
                        continue;
                    }
                };

                let full_table_name = format!("{}.{}", db, table);

                // Iterate part directories under this UUID directory
                for part_entry in std::fs::read_dir(uuid_entry.path())? {
                    let part_entry = part_entry?;
                    // Non-directory entries (e.g. frozen_metadata.txt) are
                    // already filtered here; only directories can be parts.
                    if !part_entry.file_type()?.is_dir() {
                        continue;
                    }

                    let part_name = match part_entry.file_name().to_str() {
                        Some(name) => name.to_string(),
                        None => {
                            warn!(path = %part_entry.path().display(), "Non-UTF-8 directory name, skipping");
                            continue;
                        }
                    };

                    // Verify this is a real part by checking for checksums.txt
                    let checksums_path = part_entry.path().join("checksums.txt");
                    if !checksums_path.exists() {
                        debug!(
                            part = %part_name,
                            "Skipping directory without checksums.txt"
                        );
                        continue;
                    }

                    // Compute CRC64 of checksums.txt
                    let crc64 = compute_crc64(&checksums_path).with_context(|| {
                        format!(
                            "Failed to compute CRC64 for {}/{}",
                            full_table_name, part_name
                        )
                    })?;

                    if is_s3 {
                        // S3 disk part: parse metadata files to extract object references
                        let (s3_objects, part_size) = collect_s3_part_metadata(&part_entry.path())?;

                        info!(
                            table = %full_table_name,
                            part = %part_name,
                            disk = %disk_name,
                            objects = s3_objects.len(),
                            size = part_size,
                            "Collected S3 disk part metadata"
                        );

                        // Stage S3 disk metadata files to per-disk backup dir so they
                        // survive UNFREEZE and are available for upload (CopyObject needs
                        // the metadata files alongside the S3 object references).
                        let disk_path_trimmed = disk_path.trim_end_matches('/');
                        let per_disk_dir = per_disk_backup_dir(disk_path_trimmed, backup_name);
                        if logged_disks.insert(format!("{}:s3meta", &disk_name)) {
                            info!(
                                disk = %disk_name,
                                path = %per_disk_dir.display(),
                                "staging S3 disk metadata to per-disk backup dir"
                            );
                        }
                        let staging_dir = per_disk_dir
                            .join("shadow")
                            .join(encode_path_component(&db))
                            .join(encode_path_component(&table))
                            .join(&part_name);
                        hardlink_dir(&part_entry.path(), &staging_dir, skip_projections)?;

                        let part_info =
                            PartInfo::new(part_name, part_size, crc64)
                                .with_s3_objects(s3_objects);

                        result
                            .entry(full_table_name.clone())
                            .or_default()
                            .push(CollectedPart {
                                database: db.clone(),
                                table: table.clone(),
                                part_info,
                                disk_name: disk_name.clone(),
                            });
                    } else {
                        // Local disk part: hardlink files to backup staging
                        let part_size = dir_size(&part_entry.path())?;

                        let disk_path_trimmed = disk_path.trim_end_matches('/');
                        let per_disk_dir = per_disk_backup_dir(disk_path_trimmed, backup_name);
                        if logged_disks.insert(disk_name.clone()) {
                            info!(
                                disk = %disk_name,
                                path = %per_disk_dir.display(),
                                "staging per-disk backup dir"
                            );
                        }
                        let staging_dir = per_disk_dir
                            .join("shadow")
                            .join(encode_path_component(&db))
                            .join(encode_path_component(&table))
                            .join(&part_name);

                        hardlink_dir(&part_entry.path(), &staging_dir, skip_projections)?;

                        let part_info = PartInfo::new(part_name, part_size, crc64);

                        result
                            .entry(full_table_name.clone())
                            .or_default()
                            .push(CollectedPart {
                                database: db.clone(),
                                table: table.clone(),
                                part_info,
                                disk_name: disk_name.clone(),
                            });
                    }
                }
            }
        }
    }

    Ok(result)
}

/// Parse metadata files in an S3 disk part directory to extract S3 object references.
///
/// Reads all files in the part directory as metadata files,
/// parses them using the object disk metadata parser, and builds a list of
/// S3ObjectInfo entries. Returns the list and the total size of all objects.
fn collect_s3_part_metadata(part_dir: &Path) -> Result<(Vec<S3ObjectInfo>, u64)> {
    let mut s3_objects = Vec::new();
    let mut total_size: u64 = 0;

    for entry in std::fs::read_dir(part_dir)
        .with_context(|| format!("Failed to read S3 part dir: {}", part_dir.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }

        let file_name = match entry.file_name().to_str() {
            Some(name) => name.to_string(),
            None => {
                warn!(path = %entry.path().display(), "Non-UTF-8 file name, skipping");
                continue;
            }
        };

        // Try to parse this file as object disk metadata
        let content = std::fs::read_to_string(entry.path())
            .with_context(|| format!("Failed to read metadata file: {}", entry.path().display()))?;

        match object_disk::parse_metadata(&content) {
            Ok(metadata) => {
                for obj_ref in &metadata.objects {
                    s3_objects.push(S3ObjectInfo {
                        path: obj_ref.relative_path.clone(),
                        size: obj_ref.size,
                        backup_key: String::new(), // Set during upload
                    });
                    total_size += obj_ref.size;
                }
            }
            Err(e) => {
                // Not all files in the part dir may be metadata files --
                // skip files that don't parse as metadata
                debug!(
                    file = %file_name,
                    error = %e,
                    "Skipping non-metadata file in S3 part directory"
                );
            }
        }
    }

    Ok((s3_objects, total_size))
}

/// Recursively hardlink all files from src_dir to dst_dir.
///
/// Creates dst_dir and any needed subdirectories. On EXDEV (cross-device)
/// error, falls back to copying.
///
/// If `skip_proj_patterns` is non-empty, any subdirectory ending in `.proj`
/// whose stem matches one of the patterns is skipped entirely (not hardlinked).
/// The special pattern `*` matches all projections.
fn hardlink_dir(src_dir: &Path, dst_dir: &Path, skip_proj_patterns: &[String]) -> Result<()> {
    std::fs::create_dir_all(dst_dir)
        .with_context(|| format!("Failed to create staging dir: {}", dst_dir.display()))?;

    let mut walker = WalkDir::new(src_dir).into_iter();

    while let Some(entry_result) = walker.next() {
        let entry = entry_result
            .with_context(|| format!("Failed to walk directory: {}", src_dir.display()))?;

        let relative = entry
            .path()
            .strip_prefix(src_dir)
            .context("Failed to strip prefix from shadow path")?;

        let target = dst_dir.join(relative);

        if entry.file_type().is_dir() {
            // Check if this directory is a .proj directory that should be skipped
            if !skip_proj_patterns.is_empty() {
                if let Some(dir_name) = entry.path().file_name().and_then(|n| n.to_str()) {
                    if let Some(stem) = dir_name.strip_suffix(".proj") {
                        if should_skip_projection(stem, skip_proj_patterns) {
                            info!(
                                projection = %dir_name,
                                path = %entry.path().display(),
                                "Skipping projection directory"
                            );
                            walker.skip_current_dir();
                            continue;
                        }
                    }
                }
            }
            std::fs::create_dir_all(&target)?;
        } else {
            // Try hardlink first, fall back to copy on cross-device
            if let Err(e) = std::fs::hard_link(entry.path(), &target) {
                let is_exdev = e.raw_os_error() == Some(libc::EXDEV);
                if is_exdev {
                    debug!(
                        src = %entry.path().display(),
                        dst = %target.display(),
                        "Cross-device hardlink failed, falling back to copy"
                    );
                    std::fs::copy(entry.path(), &target)?;
                } else {
                    return Err(e).with_context(|| {
                        format!(
                            "Failed to hardlink {} -> {}",
                            entry.path().display(),
                            target.display()
                        )
                    });
                }
            }
        }
    }

    Ok(())
}

/// Check whether a projection stem should be skipped based on the patterns.
///
/// Patterns support glob matching via `glob::Pattern`. The special pattern `*`
/// matches all projections.
fn should_skip_projection(stem: &str, patterns: &[String]) -> bool {
    for pattern in patterns {
        if pattern == "*" {
            return true;
        }
        if let Ok(glob_pat) = glob::Pattern::new(pattern) {
            if glob_pat.matches(stem) {
                return true;
            }
        }
    }
    false
}

/// Calculate the total size of all files in a directory.
pub fn dir_size(path: &Path) -> Result<u64> {
    let mut total: u64 = 0;
    for entry in WalkDir::new(path) {
        let entry = entry?;
        if entry.file_type().is_file() {
            total += entry.metadata()?.len();
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_part_name_standard() {
        let result = parse_part_name("202401_1_50_3").unwrap();
        assert_eq!(result, ("202401".to_string(), 1, 50, 3));
    }

    #[test]
    fn test_parse_part_name_simple() {
        let result = parse_part_name("all_0_0_0").unwrap();
        assert_eq!(result, ("all".to_string(), 0, 0, 0));
    }

    #[test]
    fn test_parse_part_name_with_underscores_in_partition() {
        // Partition ID can contain underscores (tuple partitions)
        let result = parse_part_name("2024_01_15_1_50_3").unwrap();
        assert_eq!(result, ("2024_01_15".to_string(), 1, 50, 3));
    }

    #[test]
    fn test_parse_part_name_invalid() {
        assert!(parse_part_name("invalid").is_none());
        assert!(parse_part_name("").is_none());
        assert!(parse_part_name("a_b").is_none());
    }

    #[test]
    fn test_encode_path_component_simple() {
        assert_eq!(encode_path_component("default"), "default");
        assert_eq!(encode_path_component("my_database"), "my_database");
    }

    #[test]
    fn test_encode_path_component_special_chars() {
        assert_eq!(encode_path_component("my db"), "my%20db");
        assert_eq!(encode_path_component("test+table"), "test%2Btable");
    }

    #[test]
    fn test_encode_path_component_safe_chars() {
        // encode_path_component does NOT preserve `/` (unlike old url_encode_path)
        assert_eq!(encode_path_component("a-b_c.d"), "a-b_c.d");
    }

    #[test]
    fn test_hardlink_dir_roundtrip() {
        let src_dir = tempfile::tempdir().unwrap();
        let dst_dir = tempfile::tempdir().unwrap();
        let dst_path = dst_dir.path().join("linked");

        // Create source files
        std::fs::write(src_dir.path().join("file1.txt"), b"hello").unwrap();
        std::fs::create_dir(src_dir.path().join("subdir")).unwrap();
        std::fs::write(src_dir.path().join("subdir/file2.txt"), b"world").unwrap();

        hardlink_dir(src_dir.path(), &dst_path, &[]).unwrap();

        assert!(dst_path.join("file1.txt").exists());
        assert!(dst_path.join("subdir/file2.txt").exists());
        assert_eq!(
            std::fs::read_to_string(dst_path.join("file1.txt")).unwrap(),
            "hello"
        );
        assert_eq!(
            std::fs::read_to_string(dst_path.join("subdir/file2.txt")).unwrap(),
            "world"
        );
    }

    #[test]
    fn test_dir_size_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let size = dir_size(dir.path()).unwrap();
        assert_eq!(size, 0);
    }

    #[test]
    fn test_dir_size_with_files() {
        let dir = tempfile::tempdir().unwrap();
        // Create two files with known sizes
        std::fs::write(dir.path().join("file1.txt"), b"hello").unwrap(); // 5 bytes
        std::fs::create_dir(dir.path().join("subdir")).unwrap();
        std::fs::write(dir.path().join("subdir/file2.txt"), b"world!").unwrap(); // 6 bytes
        let size = dir_size(dir.path()).unwrap();
        assert_eq!(size, 11);
    }

    #[test]
    fn test_freeze_name_generation() {
        use crate::clickhouse::client::freeze_name;
        let name = freeze_name("daily-20240115", "default", "trades");
        assert_eq!(name, "chbackup_daily_20240115_default_trades");
    }

    /// Helper: create a mock shadow directory for a local disk part.
    fn create_local_shadow(
        data_path: &Path,
        freeze_name: &str,
        uuid: &str,
        prefix_3: &str,
        part_name: &str,
    ) {
        let part_dir = data_path
            .join("shadow")
            .join(freeze_name)
            .join("store")
            .join(prefix_3)
            .join(uuid)
            .join(part_name);
        std::fs::create_dir_all(&part_dir).unwrap();
        std::fs::write(part_dir.join("checksums.txt"), b"fake checksum data").unwrap();
        std::fs::write(part_dir.join("data.bin"), b"fake binary data here").unwrap();
    }

    /// Helper: create a mock shadow directory for an S3 disk part with metadata files.
    fn create_s3_shadow(
        disk_path: &Path,
        freeze_name: &str,
        uuid: &str,
        prefix_3: &str,
        part_name: &str,
        metadata_content: &str,
    ) {
        let part_dir = disk_path
            .join("shadow")
            .join(freeze_name)
            .join("store")
            .join(prefix_3)
            .join(uuid)
            .join(part_name);
        std::fs::create_dir_all(&part_dir).unwrap();
        std::fs::write(part_dir.join("checksums.txt"), b"fake checksum data").unwrap();
        // Write a metadata file (simulating an S3 disk part)
        std::fs::write(part_dir.join("data.bin"), metadata_content).unwrap();
    }

    #[test]
    fn test_collect_parts_local_disk_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let data_path = tmp.path().join("clickhouse");
        let backup_dir = tmp.path().join("backup");
        std::fs::create_dir_all(&data_path).unwrap();
        std::fs::create_dir_all(&backup_dir).unwrap();

        let uuid = "abcdef12-3456-7890-abcd-ef1234567890";
        let freeze = "chbackup_test_default_trades";
        create_local_shadow(&data_path, freeze, uuid, "abc", "202401_1_50_3");

        let tables = vec![TableRow {
            database: "default".to_string(),
            name: "trades".to_string(),
            engine: "MergeTree".to_string(),
            create_table_query: "CREATE TABLE ...".to_string(),
            uuid: uuid.to_string(),
            data_paths: vec![format!("{}/store/abc/{}/", data_path.display(), uuid)],
            total_bytes: Some(1000),
        }];

        let disk_type_map = BTreeMap::from([("default".to_string(), "local".to_string())]);
        let disk_paths = BTreeMap::from([(
            "default".to_string(),
            data_path.to_string_lossy().to_string(),
        )]);

        let result = collect_parts(
            &data_path.to_string_lossy(),
            freeze,
            "test-backup",
            &tables,
            &disk_type_map,
            &disk_paths,
            &[],
            &[],
            &[],
        )
        .unwrap();

        assert!(result.contains_key("default.trades"));
        let parts = &result["default.trades"];
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].part_info.name, "202401_1_50_3");
        assert_eq!(parts[0].disk_name, "default");
        assert!(parts[0].part_info.s3_objects.is_none());

        // Verify hardlink was created in per-disk staging dir
        // When disk_path == data_path (default disk), per_disk_backup_dir(disk_path, name)
        // resolves to {data_path}/backup/{name} which is different from backup_dir
        // (the test uses a separate backup_dir, so we check the per-disk path).
        let per_disk_staging = per_disk_backup_dir(data_path.to_str().unwrap(), "test-backup");
        let staged = per_disk_staging
            .join("shadow")
            .join("default")
            .join("trades")
            .join("202401_1_50_3")
            .join("data.bin");
        assert!(staged.exists());
    }

    #[test]
    fn test_collect_parts_detects_s3_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        let local_data = tmp.path().join("clickhouse");
        let s3_disk_path = tmp.path().join("s3disk");
        let backup_dir = tmp.path().join("backup");
        std::fs::create_dir_all(&local_data).unwrap();
        std::fs::create_dir_all(&s3_disk_path).unwrap();
        std::fs::create_dir_all(&backup_dir).unwrap();

        let uuid = "abcdef12-3456-7890-abcd-ef1234567890";
        let freeze = "chbackup_test_default_trades";

        // Create S3 disk shadow with metadata content
        let metadata = "2\n1\t500\n500\tstore/abc/def/data.bin\n0\n";
        create_s3_shadow(
            &s3_disk_path,
            freeze,
            uuid,
            "abc",
            "202401_1_50_3",
            metadata,
        );

        let tables = vec![TableRow {
            database: "default".to_string(),
            name: "trades".to_string(),
            engine: "MergeTree".to_string(),
            create_table_query: "CREATE TABLE ...".to_string(),
            uuid: uuid.to_string(),
            data_paths: vec![format!("{}/store/abc/{}/", s3_disk_path.display(), uuid)],
            total_bytes: Some(1000),
        }];

        let disk_type_map = BTreeMap::from([
            ("default".to_string(), "local".to_string()),
            ("s3disk".to_string(), "s3".to_string()),
        ]);
        let disk_paths = BTreeMap::from([
            (
                "default".to_string(),
                local_data.to_string_lossy().to_string(),
            ),
            (
                "s3disk".to_string(),
                s3_disk_path.to_string_lossy().to_string(),
            ),
        ]);

        let result = collect_parts(
            &local_data.to_string_lossy(),
            freeze,
            "test-backup",
            &tables,
            &disk_type_map,
            &disk_paths,
            &[],
            &[],
            &[],
        )
        .unwrap();

        assert!(result.contains_key("default.trades"));
        let parts = &result["default.trades"];
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].part_info.name, "202401_1_50_3");
        assert_eq!(parts[0].disk_name, "s3disk");
        assert!(parts[0].part_info.s3_objects.is_some());

        let s3_objs = parts[0].part_info.s3_objects.as_ref().unwrap();
        assert_eq!(s3_objs.len(), 1);
        assert_eq!(s3_objs[0].path, "store/abc/def/data.bin");
        assert_eq!(s3_objs[0].size, 500);
        assert_eq!(parts[0].part_info.size, 500); // Size from metadata, not dir_size

        // Verify NO hardlink was created for S3 parts (data is on S3)
        let staged = backup_dir
            .join("shadow")
            .join("default")
            .join("trades")
            .join("202401_1_50_3")
            .join("data.bin");
        assert!(!staged.exists());
    }

    #[test]
    fn test_hardlink_dir_skips_projections() {
        let src_dir = tempfile::tempdir().unwrap();
        let dst_dir = tempfile::tempdir().unwrap();
        let dst_path = dst_dir.path().join("linked");

        // Create source files including a .proj subdirectory
        std::fs::write(src_dir.path().join("data.bin"), b"data").unwrap();
        std::fs::write(src_dir.path().join("checksums.txt"), b"checksums").unwrap();
        std::fs::create_dir(src_dir.path().join("my_agg.proj")).unwrap();
        std::fs::write(src_dir.path().join("my_agg.proj/data.bin"), b"proj data").unwrap();

        // Skip ALL projections
        hardlink_dir(src_dir.path(), &dst_path, &["*".to_string()]).unwrap();

        assert!(dst_path.join("data.bin").exists());
        assert!(dst_path.join("checksums.txt").exists());
        assert!(
            !dst_path.join("my_agg.proj").exists(),
            "Projection directory should be skipped"
        );
    }

    #[test]
    fn test_skip_projections_empty_list_keeps_all() {
        let src_dir = tempfile::tempdir().unwrap();
        let dst_dir = tempfile::tempdir().unwrap();
        let dst_path = dst_dir.path().join("linked");

        std::fs::write(src_dir.path().join("data.bin"), b"data").unwrap();
        std::fs::create_dir(src_dir.path().join("my_agg.proj")).unwrap();
        std::fs::write(src_dir.path().join("my_agg.proj/data.bin"), b"proj data").unwrap();

        // Empty skip list -- keep all projections
        hardlink_dir(src_dir.path(), &dst_path, &[]).unwrap();

        assert!(dst_path.join("data.bin").exists());
        assert!(
            dst_path.join("my_agg.proj").exists(),
            "Projection directory should be preserved"
        );
        assert!(dst_path.join("my_agg.proj/data.bin").exists());
    }

    #[test]
    fn test_skip_projections_glob_pattern() {
        let src_dir = tempfile::tempdir().unwrap();
        let dst_dir = tempfile::tempdir().unwrap();
        let dst_path = dst_dir.path().join("linked");

        std::fs::write(src_dir.path().join("data.bin"), b"data").unwrap();
        std::fs::create_dir(src_dir.path().join("my_agg.proj")).unwrap();
        std::fs::write(src_dir.path().join("my_agg.proj/data.bin"), b"proj data").unwrap();
        std::fs::create_dir(src_dir.path().join("other.proj")).unwrap();
        std::fs::write(src_dir.path().join("other.proj/data.bin"), b"other proj").unwrap();

        // Skip only projections matching "my_*"
        hardlink_dir(src_dir.path(), &dst_path, &["my_*".to_string()]).unwrap();

        assert!(dst_path.join("data.bin").exists());
        assert!(
            !dst_path.join("my_agg.proj").exists(),
            "my_agg.proj should be skipped"
        );
        assert!(
            dst_path.join("other.proj").exists(),
            "other.proj should be preserved"
        );
    }

    #[test]
    fn test_collected_part_has_disk_name() {
        // Verify CollectedPart struct has disk_name field
        let part = CollectedPart {
            database: "default".to_string(),
            table: "trades".to_string(),
            part_info: PartInfo::new("202401_1_50_3", 1024, 12345),
            disk_name: "s3disk".to_string(),
        };
        assert_eq!(part.disk_name, "s3disk");
        assert_eq!(part.database, "default");
        assert_eq!(part.table, "trades");
    }

    // -- collect_parts per-disk staging test --

    #[test]
    fn test_collect_parts_per_disk_staging_dir() {
        // Two "disks" (subdirs on same FS). Verify parts from each disk are
        // hardlinked to their respective {disk_path}/backup/{name}/shadow/ dir.
        let tmp = tempfile::tempdir().unwrap();
        let disk1 = tmp.path().join("disk1"); // default disk
        let disk2 = tmp.path().join("disk2"); // second NVMe

        std::fs::create_dir_all(&disk1).unwrap();
        std::fs::create_dir_all(&disk2).unwrap();

        let backup_dir = tmp.path().join("backup_metadata");
        std::fs::create_dir_all(&backup_dir).unwrap();

        let uuid1 = "11111111-1111-1111-1111-111111111111";
        let uuid2 = "22222222-2222-2222-2222-222222222222";
        let freeze = "chbackup_test_default_t1";

        // Create shadow on disk1 for table t1
        create_local_shadow(&disk1, freeze, uuid1, "111", "part_a_1_1_0");
        // Create shadow on disk2 for table t2
        create_local_shadow(&disk2, freeze, uuid2, "222", "part_b_1_1_0");

        let tables = vec![
            TableRow {
                database: "db1".to_string(),
                name: "t1".to_string(),
                engine: "MergeTree".to_string(),
                create_table_query: "CREATE TABLE ...".to_string(),
                uuid: uuid1.to_string(),
                data_paths: vec![format!("{}/store/111/{}/", disk1.display(), uuid1)],
                total_bytes: Some(100),
            },
            TableRow {
                database: "db1".to_string(),
                name: "t2".to_string(),
                engine: "MergeTree".to_string(),
                create_table_query: "CREATE TABLE ...".to_string(),
                uuid: uuid2.to_string(),
                data_paths: vec![format!("{}/store/222/{}/", disk2.display(), uuid2)],
                total_bytes: Some(100),
            },
        ];

        let disk_type_map = BTreeMap::from([
            ("default".to_string(), "local".to_string()),
            ("nvme1".to_string(), "local".to_string()),
        ]);
        let disk_paths = BTreeMap::from([
            ("default".to_string(), disk1.to_string_lossy().to_string()),
            ("nvme1".to_string(), disk2.to_string_lossy().to_string()),
        ]);

        let result = collect_parts(
            &disk1.to_string_lossy(),
            freeze,
            "my-backup",
            &tables,
            &disk_type_map,
            &disk_paths,
            &[],
            &[],
            &[],
        )
        .unwrap();

        // Verify t1 part is staged to disk1's per-disk dir
        let disk1_staged = per_disk_backup_dir(disk1.to_str().unwrap(), "my-backup")
            .join("shadow")
            .join("db1")
            .join("t1")
            .join("part_a_1_1_0")
            .join("data.bin");
        assert!(
            disk1_staged.exists(),
            "Part from disk1 should be staged to disk1's per-disk backup dir"
        );

        // Verify t2 part is staged to disk2's per-disk dir
        let disk2_staged = per_disk_backup_dir(disk2.to_str().unwrap(), "my-backup")
            .join("shadow")
            .join("db1")
            .join("t2")
            .join("part_b_1_1_0")
            .join("data.bin");
        assert!(
            disk2_staged.exists(),
            "Part from disk2 should be staged to disk2's per-disk backup dir"
        );

        // Verify parts NOT staged to backup_dir (old behavior)
        let old_staged = backup_dir.join("shadow").join("db1").join("t1");
        assert!(
            !old_staged.exists(),
            "Parts should NOT be staged to backup_dir anymore"
        );

        // Verify result map
        assert_eq!(result.len(), 2);
        assert!(result.contains_key("db1.t1"));
        assert!(result.contains_key("db1.t2"));
    }

    // -- per_disk_backup_dir tests --

    #[test]
    fn test_per_disk_backup_dir_default_disk() {
        // When disk_path == data_path, result should equal {data_path}/backup/{name}
        let data_path = "/var/lib/clickhouse";
        let result = per_disk_backup_dir(data_path, "daily-2024");
        assert_eq!(
            result,
            PathBuf::from("/var/lib/clickhouse/backup/daily-2024")
        );
    }

    #[test]
    fn test_per_disk_backup_dir_non_default_disk() {
        // When disk_path != data_path, result should be {disk_path}/backup/{name}
        let disk_path = "/mnt/nvme1/clickhouse";
        let result = per_disk_backup_dir(disk_path, "daily-2024");
        assert_eq!(
            result,
            PathBuf::from("/mnt/nvme1/clickhouse/backup/daily-2024")
        );
    }

    // -- resolve_shadow_part_path tests --

    #[test]
    fn test_resolve_shadow_part_path_per_disk_exists() {
        // When per-disk candidate exists, it should be returned.
        let tmp = tempfile::tempdir().unwrap();
        let disk_path = tmp.path().join("nvme1");
        let backup_dir = tmp.path().join("default_backup");

        // Create per-disk candidate
        let per_disk_part = disk_path
            .join("backup")
            .join("daily")
            .join("shadow")
            .join("default")
            .join("trades")
            .join("202401_1_50_3");
        std::fs::create_dir_all(&per_disk_part).unwrap();

        let disks = BTreeMap::from([("nvme1".to_string(), disk_path.to_string_lossy().to_string())]);

        let result = resolve_shadow_part_path(
            &backup_dir,
            &disks,
            "daily",
            "nvme1",
            "default",
            "trades",
            "default",
            "trades",
            "202401_1_50_3",
        );

        assert_eq!(result, Some(per_disk_part));
    }

    #[test]
    fn test_resolve_shadow_part_path_fallback_to_legacy_encoded() {
        // When per-disk path doesn't exist, should fall back to legacy encoded
        // path in backup_dir/shadow/.
        let tmp = tempfile::tempdir().unwrap();
        let disk_path = tmp.path().join("nvme1");
        let backup_dir = tmp.path().join("default_backup");

        // Create legacy encoded path (not per-disk)
        let legacy_path = backup_dir
            .join("shadow")
            .join("default")
            .join("trades")
            .join("202401_1_50_3");
        std::fs::create_dir_all(&legacy_path).unwrap();

        let disks = BTreeMap::from([("nvme1".to_string(), disk_path.to_string_lossy().to_string())]);

        let result = resolve_shadow_part_path(
            &backup_dir,
            &disks,
            "daily",
            "nvme1",
            "default",
            "trades",
            "default",
            "trades",
            "202401_1_50_3",
        );

        assert_eq!(result, Some(legacy_path));
    }

    #[test]
    fn test_resolve_shadow_part_path_fallback_to_legacy_plain() {
        // When per-disk and encoded legacy don't exist, should fall back to
        // plain (unencoded) path for very old backups.
        let tmp = tempfile::tempdir().unwrap();
        let backup_dir = tmp.path().join("default_backup");

        // Create legacy plain path with special chars (unencoded)
        let legacy_plain = backup_dir
            .join("shadow")
            .join("my db")
            .join("my+table")
            .join("202401_1_50_3");
        std::fs::create_dir_all(&legacy_plain).unwrap();

        let disks = BTreeMap::new();

        let result = resolve_shadow_part_path(
            &backup_dir,
            &disks,
            "daily",
            "default",
            "my%20db",
            "my%2Btable",
            "my db",
            "my+table",
            "202401_1_50_3",
        );

        assert_eq!(result, Some(legacy_plain));
    }

    #[test]
    fn test_resolve_shadow_part_path_no_disk_in_manifest() {
        // When disk_name is not in manifest.disks, should fall back to legacy path.
        let tmp = tempfile::tempdir().unwrap();
        let backup_dir = tmp.path().join("default_backup");

        // Create legacy encoded path
        let legacy_path = backup_dir
            .join("shadow")
            .join("default")
            .join("trades")
            .join("202401_1_50_3");
        std::fs::create_dir_all(&legacy_path).unwrap();

        // Empty disks map -- disk_name "nvme1" not found
        let disks = BTreeMap::new();

        let result = resolve_shadow_part_path(
            &backup_dir,
            &disks,
            "daily",
            "nvme1",
            "default",
            "trades",
            "default",
            "trades",
            "202401_1_50_3",
        );

        assert_eq!(result, Some(legacy_path));
    }

    #[test]
    fn test_resolve_shadow_part_path_plain_skipped_when_same() {
        // When plain == encoded (common case), step 3 is skipped (no redundant FS check).
        // Verify None is returned when only step 3 would match but plain == encoded.
        let tmp = tempfile::tempdir().unwrap();
        let backup_dir = tmp.path().join("default_backup");

        // Don't create any paths -- result should be None
        let disks = BTreeMap::new();

        let result = resolve_shadow_part_path(
            &backup_dir,
            &disks,
            "daily",
            "default",
            "default",
            "trades",
            "default", // plain == encoded
            "trades",  // plain == encoded
            "202401_1_50_3",
        );

        assert_eq!(result, None);
    }

    #[test]
    fn test_resolve_shadow_part_path_not_found() {
        // When no path exists at any location, returns None.
        let tmp = tempfile::tempdir().unwrap();
        let backup_dir = tmp.path().join("default_backup");
        std::fs::create_dir_all(&backup_dir).unwrap();

        let disks = BTreeMap::from([(
            "default".to_string(),
            tmp.path().to_string_lossy().to_string(),
        )]);

        let result = resolve_shadow_part_path(
            &backup_dir,
            &disks,
            "daily",
            "default",
            "db",
            "table",
            "db",
            "table",
            "nonexistent_part",
        );

        assert_eq!(result, None);
    }

    #[test]
    fn test_build_uuid_map_skips_null_uuid() {
        // build_uuid_map should skip null UUIDs (all zeros)
        let tables = vec![TableRow {
            database: "default".to_string(),
            name: "log_table".to_string(),
            engine: "Log".to_string(),
            create_table_query: "CREATE TABLE ...".to_string(),
            uuid: "00000000-0000-0000-0000-000000000000".to_string(),
            data_paths: vec!["/var/lib/clickhouse/data/default/log_table/".to_string()],
            total_bytes: Some(100),
        }];

        let map = build_uuid_map(&tables);
        // The null UUID should NOT be in the map
        assert!(
            !map.contains_key("00000000-0000-0000-0000-000000000000"),
            "Null UUID should not be inserted into uuid_map"
        );
        // But data_paths-derived UUID (last path component) should be present
        assert!(map.contains_key("log_table"));
    }

    #[test]
    fn test_build_uuid_map_multiple_data_paths() {
        // Verify build_uuid_map handles tables with multiple data_paths
        let tables = vec![TableRow {
            database: "default".to_string(),
            name: "trades".to_string(),
            engine: "MergeTree".to_string(),
            create_table_query: "CREATE TABLE ...".to_string(),
            uuid: "abcdef12-3456-7890-abcd-ef1234567890".to_string(),
            data_paths: vec![
                "/var/lib/clickhouse/store/abc/abcdef12-3456-7890-abcd-ef1234567890/".to_string(),
                "/mnt/nvme2/store/abc/abcdef12-3456-7890-abcd-ef1234567890/".to_string(),
            ],
            total_bytes: Some(1000),
        }];

        let map = build_uuid_map(&tables);
        // Both data_paths should have entries for the UUID directory name
        assert!(map.contains_key("abcdef12-3456-7890-abcd-ef1234567890"));
        let entry = map.get("abcdef12-3456-7890-abcd-ef1234567890").unwrap();
        assert_eq!(entry.0, "default");
        assert_eq!(entry.1, "trades");
    }

    #[test]
    fn test_build_uuid_map_empty_uuid() {
        // Tables with empty uuid should not insert empty key
        let tables = vec![TableRow {
            database: "default".to_string(),
            name: "log_table".to_string(),
            engine: "Log".to_string(),
            create_table_query: "CREATE TABLE ...".to_string(),
            uuid: "".to_string(),
            data_paths: vec!["/var/lib/clickhouse/data/default/log_table/".to_string()],
            total_bytes: Some(100),
        }];

        let map = build_uuid_map(&tables);
        assert!(!map.contains_key(""));
    }

    #[test]
    fn test_should_skip_projection_no_match() {
        // No patterns match -> should not skip
        assert!(!should_skip_projection("my_agg", &["other_*".to_string()]));
    }

    #[test]
    fn test_should_skip_projection_star_matches_all() {
        // Special pattern "*" matches everything
        assert!(should_skip_projection("any_projection", &["*".to_string()]));
    }

    #[test]
    fn test_should_skip_projection_glob_match() {
        // Glob pattern "my_*" should match "my_agg"
        assert!(should_skip_projection("my_agg", &["my_*".to_string()]));
    }

    #[test]
    fn test_should_skip_projection_no_patterns() {
        // Empty pattern list -> never skip
        assert!(!should_skip_projection("anything", &[]));
    }

    #[test]
    fn test_dir_size_nonexistent_dir() {
        // dir_size on a nonexistent path should error
        let path = std::path::Path::new("/tmp/nonexistent_dir_chbackup_test_12345");
        let result = dir_size(path);
        assert!(result.is_err());
    }

    #[test]
    fn test_per_disk_backup_dir_trailing_slash() {
        // Verify trailing slashes in disk_path are handled
        let result = per_disk_backup_dir("/mnt/disk1/", "backup-1");
        assert_eq!(result, PathBuf::from("/mnt/disk1/backup/backup-1"));
    }

    #[test]
    fn test_collect_s3_part_metadata_local_dir() {
        // When a part directory has no parseable metadata files, s3_objects
        // should be empty (all files fail to parse and are silently skipped)
        let tmp = tempfile::tempdir().unwrap();
        let part_dir = tmp.path().join("part_1_1_0");
        std::fs::create_dir_all(&part_dir).unwrap();
        std::fs::write(part_dir.join("data.bin"), b"not metadata").unwrap();
        std::fs::write(part_dir.join("checksums.txt"), b"checksum data").unwrap();

        let (objects, total_size) = collect_s3_part_metadata(&part_dir).unwrap();
        assert!(objects.is_empty());
        assert_eq!(total_size, 0);
    }

    #[test]
    fn test_collect_s3_part_metadata_with_valid_metadata() {
        // Test that collect_s3_part_metadata correctly parses metadata files
        let tmp = tempfile::tempdir().unwrap();
        let part_dir = tmp.path().join("part_1_1_0");
        std::fs::create_dir_all(&part_dir).unwrap();

        // Write a valid metadata file
        let metadata = "2\n1\t500\n500\tstore/abc/data.bin\n0\n";
        std::fs::write(part_dir.join("data.bin"), metadata).unwrap();
        // checksums.txt is not a metadata file -- will fail to parse and be skipped
        std::fs::write(part_dir.join("checksums.txt"), b"checksum data").unwrap();

        let (objects, total_size) = collect_s3_part_metadata(&part_dir).unwrap();
        assert_eq!(objects.len(), 1);
        assert_eq!(objects[0].path, "store/abc/data.bin");
        assert_eq!(objects[0].size, 500);
        assert_eq!(total_size, 500);
    }
}
