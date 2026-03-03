//! Manifest types for backup metadata.
//!
//! The `BackupManifest` is the central data structure that flows between all
//! backup/upload/download/restore/list commands. It is serialized to JSON as
//! `metadata.json` in each backup directory.
//!
//! Format matches design doc section 7.1.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Top-level backup manifest. Self-contained: every backup is independently
/// restorable without needing to follow an incremental chain.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackupManifest {
    /// Schema version for forward compatibility.
    #[serde(default = "default_manifest_version")]
    pub manifest_version: u32,

    /// Backup name (e.g. "daily-2024-01-15").
    pub name: String,

    /// Creation timestamp.
    pub timestamp: DateTime<Utc>,

    /// ClickHouse server version at backup time.
    #[serde(default)]
    pub clickhouse_version: String,

    /// chbackup binary version.
    #[serde(default)]
    pub chbackup_version: String,

    /// Compression format: "lz4", "zstd", "gzip", "none".
    #[serde(default = "default_data_format")]
    pub data_format: String,

    /// Total compressed size of all parts in bytes.
    #[serde(default)]
    pub compressed_size: u64,

    /// Total metadata size in bytes.
    #[serde(default)]
    pub metadata_size: u64,

    /// Disk name -> disk path mapping from ClickHouse.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub disks: BTreeMap<String, String>,

    /// Disk name -> disk type mapping (e.g. "local", "s3").
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub disk_types: BTreeMap<String, String>,

    /// Disk name -> remote_path mapping for S3 disks (e.g. "s3disk" -> "s3://bucket/prefix/").
    /// Empty for local disks. Used by upload to determine CopyObject source.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub disk_remote_paths: BTreeMap<String, String>,

    /// Tables included in this backup. Key is "db.table".
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tables: BTreeMap<String, TableManifest>,

    /// Databases included in this backup.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub databases: Vec<DatabaseInfo>,

    /// User-defined functions backed up.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub functions: Vec<String>,

    /// Named collections backed up.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub named_collections: Vec<String>,

    /// RBAC metadata (path to access/ directory in S3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rbac: Option<RbacInfo>,

    /// Total size of RBAC (access/) files in bytes.
    #[serde(default)]
    pub rbac_size: u64,

    /// Total size of ClickHouse config backup files in bytes.
    #[serde(default)]
    pub config_size: u64,
}

/// Per-table metadata within a backup.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableManifest {
    /// CREATE TABLE DDL statement.
    pub ddl: String,

    /// Table UUID (from system.tables).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,

    /// Engine name (e.g. "ReplicatedMergeTree", "Dictionary").
    #[serde(default)]
    pub engine: String,

    /// Total uncompressed data size in bytes.
    #[serde(default)]
    pub total_bytes: u64,

    /// Parts grouped by disk name. Key is disk name (e.g. "default", "s3disk").
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub parts: BTreeMap<String, Vec<PartInfo>>,

    /// Pending mutations at backup time.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_mutations: Vec<MutationInfo>,

    /// True if this table has DDL only (no data parts). E.g. dictionaries, views.
    #[serde(default)]
    pub metadata_only: bool,

    /// Tables this object depends on (e.g. a dictionary depends on its source table).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<String>,
}

/// Information about a single data part.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PartInfo {
    /// Part directory name (e.g. "202401_1_50_3").
    pub name: String,

    /// Uncompressed size of the part in bytes.
    #[serde(default)]
    pub size: u64,

    /// S3 key for the compressed archive (e.g. "prefix/backup/data/db/table/part.tar.lz4").
    #[serde(default)]
    pub backup_key: String,

    /// Source: "uploaded" for parts directly uploaded, or "carried:base_backup_name"
    /// for parts referencing another backup's data.
    #[serde(default = "default_source")]
    pub source: String,

    /// CRC64/XZ checksum of the part's checksums.txt file.
    #[serde(default)]
    pub checksum_crc64: u64,

    /// S3 object disk references (for parts on S3 object disks).
    /// None for local disk parts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s3_objects: Option<Vec<S3ObjectInfo>>,

    /// Compressed size of this part on S3 in bytes. Set during upload for newly
    /// uploaded parts; carried forward from the base manifest for incremental parts.
    /// Zero for old manifests that predate this field (backward compatible).
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub backup_size: u64,
}

