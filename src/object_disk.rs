//! Object disk metadata parser for ClickHouse S3 disk parts.
//!
//! ClickHouse stores data parts on S3 "object disks" with metadata files
//! that describe which S3 objects belong to each part. There are 5 metadata
//! format versions (see design doc section 3.7):
//!
//! | Version | Name                  | Path Format                          |
//! |---------|-----------------------|--------------------------------------|
//! | 1       | VersionAbsolutePaths  | Absolute S3 paths                    |
//! | 2       | VersionRelativePath   | Relative to disk root                |
//! | 3       | VersionReadOnlyFlag   | v2 + ReadOnly flag                   |
//! | 4       | VersionInlineData     | Small data inlined (ObjectSize=0)    |
//! | 5       | VersionFullObjectKey  | Complete object key (CH 24.1+)       |
//!
//! Version 5 (`VERSION_FULL_OBJECT_KEY`) has existed since ClickHouse 24.1 --
//! including 24.8, which this project CI-tests. ClickHouse reads those keys via
//! `ObjectStorageKey::createAsAbsolute`, meaning the stored string is the COMPLETE
//! object key and is *not* joined with the disk's key prefix. For versions 1-4 the
//! stored string is relative and ClickHouse joins it with the disk prefix itself.
//! Any key written back into a metadata file must respect that split -- see
//! [`restore_object_keys`].
//!
//! Metadata file format:
//! ```text
//! {version}
//! {object_count}\t{total_size}
//! {obj1_size}\t{obj1_path}
//! {obj2_size}\t{obj2_path}
//! {ref_count}
//! {read_only}       <- only if version >= 3
//! {inline_data}     <- only if version >= 4
//! ```

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use tracing::{info, warn};

use crate::clickhouse::client::DiskRow;

/// Parsed representation of a ClickHouse object disk metadata file.
#[derive(Debug, Clone, PartialEq)]
pub struct ObjectDiskMetadata {
    /// Metadata format version (1-5).
    pub version: u32,
    /// S3 object references within this part.
    pub objects: Vec<ObjectRef>,
    /// Total size of all objects in bytes.
    pub total_size: u64,
    /// Reference count (used by ClickHouse for deduplication).
    pub ref_count: u32,
    /// Read-only flag (version >= 3).
    pub read_only: bool,
    /// Inline data string for small objects (version >= 4, when ObjectSize == 0).
    pub inline_data: Option<String>,
}

/// Reference to a single S3 object within a part.
#[derive(Debug, Clone, PartialEq)]
pub struct ObjectRef {
    /// Object key exactly as stored in the metadata file: relative to the disk
    /// key prefix for v2-v4, a complete object key for v5, an absolute S3 URL
    /// for v1. Use [`disk_relative_key`] to normalize it to a disk-relative key.
    pub relative_path: String,
    /// Object size in bytes.
    pub size: u64,
}

/// Parse a ClickHouse object disk metadata file.
///
/// Handles all 5 format versions per design doc section 3.7. Object keys are
/// kept exactly as written in the file -- truncating a v5 key would discard
/// path components that are part of the key ClickHouse reads.
pub fn parse_metadata(content: &str) -> Result<ObjectDiskMetadata> {
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        anyhow::bail!("Empty metadata file");
    }

    // Line 0: version
    let version: u32 = lines[0]
        .trim()
        .parse()
        .context("Failed to parse metadata version")?;

    if !(1..=5).contains(&version) {
        anyhow::bail!("Unsupported metadata version: {}", version);
    }

    if lines.len() < 3 {
        anyhow::bail!(
            "Metadata file too short: expected at least 3 lines, got {}",
            lines.len()
        );
    }

    // Line 1: object_count \t total_size
    let header_parts: Vec<&str> = lines[1].split('\t').collect();
    if header_parts.len() < 2 {
        anyhow::bail!("Invalid metadata header line: '{}'", lines[1]);
    }
    let object_count: usize = header_parts[0]
        .trim()
        .parse()
        .context("Failed to parse object count")?;
    let total_size: u64 = header_parts[1]
        .trim()
        .parse()
        .context("Failed to parse total size")?;

    // Lines 2..2+object_count: size \t path
    let mut objects = Vec::with_capacity(object_count);
    for i in 0..object_count {
        let line_idx = 2 + i;
        if line_idx >= lines.len() {
            anyhow::bail!(
                "Metadata file truncated: expected {} objects, got {}",
                object_count,
                i
            );
        }
        let obj_parts: Vec<&str> = lines[line_idx].split('\t').collect();
        if obj_parts.len() < 2 {
            anyhow::bail!("Invalid object line: '{}'", lines[line_idx]);
        }
        let size: u64 = obj_parts[0]
            .trim()
            .parse()
            .context("Failed to parse object size")?;
        objects.push(ObjectRef {
            relative_path: obj_parts[1].trim().to_string(),
            size,
        });
    }

    // After objects: ref_count line
    let ref_count_idx = 2 + object_count;
    let ref_count: u32 = if ref_count_idx < lines.len() {
        lines[ref_count_idx]
            .trim()
            .parse()
            .context("Failed to parse ref_count")?
    } else {
        0
    };

    // Version >= 3: read_only flag
    let read_only_idx = 3 + object_count;
    let read_only = if version >= 3 && read_only_idx < lines.len() {
        lines[read_only_idx].trim() == "1"
    } else {
        false
    };

    // Version >= 4: inline_data
    let inline_data_idx = 4 + object_count;
    let inline_data = if version >= 4 && inline_data_idx < lines.len() {
        let data = lines[inline_data_idx].to_string();
        if data.is_empty() {
            None
        } else {
            Some(data)
        }
    } else {
        None
    };

    Ok(ObjectDiskMetadata {
        version,
        objects,
        total_size,
        ref_count,
        read_only,
        inline_data,
    })
}

