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

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::{debug, info, warn};
use walkdir::WalkDir;

use super::checksum::compute_crc64;
use crate::clickhouse::client::TableRow;
use crate::manifest::{PartInfo, S3ObjectInfo};
use crate::object_disk;
use crate::table_filter::is_disk_excluded;

/// URL-encode a database or table name for use in file paths.
///
/// Replaces non-alphanumeric chars (except `/`, `-`, `_`, `.`) with
/// percent-encoded form.
pub fn url_encode_path(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_alphanumeric() || c == '/' || c == '-' || c == '_' || c == '.' {
            result.push(c);
        } else {
            // Percent-encode the character
            for byte in c.to_string().as_bytes() {
                result.push_str(&format!("%{:02X}", byte));
            }
        }
    }
    result
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
    backup_dir: &Path,
    tables: &[TableRow],
    disk_type_map: &HashMap<String, String>,
    disk_paths: &HashMap<String, String>,
    skip_disks: &[String],
    skip_disk_types: &[String],
) -> Result<HashMap<String, Vec<CollectedPart>>> {
    let uuid_map = build_uuid_map(tables);
    let mut result: HashMap<String, Vec<CollectedPart>> = HashMap::new();

    // Build reverse map: normalized disk path -> disk name
    let disk_path_to_name: HashMap<String, String> = disk_paths
        .iter()
        .map(|(name, path)| {
            let normalized = path.trim_end_matches('/').to_string();
            (normalized, name.clone())
        })
        .collect();

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
        let disk_type = disk_type_map.get(disk_name).map(|s| s.as_str()).unwrap_or("");
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

        let is_s3 = disk_type_map
            .get(disk_name)
            .is_some_and(|t| object_disk::is_s3_disk(t));

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

                let uuid_dir_name = uuid_entry.file_name().to_string_lossy().to_string();

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
                    if !part_entry.file_type()?.is_dir() {
                        continue;
                    }

                    let part_name = part_entry.file_name().to_string_lossy().to_string();

                    // Skip frozen_metadata.txt (not a part directory)
                    if part_name == "frozen_metadata.txt" {
                        continue;
                    }

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

                        let part_info = PartInfo {
                            name: part_name,
                            size: part_size,
                            backup_key: String::new(), // Set during upload
                            source: "uploaded".to_string(),
                            checksum_crc64: crc64,
                            s3_objects: Some(s3_objects),
                        };

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

                        let staging_dir = backup_dir
                            .join("shadow")
                            .join(url_encode_path(&db))
                            .join(url_encode_path(&table))
                            .join(&part_name);

                        hardlink_dir(&part_entry.path(), &staging_dir)?;

                        let part_info = PartInfo {
                            name: part_name,
                            size: part_size,
                            backup_key: String::new(), // Set during upload
                            source: "uploaded".to_string(),
                            checksum_crc64: crc64,
                            s3_objects: None,
                        };

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

    // Suppress unused variable warning for disk_path_to_name (reserved for future use)
    let _ = &disk_path_to_name;

    Ok(result)
}

/// Parse metadata files in an S3 disk part directory to extract S3 object references.
///
/// Reads all non-checksums.txt files in the part directory as metadata files,
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

        let file_name = entry.file_name().to_string_lossy().to_string();

        // Skip checksums.txt -- it's the checksum file, not a metadata file
        if file_name == "checksums.txt" {
            continue;
        }

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
fn hardlink_dir(src_dir: &Path, dst_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dst_dir)
        .with_context(|| format!("Failed to create staging dir: {}", dst_dir.display()))?;

    for entry in WalkDir::new(src_dir) {
        let entry =
            entry.with_context(|| format!("Failed to walk directory: {}", src_dir.display()))?;

        let relative = entry
            .path()
            .strip_prefix(src_dir)
            .context("Failed to strip prefix from shadow path")?;

        let target = dst_dir.join(relative);

        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&target)?;
        } else {
            // Try hardlink first, fall back to copy on cross-device
            if let Err(e) = std::fs::hard_link(entry.path(), &target) {
                // EXDEV = cross-device link (error code 18 on Linux/macOS)
                let is_exdev = e.raw_os_error() == Some(18);
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

/// Calculate the total size of all files in a directory.
fn dir_size(path: &Path) -> Result<u64> {
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
    fn test_url_encode_path_simple() {
        assert_eq!(url_encode_path("default"), "default");
        assert_eq!(url_encode_path("my_database"), "my_database");
    }

    #[test]
    fn test_url_encode_path_special_chars() {
        assert_eq!(url_encode_path("my db"), "my%20db");
        assert_eq!(url_encode_path("test+table"), "test%2Btable");
    }

    #[test]
    fn test_url_encode_path_preserves_safe_chars() {
        assert_eq!(url_encode_path("a-b_c.d/e"), "a-b_c.d/e");
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

        hardlink_dir(src_dir.path(), &dst_path).unwrap();

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

        let disk_type_map = HashMap::from([("default".to_string(), "local".to_string())]);
        let disk_paths = HashMap::from([(
            "default".to_string(),
            data_path.to_string_lossy().to_string(),
        )]);

        let result = collect_parts(
            &data_path.to_string_lossy(),
            freeze,
            &backup_dir,
            &tables,
            &disk_type_map,
            &disk_paths,
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

        // Verify hardlink was created in staging
        let staged = backup_dir
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

        let disk_type_map = HashMap::from([
            ("default".to_string(), "local".to_string()),
            ("s3disk".to_string(), "s3".to_string()),
        ]);
        let disk_paths = HashMap::from([
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
            &backup_dir,
            &tables,
            &disk_type_map,
            &disk_paths,
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
    fn test_collected_part_has_disk_name() {
        // Verify CollectedPart struct has disk_name field
        let part = CollectedPart {
            database: "default".to_string(),
            table: "trades".to_string(),
            part_info: PartInfo {
                name: "202401_1_50_3".to_string(),
                size: 1024,
                backup_key: String::new(),
                source: "uploaded".to_string(),
                checksum_crc64: 12345,
                s3_objects: None,
            },
            disk_name: "s3disk".to_string(),
        };
        assert_eq!(part.disk_name, "s3disk");
        assert_eq!(part.database, "default");
        assert_eq!(part.table, "trades");
    }
}