impl PartInfo {
    /// Create a new PartInfo with default values for a freshly collected part.
    pub fn new(name: impl Into<String>, size: u64, crc64: u64) -> Self {
        Self {
            name: name.into(),
            size,
            backup_key: String::new(),
            source: "uploaded".to_string(),
            checksum_crc64: crc64,
            s3_objects: None,
            backup_size: 0,
        }
    }

    /// Set S3 objects for an S3 disk part.
    pub fn with_s3_objects(mut self, objects: Vec<S3ObjectInfo>) -> Self {
        self.s3_objects = Some(objects);
        self
    }
}

/// Reference to an S3 object within a part (for S3 object disk parts).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct S3ObjectInfo {
    /// Object path relative to disk root.
    pub path: String,

    /// Object size in bytes.
    pub size: u64,

    /// S3 key where this object is stored in the backup.
    #[serde(default)]
    pub backup_key: String,
}

impl TableManifest {
    /// Create a minimal TableManifest for testing. The DDL is auto-generated from
    /// the engine name, and all other fields use sensible defaults (empty parts,
    /// no mutations, no dependencies, metadata_only = false).
    #[cfg(test)]
    pub fn test_new(engine: &str) -> Self {
        Self {
            ddl: format!(
                "CREATE TABLE test (id UInt64) ENGINE = {} ORDER BY id",
                engine
            ),
            uuid: None,
            engine: engine.to_string(),
            total_bytes: 0,
            parts: std::collections::BTreeMap::new(),
            pending_mutations: Vec::new(),
            metadata_only: false,
            dependencies: Vec::new(),
        }
    }

    #[cfg(test)]
    pub fn with_parts(mut self, parts: std::collections::BTreeMap<String, Vec<PartInfo>>) -> Self {
        self.parts = parts;
        self
    }

    #[cfg(test)]
    pub fn with_ddl(mut self, ddl: impl Into<String>) -> Self {
        self.ddl = ddl.into();
        self
    }

    #[cfg(test)]
    pub fn with_metadata_only(mut self, metadata_only: bool) -> Self {
        self.metadata_only = metadata_only;
        self
    }

    #[cfg(test)]
    pub fn with_dependencies(mut self, deps: Vec<String>) -> Self {
        self.dependencies = deps;
        self
    }

    #[cfg(test)]
    pub fn with_total_bytes(mut self, bytes: u64) -> Self {
        self.total_bytes = bytes;
        self
    }

    #[cfg(test)]
    pub fn with_uuid(mut self, uuid: impl Into<String>) -> Self {
        self.uuid = Some(uuid.into());
        self
    }
}

/// Database metadata in the manifest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DatabaseInfo {
    /// Database name.
    pub name: String,

    /// CREATE DATABASE DDL statement.
    pub ddl: String,
}

impl DatabaseInfo {
    /// Create a minimal DatabaseInfo for testing. The DDL is auto-generated as
    /// `CREATE DATABASE {name} ENGINE = Atomic`.
    #[cfg(test)]
    pub fn test_new(name: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            ddl: format!("CREATE DATABASE {} ENGINE = Atomic", name),
            name,
        }
    }
}

/// Mutation metadata in the manifest (pending mutations at backup time).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MutationInfo {
    /// Mutation ID from system.mutations.
    pub mutation_id: String,

    /// Mutation command (e.g. "UPDATE x = 1 WHERE id = 5").
    pub command: String,

    /// Parts that still need this mutation applied.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts_to_do: Vec<String>,
}

/// RBAC metadata pointer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RbacInfo {
    /// S3 path prefix for RBAC files.
    pub path: String,
}

// -- Default value helpers --

fn default_manifest_version() -> u32 {
    1
}

fn default_data_format() -> String {
    "lz4".to_string()
}

fn default_source() -> String {
    "uploaded".to_string()
}

fn is_zero_u64(v: &u64) -> bool {
    *v == 0
}

// -- File I/O helpers --

