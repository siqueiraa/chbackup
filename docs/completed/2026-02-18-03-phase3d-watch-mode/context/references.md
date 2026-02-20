# Symbol & Reference Analysis -- Phase 3d Watch Mode

## Key Types and Their Locations

### Config Types

| Symbol | File | Line | Notes |
|--------|------|------|-------|
| `Config` | `src/config.rs` | 8 | Top-level config struct |
| `WatchConfig` | `src/config.rs` | 392 | Watch mode config -- 7 fields (missing `tables`) |
| `ApiConfig` | `src/config.rs` | 426 | Has `watch_is_main_process: bool` at line 476 |
| `parse_duration_secs` | `src/config.rs` | 1248 | **Private** -- needs `pub` for watch module |

### Server Types

| Symbol | File | Line | Notes |
|--------|------|------|-------|
| `AppState` | `src/server/state.rs` | 21 | Shared server state -- needs watch fields |
| `RunningOp` | `src/server/state.rs` | 37 | Operation tracking -- watch ops may bypass this |
| `Metrics` | `src/server/metrics.rs` | 18 | Has 4 watch metrics already registered |
| `ActionLog` | `src/server/actions.rs` | 50 | Ring buffer for action history |
| `start_server` | `src/server/mod.rs` | 109 | Server lifecycle -- needs watch loop integration |
| `build_router` | `src/server/mod.rs` | 38 | Router assembly -- stubs need replacing |

### Core Operations (called by watch loop)

| Symbol | File | Signature | Return |
|--------|------|-----------|--------|
| `backup::create` | `src/backup/mod.rs:64` | `(config, ch, name, table_pattern, schema_only, diff_from, partitions, skip_check_parts_columns)` | `Result<BackupManifest>` |
| `upload::upload` | `src/upload/mod.rs:165` | `(config, s3, name, backup_dir, delete_local, diff_from_remote, resume)` | `Result<()>` |
| `list::list_remote` | `src/list.rs:128` | `(s3)` | `Result<Vec<BackupSummary>>` |
| `list::delete_local` | `src/list.rs:220` | `(data_path, backup_name)` | `Result<()>` |
| `list::retention_local` | `src/list.rs:411` | `(data_path, keep)` | `Result<usize>` |
| `list::retention_remote` | `src/list.rs:629` | `(s3, keep)` | `Result<usize>` |
| `list::effective_retention_local` | `src/list.rs:380` | `(config)` | `i32` |
| `list::effective_retention_remote` | `src/list.rs:393` | `(config)` | `i32` |
| `list::BackupSummary` | `src/list.rs:28` | struct | Has `name`, `timestamp`, `is_broken` fields |

### CLI Types

| Symbol | File | Line | Notes |
|--------|------|------|-------|
| `Command::Watch` | `src/cli.rs` | 309 | Has `watch_interval`, `full_interval`, `name_template`, `tables` |
| `Command::Server` | `src/cli.rs` | 328 | Has `watch: bool` flag |

### ClickHouse Client

| Symbol | File | Notes |
|--------|------|-------|
| `ChClient` | `src/clickhouse/client.rs:12` | Clone, wraps clickhouse-rs HTTP client |
| `ChClient::get_version` | `src/clickhouse/client.rs` | `async fn() -> Result<String>` |
| `ChClient::get_disks` | `src/clickhouse/client.rs` | `async fn() -> Result<Vec<DiskRow>>` |

**Missing method:** No `query_macros()` method exists for resolving `{shard}` and other macros from `system.macros`. Needs to be added.

### S3 Client

| Symbol | File | Notes |
|--------|------|-------|
| `S3Client` | `src/storage/s3.rs` | Clone, wraps aws-sdk-s3 |

## References Analysis

### Files That Import Watch-Related Types

- `src/main.rs` -- uses `Command::Watch`, `Command::Server { watch }`, `WatchConfig` (via Config)
- `src/server/mod.rs` -- `start_server()` receives watch flag but ignores it
- `src/server/routes.rs` -- stub handlers for `/api/v1/watch/*`
- `src/server/metrics.rs` -- registers `watch_state`, `watch_last_full_timestamp`, `watch_last_incremental_timestamp`, `watch_consecutive_errors`
- `src/config.rs` -- defines `WatchConfig`, validates intervals when `watch.enabled`

### Signal Handling Patterns in Codebase

Current signal handling is in `src/server/mod.rs`:
- Line 157: `tokio::signal::ctrl_c().await.ok()` -- TLS path
- Line 184: `tokio::signal::ctrl_c().await.ok()` -- plain path

Both are in anonymous async blocks for graceful shutdown. Watch mode will need SIGHUP added alongside ctrl_c.

### Callers of `start_server`

Only caller: `src/main.rs:398` in the `Command::Server { watch }` match arm:
```rust
chbackup::server::start_server(Arc::new(config), ch, s3).await?;
```

The `watch` boolean is checked at line 391 but only produces a warning. The `start_server` function does not accept a `watch` parameter.

### Data Flow for Watch Loop

The watch loop orchestrates existing functions in this sequence:
1. `list::list_remote(s3)` -- Resume: scan existing backups
2. Decide full vs incremental based on timestamps
3. `backup::create(config, ch, name, tables, ...)` -- Create local backup
4. `upload::upload(config, s3, name, ...)` -- Upload to S3
5. `list::delete_local(data_path, name)` -- Delete local (if configured)
6. `list::retention_local(data_path, keep)` + `list::retention_remote(s3, keep)` -- Retention
7. Sleep for `watch_interval` or `retry_interval`

### WatchConfig Fields Used by CLI

The CLI `Watch` command at `src/cli.rs:309-325` accepts:
- `--watch-interval` -> `watch_interval: Option<String>`
- `--full-interval` -> `full_interval: Option<String>`
- `--name-template` -> `name_template: Option<String>`
- `-t / --tables` -> `tables: Option<String>`

These should override corresponding `WatchConfig` fields when provided.

### Name Template Macro Expansion

Design 10.3 specifies:
- `{type}` -> "full" or "incr"
- `{time:FORMAT}` -> strftime-formatted current time
- `{shard}` and other CH macros -> resolved from `system.macros`

The `system.macros` table has columns: `macro` (String), `substitution` (String).
Query: `SELECT macro, substitution FROM system.macros`

### `delete_local_after_upload` Config vs CLI

- WatchConfig has `delete_local_after_upload: bool` (default true)
- The `upload::upload()` function accepts `delete_local: bool` parameter
- Watch loop should pass `config.watch.delete_local_after_upload`

### Existing `retention_local` and `retention_remote` Return Types

Both return `Result<usize>` (count of deleted backups). The watch loop needs these for logging but does not need to propagate errors (retention is best-effort per design 10.7).