/// Join a disk key prefix with a disk-relative key, tolerating an empty prefix
/// and stray slashes on either side of the join.
fn join_disk_key(disk_key_prefix: &str, disk_relative: &str) -> String {
    let prefix = disk_key_prefix.trim_matches('/');
    if prefix.is_empty() {
        disk_relative.to_string()
    } else {
        format!("{}/{}", prefix, disk_relative)
    }
}

/// Normalize a stored object key to a key relative to the disk's key prefix.
///
/// A v5 key is stored complete, so it starts with the source disk's key prefix;
/// stripping that prefix yields the same disk-relative form v2-v4 store directly.
/// Keys that do not carry the prefix (v2-v4, or a v5 disk whose prefix is empty)
/// are returned unchanged. This is what the manifest records, which is what makes
/// [`upload_source_key`] able to rebuild the source key without a new manifest field.
pub fn disk_relative_key(stored_key: &str, disk_key_prefix: &str) -> String {
    let prefix = disk_key_prefix.trim_matches('/');
    if prefix.is_empty() {
        return stored_key.to_string();
    }
    stored_key
        .strip_prefix(&format!("{}/", prefix))
        .unwrap_or(stored_key)
        .to_string()
}

/// Rebuild the CopyObject source key for an object stored on a source disk.
///
/// Inverse of [`disk_relative_key`]: `upload_source_key(disk_relative_key(k, p), p) == k`
/// for any key `k` that lives under prefix `p`.
pub fn upload_source_key(disk_relative: &str, source_prefix: &str) -> String {
    join_disk_key(source_prefix, disk_relative)
}

/// The two keys a restored object must agree on.
#[derive(Debug, Clone, PartialEq)]
pub struct RestoreObjectKeys {
    /// Destination of the CopyObject, relative to the destination disk's key prefix.
    pub copy_dest_relative_key: String,
    /// Key to write into the rewritten metadata file, in the form ClickHouse
    /// interprets for this metadata version.
    pub metadata_key: String,
}

/// Derive the restore-side keys for one object: where it is copied to, and how
/// that destination must be spelled inside the metadata file.
///
/// Both keys denote the same object. They differ in spelling because ClickHouse
/// resolves a v5 key with `createAsAbsolute` (the metadata carries the complete
/// key, prefix included) but a v2-v4 key relative to the disk key prefix (which
/// ClickHouse prepends itself). Emitting the relative form for v5 -- or the
/// absolute form for v2-v4 -- points the restored part at a key that does not exist.
pub fn restore_object_keys(
    version: u32,
    disk_relative: &str,
    uuid_prefix: &str,
    dest_disk_key_prefix: &str,
) -> RestoreObjectKeys {
    let copy_dest_relative_key = format!("{}/{}", uuid_prefix.trim_end_matches('/'), disk_relative);
    let metadata_key = if version >= 5 {
        join_disk_key(dest_disk_key_prefix, &copy_dest_relative_key)
    } else {
        copy_dest_relative_key.clone()
    };
    RestoreObjectKeys {
        copy_dest_relative_key,
        metadata_key,
    }
}

/// Rewrite metadata to point at the restored objects.
///
/// Object keys are re-derived through [`restore_object_keys`], so v5 files get the
/// complete destination key and v2-v4 files keep the bare disk-relative form.
/// Sets RefCount=0 and ReadOnly=false per design doc section 5.4 step 5.
/// Preserves inline data for v4+ objects.
pub fn rewrite_metadata(
    metadata: &ObjectDiskMetadata,
    uuid_prefix: &str,
    dest_disk_key_prefix: &str,
) -> String {
    let mut result = String::new();

    // Version
    result.push_str(&metadata.version.to_string());
    result.push('\n');

    // Object count and total size
    result.push_str(&format!(
        "{}\t{}\n",
        metadata.objects.len(),
        metadata.total_size
    ));

    // Object lines with rewritten keys
    for obj in &metadata.objects {
        let keys = restore_object_keys(
            metadata.version,
            &obj.relative_path,
            uuid_prefix,
            dest_disk_key_prefix,
        );
        result.push_str(&format!("{}\t{}\n", obj.size, keys.metadata_key));
    }

    // RefCount = 0 (per design doc)
    result.push_str("0\n");

    // ReadOnly = false (per design doc)
    if metadata.version >= 3 {
        result.push_str("0\n");
    }

    // Preserve inline data
    if metadata.version >= 4 {
        if let Some(ref data) = metadata.inline_data {
            result.push_str(data);
            result.push('\n');
        } else {
            result.push('\n');
        }
    }

    result
}