impl BackupManifest {
    /// Save the manifest as JSON to the given file path.
    pub fn save_to_file(&self, path: &Path) -> Result<()> {
        let json =
            serde_json::to_string_pretty(self).context("Failed to serialize manifest to JSON")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
        }
        std::fs::write(path, &json)
            .with_context(|| format!("Failed to write manifest to: {}", path.display()))?;
        Ok(())
    }

    /// Load a manifest from a JSON file.
    pub fn load_from_file(path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read manifest from: {}", path.display()))?;
        let manifest: BackupManifest = serde_json::from_str(&contents)
            .with_context(|| format!("Failed to parse manifest from: {}", path.display()))?;
        Ok(manifest)
    }

    /// Deserialize a manifest from a JSON byte slice.
    pub fn from_json_bytes(data: &[u8]) -> Result<Self> {
        let manifest: BackupManifest =
            serde_json::from_slice(data).context("Failed to parse manifest from JSON bytes")?;
        Ok(manifest)
    }

    /// Serialize the manifest to JSON bytes.
    pub fn to_json_bytes(&self) -> Result<Vec<u8>> {
        let json = serde_json::to_vec_pretty(self)
            .context("Failed to serialize manifest to JSON bytes")?;
        Ok(json)
    }
}

// -- Test builder helpers --

#[cfg(test)]
impl BackupManifest {
    /// Create a minimal manifest for testing with sensible defaults.
    pub fn test_new(name: impl Into<String>) -> Self {
        Self {
            manifest_version: 1,
            name: name.into(),
            timestamp: chrono::Utc::now(),
            clickhouse_version: "24.1.3.31".to_string(),
            chbackup_version: "0.1.0".to_string(),
            data_format: "lz4".to_string(),
            compressed_size: 0,
            metadata_size: 0,
            disks: BTreeMap::new(),
            disk_types: BTreeMap::new(),
            disk_remote_paths: BTreeMap::new(),
            tables: BTreeMap::new(),
            databases: Vec::new(),
            functions: Vec::new(),
            named_collections: Vec::new(),
            rbac: None,
            rbac_size: 0,
            config_size: 0,
        }
    }

    pub fn with_tables(mut self, tables: BTreeMap<String, TableManifest>) -> Self {
        self.tables = tables;
        self
    }

    pub fn with_databases(mut self, databases: Vec<DatabaseInfo>) -> Self {
        self.databases = databases;
        self
    }

    pub fn with_disks(mut self, disks: BTreeMap<String, String>) -> Self {
        self.disks = disks;
        self
    }

    pub fn with_disk_types(mut self, disk_types: BTreeMap<String, String>) -> Self {
        self.disk_types = disk_types;
        self
    }

    pub fn with_disk_remote_paths(mut self, disk_remote_paths: BTreeMap<String, String>) -> Self {
        self.disk_remote_paths = disk_remote_paths;
        self
    }

    pub fn with_compressed_size(mut self, size: u64) -> Self {
        self.compressed_size = size;
        self
    }

    pub fn with_metadata_size(mut self, size: u64) -> Self {
        self.metadata_size = size;
        self
    }

    pub fn with_timestamp(mut self, timestamp: DateTime<Utc>) -> Self {
        self.timestamp = timestamp;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_manifest() -> BackupManifest {
        let mut parts = BTreeMap::new();
        parts.insert(
            "default".to_string(),
            vec![
                {
                    let mut p = PartInfo::new("202401_1_50_3", 134_217_728, 12345678901234);
                    p.backup_key =
                        "chbackup/daily/data/default/trades/202401_1_50_3.tar.lz4".to_string();
                    p
                },
                {
                    let mut p = PartInfo::new("202402_1_1_0", 4096, 11111111111111);
                    p.backup_key =
                        "chbackup/daily/data/default/trades/202402_1_1_0.tar.lz4".to_string();
                    p
                },
            ],
        );

        let mut tables = BTreeMap::new();
        tables.insert(
            "default.trades".to_string(),
            TableManifest::test_new("MergeTree")
                .with_ddl("CREATE TABLE default.trades (id UInt64) ENGINE = MergeTree ORDER BY id")
                .with_uuid("5f3a7b2c-1234-5678-9abc-def012345678")
                .with_total_bytes(134_221_824)
                .with_parts(parts),
        );

        BackupManifest::test_new("daily-2024-01-15")
            .with_compressed_size(1_073_741_824)
            .with_metadata_size(524_288)
            .with_disks(BTreeMap::from([(
                "default".to_string(),
                "/var/lib/clickhouse".to_string(),
            )]))
            .with_disk_types(BTreeMap::from([(
                "default".to_string(),
                "local".to_string(),
            )]))
            .with_tables(tables)
            .with_databases(vec![DatabaseInfo::test_new("default")])
    }

    #[test]
    fn test_manifest_serialize_roundtrip() {
        let manifest = sample_manifest();
        let json = serde_json::to_string_pretty(&manifest).unwrap();
        let deserialized: BackupManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(manifest, deserialized);
    }

    #[test]
    fn test_manifest_default_values() {
        let json = r#"{
            "name": "test",
            "timestamp": "2024-01-15T02:00:00Z"
        }"#;
        let manifest: BackupManifest = serde_json::from_str(json).unwrap();
        assert_eq!(manifest.manifest_version, 1);
        assert_eq!(manifest.data_format, "lz4");
        assert_eq!(manifest.compressed_size, 0);
        assert!(manifest.tables.is_empty());
        assert!(manifest.databases.is_empty());
        assert!(manifest.functions.is_empty());
        assert!(manifest.named_collections.is_empty());
        assert!(manifest.rbac.is_none());
    }

