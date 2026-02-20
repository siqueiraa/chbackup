# Manifest/Metadata Format Comparison: Go vs Rust

## Summary

The Go and Rust implementations use **fundamentally different metadata architectures**. Go uses a **two-level split** (top-level `metadata.json` + per-table JSON files in `metadata/{db}/{table}.json`), while Rust uses a **single self-contained** `metadata.json` that embeds all table details inline. This means **the two formats are NOT wire-compatible** -- Go cannot read Rust manifests and vice versa without a translation layer.

---

## 1. Architectural Difference: Split vs Monolithic

### Go: Two-Level Metadata

The Go backup writes **two kinds** of JSON files to S3:

1. **Top-level `{backup_name}/metadata.json`** -- `BackupMetadata` struct. Contains backup-level info plus a flat list of `TableTitle` (just `{database, table}` pairs). Does NOT contain table DDL, parts, or any table-level detail.

2. **Per-table `{backup_name}/metadata/{db}/{table}.json`** -- `TableMetadata` struct. Contains CREATE DDL, parts map, files map, checksums, mutations, dependencies, UUID, sizes.

### Rust: Single Monolithic Manifest

The Rust backup writes **one** JSON file:

- **`{backup_name}/metadata.json`** -- `BackupManifest` struct. Contains everything: backup-level info AND full `TableManifest` structs (with DDL, parts, mutations, etc.) embedded in a `HashMap<String, TableManifest>`.

### Impact

- A Go restore downloads `metadata.json` first for the table list, then downloads each per-table metadata JSON separately (with concurrency).
- A Rust restore downloads one `metadata.json` and has everything.
- **Cross-compatibility**: Go tools cannot find table details in a Rust manifest (they look for separate files under `metadata/`). Rust tools cannot assemble table details from Go's split files (they expect everything in the top-level JSON).

---

## 2. Top-Level Backup Metadata Fields

| Go Field (JSON key) | Go Type | Rust Field (JSON key) | Rust Type | Notes |
|---|---|---|---|---|
| `backup_name` | `string` | `name` | `String` | **DIFFERENT JSON KEY** |
| `creation_date` | `time.Time` | `timestamp` | `DateTime<Utc>` | **DIFFERENT JSON KEY** |
| `version` | `string` | `chbackup_version` | `String` | **DIFFERENT JSON KEY** -- Go's `version` is the clickhouse-backup binary version |
| `clickhouse_version` | `string` | `clickhouse_version` | `String` | Same |
| `data_format` | `string` | `data_format` | `String` | Same |
| `compressed_size` | `uint64` | `compressed_size` | `u64` | Same |
| `metadata_size` | `uint64` | `metadata_size` | `u64` | Same |
| `data_size` | `uint64` | -- | -- | **MISSING in Rust** -- uncompressed data size |
| `object_disk_size` | `uint64` | -- | -- | **MISSING in Rust** -- size of S3 object disk data |
| `rbac_size` | `uint64` | -- | -- | **MISSING in Rust** (hardcoded to 0 in list) |
| `config_size` | `uint64` | -- | -- | **MISSING in Rust** (hardcoded to 0 in list) |
| `named_collections_size` | `uint64` | -- | -- | **MISSING in Rust** |
| `disks` | `map[string]string` | `disks` | `HashMap<String, String>` | Same |
| `disk_types` | `map[string]string` | `disk_types` | `HashMap<String, String>` | Same |
| `tables` | `[]TableTitle` | `tables` | `HashMap<String, TableManifest>` | **STRUCTURALLY DIFFERENT** -- Go is flat list of `{database, table}`; Rust embeds full table metadata |
| `databases` | `[]DatabasesMeta` | `databases` | `Vec<DatabaseInfo>` | Different struct (see section 5) |
| `functions` | `[]FunctionsMeta` | `functions` | `Vec<String>` | **DIFFERENT** -- Go stores `{name, create_query}`; Rust stores only names |
| `required_backup` | `string` | -- | -- | **MISSING in Rust** -- incremental chain base backup name (Rust uses `source` field per-part instead) |
| `tags` | `string` | -- | -- | **MISSING in Rust** -- comma-separated tags like "regular,embedded" |
| -- | -- | `manifest_version` | `u32` | **MISSING in Go** -- Rust has schema versioning |
| -- | -- | `disk_remote_paths` | `HashMap<String, String>` | **MISSING in Go** -- Rust tracks S3 disk remote paths |
| -- | -- | `named_collections` | `Vec<String>` | Go does not have a list; uses `named_collections_size` |
| -- | -- | `rbac` | `Option<RbacInfo>` | Go uses `rbac_size` instead of a struct |

