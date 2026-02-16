//! Shadow directory walk and part collection.
//!
//! After FREEZE, ClickHouse places hardlinks in:
//!   `{data_path}/shadow/{freeze_name}/store/{shard_hex_prefix}/{table_hex_uuid}/{part_name}/...`
//!
//! This module walks those directories, hardlinks files to the backup staging area,
//! and computes CRC64 checksums.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::{debug, warn};
use walkdir::WalkDir;

use super::checksum::compute_crc64;
use crate::clickhouse::client::TableRow;
use crate::manifest::PartInfo;

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
        if !table.uuid.is_empty()
            && table.uuid != "00000000-0000-0000-0000-000000000000"
        {
            map.insert(
                table.uuid.clone(),
                (table.database.clone(), table.name.clone()),
            );
        }
    }
    map
}

/// Represents a collected part from the shadow directory.
pub struct CollectedPart {
    pub database: String,
    pub table: String,
    pub part_info: PartInfo,
}

/// Walk the shadow directory for a given freeze name and collect parts.
///
/// For each part found:
/// 1. Identifies the owning table via UUID mapping
/// 2. Hardlinks (or copies) files to the backup staging directory
/// 3. Computes CRC64 checksum of the checksums.txt file
///
/// Returns a mapping of "db.table" -> Vec<PartInfo>.
pub fn collect_parts(
    data_path: &str,
    freeze_name: &str,
    backup_dir: &Path,
    tables: &[TableRow],
) -> Result<HashMap<String, Vec<PartInfo>>> {
    let shadow_dir = PathBuf::from(data_path)
        .join("shadow")
        .join(freeze_name);

    if !shadow_dir.exists() {
        debug!(
            shadow_dir = %shadow_dir.display(),
            "Shadow directory does not exist (table may have no data)"
        );
        return Ok(HashMap::new());
    }

    let uuid_map = build_uuid_map(tables);
    let mut result: HashMap<String, Vec<PartInfo>> = HashMap::new();

    // Walk the shadow directory looking for part directories
    // Structure: shadow/{freeze_name}/store/{3char_prefix}/{uuid_dir}/{part_name}/...
    let store_dir = shadow_dir.join("store");
    if !store_dir.exists() {
        debug!(
            store_dir = %store_dir.display(),
            "Store directory does not exist in shadow"
        );
        return Ok(HashMap::new());
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

            let uuid_dir_name = uuid_entry
                .file_name()
                .to_string_lossy()
                .to_string();

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

                let part_name = part_entry
                    .file_name()
                    .to_string_lossy()
                    .to_string();

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
                let crc64 = compute_crc64(&checksums_path)
                    .with_context(|| {
                        format!(
                            "Failed to compute CRC64 for {}/{}",
                            full_table_name, part_name
                        )
                    })?;

                // Compute part size
                let part_size = dir_size(&part_entry.path())?;

                // Hardlink (or copy) files to backup staging directory
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
                    .push(part_info);
            }
        }
    }

    Ok(result)
}

/// Recursively hardlink all files from src_dir to dst_dir.
///
/// Creates dst_dir and any needed subdirectories. On EXDEV (cross-device)
/// error, falls back to copying.
fn hardlink_dir(src_dir: &Path, dst_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dst_dir)
        .with_context(|| format!("Failed to create staging dir: {}", dst_dir.display()))?;

    for entry in WalkDir::new(src_dir) {
        let entry = entry
            .with_context(|| format!("Failed to walk directory: {}", src_dir.display()))?;

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
}
