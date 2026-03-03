# Symbol Verification

## Testable Pure Functions (verified signatures)

### src/main.rs (0% coverage, 549 missed lines)

| Function | Signature | Location | Testable? |
|----------|-----------|----------|-----------|
| `backup_name_from_command` | `fn(cmd: &Command) -> Option<&str>` | main.rs:24 | YES - pure enum match |
| `resolve_backup_name` | `fn(name: Option<String>) -> Result<String>` | main.rs:726 | YES - calls validate_backup_name + checks reserved names |
| `backup_name_required` | `fn(name: Option<String>, command: &str) -> Result<String>` | main.rs:744 | YES - validates + requires name |
| `map_cli_location` | `fn(loc: cli::Location) -> list::Location` | main.rs:756 | YES - pure enum mapping |
| `map_cli_list_format` | `fn(fmt: cli::ListFormat) -> list::ListFormat` | main.rs:764 | YES - pure enum mapping |
| `merge_skip_projections` | `fn(cli_flag: Option<&str>, config_list: &[String]) -> Vec<String>` | main.rs:808 | YES - pure string split/merge |
| `resolve_local_shortcut` | `fn(name: &str, data_path: &str) -> Result<String>` | main.rs:778 | PARTIAL - needs local dir |
| `resolve_remote_shortcut` | `async fn(name: &str, s3: &S3Client) -> Result<String>` | main.rs:793 | NO - needs S3 |
| `acquire_lock` | `fn(cmd_name: &str, backup_name: Option<&str>) -> Result<Option<PidLock>>` | main.rs:42 | NO - filesystem side effects |

### src/backup/mod.rs (46.41% coverage, 515 missed lines)

| Function | Signature | Location | Testable? |
|----------|-----------|----------|-----------|
| `normalize_uuid` | `fn(uuid: &str) -> Option<String>` | mod.rs:43 | YES - pure string check |
| `parse_partition_list` | `fn(partitions: Option<&str>) -> Vec<String>` | mod.rs:56 | YES - pure string split |
| `is_ignorable_freeze_error` | `fn(err_msg: &str) -> bool` | mod.rs:84 | YES - pure string match |
| `is_metadata_only_engine` | `fn(engine: &str) -> bool` | mod.rs:868 | YES - pure matches! |
| `is_benign_type` | `fn(type_str: &str) -> bool` | mod.rs:890 | YES - pure starts_with |
| `filter_benign_type_drift` | `fn(inconsistencies: Vec<ColumnInconsistency>) -> Vec<ColumnInconsistency>` | mod.rs:902 | YES - pure filter |

### src/download/mod.rs (44.27% coverage, 632 missed lines)

| Function | Signature | Location | Testable? |
|----------|-----------|----------|-----------|
| `sanitize_relative_path` | `fn(input: &str) -> PathBuf` | mod.rs:44 | YES - pure path sanitization, SECURITY-CRITICAL |
| `resolve_download_target_dir` | `fn(manifest_disks: &BTreeMap<String, String>, disk_name: &str, backup_name: &str, backup_dir: &Path) -> PathBuf` | mod.rs:62 | PARTIAL - checks Path::exists() |
| `check_disk_space` | `fn(backup_dir: &Path, required_bytes: u64) -> Result<()>` | mod.rs:105 | PARTIAL - uses statvfs |

### src/restore/attach.rs (45.95% coverage, 507 missed lines)

| Function | Signature | Location | Testable? |
|----------|-----------|----------|-----------|
| `uuid_s3_prefix` | `fn(uuid: &str) -> String` | attach.rs:132 | YES - pure string format |
| `is_attach_warning` | `fn(e: &anyhow::Error) -> bool` | attach.rs:915 | YES - pure string match |
| `hardlink_or_copy_dir` | `fn(src: &Path, dst: &Path) -> Result<()>` | attach.rs:927 | YES - filesystem (tempdir) |
| `detect_clickhouse_ownership` | `fn(data_path: &Path) -> Result<(Option<u32>, Option<u32>)>` | attach.rs:1024 | YES - filesystem (tempdir) |
| `get_table_data_path` | `fn(data_paths: &[String], data_path_config: &str, db: &str, table: &str) -> PathBuf` | attach.rs:1058 | YES - pure path logic |

