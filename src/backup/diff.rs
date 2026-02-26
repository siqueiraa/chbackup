//! Incremental diff logic for --diff-from.
//!
//! Compares current backup parts against a base backup manifest.
//! Parts matching by (table_key, disk_name, part_name, checksum_crc64)
//! are marked as carried forward, referencing the original S3 key.

use std::collections::HashMap;

use tracing::{info, warn};

use crate::manifest::{BackupManifest, PartInfo};

/// Result of an incremental diff comparison.
pub struct DiffResult {
    /// Number of parts carried forward from base.
    pub carried: usize,
    /// Number of parts that will be uploaded (new or changed).
    pub uploaded: usize,
    /// Number of parts with CRC64 mismatch (same name, different data).
    pub crc_mismatches: usize,
}

/// Compare current manifest parts against a base manifest and mark matching
/// parts as carried forward.
///
/// Mutates `current.tables[*].parts[*]` in place:
/// - Matching parts: `source = "carried:{base_name}"`, `backup_key = base_part.backup_key`
/// - Non-matching parts: unchanged (`source = "uploaded"`)
pub fn diff_parts(current: &mut BackupManifest, base: &BackupManifest) -> DiffResult {
    // Build lookup: (table_key, disk_name, part_name) -> &PartInfo
    let mut base_lookup: HashMap<(&str, &str, &str), &PartInfo> = HashMap::new();
    for (table_key, table_manifest) in &base.tables {
        for (disk_name, parts) in &table_manifest.parts {
            for part in parts {
                base_lookup.insert(
                    (table_key.as_str(), disk_name.as_str(), part.name.as_str()),
                    part,
                );
            }
        }
    }

    let base_name = &base.name;
    let mut carried = 0usize;
    let mut uploaded = 0usize;
    let mut crc_mismatches = 0usize;

    for (table_key, table_manifest) in &mut current.tables {
        for (disk_name, parts) in &mut table_manifest.parts {
            for part in parts.iter_mut() {
                if let Some(base_part) =
                    base_lookup.get(&(table_key.as_str(), disk_name.as_str(), part.name.as_str()))
                {
                    if part.checksum_crc64 == base_part.checksum_crc64 {
                        part.source = format!("carried:{}", base_name);
                        part.backup_key = base_part.backup_key.clone();
                        part.s3_objects = base_part.s3_objects.clone();
                        carried += 1;
                    } else {
                        warn!(
                            table = %table_key,
                            part = %part.name,
                            current_crc = part.checksum_crc64,
                            base_crc = base_part.checksum_crc64,
                            "Part has same name but different checksum, will re-upload"
                        );
                        crc_mismatches += 1;
                        uploaded += 1;
                    }
                } else {
                    uploaded += 1;
                }
            }
        }
    }

    info!(
        base = %base_name,
        carried = carried,
        uploaded = uploaded,
        crc_mismatches = crc_mismatches,
        "Incremental diff complete"
    );

    DiffResult {
        carried,
        uploaded,
        crc_mismatches,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{BackupManifest, DatabaseInfo, PartInfo, TableManifest};
    use std::collections::BTreeMap;

    /// Helper: create a PartInfo with given name, crc64, and optionally a backup_key.
    fn make_part(name: &str, crc64: u64, backup_key: &str) -> PartInfo {
        let mut p = PartInfo::new(name, 1024, crc64);
        p.backup_key = backup_key.to_string();
        p
    }

    /// Helper: create a minimal TableManifest with given parts on a specified disk.
    fn make_table(disk: &str, parts: Vec<PartInfo>) -> TableManifest {
        let mut parts_map = BTreeMap::new();
        parts_map.insert(disk.to_string(), parts);
        TableManifest::test_new("MergeTree").with_parts(parts_map)
    }

    /// Helper: create a minimal BackupManifest with given name and tables.
    fn make_manifest(name: &str, tables: BTreeMap<String, TableManifest>) -> BackupManifest {
        BackupManifest::test_new(name)
            .with_tables(tables)
            .with_databases(vec![DatabaseInfo::test_new("default")])
    }

    #[test]
    fn test_diff_parts_no_base() {
        // Base manifest has no tables -- all current parts should remain "uploaded"
        let base = make_manifest("base-backup", BTreeMap::new());

        let mut tables = BTreeMap::new();
        tables.insert(
            "default.trades".to_string(),
            make_table(
                "default",
                vec![
                    make_part("202401_1_50_3", 111, ""),
                    make_part("202402_1_1_0", 222, ""),
                ],
            ),
        );
        let mut current = make_manifest("new-backup", tables);

        let result = diff_parts(&mut current, &base);

        assert_eq!(result.carried, 0);
        assert_eq!(result.uploaded, 2);
        assert_eq!(result.crc_mismatches, 0);

        // Verify all parts remain "uploaded"
        let parts = &current.tables["default.trades"].parts["default"];
        for part in parts {
            assert_eq!(part.source, "uploaded");
        }
    }

    #[test]
    fn test_diff_parts_all_match() {
        // All parts match by name+CRC64 -- all should become carried
        let base_key1 = "s3://bucket/base/data/default/trades/202401_1_50_3.tar.lz4";
        let base_key2 = "s3://bucket/base/data/default/trades/202402_1_1_0.tar.lz4";

        let mut base_tables = BTreeMap::new();
        base_tables.insert(
            "default.trades".to_string(),
            make_table(
                "default",
                vec![
                    make_part("202401_1_50_3", 111, base_key1),
                    make_part("202402_1_1_0", 222, base_key2),
                ],
            ),
        );
        let base = make_manifest("base-backup", base_tables);

        let mut current_tables = BTreeMap::new();
        current_tables.insert(
            "default.trades".to_string(),
            make_table(
                "default",
                vec![
                    make_part("202401_1_50_3", 111, ""),
                    make_part("202402_1_1_0", 222, ""),
                ],
            ),
        );
        let mut current = make_manifest("new-backup", current_tables);

        let result = diff_parts(&mut current, &base);

        assert_eq!(result.carried, 2);
        assert_eq!(result.uploaded, 0);
        assert_eq!(result.crc_mismatches, 0);

        // Verify carried parts have correct source and backup_key
        let parts = &current.tables["default.trades"].parts["default"];
        for part in parts {
            assert_eq!(part.source, "carried:base-backup");
        }
        assert_eq!(parts[0].backup_key, base_key1);
        assert_eq!(parts[1].backup_key, base_key2);
    }

    #[test]
    fn test_diff_parts_partial_match() {
        // One part matches, one is new (not in base)
        let base_key = "s3://bucket/base/data/default/trades/202401_1_50_3.tar.lz4";

        let mut base_tables = BTreeMap::new();
        base_tables.insert(
            "default.trades".to_string(),
            make_table("default", vec![make_part("202401_1_50_3", 111, base_key)]),
        );
        let base = make_manifest("base-backup", base_tables);

        let mut current_tables = BTreeMap::new();
        current_tables.insert(
            "default.trades".to_string(),
            make_table(
                "default",
                vec![
                    make_part("202401_1_50_3", 111, ""),
                    make_part("202403_1_1_0", 333, ""), // new part, not in base
                ],
            ),
        );
        let mut current = make_manifest("new-backup", current_tables);

        let result = diff_parts(&mut current, &base);

        assert_eq!(result.carried, 1);
        assert_eq!(result.uploaded, 1);
        assert_eq!(result.crc_mismatches, 0);

        // Verify the matched part is carried
        let parts = &current.tables["default.trades"].parts["default"];
        let part_50_3 = parts.iter().find(|p| p.name == "202401_1_50_3").unwrap();
        assert_eq!(part_50_3.source, "carried:base-backup");
        assert_eq!(part_50_3.backup_key, base_key);

        // Verify the new part remains uploaded
        let part_1_0 = parts.iter().find(|p| p.name == "202403_1_1_0").unwrap();
        assert_eq!(part_1_0.source, "uploaded");
    }

    #[test]
    fn test_diff_parts_crc64_mismatch() {
        // Same part name but different CRC64 -- part stays "uploaded" (re-uploaded)
        let base_key = "s3://bucket/base/data/default/trades/202401_1_50_3.tar.lz4";

        let mut base_tables = BTreeMap::new();
        base_tables.insert(
            "default.trades".to_string(),
            make_table("default", vec![make_part("202401_1_50_3", 111, base_key)]),
        );
        let base = make_manifest("base-backup", base_tables);

        let mut current_tables = BTreeMap::new();
        current_tables.insert(
            "default.trades".to_string(),
            make_table(
                "default",
                vec![make_part("202401_1_50_3", 999, "")], // same name, different CRC64
            ),
        );
        let mut current = make_manifest("new-backup", current_tables);

        let result = diff_parts(&mut current, &base);

        assert_eq!(result.carried, 0);
        assert_eq!(result.uploaded, 1);
        assert_eq!(result.crc_mismatches, 1);

        // Part should remain "uploaded" since CRC64 doesn't match
        let parts = &current.tables["default.trades"].parts["default"];
        assert_eq!(parts[0].source, "uploaded");
        assert!(parts[0].backup_key.is_empty()); // backup_key not copied
    }

    #[test]
    fn test_diff_parts_multi_disk() {
        // Parts on different disks within same table should be compared correctly
        // (disk name must match too)
        let base_key_default = "s3://bucket/base/data/default/trades/part_default.tar.lz4";
        let base_key_ssd = "s3://bucket/base/data/ssd/trades/part_ssd.tar.lz4";

        let mut base_parts_map = BTreeMap::new();
        base_parts_map.insert(
            "default".to_string(),
            vec![make_part("part_default", 100, base_key_default)],
        );
        base_parts_map.insert(
            "ssd".to_string(),
            vec![make_part("part_ssd", 200, base_key_ssd)],
        );
        let base_table = TableManifest::test_new("MergeTree").with_parts(base_parts_map);
        let mut base_tables = BTreeMap::new();
        base_tables.insert("default.trades".to_string(), base_table);
        let base = make_manifest("base-backup", base_tables);

        // Current has same parts on same disks with same CRC64
        let mut current_parts_map = BTreeMap::new();
        current_parts_map.insert(
            "default".to_string(),
            vec![make_part("part_default", 100, "")],
        );
        current_parts_map.insert("ssd".to_string(), vec![make_part("part_ssd", 200, "")]);
        let current_table = TableManifest::test_new("MergeTree").with_parts(current_parts_map);
        let mut current_tables = BTreeMap::new();
        current_tables.insert("default.trades".to_string(), current_table);
        let mut current = make_manifest("new-backup", current_tables);

        let result = diff_parts(&mut current, &base);

        assert_eq!(result.carried, 2);
        assert_eq!(result.uploaded, 0);
        assert_eq!(result.crc_mismatches, 0);

        // Verify each disk's part was correctly matched
        let default_parts = &current.tables["default.trades"].parts["default"];
        assert_eq!(default_parts[0].source, "carried:base-backup");
        assert_eq!(default_parts[0].backup_key, base_key_default);

        let ssd_parts = &current.tables["default.trades"].parts["ssd"];
        assert_eq!(ssd_parts[0].source, "carried:base-backup");
        assert_eq!(ssd_parts[0].backup_key, base_key_ssd);
    }

    #[test]
    fn test_diff_parts_extra_table_in_base() {
        // Base has a table not in current -- gracefully ignored
        let mut base_tables = BTreeMap::new();
        base_tables.insert(
            "default.trades".to_string(),
            make_table(
                "default",
                vec![make_part("part1", 111, "s3://bucket/base/part1.tar.lz4")],
            ),
        );
        base_tables.insert(
            "default.orders".to_string(), // extra table in base, not in current
            make_table(
                "default",
                vec![make_part(
                    "part_orders",
                    999,
                    "s3://bucket/base/orders.tar.lz4",
                )],
            ),
        );
        let base = make_manifest("base-backup", base_tables);

        let mut current_tables = BTreeMap::new();
        current_tables.insert(
            "default.trades".to_string(),
            make_table("default", vec![make_part("part1", 111, "")]),
        );
        let mut current = make_manifest("new-backup", current_tables);

        let result = diff_parts(&mut current, &base);

        assert_eq!(result.carried, 1);
        assert_eq!(result.uploaded, 0);
        assert_eq!(result.crc_mismatches, 0);

        // The extra table in base should not affect current
        assert!(!current.tables.contains_key("default.orders"));

        // The matching part should be carried
        let parts = &current.tables["default.trades"].parts["default"];
        assert_eq!(parts[0].source, "carried:base-backup");
    }

    #[test]
    fn test_diff_parts_carries_s3_objects() {
        // Base manifest has a part on "s3disk" with s3_objects populated.
        // Current has same part with s3_objects: None (just created by collect_parts).
        // After diff_parts(), current part should have s3_objects copied from base.
        use crate::manifest::S3ObjectInfo;

        let s3_objects = vec![S3ObjectInfo {
            path: "store/abc/data.bin".to_string(),
            size: 100,
            backup_key: "chbackup/base/objects/data.bin".to_string(),
        }];

        let mut base_part = make_part("202401_1_50_3", 111, "s3://bucket/base/part.tar.lz4");
        base_part.s3_objects = Some(s3_objects.clone());

        let mut base_tables = BTreeMap::new();
        base_tables.insert(
            "default.trades".to_string(),
            make_table("s3disk", vec![base_part]),
        );
        let base = make_manifest("base-backup", base_tables);

        // Current has same part but with s3_objects: None
        let current_part = make_part("202401_1_50_3", 111, "");
        assert!(current_part.s3_objects.is_none()); // verify None before diff

        let mut current_tables = BTreeMap::new();
        current_tables.insert(
            "default.trades".to_string(),
            make_table("s3disk", vec![current_part]),
        );
        let mut current = make_manifest("new-backup", current_tables);

        let result = diff_parts(&mut current, &base);

        assert_eq!(result.carried, 1);
        assert_eq!(result.uploaded, 0);
        assert_eq!(result.crc_mismatches, 0);

        // Verify s3_objects was carried forward
        let parts = &current.tables["default.trades"].parts["s3disk"];
        assert_eq!(parts[0].source, "carried:base-backup");
        assert_eq!(parts[0].backup_key, "s3://bucket/base/part.tar.lz4");
        assert!(parts[0].s3_objects.is_some());
        let carried_objects = parts[0].s3_objects.as_ref().unwrap();
        assert_eq!(carried_objects.len(), 1);
        assert_eq!(carried_objects[0].path, "store/abc/data.bin");
        assert_eq!(carried_objects[0].size, 100);
        assert_eq!(
            carried_objects[0].backup_key,
            "chbackup/base/objects/data.bin"
        );
    }

    #[test]
    fn test_diff_parts_local_parts_s3_objects_none_unchanged() {
        // Local disk parts have s3_objects: None in both base and current.
        // After diff_parts, s3_objects should remain None (cloning None is a no-op).
        let base_key = "s3://bucket/base/data/default/trades/part1.tar.lz4";

        let mut base_tables = BTreeMap::new();
        base_tables.insert(
            "default.trades".to_string(),
            make_table("default", vec![make_part("part1", 111, base_key)]),
        );
        let base = make_manifest("base-backup", base_tables);

        let mut current_tables = BTreeMap::new();
        current_tables.insert(
            "default.trades".to_string(),
            make_table("default", vec![make_part("part1", 111, "")]),
        );
        let mut current = make_manifest("new-backup", current_tables);

        let result = diff_parts(&mut current, &base);

        assert_eq!(result.carried, 1);

        let parts = &current.tables["default.trades"].parts["default"];
        assert!(parts[0].s3_objects.is_none()); // Still None for local parts
    }
}