### Critical JSON Key Differences

These three field name differences alone break bidirectional parsing:

| Go JSON key | Rust JSON key | Field meaning |
|---|---|---|
| `backup_name` | `name` | Backup name |
| `creation_date` | `timestamp` | Creation time |
| `version` | `chbackup_version` | Tool version |

---

## 3. Table Metadata Fields

### Go: `TableMetadata` (separate per-table JSON file)

```go
type TableMetadata struct {
    Files                map[string][]string   `json:"files,omitempty"`
    RebalancedFiles      map[string]string     `json:"rebalanced_files,omitempty"`
    Table                string                `json:"table"`
    Database             string                `json:"database"`
    UUID                 string                `json:"uuid,omitempty"`
    Parts                map[string][]Part     `json:"parts"`
    Query                string                `json:"query"`
    Size                 map[string]int64      `json:"size"`
    TotalBytes           uint64                `json:"total_bytes,omitempty"`
    DependenciesTable    string                `json:"dependencies_table,omitempty"`
    DependenciesDatabase string                `json:"dependencies_database,omitempty"`
    Mutations            []MutationMetadata    `json:"mutations,omitempty"`
    MetadataOnly         bool                  `json:"metadata_only"`
    LocalFile            string                `json:"local_file,omitempty"`
    Checksums            map[string]uint64     `json:"checksums,omitempty"`
}
```

### Rust: `TableManifest` (embedded in top-level manifest)

```rust
pub struct TableManifest {
    pub ddl: String,                              // Go: "query"
    pub uuid: Option<String>,                     // Go: "uuid"
    pub engine: String,                           // MISSING in Go
    pub total_bytes: u64,                         // Go: "total_bytes"
    pub parts: HashMap<String, Vec<PartInfo>>,    // Go: "parts"
    pub pending_mutations: Vec<MutationInfo>,     // Go: "mutations"
    pub metadata_only: bool,                      // Go: "metadata_only"
    pub dependencies: Vec<String>,                // Go uses two strings (table + database)
}
```

### Field-by-Field Comparison

| Go Field (JSON key) | Rust Field (JSON key) | Notes |
|---|---|---|
| `query` | `ddl` | **DIFFERENT JSON KEY** -- both hold CREATE DDL |
| `table` | -- | **MISSING in Rust** -- Rust uses the HashMap key "db.table" instead |
| `database` | -- | **MISSING in Rust** -- same reason |
| `uuid` | `uuid` | Same |
| `parts` | `parts` | Same structure: `map[disk][]Part` |
| `total_bytes` | `total_bytes` | Same |
| `mutations` | `pending_mutations` | **DIFFERENT JSON KEY** |
| `metadata_only` | `metadata_only` | Same |
| `dependencies_table` | `dependencies` | **DIFFERENT** -- Go has two separate strings; Rust has a Vec |
| `dependencies_database` | -- | Go has separate database dependency; Rust merges into Vec |
| `files` | -- | **MISSING in Rust** -- Go maps disk -> list of uploaded S3 keys |
| `rebalanced_files` | -- | **MISSING in Rust** -- Go tracks rebalanced file mappings |
| `size` | -- | **MISSING in Rust** -- Go maps disk -> byte size |
| `local_file` | -- | **MISSING in Rust** -- runtime field, path to local JSON file |
| `checksums` | -- | **MISSING in Rust** -- Go: `map[part_name]uint64` at table level |
| -- | `engine` | **MISSING in Go** -- Rust stores engine name |

---

## 4. Part Info Fields

### Go: `Part`

```go
type Part struct {
    Name           string `json:"name"`
    Required       bool   `json:"required,omitempty"`
    RebalancedDisk string `json:"rebalanced_disk,omitempty"`
}
```

### Rust: `PartInfo`

```rust
pub struct PartInfo {
    pub name: String,
    pub size: u64,
    pub backup_key: String,
    pub source: String,
    pub checksum_crc64: u64,
    pub s3_objects: Option<Vec<S3ObjectInfo>>,
}
```

### Comparison