    #[test]
    fn test_manifest_file_roundtrip() {
        let manifest = sample_manifest();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.json");

        manifest.save_to_file(&path).unwrap();
        let loaded = BackupManifest::load_from_file(&path).unwrap();
        assert_eq!(manifest, loaded);
    }

    #[test]
    fn test_manifest_json_bytes_roundtrip() {
        let manifest = sample_manifest();
        let bytes = manifest.to_json_bytes().unwrap();
        let loaded = BackupManifest::from_json_bytes(&bytes).unwrap();
        assert_eq!(manifest, loaded);
    }

    #[test]
    fn test_manifest_matches_design_doc_example() {
        // Verify the struct can deserialize a JSON example that matches design doc 7.1 format
        let json = r#"{
            "manifest_version": 1,
            "name": "daily-2024-01-15",
            "timestamp": "2024-01-15T02:00:00Z",
            "clickhouse_version": "24.1.3.31",
            "chbackup_version": "0.1.0",
            "data_format": "lz4",
            "compressed_size": 1073741824,
            "metadata_size": 524288,
            "disks": { "default": "/var/lib/clickhouse", "s3disk": "/var/lib/clickhouse/disks/s3" },
            "disk_types": { "s3disk": "s3", "default": "local" },
            "tables": {
                "default.trades": {
                    "ddl": "CREATE TABLE default.trades (...) ENGINE = ReplicatedMergeTree(...)",
                    "uuid": "5f3a7b2c-1234",
                    "engine": "ReplicatedMergeTree",
                    "total_bytes": 5368709120,
                    "parts": {
                        "s3disk": [
                            {
                                "name": "202401_1_50_3",
                                "size": 134217728,
                                "backup_key": "chbackup/daily-2024-01-15/default/trades/s3disk/202401_1_50_3.tar.lz4",
                                "source": "uploaded",
                                "checksum_crc64": 12345678901234,
                                "s3_objects": [
                                    {
                                        "path": "store/abc/def/202401_1_50_3/data.bin",
                                        "size": 134217000,
                                        "backup_key": "chbackup/daily-2024-01-15/objects/store/abc/def/202401_1_50_3/data.bin"
                                    }
                                ]
                            }
                        ],
                        "default": [
                            {
                                "name": "202402_1_1_0",
                                "size": 4096,
                                "backup_key": "chbackup/daily-2024-01-15/default/trades/default/202402_1_1_0.tar.lz4",
                                "source": "uploaded",
                                "checksum_crc64": 11111111111111
                            }
                        ]
                    },
                    "pending_mutations": [],
                    "metadata_only": false,
                    "dependencies": []
                },
                "default.user_dict": {
                    "ddl": "CREATE DICTIONARY default.user_dict (...)",
                    "engine": "Dictionary",
                    "metadata_only": true,
                    "dependencies": ["default.users"]
                }
            },
            "databases": [
                { "name": "default", "ddl": "CREATE DATABASE default ENGINE = Atomic" }
            ],
            "functions": [],
            "named_collections": [],
            "rbac": { "path": "chbackup/daily-2024-01-15/access/" }
        }"#;

        let manifest: BackupManifest = serde_json::from_str(json).unwrap();

        assert_eq!(manifest.manifest_version, 1);
        assert_eq!(manifest.name, "daily-2024-01-15");
        assert_eq!(manifest.clickhouse_version, "24.1.3.31");
        assert_eq!(manifest.data_format, "lz4");
        assert_eq!(manifest.compressed_size, 1_073_741_824);
        assert_eq!(manifest.tables.len(), 2);