/// Serialize metadata back to its text format (without path rewriting).
///
/// Produces output that matches the original format for the given version.
pub fn serialize_metadata(metadata: &ObjectDiskMetadata) -> String {
    let mut result = String::new();

    // Version
    result.push_str(&metadata.version.to_string());
    result.push('\n');

    // Object count and total size
    result.push_str(&format!(
        "{}\t{}\n",
        metadata.objects.len(),
        metadata.total_size
    ));

    // Object lines
    for obj in &metadata.objects {
        result.push_str(&format!("{}\t{}\n", obj.size, obj.relative_path));
    }

    // RefCount
    result.push_str(&format!("{}\n", metadata.ref_count));

    // ReadOnly (version >= 3)
    if metadata.version >= 3 {
        result.push_str(if metadata.read_only { "1\n" } else { "0\n" });
    }

    // InlineData (version >= 4)
    if metadata.version >= 4 {
        if let Some(ref data) = metadata.inline_data {
            result.push_str(data);
            result.push('\n');
        } else {
            result.push('\n');
        }
    }

    result
}

/// Check if a disk type represents an S3 object disk.
///
/// Per design doc section 16.2, S3 object disks have type "s3" or
/// "object_storage". ClickHouse 24.8+ reports the type as "ObjectStorage"
/// (capitalized), so comparison is case-insensitive.
pub fn is_s3_disk(disk_type: &str) -> bool {
    let lower = disk_type.to_ascii_lowercase();
    lower == "s3" || lower == "object_storage" || lower == "objectstorage"
}

/// Check if a disk is a cache layer (e.g., `s3_cache`).
///
/// Cache disks are not real storage — they wrap another disk and should be
/// skipped during backup. ClickHouse reports a non-empty `cache_path` in
/// `system.disks` for cache-layer disks.
pub fn is_cache_disk(disk: &DiskRow) -> bool {
    !disk.cache_path.is_empty()
}

/// Normalize disk type for CH 24.8+ where ObjectStorage replaced "s3".
///
/// CH 24.8+ reports disk type as "ObjectStorage" with a separate
/// `object_storage_type` field (e.g. "S3"). This function maps that
/// combination back to the lowercase storage type for downstream
/// `is_s3_disk()` compatibility.
pub fn normalize_disk_type(disk_type: &str, object_storage_type: &str) -> String {
    if disk_type.eq_ignore_ascii_case("objectstorage") && !object_storage_type.is_empty() {
        object_storage_type.to_ascii_lowercase()
    } else {
        disk_type.to_lowercase()
    }
}

/// Build disk remote paths with S3 endpoint discovery fallback for CH 24.8+.
///
/// Collects `remote_path` from disks that have it, then discovers endpoints
/// from ClickHouse config files for any S3 disks that lack `remote_path`
/// (common in CH 24.8 where `system.disks` doesn't expose it).
pub fn build_disk_remote_paths(disks: &[DiskRow], config_dir: &str) -> BTreeMap<String, String> {
    let mut paths: BTreeMap<String, String> = disks
        .iter()
        .filter(|d| !d.remote_path.is_empty())
        .map(|d| (d.name.clone(), d.remote_path.clone()))
        .collect();

    let s3_without: Vec<&str> = disks
        .iter()
        .filter(|d| {
            let eff = normalize_disk_type(&d.disk_type, &d.object_storage_type);
            is_s3_disk(&eff) && !paths.contains_key(&d.name) && !is_cache_disk(d)
        })
        .map(|d| d.name.as_str())
        .collect();

    if !s3_without.is_empty() {
        info!(
            disks = ?s3_without,
            "S3 disks without remote_path, discovering from ClickHouse config"
        );
        let discovered = crate::clickhouse::client::discover_s3_disk_endpoints(config_dir);
        for name in &s3_without {
            if let Some(uri) = discovered.get(*name) {
                paths.insert(name.to_string(), uri.clone());
            } else {
                warn!(disk = %name, "Could not discover S3 endpoint for disk");
            }
        }
    }

    // Strategy A: Resolve cache/alias disks via same-path matching.
    // When two disks share the same normalized path, copy the endpoint from the
    // resolved disk to the unresolved one. Handles production case where s3_cache
    // shares path with underlying s3 disk.
    let mut disk_by_path: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, d) in disks.iter().enumerate() {
        let normalized = d.path.trim_end_matches('/').to_string();
        disk_by_path.entry(normalized).or_default().push(i);
    }
    for indices in disk_by_path.values() {
        // Find the first disk at this path that has a resolved endpoint
        let resolved = indices
            .iter()
            .find(|&&i| paths.contains_key(&disks[i].name));
        if let Some(&source_idx) = resolved {
            let source_endpoint = paths[&disks[source_idx].name].clone();
            for &i in indices {
                if !paths.contains_key(&disks[i].name) {
                    let eff =
                        normalize_disk_type(&disks[i].disk_type, &disks[i].object_storage_type);
                    if is_s3_disk(&eff) {
                        paths.insert(disks[i].name.clone(), source_endpoint.clone());
                        info!(
                            disk = %disks[i].name,
                            source_disk = %disks[source_idx].name,
                            "Inherited S3 endpoint from same-path disk"
                        );
                    }
                }
            }
        }
    }

    // Strategy B: XML-based cache->underlying disk mapping for disks still unresolved.
    let unresolved: Vec<&str> = disks
        .iter()
        .filter(|d| {
            let eff = normalize_disk_type(&d.disk_type, &d.object_storage_type);
            is_s3_disk(&eff) && !paths.contains_key(&d.name)
        })
        .map(|d| d.name.as_str())
        .collect();
    if !unresolved.is_empty() {
        let cache_refs = crate::clickhouse::client::discover_cache_disk_refs(config_dir);
        for name in &unresolved {
            if let Some(underlying) = cache_refs.get(*name) {
                if let Some(endpoint) = paths.get(underlying) {
                    paths.insert(name.to_string(), endpoint.clone());
                    info!(
                        disk = %name,
                        underlying = %underlying,
                        "Resolved cache disk via XML config reference"
                    );
                }
            }
        }
    }

    paths
}

