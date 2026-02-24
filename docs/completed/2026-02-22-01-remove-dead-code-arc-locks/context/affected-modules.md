# Affected Modules Analysis

## Summary

- **Modules to update:** 3 (src/clickhouse, src/storage, src/restore)
- **Modules unchanged:** 1 (src/server -- all wrapping justified)
- **Standalone files:** 2 (src/progress.rs, src/error.rs -- low priority)
- **Git base:** HEAD (master branch)

## Dead Code Inventory

### Category A: Definite Dead Code (remove)

| Item | File | Evidence |
|------|------|----------|
| `ChClient::debug` field | `src/clickhouse/client.rs:24` | `#[allow(dead_code)]`; set in new() but never read |
| `ChClient::inner()` method | `src/clickhouse/client.rs:252-254` | Zero callers in entire codebase |
| `S3Client::inner()` method | `src/storage/s3.rs:220-222` | Zero callers in entire codebase |
| `S3Client::concurrency()` method | `src/storage/s3.rs:235-237` | Zero callers in entire codebase |
| `S3Client::object_disk_path()` method | `src/storage/s3.rs:243-245` | Zero callers in entire codebase |
| `S3Client::concurrency` field | `src/storage/s3.rs:58` | Only read by dead `concurrency()` getter |
| `S3Client::object_disk_path` field | `src/storage/s3.rs:60` | Only read by dead `object_disk_path()` getter |
| `attach_parts()` function | `src/restore/attach.rs:486-491` | `#[allow(dead_code)]`; only `attach_parts_owned` used |

### Category B: Test-Only Public APIs (judgment call)

| Item | File | Evidence |
|------|------|----------|
| `ProgressTracker::disabled()` | `src/progress.rs:48-50` | Only called from `#[cfg(test)]` |
| `ProgressTracker::is_active()` | `src/progress.rs:67-69` | Only called from `#[cfg(test)]` in progress.rs |
| `ActionLog::running()` | `src/server/actions.rs:118-122` | Only called from `#[cfg(test)]` blocks |

### Category C: Unused Metrics (not recommended to remove)

| Item | File | Reason to keep |
|------|------|----------------|
| `Metrics::parts_uploaded_total` | `src/server/metrics.rs:32` | Prometheus convention: metrics should exist at zero |
| `Metrics::parts_skipped_incremental_total` | `src/server/metrics.rs:35` | Same -- registered for future instrumentation |

### Category D: Unused Error Variants (not recommended to remove)

| Item | File | Reason to keep |
|------|------|----------------|
| `ChBackupError::ClickHouseError` | `src/error.rs:7` | Part of error taxonomy for exit code mapping |
| `ChBackupError::S3Error` | `src/error.rs:10` | Same |
| `ChBackupError::ConfigError` | `src/error.rs:13` | Same |
| `ChBackupError::BackupError` | `src/error.rs:19` | Matched in exit_code() for code 3 |
| `ChBackupError::RestoreError` | `src/error.rs:22` | Part of taxonomy |
| `ChBackupError::ManifestError` | `src/error.rs:25` | Matched in exit_code() for code 3 |

### Category E: Intentionally Unused (do NOT remove)

| Item | File | Reason |
|------|------|--------|
| `ListParams::format` field | `src/server/routes.rs:74` | Documented: stored for integration table DDL compatibility |

## Arc/Mutex/ArcSwap Verdict

**All Arc/Mutex/ArcSwap wrapping in AppState is REQUIRED.** Analysis:

1. `Arc<ArcSwap<Config/ChClient/S3Client>>` -- Both `.load()` and `.store()` are called from routes. ArcSwap enables lock-free hot-swap via `/api/v1/restart` and `/api/v1/reload`.

2. `Arc<Mutex<...>>` on action_log, running_ops, watch_shutdown_tx, watch_reload_tx, watch_status, manifest_cache -- All are mutated from multiple concurrent async tasks. `tokio::sync::Mutex` is correct because locks are held across `.await` points.

3. `Arc<Semaphore>` -- Shared for concurrency control across handlers.

4. `ManifestCache` in `Arc<Mutex<...>>` -- Written by `list_remote_cached()` and invalidated by multiple operations. Required.

**No unnecessary Arc, Mutex, or locks were found.**

## CLAUDE.md Tasks

| Module | CLAUDE.md Status | Action |
|--------|------------------|--------|
| src/clickhouse | EXISTS | UPDATE after removing debug field and inner() from public API docs |
| src/storage | EXISTS | UPDATE after removing inner(), concurrency(), object_disk_path() from public API docs |
| src/restore | EXISTS | UPDATE after removing attach_parts() from public API docs |
| src/server | EXISTS | NO CHANGE needed |