        let trades = manifest.tables.get("default.trades").unwrap();
        assert_eq!(trades.engine, "ReplicatedMergeTree");
        assert_eq!(trades.total_bytes, 5_368_709_120);
        assert!(!trades.metadata_only);

        let s3disk_parts = trades.parts.get("s3disk").unwrap();
        assert_eq!(s3disk_parts.len(), 1);
        assert_eq!(s3disk_parts[0].name, "202401_1_50_3");
        assert_eq!(s3disk_parts[0].checksum_crc64, 12345678901234);
        assert!(s3disk_parts[0].s3_objects.is_some());
        assert_eq!(s3disk_parts[0].s3_objects.as_ref().unwrap().len(), 1);

        let default_parts = trades.parts.get("default").unwrap();
        assert_eq!(default_parts.len(), 1);
        assert_eq!(default_parts[0].name, "202402_1_1_0");
        assert!(default_parts[0].s3_objects.is_none());

        let dict = manifest.tables.get("default.user_dict").unwrap();
        assert!(dict.metadata_only);
        assert_eq!(dict.dependencies, vec!["default.users"]);

        assert_eq!(manifest.databases.len(), 1);
        assert_eq!(manifest.databases[0].name, "default");
        assert!(manifest.rbac.is_some());
    }

    #[test]
    fn test_part_info_default_source() {
        let json = r#"{
            "name": "202401_1_1_0",
            "size": 100
        }"#;
        let part: PartInfo = serde_json::from_str(json).unwrap();
        assert_eq!(part.source, "uploaded");
        assert_eq!(part.checksum_crc64, 0);
        assert!(part.s3_objects.is_none());
    }

    #[test]
    fn test_manifest_rbac_config_size_fields() {
        let mut manifest = sample_manifest();
        manifest.rbac_size = 1024;
        manifest.config_size = 2048;

        let json = serde_json::to_string_pretty(&manifest).unwrap();
        let deserialized: BackupManifest = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.rbac_size, 1024);
        assert_eq!(deserialized.config_size, 2048);
    }

    #[test]
    fn test_manifest_backward_compat_no_rbac_config_size() {
        // Deserialize a JSON string WITHOUT rbac_size/config_size
        // and verify both default to 0.
        let json = r#"{
            "name": "old-backup",
            "timestamp": "2024-01-15T02:00:00Z"
        }"#;
        let manifest: BackupManifest = serde_json::from_str(json).unwrap();
        assert_eq!(manifest.rbac_size, 0);
        assert_eq!(manifest.config_size, 0);
    }

    #[test]
    fn test_table_manifest_empty_parts_not_serialized() {
        let table = TableManifest::test_new("MergeTree").with_metadata_only(true);
        let json = serde_json::to_string(&table).unwrap();
        // Empty parts, mutations, and dependencies should not appear in output
        assert!(!json.contains("\"parts\""));
        assert!(!json.contains("\"pending_mutations\""));
        assert!(!json.contains("\"dependencies\""));
    }

    // -----------------------------------------------------------------------
    // PartInfo builder and field tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_part_info_new_defaults() {
        let part = PartInfo::new("202401_1_50_3", 134_217_728, 12345);
        assert_eq!(part.name, "202401_1_50_3");
        assert_eq!(part.size, 134_217_728);
        assert_eq!(part.checksum_crc64, 12345);
        assert_eq!(part.source, "uploaded");
        assert!(part.backup_key.is_empty());
        assert!(part.s3_objects.is_none());
        assert_eq!(part.backup_size, 0);
    }

    #[test]
    fn test_part_info_with_s3_objects() {
        let objects = vec![
            S3ObjectInfo {
                path: "store/abc/data.bin".to_string(),
                size: 1000,
                backup_key: "backup/objects/data.bin".to_string(),
            },
            S3ObjectInfo {
                path: "store/abc/index.bin".to_string(),
                size: 200,
                backup_key: "backup/objects/index.bin".to_string(),
            },
        ];

        let part = PartInfo::new("all_0_0_0", 1200, 99999).with_s3_objects(objects);
        assert!(part.s3_objects.is_some());
        let objs = part.s3_objects.unwrap();
        assert_eq!(objs.len(), 2);
        assert_eq!(objs[0].path, "store/abc/data.bin");
        assert_eq!(objs[1].size, 200);
    }

    #[test]
    fn test_part_info_backup_size_skip_serializing_when_zero() {
        let part = PartInfo::new("all_0_0_0", 100, 0);
        let json = serde_json::to_string(&part).unwrap();
        // backup_size = 0 should be skipped via skip_serializing_if
        assert!(
            !json.contains("\"backup_size\""),
            "backup_size=0 should not be serialized, got: {}",
            json
        );

        // But non-zero should appear
        let mut part2 = PartInfo::new("all_1_1_0", 100, 0);
        part2.backup_size = 500;
        let json2 = serde_json::to_string(&part2).unwrap();
        assert!(
            json2.contains("\"backup_size\":500"),
            "Non-zero backup_size should be serialized, got: {}",
            json2
        );
    }

    // -----------------------------------------------------------------------
    // MutationInfo serialization
    // -----------------------------------------------------------------------

    #[test]
    fn test_mutation_info_serialize_roundtrip() {
        let mutation = MutationInfo {
            mutation_id: "0000000042".to_string(),
            command: "UPDATE x = 1 WHERE id = 5".to_string(),
            parts_to_do: vec!["part_1".to_string(), "part_2".to_string()],
        };

        let json = serde_json::to_string_pretty(&mutation).unwrap();
        let deser: MutationInfo = serde_json::from_str(&json).unwrap();

        assert_eq!(deser.mutation_id, "0000000042");
        assert_eq!(deser.command, "UPDATE x = 1 WHERE id = 5");
        assert_eq!(deser.parts_to_do.len(), 2);
    }

    #[test]
    fn test_mutation_info_empty_parts_to_do_not_serialized() {
        let mutation = MutationInfo {
            mutation_id: "0000000001".to_string(),
            command: "DELETE WHERE id = 1".to_string(),
            parts_to_do: Vec::new(),
        };

        let json = serde_json::to_string(&mutation).unwrap();
        assert!(
            !json.contains("\"parts_to_do\""),
            "Empty parts_to_do should not be serialized"
        );
    }

    // -----------------------------------------------------------------------
    // Manifest with all optional fields populated
    // -----------------------------------------------------------------------

    #[test]
    fn test_manifest_all_optional_fields() {
        let mut tables = BTreeMap::new();
        let mut parts = BTreeMap::new();
        parts.insert(
            "default".to_string(),
            vec![{
                let mut p = PartInfo::new("all_0_0_0", 500, 11111);
                p.backup_key = "backup/data/part.tar.lz4".to_string();
                p.backup_size = 300;
                p
            }],
        );
        tables.insert(
            "mydb.mytable".to_string(),
            TableManifest::test_new("ReplicatedMergeTree")
                .with_ddl("CREATE TABLE mydb.mytable (id UInt64) ENGINE = ReplicatedMergeTree ORDER BY id")
                .with_uuid("12345678-abcd-efgh-ijkl-mnopqrstuvwx")
                .with_total_bytes(500)
                .with_parts(parts)
                .with_dependencies(vec!["mydb.other".to_string()]),
        );

        let manifest = BackupManifest {
            manifest_version: 2,
            name: "full-backup".to_string(),
            timestamp: chrono::Utc::now(),
            clickhouse_version: "24.8.1.123".to_string(),
            chbackup_version: "0.2.0".to_string(),
            data_format: "zstd".to_string(),
            compressed_size: 300,
            metadata_size: 1024,
            disks: BTreeMap::from([
                ("default".to_string(), "/var/lib/clickhouse".to_string()),
                ("s3disk".to_string(), "/mnt/s3".to_string()),
            ]),
            disk_types: BTreeMap::from([
                ("default".to_string(), "local".to_string()),
                ("s3disk".to_string(), "s3".to_string()),
            ]),
            disk_remote_paths: BTreeMap::from([(
                "s3disk".to_string(),
                "s3://mybucket/data/".to_string(),
            )]),
            tables,
            databases: vec![DatabaseInfo::test_new("mydb")],
            functions: vec!["my_udf".to_string()],
            named_collections: vec!["my_collection".to_string()],
            rbac: Some(RbacInfo {
                path: "backup/access/".to_string(),
            }),
            rbac_size: 2048,
            config_size: 4096,
        };

        // Round-trip
        let json = serde_json::to_string_pretty(&manifest).unwrap();
        let deser: BackupManifest = serde_json::from_str(&json).unwrap();

        assert_eq!(deser.manifest_version, 2);
        assert_eq!(deser.data_format, "zstd");
        assert_eq!(deser.functions, vec!["my_udf"]);
        assert_eq!(deser.named_collections, vec!["my_collection"]);
        assert!(deser.rbac.is_some());
        assert_eq!(deser.rbac.unwrap().path, "backup/access/");
        assert_eq!(deser.rbac_size, 2048);
        assert_eq!(deser.config_size, 4096);
        assert_eq!(deser.disks.len(), 2);
        assert_eq!(deser.disk_types.len(), 2);
        assert_eq!(deser.disk_remote_paths.len(), 1);
        assert_eq!(deser.databases.len(), 1);
        assert_eq!(deser.tables.len(), 1);

        let table = deser.tables.get("mydb.mytable").unwrap();
        assert_eq!(table.dependencies, vec!["mydb.other"]);
        assert!(table.uuid.is_some());
        assert_eq!(
            table.uuid.as_ref().unwrap(),
            "12345678-abcd-efgh-ijkl-mnopqrstuvwx"
        );

        let part = &table.parts["default"][0];
        assert_eq!(part.backup_size, 300);
        assert_eq!(part.checksum_crc64, 11111);
    }

    // -----------------------------------------------------------------------
    // save_to_file creates nested directories
    // -----------------------------------------------------------------------

    #[test]
    fn test_save_to_file_creates_nested_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let deep_path = dir
            .path()
            .join("a")
            .join("b")
            .join("c")
            .join("metadata.json");

        assert!(!deep_path.exists());

        let manifest = BackupManifest::test_new("nested-test");
        manifest.save_to_file(&deep_path).unwrap();

        assert!(deep_path.exists());
        let loaded = BackupManifest::load_from_file(&deep_path).unwrap();
        assert_eq!(loaded.name, "nested-test");
    }

    // -----------------------------------------------------------------------
    // from_json_bytes error cases
    // -----------------------------------------------------------------------

    #[test]
    fn test_from_json_bytes_invalid_json() {
        let result = BackupManifest::from_json_bytes(b"not valid json");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("Failed to parse manifest"),
            "Expected parse error, got: {}",
            err
        );
    }

    #[test]
    fn test_from_json_bytes_empty() {
        let result = BackupManifest::from_json_bytes(b"");
        assert!(result.is_err());
    }

    #[test]
    fn test_from_json_bytes_missing_required_field() {
        // JSON object but missing required 'name' field
        let result = BackupManifest::from_json_bytes(b"{}");
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // load_from_file error case
    // -----------------------------------------------------------------------

    #[test]
    fn test_load_from_file_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.json");
        let result = BackupManifest::load_from_file(&path);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Failed to read manifest"));
    }

    #[test]
    fn test_load_from_file_invalid_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.json");
        std::fs::write(&path, "not json").unwrap();
        let result = BackupManifest::load_from_file(&path);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Failed to parse manifest"));
    }

    // -----------------------------------------------------------------------
    // to_json_bytes produces valid JSON
    // -----------------------------------------------------------------------

    #[test]
    fn test_to_json_bytes_valid_json() {
        let manifest = BackupManifest::test_new("bytes-test")
            .with_compressed_size(1024)
            .with_metadata_size(256);

        let bytes = manifest.to_json_bytes().unwrap();
        assert!(!bytes.is_empty());

        // Should be valid JSON
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed["name"], "bytes-test");
        assert_eq!(parsed["compressed_size"], 1024);
    }

    // -----------------------------------------------------------------------
    // S3ObjectInfo field tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_s3_object_info_serialization() {
        let obj = S3ObjectInfo {
            path: "store/abc/def/data.bin".to_string(),
            size: 134_217_000,
            backup_key: "backup/objects/data.bin".to_string(),
        };

        let json = serde_json::to_string(&obj).unwrap();
        let deser: S3ObjectInfo = serde_json::from_str(&json).unwrap();

        assert_eq!(deser.path, "store/abc/def/data.bin");
        assert_eq!(deser.size, 134_217_000);
        assert_eq!(deser.backup_key, "backup/objects/data.bin");
    }

    // -----------------------------------------------------------------------
    // DatabaseInfo tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_database_info_serialization() {
        let db = DatabaseInfo {
            name: "production".to_string(),
            ddl: "CREATE DATABASE production ENGINE = Atomic".to_string(),
        };

        let json = serde_json::to_string(&db).unwrap();
        let deser: DatabaseInfo = serde_json::from_str(&json).unwrap();

        assert_eq!(deser.name, "production");
        assert_eq!(deser.ddl, "CREATE DATABASE production ENGINE = Atomic");
    }

    // -----------------------------------------------------------------------
    // RbacInfo tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_rbac_info_serialization() {
        let rbac = RbacInfo {
            path: "chbackup/daily/access/".to_string(),
        };

        let json = serde_json::to_string(&rbac).unwrap();
        let deser: RbacInfo = serde_json::from_str(&json).unwrap();

        assert_eq!(deser.path, "chbackup/daily/access/");
    }

    // -----------------------------------------------------------------------
    // TableManifest builder method tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_table_manifest_builder_methods() {
        let table = TableManifest::test_new("MergeTree")
            .with_ddl("CREATE TABLE t (x Int32) ENGINE = MergeTree ORDER BY x")
            .with_uuid("aaaa-bbbb-cccc")
            .with_total_bytes(123456)
            .with_metadata_only(true)
            .with_dependencies(vec!["dep1".to_string(), "dep2".to_string()]);

        assert_eq!(
            table.ddl,
            "CREATE TABLE t (x Int32) ENGINE = MergeTree ORDER BY x"
        );
        assert_eq!(table.uuid.as_deref(), Some("aaaa-bbbb-cccc"));
        assert_eq!(table.total_bytes, 123456);
        assert!(table.metadata_only);
        assert_eq!(table.dependencies, vec!["dep1", "dep2"]);
        assert_eq!(table.engine, "MergeTree");
    }

    // -----------------------------------------------------------------------
    // Manifest builder method tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_manifest_builder_methods() {
        use chrono::TimeZone;

        let ts = chrono::Utc.with_ymd_and_hms(2025, 6, 1, 12, 30, 0).unwrap();
        let manifest = BackupManifest::test_new("builder-test")
            .with_timestamp(ts)
            .with_compressed_size(5000)
            .with_metadata_size(500)
            .with_disks(BTreeMap::from([("d".to_string(), "/data".to_string())]))
            .with_disk_types(BTreeMap::from([("d".to_string(), "local".to_string())]))
            .with_disk_remote_paths(BTreeMap::from([("s3".to_string(), "s3://b/p".to_string())]))
            .with_databases(vec![DatabaseInfo::test_new("db1")])
            .with_tables(BTreeMap::from([(
                "db1.t1".to_string(),
                TableManifest::test_new("Log"),
            )]));

        assert_eq!(manifest.name, "builder-test");
        assert_eq!(manifest.timestamp, ts);
        assert_eq!(manifest.compressed_size, 5000);
        assert_eq!(manifest.metadata_size, 500);
        assert_eq!(manifest.disks.len(), 1);
        assert_eq!(manifest.disk_types.len(), 1);
        assert_eq!(manifest.disk_remote_paths.len(), 1);
        assert_eq!(manifest.databases.len(), 1);
        assert_eq!(manifest.tables.len(), 1);
    }

    // -----------------------------------------------------------------------
    // Manifest backward compatibility with Go-format JSON
    // -----------------------------------------------------------------------

    #[test]
    fn test_manifest_go_compat_extra_fields_ignored() {
        // Simulate a manifest from Go clickhouse-backup with extra fields
        let json = r#"{
            "name": "go-backup",
            "timestamp": "2024-06-01T00:00:00Z",
            "extra_unknown_field": "should be ignored",
            "another_unknown": 42
        }"#;

        // Should deserialize successfully, ignoring unknown fields
        // (serde default behavior with deny_unknown_fields absent)
        let result = serde_json::from_str::<BackupManifest>(json);
        assert!(result.is_ok(), "Should tolerate unknown fields");
        assert_eq!(result.unwrap().name, "go-backup");
    }

    // -----------------------------------------------------------------------
    // PartInfo source field variations
    // -----------------------------------------------------------------------

    #[test]
    fn test_part_info_carried_source() {
        let json = r#"{
            "name": "all_0_0_0",
            "size": 500,
            "source": "carried:base-2024-01-01",
            "backup_key": "base/data/part.tar.lz4"
        }"#;

        let part: PartInfo = serde_json::from_str(json).unwrap();
        assert_eq!(part.source, "carried:base-2024-01-01");
        assert_eq!(part.name, "all_0_0_0");
        assert_eq!(part.backup_key, "base/data/part.tar.lz4");
    }
}
