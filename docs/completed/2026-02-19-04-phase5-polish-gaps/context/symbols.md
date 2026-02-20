# Symbol Verification Table

All types and APIs verified against source code (Phase 0.6).

## Types Used

| Variable/Field | Assumed Type | Actual Type | Verification Location |
|---|---|---|---|
| `Config.general.disable_progress_bar` | `bool` | `bool` | config.rs:47 |
| `BackupConfig.skip_projections` | `Vec<String>` | `Vec<String>` | config.rs:370 |
| `ChBackupError` | enum | enum with 8 variants | error.rs:4-29 |
| `BackupManifest.metadata_size` | `u64` | `u64` | manifest.rs:48 |
| `BackupManifest.tables` | `HashMap<String, TableManifest>` | `HashMap<String, TableManifest>` | manifest.rs:65 |
| `BackupSummary` | struct | struct with 7 fields | list.rs:28-44 |
| `BackupSummary.metadata_size` | DOES NOT EXIST | N/A | list.rs:28 -- must add |
| `ListResponse.metadata_size` | `u64` | `u64` | server/routes.rs:73 |
| `ListResponse.rbac_size` | `u64` | `u64` | server/routes.rs:74 |
| `ListResponse.config_size` | `u64` | `u64` | server/routes.rs:75 |
| `AppState` | struct | struct with Arc-wrapped fields | server/state.rs |
| `TableRow` | struct | struct with 7 fields | clickhouse/client.rs:25-33 |
| `TableRow.database` | `String` | `String` | clickhouse/client.rs:26 |
| `TableRow.name` | `String` | `String` | clickhouse/client.rs:27 |
| `TableRow.engine` | `String` | `String` | clickhouse/client.rs:28 |
| `TableRow.total_bytes` | `Option<u64>` | `Option<u64>` | clickhouse/client.rs:32 |
| `TableManifest.engine` | `String` | `String` | manifest.rs:96 |
| `PartInfo.name` | `String` | `String` | manifest.rs (verified) |
| `PartInfo.size` | `u64` | `u64` | manifest.rs (verified) |
| `TableFilter` | struct | struct | table_filter.rs |
| `StatusCode` | axum type | `axum::http::StatusCode` | server/routes.rs:9 |

## Functions Used

| Function | Signature | Location |
|---|---|---|
| `ChClient::list_tables()` | `async fn(&self) -> Result<Vec<TableRow>>` | clickhouse/client.rs:288 |
| `ChClient::list_all_tables()` | `async fn(&self) -> Result<Vec<TableRow>>` | clickhouse/client.rs:315 |
| `ChClient::ping()` | `async fn(&self) -> Result<()>` | clickhouse/client.rs (verified) |
| `list::format_size(bytes)` | `fn(u64) -> String` | list.rs:887 |
| `collect_parts()` | `fn(data_path, freeze_name, backup_dir, tables, disk_type_map, disk_paths, skip_disks, skip_disk_types) -> Result<HashMap<String, Vec<CollectedPart>>>` | backup/collect.rs:116 |
| `summary_to_list_response()` | `fn(BackupSummary, &str) -> ListResponse` | server/routes.rs:274 |
| `parse_backup_summary()` | `fn(&str, &Path) -> BackupSummary` | list.rs:808 |
| `build_router()` | `fn(AppState) -> Router` | server/mod.rs:41 |
| `restart_stub()` | `async fn() -> (StatusCode, &'static str)` | server/routes.rs:1187 |
| `tables_stub()` | `async fn() -> (StatusCode, &'static str)` | server/routes.rs:1192 |
| `TableFilter::new()` | `fn(&str) -> TableFilter` | table_filter.rs |
| `TableFilter::matches()` | `fn(&self, &str, &str) -> bool` | table_filter.rs |
| `hardlink_dir()` | `fn(src, dst) -> Result<()>` | backup/collect.rs (local) |

## Constants / Exit Codes (from design 11.6)

| Code | Meaning | Exists in Code? |
|---|---|---|
| 0 | Success | Yes (implicit from Ok(())) |
| 1 | General error | Yes (implicit from anyhow Err) |
| 2 | Usage error | No -- must implement |
| 3 | Backup not found | No -- must implement |
| 4 | Lock conflict | No -- must implement |
| 130 | SIGINT | No -- must implement |
| 143 | SIGTERM | No -- must implement |