| Go Field | Rust Field | Notes |
|---|---|---|
| `name` | `name` | Same |
| `required` | -- | **MISSING in Rust** -- Go uses bool to mark parts that exist in the base backup (incremental). Rust uses `source: "carried:base_name"` string instead |
| `rebalanced_disk` | -- | **MISSING in Rust** -- Go feature for disk rebalancing |
| -- | `size` | **MISSING in Go Part** -- Go stores sizes at table level in `Size` map and `Checksums` |
| -- | `backup_key` | **MISSING in Go Part** -- Go stores uploaded keys in `Files` map at table level |
| -- | `source` | **MISSING in Go** -- Rust tracks whether part was "uploaded" or "carried:base" |
| -- | `checksum_crc64` | **MISSING in Go Part** -- Go stores checksums at table level in `Checksums` map |
| -- | `s3_objects` | **MISSING in Go Part** -- Rust embeds S3 object references per-part |

**Key design difference**: Go's `Part` is minimal (just name + flags), with sizes, checksums, and S3 keys tracked at the `TableMetadata` level. Rust's `PartInfo` is self-contained with all metadata per-part.

---

## 5. S3 Object Info

### Go

Go does NOT have an explicit S3 object info struct in the manifest. Object disk data is handled at the storage/upload layer, not serialized into per-part metadata. The `Files` map on `TableMetadata` tracks uploaded S3 keys, and `object_disk_size` at the backup level tracks total size.

### Rust: `S3ObjectInfo`

```rust
pub struct S3ObjectInfo {
    pub path: String,
    pub size: u64,
    pub backup_key: String,
}
```

This is a Rust-only addition -- embedded per-part for S3 object disk parts. No Go equivalent in the manifest format.

---

## 6. Database Info

### Go: `DatabasesMeta`

```go
type DatabasesMeta struct {
    Name   string `json:"name"`
    Engine string `json:"engine"`
    Query  string `json:"query"`
}
```

### Rust: `DatabaseInfo`

```rust
pub struct DatabaseInfo {
    pub name: String,
    pub ddl: String,
}
```

### Differences

| Go Field | Rust Field | Notes |
|---|---|---|
| `name` | `name` | Same |
| `engine` | -- | **MISSING in Rust** -- Go stores engine name separately |
| `query` | `ddl` | **DIFFERENT JSON KEY** -- both store CREATE DATABASE DDL |

---

## 7. Function Metadata

### Go: `FunctionsMeta`

```go
type FunctionsMeta struct {
    Name        string `json:"name"`
    CreateQuery string `json:"create_query"`
}
```

### Rust

```rust
pub functions: Vec<String>  // just names, no DDL
```

**Gap**: Rust stores only function names, not the CREATE FUNCTION DDL. This means Rust cannot restore user-defined functions from a backup.

---

## 8. Mutation Metadata

### Go: `MutationMetadata`

```go
type MutationMetadata struct {
    MutationId string `json:"mutation_id" ch:"mutation_id"`
    Command    string `json:"command" ch:"command"`
}
```

### Rust: `MutationInfo`

```rust
pub struct MutationInfo {
    pub mutation_id: String,
    pub command: String,
    pub parts_to_do: Vec<String>,
}
```

### Differences

| Go Field | Rust Field | Notes |
|---|---|---|
| `mutation_id` | `mutation_id` | Same |
| `command` | `command` | Same |
| -- | `parts_to_do` | **MISSING in Go** -- Rust extends with parts tracking |

---

## 9. Incremental Backup Tracking

### Go Approach

- Top-level `required_backup` field names the base backup.
- Per-part `required: true` boolean marks parts that exist in the base and should NOT be re-uploaded.
- During download/restore, Go reads `required_backup` to find the base, then fetches parts marked `required: false` from this backup and `required: true` parts from the base.

### Rust Approach

