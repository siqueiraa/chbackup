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
//! | 5       | VersionFullObjectKey  | Full object key (CH 25.10+)          |
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

use anyhow::{Context, Result};

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
    /// Object path (relative to disk root for v2+, absolute for v1).
    pub relative_path: String,
    /// Object size in bytes.
    pub size: u64,
}

/// Parse a ClickHouse object disk metadata file.
///
/// Handles all 5 format versions per design doc section 3.7.
/// For version 5 (FullObjectKey), extracts the last 2 path components
/// to normalize to a relative path.
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
        let path = obj_parts[1].trim().to_string();

        // Version 5: extract last 2 path components for relative path
        let relative_path = if version == 5 {
            extract_relative_path_v5(&path)
        } else {
            path
        };

        objects.push(ObjectRef {
            relative_path,
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

/// Extract the last 2 path components from a full S3 key (version 5).
///
/// Example: `s3://bucket/store/abc/def/data.bin` -> `def/data.bin`
/// Example: `store/abc/def/ghi/data.bin` -> `ghi/data.bin`
fn extract_relative_path_v5(full_path: &str) -> String {
    let parts: Vec<&str> = full_path.rsplitn(3, '/').collect();
    if parts.len() >= 2 {
        // rsplitn gives: [last, second_to_last, rest...]
        format!("{}/{}", parts[1], parts[0])
    } else {
        full_path.to_string()
    }
}

/// Rewrite metadata with a new path prefix for restore.
///
/// Updates object paths to use the new prefix, sets RefCount=0 and
/// ReadOnly=false per design doc section 5.4 step 5.
/// Preserves inline data for v4+ objects.
pub fn rewrite_metadata(metadata: &ObjectDiskMetadata, new_prefix: &str) -> String {
    let new_prefix = new_prefix.trim_end_matches('/');
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

    // Object lines with rewritten paths
    for obj in &metadata.objects {
        let new_path = format!("{}/{}", new_prefix, obj.relative_path);
        result.push_str(&format!("{}\t{}\n", obj.size, new_path));
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
        // Version 5: full absolute key, we extract last 2 path components
        let content = "5\n\
                        1\t1024\n\
                        1024\tstore/abc/def/ghi/data.bin\n\
                        1\n\
                        0\n\
                        \n";
        let meta = parse_metadata(content).unwrap();
        assert_eq!(meta.version, 5);
        assert_eq!(meta.objects.len(), 1);
        // Last 2 path components: ghi/data.bin
        assert_eq!(meta.objects[0].relative_path, "ghi/data.bin");
        assert_eq!(meta.objects[0].size, 1024);
    }

    #[test]
    fn test_parse_v5_long_path() {
        let content = "5\n\
                        1\t2048\n\
                        2048\ts3://mybucket/prefix/store/abc/def/202401_1_50_3/data.bin\n\
                        0\n\
                        0\n\
                        \n";
        let meta = parse_metadata(content).unwrap();
        assert_eq!(meta.version, 5);
        // Last 2 components: 202401_1_50_3/data.bin
        assert_eq!(meta.objects[0].relative_path, "202401_1_50_3/data.bin");
    }

    #[test]
    fn test_rewrite_metadata_v2() {
        let content = "2\n\
                        2\t700\n\
                        500\tstore/abc/def/data.bin\n\
                        200\tstore/abc/def/index.mrk\n\
                        3\n";
        let meta = parse_metadata(content).unwrap();
        let rewritten = rewrite_metadata(&meta, "store/new_uuid/xyz");

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

        let rewritten = rewrite_metadata(&meta, "store/new");
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
        let rewritten = rewrite_metadata(&meta, "store/new");

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
    fn test_extract_relative_path_v5() {
        assert_eq!(
            extract_relative_path_v5("store/abc/def/data.bin"),
            "def/data.bin"
        );
        assert_eq!(
            extract_relative_path_v5("s3://mybucket/prefix/store/abc/def/202401_1_50_3/data.bin"),
            "202401_1_50_3/data.bin"
        );
        assert_eq!(extract_relative_path_v5("data.bin"), "data.bin");
    }

    #[test]
    fn test_rewrite_metadata_trailing_slash_prefix() {
        let content = "2\n\
                        1\t500\n\
                        500\tstore/abc/data.bin\n\
                        1\n";
        let meta = parse_metadata(content).unwrap();
        let rewritten = rewrite_metadata(&meta, "store/new/");

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
        let rewritten = rewrite_metadata(&meta, "store/new");
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
}
