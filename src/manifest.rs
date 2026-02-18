//! Manifest types for backup metadata.
//!
//! The `BackupManifest` is the central data structure that flows between all
//! backup/upload/download/restore/list commands. It is serialized to JSON as
//! `metadata.json` in each backup directory.
//!
//! Format matches design doc section 7.1.

use std::collections::HashMap;
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
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub disks: HashMap<String, String>,

    /// Disk name -> disk type mapping (e.g. "local", "s3").
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub disk_types: HashMap<String, String>,

    /// Disk name -> remote_path mapping for S3 disks (e.g. "s3disk" -> "s3://bucket/prefix/").
    /// Empty for local disks. Used by upload to determine CopyObject source.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub disk_remote_paths: HashMap<String, String>,

    /// Tables included in this backup. Key is "db.table".
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub tables: HashMap<String, TableManifest>,

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
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub parts: HashMap<String, Vec<PartInfo>>,

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

/// Database metadata in the manifest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DatabaseInfo {
    /// Database name.
    pub name: String,

    /// CREATE DATABASE DDL statement.
    pub ddl: String,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_manifest() -> BackupManifest {
        let mut tables = HashMap::new();
        let mut parts = HashMap::new();
        parts.insert(
            "default".to_string(),
            vec![
                PartInfo {
                    name: "202401_1_50_3".to_string(),
                    size: 134_217_728,
                    backup_key: "chbackup/daily/data/default/trades/202401_1_50_3.tar.lz4"
                        .to_string(),
                    source: "uploaded".to_string(),
                    checksum_crc64: 12345678901234,
                    s3_objects: None,
                },
                PartInfo {
                    name: "202402_1_1_0".to_string(),
                    size: 4096,
                    backup_key: "chbackup/daily/data/default/trades/202402_1_1_0.tar.lz4"
                        .to_string(),
                    source: "uploaded".to_string(),
                    checksum_crc64: 11111111111111,
                    s3_objects: None,
                },
            ],
        );

        tables.insert(
            "default.trades".to_string(),
            TableManifest {
                ddl: "CREATE TABLE default.trades (id UInt64) ENGINE = MergeTree ORDER BY id"
                    .to_string(),
                uuid: Some("5f3a7b2c-1234-5678-9abc-def012345678".to_string()),
                engine: "MergeTree".to_string(),
                total_bytes: 134_221_824,
                parts,
                pending_mutations: Vec::new(),
                metadata_only: false,
                dependencies: Vec::new(),
            },
        );

        BackupManifest {
            manifest_version: 1,
            name: "daily-2024-01-15".to_string(),
            timestamp: Utc::now(),
            clickhouse_version: "24.1.3.31".to_string(),
            chbackup_version: "0.1.0".to_string(),
            data_format: "lz4".to_string(),
            compressed_size: 1_073_741_824,
            metadata_size: 524_288,
            disks: HashMap::from([("default".to_string(), "/var/lib/clickhouse".to_string())]),
            disk_types: HashMap::from([("default".to_string(), "local".to_string())]),
            disk_remote_paths: HashMap::new(),
            tables,
            databases: vec![DatabaseInfo {
                name: "default".to_string(),
                ddl: "CREATE DATABASE default ENGINE = Atomic".to_string(),
            }],
            functions: Vec::new(),
            named_collections: Vec::new(),
            rbac: None,
        }
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
    fn test_table_manifest_empty_parts_not_serialized() {
        let table = TableManifest {
            ddl: "CREATE TABLE test.t (id UInt64) ENGINE = MergeTree ORDER BY id".to_string(),
            uuid: None,
            engine: "MergeTree".to_string(),
            total_bytes: 0,
            parts: HashMap::new(),
            pending_mutations: Vec::new(),
            metadata_only: true,
            dependencies: Vec::new(),
        };
        let json = serde_json::to_string(&table).unwrap();
        // Empty parts, mutations, and dependencies should not appear in output
        assert!(!json.contains("\"parts\""));
        assert!(!json.contains("\"pending_mutations\""));
        assert!(!json.contains("\"dependencies\""));
    }
}