### src/server/routes.rs (44.51% coverage, 1138 missed lines)

| Function | Signature | Location | Testable? |
|----------|-----------|----------|-----------|
| `format_duration` | `fn(d: std::time::Duration) -> String` | routes.rs:1912 | YES - already tested (8 tests) |
| `paginate` | `fn<T>(items: Vec<T>, offset: Option<usize>, limit: Option<usize>) -> (Vec<T>, usize)` | routes.rs:47 | YES - already tested (8 tests) |
| `validation_error` | `fn(name: &str, e: &str) -> (StatusCode, Json<ErrorResponse>)` | routes.rs:37 | YES - already tested |
| `summary_to_list_response` | `fn(s: list::BackupSummary, location: &str) -> ListResponse` | routes.rs:821 | YES - already tested |
| `maybe_rebuild_semaphore` | `fn(state: &AppState, new_config: &Config, caller: &str)` | routes.rs:1499 | NO - needs AppState |

### src/restore/schema.rs (47.85% coverage, 400 missed lines)

| Function | Signature | Location | Testable? |
|----------|-----------|----------|-----------|
| `ensure_if_not_exists_database` | `fn(ddl: &str) -> String` | schema.rs:813 | YES - already well tested |
| `ensure_if_not_exists_table` | `fn(ddl: &str) -> String` | schema.rs:822 | YES - already well tested |
| `is_replicated_engine` | `fn(engine: &str) -> bool` | schema.rs:502 | YES - already tested |

### src/storage/s3.rs (28.28% coverage, 837 missed lines)

| Function | Signature | Location | Testable? |
|----------|-----------|----------|-----------|
| `percent_encode_s3_key` | `fn(key: &str) -> String` | s3.rs | YES - already tested |
| `parse_s3_uri` | `fn(uri: &str) -> Result<(String, String)>` | s3.rs | YES - already tested |
| `calculate_chunk_size` | `fn(data_len: u64, config_chunk_size: u64, max_parts_count: u64) -> u64` | s3.rs | YES - already tested |

## Key Types Referenced

| Type | Definition | Location |
|------|-----------|----------|
| `cli::Command` | `pub enum Command { Create{..}, Upload{..}, ... }` | src/cli.rs:50 |
| `cli::Location` | `pub enum Location { Local, Remote }` | src/cli.rs:44 |
| `cli::ListFormat` | `pub enum ListFormat { Default, Json, Yaml, Csv, Tsv }` | src/cli.rs:5 |
| `list::Location` | `pub enum Location { Local, Remote }` | src/list.rs:26 |
| `list::ListFormat` | `pub enum ListFormat { Default, Json, Yaml, Csv, Tsv }` | src/list.rs:33 |
| `ColumnInconsistency` | `pub struct { database: String, table: String, column: String, types: Vec<String> }` | src/clickhouse/client.rs:79 |
| `PartInfo` | Manifest part info struct | src/manifest.rs |
| `BackupManifest` | Central manifest structure | src/manifest.rs |
| `validate_backup_name` | `pub fn(name: &str) -> Result<(), &'static str>` | src/server/state.rs:416 |
| `generate_backup_name` | `pub fn() -> String` | src/lib.rs:25 |

## Coverage Impact Estimates

| File | Current % | Estimated After | New Tests Count |
|------|-----------|-----------------|-----------------|
| main.rs | 0% | ~8-12% | ~15-20 tests |
| backup/mod.rs | 46.41% | ~52-56% | ~15-20 tests |
| download/mod.rs | 44.27% | ~47-50% | ~8-10 tests |
| restore/attach.rs | 45.95% | ~52-56% | ~12-15 tests |
| restore/schema.rs | 47.85% | ~48-49% | ~2-3 tests (mostly already covered) |
| CI gate | 35% | 55% | N/A (config change) |

Note: main.rs coverage % improvement is limited because the `run()` async function (the bulk of the file) cannot be unit tested. The pure helper functions at the bottom represent a relatively small fraction of total lines.