- No top-level `required_backup` field (though Rust's retention code checks `required_backups` -- plural -- which may be computed at runtime).
- Per-part `source` field: `"uploaded"` for new parts, `"carried:base_backup_name"` for parts referencing another backup.
- Per-part `backup_key` points to the actual S3 key (which may be in the base backup's prefix).

### Compatibility Impact

These approaches are functionally equivalent but serialized differently. A Go incremental backup cannot be directly consumed by Rust (different field names, different semantics for the `required` flag vs `source` string).

---

## 10. Serialization Details

| Aspect | Go | Rust |
|---|---|---|
| JSON indent | Tab (`\t`) | 2 spaces (serde_json pretty default) |
| Empty collections | `omitempty` on some fields | `skip_serializing_if` on some fields |
| Null vs absent | `omitempty` omits zero values | `Option` with `skip_serializing_if = "Option::is_none"` |
| File permissions | `0640` | Default (umask-dependent) |
| `metadata_only: false` | Always serialized (no omitempty) | Always serialized (no skip) |
| `tables` in top-level | Always serialized (no omitempty) | `skip_serializing_if = "HashMap::is_empty"` |

---

## 11. Backward Compatibility Assessment

### Can Rust read Go manifests?

**NO** -- without a compatibility layer. Key blockers:

1. Go uses `backup_name`, Rust expects `name`
2. Go uses `creation_date`, Rust expects `timestamp`
3. Go uses `version` (for tool version), Rust expects `chbackup_version`
4. Go's `tables` is `[{database, table}]`, Rust expects `HashMap<String, TableManifest>` with embedded DDL/parts
5. Go stores table details in separate per-table files, not in the top-level manifest

### Can Go read Rust manifests?

**NO** -- without a compatibility layer. Key blockers:

1. Rust uses `name`, Go expects `backup_name`
2. Rust uses `timestamp`, Go expects `creation_date`
3. Rust's `tables` is a full HashMap, Go expects `[]TableTitle`
4. Go expects separate per-table JSON files under `metadata/{db}/{table}.json` which Rust does not write

### Migration Path

To enable cross-tool compatibility, one would need:
- A manifest version field (Rust has `manifest_version`, Go does not)
- A translation layer that can read both formats
- Writing both the Go-style split files AND the Rust monolithic file during upload
- Using `#[serde(alias = "...")]` in Rust to accept Go field names

---

## 12. Summary of Gaps

### Fields Rust is Missing (present in Go)

| Field | Location | Severity | Notes |
|---|---|---|---|
| `data_size` | BackupMetadata | Low | Uncompressed data size; useful for display |
| `object_disk_size` | BackupMetadata | Low | S3 disk data size; useful for display |
| `rbac_size` | BackupMetadata | Low | Currently hardcoded to 0 in Rust list API |
| `config_size` | BackupMetadata | Low | Currently hardcoded to 0 in Rust list API |
| `named_collections_size` | BackupMetadata | Low | Size tracking for named collections |
| `required_backup` | BackupMetadata | Medium | Incremental chain tracking at backup level |
| `tags` | BackupMetadata | Low | Comma-separated metadata tags |
| `files` | TableMetadata | Medium | Map of disk -> uploaded S3 keys (Go uses for download) |
| `rebalanced_files` | TableMetadata | Low | Disk rebalancing feature |
| `size` (per-disk) | TableMetadata | Low | Size per disk for a table |
| `checksums` (table-level) | TableMetadata | Medium | CRC checksums at table level (Go's equivalent to Rust per-part CRC) |
| `engine` (in DatabasesMeta) | DatabaseInfo | Low | Database engine name stored separately from DDL |
| `create_query` (functions) | Top-level | Medium | Function DDL for restore |
| `required` (per-part) | Part | N/A | Different approach -- Rust uses `source` string |
| `rebalanced_disk` | Part | Low | Disk rebalancing |

### Structural Gaps

1. **Split vs Monolithic**: The biggest gap. Go writes per-table metadata as separate S3 objects; Rust embeds everything in one file.
2. **JSON key names differ**: `backup_name`/`name`, `creation_date`/`timestamp`, `version`/`chbackup_version`, `query`/`ddl`, `mutations`/`pending_mutations`.
3. **Tables representation**: Go top-level has `[]TableTitle` (name-only); Rust has full `HashMap<String, TableManifest>`.
4. **Incremental tracking**: Go uses `required_backup` + `Part.Required` bool; Rust uses `PartInfo.source` string.
5. **Checksum location**: Go stores checksums at table level; Rust stores per-part.
6. **S3 key tracking**: Go stores in `Files` map at table level; Rust stores in `PartInfo.backup_key` per-part.

### Recommendations

If cross-compatibility with Go clickhouse-backup is a goal:

1. **Add `#[serde(alias)]`** for Go field names on `BackupManifest` to enable reading Go manifests.
2. **Add a Go-compat write mode** that writes per-table metadata files alongside the monolithic manifest.
3. **Add missing size fields** (`data_size`, `object_disk_size`, etc.) to improve list/display parity.
4. **Store function DDL** instead of just names, or restore will silently skip UDFs.
5. **Add `required_backup` field** to top-level manifest for Go-compatible incremental chain tracking.
6. **Consider Go-style `Part.Required`** as a simpler incremental flag alongside the `source` string.

If cross-compatibility is NOT a goal (Rust is a standalone replacement):

1. The current Rust monolithic design is arguably better (single fetch, self-contained, no partial state).
2. Add the missing size tracking fields for display completeness.
3. Store function DDL for proper UDF restore.
4. The JSON key differences are intentional design choices and need not change.
