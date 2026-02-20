# Data Authority Analysis

## Data Requirements for Watch Mode

| Data Needed | Source Type | Field Available | Decision |
|-------------|-------------|-----------------|----------|
| Time since last full backup | `BackupSummary` (from `list_remote`) | `timestamp: Option<DateTime<Utc>>` + `name: String` (name contains "full"/"incr") | USE EXISTING -- scan remote backups, parse name for type, use timestamp for elapsed time |
| Time since last incremental backup | `BackupSummary` (from `list_remote`) | Same as above | USE EXISTING |
| Backup name for diff-from | `BackupSummary` (from `list_remote`) | `name: String` | USE EXISTING -- find most recent non-broken backup name |
| Backup compressed size | `BackupManifest` | `compressed_size: u64` | USE EXISTING -- returned by `backup::create()` |
| ClickHouse shard macro | `system.macros` table | Not available via existing API | MUST IMPLEMENT -- add `ChClient::get_macros()` to query `SELECT macro, substitution FROM system.macros` |
| Watch interval (parsed) | `WatchConfig.watch_interval` | `String` (e.g., "1h") | USE EXISTING -- parse via `parse_duration_secs()` (needs to be made pub) |
| Full interval (parsed) | `WatchConfig.full_interval` | `String` (e.g., "24h") | USE EXISTING |
| Retry interval (parsed) | `WatchConfig.retry_interval` | `String` (e.g., "5m") | USE EXISTING |
| Retention counts | `effective_retention_local/remote(config)` | `i32` | USE EXISTING |
| Reload signal | Unix SIGHUP | Not available via existing code | MUST IMPLEMENT -- add `tokio::signal::unix::signal(SignalKind::hangup())` handler |
| Current time for template | `chrono::Utc::now()` | `DateTime<Utc>` | USE EXISTING |

## Analysis Notes

- Remote backup listing (`list_remote()`) already provides all data needed for resume-on-restart. No shadow state tracking needed.
- The backup type (full vs incremental) is NOT stored in the manifest or BackupSummary. It must be inferred from the backup name matching the name template (e.g., name contains "full" or "incr"). This is the approach used by the Go tool and specified in design 10.5.
- `BackupSummary.is_broken` can filter out broken backups during resume scan.
- All retention logic already exists in `list.rs` (retention_local, retention_remote with GC). Watch just needs to call these.
- The `create_remote` command pattern (routes.rs) already chains create+upload, providing a proven template for the watch cycle.
- Metrics fields are already registered in `Metrics` struct (watch_state, watch_last_full_timestamp, etc.) but set to default 0. Watch mode just needs to update them.

## Summary

| Category | Count |
|----------|-------|
| USE EXISTING | 9 |
| MUST IMPLEMENT | 2 |

Only two new data sources needed:
1. `ChClient::get_macros()` -- simple SELECT query for `{shard}` macro resolution
2. SIGHUP signal handler -- standard tokio unix signal pattern
