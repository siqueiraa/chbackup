# Type Verification Table

## Types Used in This Plan

| Variable/Field | Assumed Type | Actual Type | Verification |
|---|---|---|---|
| `BackupSummary` | struct (10 fields) | `struct BackupSummary { name: String, timestamp: Option<DateTime<Utc>>, size: u64, compressed_size: u64, table_count: usize, metadata_size: u64, rbac_size: u64, config_size: u64, is_broken: bool, broken_reason: Option<String> }` | Read src/list.rs:46-68 |
| `ListParams` | struct (2 fields) | `struct ListParams { location: Option<String>, desc: Option<bool> }` | Read src/server/routes.rs:65-69 |
| `ListResponse` | struct (11 fields) | `struct ListResponse { name: String, created: String, location: String, size: u64, data_size: u64, object_disk_size: u64, metadata_size: u64, rbac_size: u64, config_size: u64, compressed_size: u64, required: String }` | Read src/server/routes.rs:73-85 |
| `TablesParams` | struct (with pagination) | `struct TablesParams { table: Option<String>, all: Option<bool>, backup: Option<String>, offset: Option<usize>, limit: Option<usize> }` | Read src/server/routes.rs:88-97 |
| `PartInfo.source` | String | `pub source: String` with default `"uploaded"` | Read src/manifest.rs:141-144 |
| `PartInfo.s3_objects` | Option<Vec<S3ObjectInfo>> | `pub s3_objects: Option<Vec<S3ObjectInfo>>` | Read src/manifest.rs:152-153 |
| `S3ObjectInfo.size` | u64 | `pub size: u64` | Read src/manifest.rs:162 |
| `BackupManifest.tables` | HashMap<String, TableManifest> | `pub tables: HashMap<String, TableManifest>` | Read src/manifest.rs:65 |
| `TableManifest.parts` | HashMap<String, Vec<PartInfo>> | `pub parts: HashMap<String, Vec<PartInfo>>` | Read src/manifest.rs:112 |
| `AppState` | struct (12 fields) | Full struct at src/server/state.rs:65-85 with ArcSwap fields | Read src/server/state.rs:65 |
| `ActionRequest.command` | String | `pub command: String` | Read src/server/routes.rs:60-61 |
| `ListFormat` | enum (5 variants) | `enum ListFormat { Default, Json, Yaml, Csv, Tsv }` | Read src/list.rs:30-42 |
| `MutationInfo.parts_to_do` | Vec<String> | `pub parts_to_do: Vec<String>` | Read src/manifest.rs:191 |
| `GeneralConfig` | 14 fields (per comment) | **15 fields** (remote_cache_ttl_secs added Phase 8) | Read src/config.rs:36-96, counted fields |
| `WatchConfig` | 7 fields (per comment) | **8 fields** (delete_local_after_upload exists) | Read src/config.rs:404-435, counted fields |

## Key Type Observations

1. **BackupSummary is missing `object_disk_size` and `required` fields** -- these need to be added for MISSING-2 and MISSING-4.

2. **ListParams is missing `offset`, `limit`, and `format` fields** -- TablesParams already has `offset` and `limit` as `Option<usize>`, which is the exact pattern to follow.

3. **PartInfo.source values**: `"uploaded"` (default) or `"carried:{base_backup_name}"`. The `carried:` prefix is already used by `collect_incremental_bases()` in list.rs:959 via `strip_prefix("carried:")`. Same pattern needed for `required` field extraction.

4. **list_backups() return type**: Currently `Result<Json<Vec<ListResponse>>, ...>`. Must change to return `(headers, Json<...>)` tuple for X-Total-Count, matching tables() pattern at routes.rs:1353.

5. **Signal handler types**: `tokio::signal::unix::{signal, SignalKind}` with `SignalKind::hangup()` for SIGHUP, `SignalKind::quit()` for SIGQUIT. SIGTERM uses `SignalKind::terminate()`.

6. **No `.as_str()` anti-patterns found** -- all types are native Rust types (String, u64, bool).

7. **Config param counts are wrong in comments**: GeneralConfig comment says 14 but has 15 fields; WatchConfig comment says 7 but has 8 fields. CLAUDE.md line 55 echoes these wrong counts.

8. **`named_collection_size` referenced in CLAUDE.md line 174 does not exist in codebase** -- grep for it across all src/ returns 0 matches. Phantom reference.

9. **Design doc `required_backups` field (line 1773)**: The design doc mentions `required_backups` field but implementation uses `carried:{name}` source scanning in `collect_incremental_bases()`.

10. **Design doc `parts_to_do: 3` (line 983)**: Design shows an integer but actual MutationInfo uses `Vec<String>` (list of part names).