/// Resolve ClickHouse macros (`{cluster}`, `{replica}`, etc.) in disk remote paths.
///
/// ClickHouse config files and `system.disks` may contain unresolved macro
/// placeholders like `{cluster}` or `{replica}`. These must be replaced with
/// actual values from `system.macros` before constructing S3 CopyObject source keys.
pub fn resolve_macros_in_paths(
    paths: &mut BTreeMap<String, String>,
    macros: &std::collections::HashMap<String, String>,
) {
    if macros.is_empty() {
        return;
    }
    for (disk_name, path) in paths.iter_mut() {
        if !path.contains('{') {
            continue;
        }
        let original = path.clone();
        for (key, value) in macros {
            let pattern = format!("{{{}}}", key);
            *path = path.replace(&pattern, value);
        }
        if *path != original {
            info!(
                disk = %disk_name,
                original = %original,
                resolved = %path,
                "Resolved macros in disk remote path"
            );
        }
        if path.contains('{') {
            warn!(
                disk = %disk_name,
                path = %path,
                "Disk remote path still contains unresolved macros"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_v1_absolute_paths() {
        let content = "1\n\
                        2\t300\n\
                        100\ts3://mybucket/store/abc/data.bin\n\
                        200\ts3://mybucket/store/abc/index.mrk\n\
                        1\n";
        let meta = parse_metadata(content).unwrap();
        assert_eq!(meta.version, 1);
        assert_eq!(meta.objects.len(), 2);
        assert_eq!(meta.total_size, 300);
        assert_eq!(
            meta.objects[0].relative_path,
            "s3://mybucket/store/abc/data.bin"
        );
        assert_eq!(meta.objects[0].size, 100);
        assert_eq!(
            meta.objects[1].relative_path,
            "s3://mybucket/store/abc/index.mrk"
        );
        assert_eq!(meta.objects[1].size, 200);
        assert_eq!(meta.ref_count, 1);
        assert!(!meta.read_only);
        assert!(meta.inline_data.is_none());
    }

    #[test]
    fn test_parse_v2_relative_path() {
        let content = "2\n\
                        1\t500\n\
                        500\tstore/abc/def/data.bin\n\
                        2\n";
        let meta = parse_metadata(content).unwrap();
        assert_eq!(meta.version, 2);
        assert_eq!(meta.objects.len(), 1);
        assert_eq!(meta.total_size, 500);
        assert_eq!(meta.objects[0].relative_path, "store/abc/def/data.bin");
        assert_eq!(meta.objects[0].size, 500);
        assert_eq!(meta.ref_count, 2);
        assert!(!meta.read_only);
        assert!(meta.inline_data.is_none());
    }

    #[test]
    fn test_parse_v3_read_only_flag() {
        let content = "3\n\
                        1\t500\n\
                        500\tstore/abc/def/data.bin\n\
                        1\n\
                        1\n";
        let meta = parse_metadata(content).unwrap();
        assert_eq!(meta.version, 3);
        assert!(meta.read_only);
        assert_eq!(meta.ref_count, 1);
    }

    #[test]
    fn test_parse_v3_not_read_only() {
        let content = "3\n\
                        1\t500\n\
                        500\tstore/abc/def/data.bin\n\
                        1\n\
                        0\n";
        let meta = parse_metadata(content).unwrap();
        assert_eq!(meta.version, 3);
        assert!(!meta.read_only);
    }

    #[test]
    fn test_parse_v4_inline_data() {
        // Version 4 with inline data (ObjectSize=0 means data is inlined)
        let content = "4\n\
                        1\t0\n\
                        0\tstore/abc/def/data.bin\n\
                        1\n\
                        0\n\
                        SGVsbG8gV29ybGQ=\n";
        let meta = parse_metadata(content).unwrap();
        assert_eq!(meta.version, 4);
        assert_eq!(meta.objects.len(), 1);
        assert_eq!(meta.objects[0].size, 0);
        assert_eq!(meta.total_size, 0);
        assert!(!meta.read_only);
        assert_eq!(meta.inline_data, Some("SGVsbG8gV29ybGQ=".to_string()));
    }

    #[test]
    fn test_parse_v5_full_object_key() {
        // Version 5: the stored string is the complete object key, kept verbatim
        let content = "5\n\
                        1\t1024\n\
                        1024\tstore/abc/def/ghi/data.bin\n\
                        1\n\
                        0\n\
                        \n";
        let meta = parse_metadata(content).unwrap();
        assert_eq!(meta.version, 5);
        assert_eq!(meta.objects.len(), 1);
        assert_eq!(meta.objects[0].relative_path, "store/abc/def/ghi/data.bin");
        assert_eq!(meta.objects[0].size, 1024);
    }

    #[test]
    fn test_rewrite_metadata_v2() {
        let content = "2\n\
                        2\t700\n\
                        500\tstore/abc/def/data.bin\n\
                        200\tstore/abc/def/index.mrk\n\
                        3\n";
        let meta = parse_metadata(content).unwrap();
        let rewritten = rewrite_metadata(&meta, "store/new_uuid/xyz", "");

        let lines: Vec<&str> = rewritten.lines().collect();
        assert_eq!(lines[0], "2");
        assert_eq!(lines[1], "2\t700");
        assert_eq!(lines[2], "500\tstore/new_uuid/xyz/store/abc/def/data.bin");
        assert_eq!(lines[3], "200\tstore/new_uuid/xyz/store/abc/def/index.mrk");
        assert_eq!(lines[4], "0"); // RefCount = 0 (per design doc)
    }

    #[test]
    fn test_rewrite_metadata_v3_resets_readonly() {
        let content = "3\n\
                        1\t500\n\
                        500\tstore/abc/def/data.bin\n\
                        5\n\
                        1\n";
        let meta = parse_metadata(content).unwrap();
        assert!(meta.read_only);

        let rewritten = rewrite_metadata(&meta, "store/new", "");
        let lines: Vec<&str> = rewritten.lines().collect();
        // line 0: version, 1: header, 2: object, 3: ref_count, 4: read_only
        assert_eq!(lines[3], "0"); // RefCount = 0
        assert_eq!(lines[4], "0"); // ReadOnly = false
    }

    #[test]
    fn test_rewrite_metadata_v4_preserves_inline() {
        let content = "4\n\
                        1\t0\n\
                        0\tstore/abc/data.bin\n\
                        1\n\
                        0\n\
                        SGVsbG8gV29ybGQ=\n";
        let meta = parse_metadata(content).unwrap();
        let rewritten = rewrite_metadata(&meta, "store/new", "");

        let lines: Vec<&str> = rewritten.lines().collect();
        // line 0: version, 1: header, 2: object, 3: ref_count, 4: read_only, 5: inline_data
        assert_eq!(lines[0], "4"); // version preserved
        assert_eq!(lines[3], "0"); // RefCount = 0
        assert_eq!(lines[4], "0"); // ReadOnly = false
        assert_eq!(lines[5], "SGVsbG8gV29ybGQ="); // Inline data preserved
    }

    #[test]
    fn test_serialize_roundtrip() {
        let content = "2\n\
                        2\t700\n\
                        500\tstore/abc/def/data.bin\n\
                        200\tstore/abc/def/index.mrk\n\
                        3\n";
        let meta = parse_metadata(content).unwrap();
        let serialized = serialize_metadata(&meta);
        let reparsed = parse_metadata(&serialized).unwrap();

        assert_eq!(meta.version, reparsed.version);
        assert_eq!(meta.objects.len(), reparsed.objects.len());
        assert_eq!(meta.total_size, reparsed.total_size);
        assert_eq!(meta.ref_count, reparsed.ref_count);
        for (orig, re) in meta.objects.iter().zip(reparsed.objects.iter()) {
            assert_eq!(orig.relative_path, re.relative_path);
            assert_eq!(orig.size, re.size);
        }
    }

    #[test]
    fn test_serialize_roundtrip_v3() {
        let content = "3\n\
                        1\t500\n\
                        500\tstore/abc/data.bin\n\
                        2\n\
                        1\n";
        let meta = parse_metadata(content).unwrap();
        let serialized = serialize_metadata(&meta);
        let reparsed = parse_metadata(&serialized).unwrap();

        assert_eq!(meta.version, reparsed.version);
        assert_eq!(meta.read_only, reparsed.read_only);
    }

    #[test]
    fn test_serialize_roundtrip_v4() {
        let content = "4\n\
                        1\t0\n\
                        0\tstore/abc/data.bin\n\
                        1\n\
                        0\n\
                        SGVsbG8gV29ybGQ=\n";
        let meta = parse_metadata(content).unwrap();
        let serialized = serialize_metadata(&meta);
        let reparsed = parse_metadata(&serialized).unwrap();

        assert_eq!(meta.version, reparsed.version);
        assert_eq!(meta.inline_data, reparsed.inline_data);
    }

    #[test]
    fn test_is_s3_disk() {
        assert!(is_s3_disk("s3"));
        assert!(is_s3_disk("object_storage"));
        assert!(is_s3_disk("S3")); // case-insensitive
        assert!(is_s3_disk("ObjectStorage")); // CH 24.8+ format
        assert!(is_s3_disk("OBJECT_STORAGE")); // uppercase variant
        assert!(!is_s3_disk("local"));
        assert!(!is_s3_disk("cache"));
        assert!(!is_s3_disk(""));
    }

    #[test]
    fn test_parse_empty_content() {
        let result = parse_metadata("");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_invalid_version() {
        let content = "6\n\
                        1\t100\n\
                        100\tdata.bin\n\
                        0\n";
        let result = parse_metadata(content);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_multiple_objects() {
        let content = "2\n\
                        3\t1500\n\
                        500\tstore/abc/data.bin\n\
                        700\tstore/abc/index.mrk\n\
                        300\tstore/abc/primary.idx\n\
                        0\n";
        let meta = parse_metadata(content).unwrap();
        assert_eq!(meta.objects.len(), 3);
        assert_eq!(meta.total_size, 1500);
        assert_eq!(meta.objects[0].size, 500);
        assert_eq!(meta.objects[1].size, 700);
        assert_eq!(meta.objects[2].size, 300);
    }

    #[test]
    fn test_object_key_v4_rewrite_is_prefix_free() {
        // v2-v4 metadata is relative: ClickHouse joins the disk key prefix itself,
        // so the rewritten bytes must carry only {uuid_prefix}/{disk_relative} --
        // identical to the behavior before v5 handling was introduced.
        let content = "4\n\
                        1\t500\n\
                        500\tstore/abc/202401_1_50_3/data.bin\n\
                        7\n\
                        1\n\
                        SGVsbG8gV29ybGQ=\n";
        let meta = parse_metadata(content).unwrap();
        let rewritten = rewrite_metadata(&meta, "store/5f3/5f3a-uuid", "clickhouse-disks");

        assert_eq!(
            rewritten,
            "4\n\
             1\t500\n\
             500\tstore/5f3/5f3a-uuid/store/abc/202401_1_50_3/data.bin\n\
             0\n\
             0\n\
             SGVsbG8gV29ybGQ=\n"
        );
    }

    #[test]
    fn test_object_key_v5_full_key_survives_round_trip() {
        // A v5 key keeps every component: truncating it to the last two would make
        // the upload source key unreconstructible.
        let content = "5\n\
                        1\t2048\n\
                        2048\tclickhouse-disks/store/abc/abcdef-1234/data.bin\n\
                        1\n\
                        0\n\
                        \n";
        let meta = parse_metadata(content).unwrap();
        let stored_key = &meta.objects[0].relative_path;
        assert_eq!(
            stored_key,
            "clickhouse-disks/store/abc/abcdef-1234/data.bin"
        );

        let relative = disk_relative_key(stored_key, "clickhouse-disks");
        assert_eq!(relative, "store/abc/abcdef-1234/data.bin");
        assert_eq!(
            &upload_source_key(&relative, "clickhouse-disks"),
            stored_key
        );
    }

    #[test]
    fn test_object_key_disk_relative_key_without_prefix() {
        // v2-v4 keys (and any key not under the prefix) pass through unchanged.
        assert_eq!(
            disk_relative_key("store/abc/data.bin", "clickhouse-disks"),
            "store/abc/data.bin"
        );
        assert_eq!(
            disk_relative_key("store/abc/data.bin", ""),
            "store/abc/data.bin"
        );
    }

    #[test]
    fn test_object_key_restore_keys_are_version_aware() {
        let v5 = restore_object_keys(5, "store/abc/part/data.bin", "store/5f3/5f3a-uuid", "disks");
        assert_eq!(
            v5.copy_dest_relative_key,
            "store/5f3/5f3a-uuid/store/abc/part/data.bin"
        );
        assert_eq!(
            v5.metadata_key,
            "disks/store/5f3/5f3a-uuid/store/abc/part/data.bin"
        );

        let v4 = restore_object_keys(4, "store/abc/part/data.bin", "store/5f3/5f3a-uuid", "disks");
        assert_eq!(v4.metadata_key, v4.copy_dest_relative_key);
    }

    #[test]
    fn test_object_key_v5_rewrite_emits_absolute_key() {
        let content = "5\n\
                        1\t2048\n\
                        2048\tclickhouse-disks/store/abc/abcdef-1234/data.bin\n\
                        1\n\
                        0\n\
                        \n";
        let meta = parse_metadata(content).unwrap();
        let relative = disk_relative_key(&meta.objects[0].relative_path, "clickhouse-disks");
        let meta = ObjectDiskMetadata {
            objects: vec![ObjectRef {
                relative_path: relative,
                size: meta.objects[0].size,
            }],
            ..meta
        };

        let rewritten = rewrite_metadata(&meta, "store/5f3/5f3a-uuid", "clickhouse-disks");
        let lines: Vec<&str> = rewritten.lines().collect();
        assert_eq!(
            lines[2],
            "2048\tclickhouse-disks/store/5f3/5f3a-uuid/store/abc/abcdef-1234/data.bin"
        );
    }

    #[test]
    fn test_rewrite_metadata_trailing_slash_prefix() {
        let content = "2\n\
                        1\t500\n\
                        500\tstore/abc/data.bin\n\
                        1\n";
        let meta = parse_metadata(content).unwrap();
        let rewritten = rewrite_metadata(&meta, "store/new/", "");

        let lines: Vec<&str> = rewritten.lines().collect();
        // Should not have double slash
        assert_eq!(lines[2], "500\tstore/new/store/abc/data.bin");
    }

    #[test]
    fn test_parse_file_too_short_one_line() {
        // Covers lines 76, 78: file with only version line (< 3 lines)
        let content = "2\n";
        let result = parse_metadata(content);
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("too short"),
            "Expected 'too short' error, got: {}",
            err_msg
        );
    }

    #[test]
    fn test_parse_file_too_short_two_lines() {
        // Covers lines 76, 78: file with version and header but no object lines
        // "2\n1\t500\n" has only 2 lines, which is < 3, so it triggers the "too short" check
        let content = "2\n1\t500\n";
        let result = parse_metadata(content);
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("too short"),
            "Expected 'too short' error, got: {}",
            err_msg
        );
    }

    #[test]
    fn test_parse_header_missing_tab() {
        // Covers line 85: header line without tab separator
        let content = "2\n1 500\n500\tdata.bin\n0\n";
        let result = parse_metadata(content);
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("Invalid metadata header"),
            "Expected 'Invalid metadata header' error, got: {}",
            err_msg
        );
    }

    #[test]
    fn test_parse_truncated_objects_section() {
        // Covers lines 101: object_count says 3 but only 1 object line present
        let content = "2\n3\t1500\n500\tstore/abc/data.bin\n";
        let result = parse_metadata(content);
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("truncated"),
            "Expected 'truncated' error, got: {}",
            err_msg
        );
    }

    #[test]
    fn test_parse_invalid_object_line_no_tab() {
        // Covers line 109: object line without tab separator
        let content = "2\n1\t500\n500 store/abc/data.bin\n0\n";
        let result = parse_metadata(content);
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("Invalid object line"),
            "Expected 'Invalid object line' error, got: {}",
            err_msg
        );
    }

    #[test]
    fn test_parse_v1_missing_ref_count() {
        // Covers line 138: ref_count line missing, defaults to 0
        // v1 metadata with exactly 3 lines (version + header + 1 object, no ref_count line)
        let content = "1\n1\t100\n100\ts3://bucket/data.bin";
        let meta = parse_metadata(content).unwrap();
        assert_eq!(meta.version, 1);
        assert_eq!(meta.ref_count, 0);
        assert_eq!(meta.objects.len(), 1);
    }

    #[test]
    fn test_rewrite_metadata_v4_no_inline_data() {
        // Covers line 226: rewrite_metadata for v4 with inline_data = None
        let meta = ObjectDiskMetadata {
            version: 4,
            objects: vec![ObjectRef {
                relative_path: "store/abc/data.bin".to_string(),
                size: 500,
            }],
            total_size: 500,
            ref_count: 1,
            read_only: false,
            inline_data: None,
        };
        let rewritten = rewrite_metadata(&meta, "store/new", "");
        let lines: Vec<&str> = rewritten.lines().collect();
        assert_eq!(lines[0], "4"); // version
        assert_eq!(lines[1], "1\t500"); // object count + total size
        assert_eq!(lines[2], "500\tstore/new/store/abc/data.bin"); // object
        assert_eq!(lines[3], "0"); // ref_count = 0
        assert_eq!(lines[4], "0"); // read_only = false
                                   // Line 5 should be empty (inline_data = None produces empty line)
        assert_eq!(lines.len(), 6);
        assert_eq!(lines[5], "");
    }

    #[test]
    fn test_serialize_metadata_v4_no_inline_data() {
        // Covers line 269: serialize_metadata for v4 with inline_data = None
        let meta = ObjectDiskMetadata {
            version: 4,
            objects: vec![ObjectRef {
                relative_path: "store/abc/data.bin".to_string(),
                size: 500,
            }],
            total_size: 500,
            ref_count: 2,
            read_only: false,
            inline_data: None,
        };
        let serialized = serialize_metadata(&meta);
        let lines: Vec<&str> = serialized.lines().collect();
        assert_eq!(lines[0], "4"); // version
        assert_eq!(lines[1], "1\t500"); // header
        assert_eq!(lines[2], "500\tstore/abc/data.bin"); // object
        assert_eq!(lines[3], "2"); // ref_count
        assert_eq!(lines[4], "0"); // read_only
                                   // Line 5 should be empty (inline_data = None produces empty line)
        assert_eq!(lines.len(), 6);
        assert_eq!(lines[5], "");
    }

    #[test]
    fn test_serialize_metadata_v3_read_only_true() {
        // Covers line 260: serialize_metadata with read_only=true for v3
        let meta = ObjectDiskMetadata {
            version: 3,
            objects: vec![ObjectRef {
                relative_path: "store/abc/data.bin".to_string(),
                size: 500,
            }],
            total_size: 500,
            ref_count: 1,
            read_only: true,
            inline_data: None,
        };
        let serialized = serialize_metadata(&meta);
        let lines: Vec<&str> = serialized.lines().collect();
        assert_eq!(lines[4], "1"); // read_only = true
    }

    #[test]
    fn test_normalize_disk_type_objectstorage_s3() {
        assert_eq!(normalize_disk_type("ObjectStorage", "S3"), "s3");
    }

    #[test]
    fn test_normalize_disk_type_objectstorage_hdfs() {
        assert_eq!(normalize_disk_type("ObjectStorage", "HDFS"), "hdfs");
    }

    #[test]
    fn test_normalize_disk_type_objectstorage_empty() {
        // Empty object_storage_type means we can't normalize, keep as-is (lowercased)
        assert_eq!(normalize_disk_type("ObjectStorage", ""), "objectstorage");
    }

    #[test]
    fn test_normalize_disk_type_local() {
        assert_eq!(normalize_disk_type("local", ""), "local");
    }

    #[test]
    fn test_normalize_disk_type_s3_passthrough() {
        assert_eq!(normalize_disk_type("s3", ""), "s3");
    }

    #[test]
    fn test_resolve_macros_in_paths() {
        let mut paths = BTreeMap::from([
            (
                "s3disk".to_string(),
                "s3://bucket/clickhouse/{cluster}/{replica}/".to_string(),
            ),
            ("local".to_string(), "/data/local".to_string()),
        ]);
        let macros = std::collections::HashMap::from([
            ("cluster".to_string(), "mycluster".to_string()),
            ("replica".to_string(), "replica1".to_string()),
        ]);
        resolve_macros_in_paths(&mut paths, &macros);
        assert_eq!(
            paths["s3disk"],
            "s3://bucket/clickhouse/mycluster/replica1/"
        );
        assert_eq!(paths["local"], "/data/local");
    }

    #[test]
    fn test_resolve_macros_empty_macros() {
        let mut paths =
            BTreeMap::from([("s3disk".to_string(), "s3://bucket/{cluster}/".to_string())]);
        let macros = std::collections::HashMap::new();
        resolve_macros_in_paths(&mut paths, &macros);
        assert_eq!(paths["s3disk"], "s3://bucket/{cluster}/");
    }

    #[test]
    fn test_build_disk_remote_paths_cache_inherits_same_path() {
        // Simulate production case: s3 and s3_cache share the same path.
        // s3 has remote_path, s3_cache does not.
        let disks = vec![
            DiskRow {
                name: "s3".to_string(),
                path: "/var/lib/clickhouse/disks/s3/".to_string(),
                disk_type: "ObjectStorage".to_string(),
                remote_path: "s3://my-bucket/data/".to_string(),
                object_storage_type: "S3".to_string(),
                cache_path: "".to_string(),
            },
            DiskRow {
                name: "s3_cache".to_string(),
                path: "/var/lib/clickhouse/disks/s3/".to_string(),
                disk_type: "ObjectStorage".to_string(),
                remote_path: "".to_string(),
                object_storage_type: "S3".to_string(),
                cache_path: "/var/lib/clickhouse/cache/s3_cache".to_string(),
            },
        ];
        // Use a non-existent config dir since we don't need XML fallback here
        let paths = build_disk_remote_paths(&disks, "/nonexistent");
        assert_eq!(paths.get("s3").unwrap(), "s3://my-bucket/data/");
        assert_eq!(
            paths.get("s3_cache").unwrap(),
            "s3://my-bucket/data/",
            "Cache disk should inherit endpoint from same-path real disk"
        );
    }

    #[test]
    fn test_build_disk_remote_paths_no_same_path_no_inherit() {
        // When cache disk has a different path, Strategy A should not apply
        let disks = vec![
            DiskRow {
                name: "s3".to_string(),
                path: "/var/lib/clickhouse/disks/s3/".to_string(),
                disk_type: "ObjectStorage".to_string(),
                remote_path: "s3://my-bucket/data/".to_string(),
                object_storage_type: "S3".to_string(),
                cache_path: "".to_string(),
            },
            DiskRow {
                name: "s3_cache".to_string(),
                path: "/var/lib/clickhouse/disks/s3_cache/".to_string(),
                disk_type: "ObjectStorage".to_string(),
                remote_path: "".to_string(),
                object_storage_type: "S3".to_string(),
                cache_path: "/var/lib/clickhouse/cache/s3_cache".to_string(),
            },
        ];
        let paths = build_disk_remote_paths(&disks, "/nonexistent");
        assert_eq!(paths.get("s3").unwrap(), "s3://my-bucket/data/");
        // s3_cache has a different path, so Strategy A doesn't apply.
        // Strategy B would need XML config which doesn't exist here.
        assert!(
            !paths.contains_key("s3_cache"),
            "Cache disk with different path should not inherit via same-path strategy"
        );
    }

    #[test]
    fn test_is_cache_disk() {
        let cache = DiskRow {
            name: "s3_cache".to_string(),
            path: "/disks/s3/".to_string(),
            disk_type: "ObjectStorage".to_string(),
            remote_path: "".to_string(),
            object_storage_type: "S3".to_string(),
            cache_path: "/cache/s3_cache".to_string(),
        };
        assert!(is_cache_disk(&cache));

        let real = DiskRow {
            name: "s3".to_string(),
            path: "/disks/s3/".to_string(),
            disk_type: "ObjectStorage".to_string(),
            remote_path: "s3://bucket/".to_string(),
            object_storage_type: "S3".to_string(),
            cache_path: "".to_string(),
        };
        assert!(!is_cache_disk(&real));
    }
}
