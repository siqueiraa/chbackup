# Type Verification

## Types Used by Watch Mode

| Variable/Field | Assumed Type | Actual Type | Verification |
|---|---|---|---|
| `config.watch.watch_interval` | `String` | `String` | config.rs:400 |
| `config.watch.full_interval` | `String` | `String` | config.rs:404 |
| `config.watch.name_template` | `String` | `String` | config.rs:407 |
| `config.watch.max_consecutive_errors` | `u32` | `u32` | config.rs:411 |
| `config.watch.retry_interval` | `String` | `String` | config.rs:415 |
| `config.watch.delete_local_after_upload` | `bool` | `bool` | config.rs:419 |
| `config.watch.enabled` | `bool` | `bool` | config.rs:396 |
| `config.api.watch_is_main_process` | `bool` | `bool` | config.rs:476 |
| `BackupSummary.name` | `String` | `String` | list.rs:30 |
| `BackupSummary.timestamp` | `Option<DateTime<Utc>>` | `Option<DateTime<Utc>>` | list.rs:32 |
| `BackupSummary.is_broken` | `bool` | `bool` | list.rs:40 |
| `BackupManifest.name` | `String` | `String` | manifest.rs:25 |
| `BackupManifest.timestamp` | `DateTime<Utc>` | `DateTime<Utc>` | manifest.rs:28 |
| `BackupManifest.compressed_size` | `u64` | `u64` | manifest.rs:44 |
| `AppState.config` | `Arc<Config>` | `Arc<Config>` | server/state.rs:27 |
| `AppState.ch` | `ChClient` | `ChClient` | server/state.rs:28 |
| `AppState.s3` | `S3Client` | `S3Client` | server/state.rs:29 |
| `AppState.metrics` | `Option<Arc<Metrics>>` | `Option<Arc<Metrics>>` | server/state.rs:34 |
| `Metrics.watch_state` | `IntGauge` | `IntGauge` | server/metrics.rs:52 |
| `Metrics.watch_last_full_timestamp` | `Gauge` | `Gauge` | server/metrics.rs:55 |
| `Metrics.watch_last_incremental_timestamp` | `Gauge` | `Gauge` | server/metrics.rs:59 |
| `Metrics.watch_consecutive_errors` | `IntGauge` | `IntGauge` | server/metrics.rs:62 |
| `parse_duration_secs` return | `Result<u64>` | `Result<u64>` | config.rs:1248 (private fn) |
| `Config::load` | `fn(&Path, &[String]) -> Result<Self>` | `fn(&Path, &[String]) -> Result<Self>` | config.rs:810 |

## Key API Signatures Verified

### backup::create (src/backup/mod.rs:64)
```rust
pub async fn create(
    config: &Config,
    ch: &ChClient,
    backup_name: &str,
    table_pattern: Option<&str>,
    schema_only: bool,
    diff_from: Option<&str>,
    partitions: Option<&str>,
    skip_check_parts_columns: bool,
) -> Result<BackupManifest>
```

### upload::upload (src/upload/mod.rs:165)
```rust
pub async fn upload(
    config: &Config,
    s3: &S3Client,
    backup_name: &str,
    backup_dir: &Path,
    delete_local: bool,
    diff_from_remote: Option<&str>,
    resume: bool,
) -> Result<()>
```

### list::list_remote (src/list.rs:128)
```rust
pub async fn list_remote(s3: &S3Client) -> Result<Vec<BackupSummary>>
```

### list::retention_local (src/list.rs:411)
```rust
pub fn retention_local(data_path: &str, keep: i32) -> Result<usize>
```

### list::retention_remote (src/list.rs:629)
```rust
pub async fn retention_remote(s3: &S3Client, keep: i32) -> Result<usize>
```

### list::delete_local (src/list.rs:220)
```rust
pub fn delete_local(data_path: &str, backup_name: &str) -> Result<()>
```

### list::effective_retention_local (src/list.rs:380)
```rust
pub fn effective_retention_local(config: &Config) -> i32
```

### list::effective_retention_remote (src/list.rs:393)
```rust
pub fn effective_retention_remote(config: &Config) -> i32
```

## Functions That Do NOT Exist (Must Create)

| Function | Purpose |
|---|---|
| `ChClient::get_macros()` | Query `system.macros` for `{shard}` template resolution |
| `parse_duration_secs` (public) | Currently private in config.rs, needs to be made public |
| `resolve_name_template()` | Resolve `{type}`, `{time:FORMAT}`, `{shard}` macros in backup name |
| `watch::run_watch_loop()` | Main watch state machine loop |
| `watch::resume_state()` | Scan remote backups, determine next backup type |

## Notes

- `parse_duration_secs` is private (`fn` not `pub fn`) in config.rs:1248. Must be made `pub` for watch module to use it.
- No `get_macros()` method exists on `ChClient`. Need to add a query for `SELECT macro, substitution FROM system.macros`.
- No macro/shard-related code exists anywhere in the ClickHouse client module.
- `BackupSummary` does NOT have a `type` field (full vs incr). Type must be inferred from backup name matching the template, or by checking manifest details.
