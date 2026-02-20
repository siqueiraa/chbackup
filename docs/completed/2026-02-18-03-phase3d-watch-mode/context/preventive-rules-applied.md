# Preventive Rules Applied

## Rules Checked

### RC-006: Plan code snippets use unverified APIs
**Status:** APPLIED
**Action:** All function signatures verified via grep. Key functions:
- `backup::create(config, ch, backup_name, table_pattern, schema_only, diff_from, partitions, skip_check_parts_columns) -> Result<BackupManifest>` (src/backup/mod.rs:64)
- `upload::upload(config, s3, backup_name, backup_dir, delete_local, diff_from_remote, resume) -> Result<()>` (src/upload/mod.rs:165)
- `list::list_remote(s3) -> Result<Vec<BackupSummary>>` (src/list.rs:128)
- `list::retention_local(data_path, keep) -> Result<usize>` (src/list.rs:411)
- `list::retention_remote(s3, keep) -> Result<usize>` (src/list.rs:629)
- `list::effective_retention_local(config) -> i32` (src/list.rs:380)
- `list::effective_retention_remote(config) -> i32` (src/list.rs:393)
- `list::delete_local(data_path, backup_name) -> Result<()>` (src/list.rs:220)
- `parse_duration_secs(s) -> Result<u64>` (src/config.rs:1248) -- PRIVATE, not pub
**Note:** `parse_duration_secs` is private to config.rs. Watch module will need its own duration parsing or config.rs must expose it.

### RC-011: State machine flags missing exit path transitions
**Status:** APPLIED -- HIGH RELEVANCE
**Action:** Watch mode IS a state machine. Plan must verify:
- `consecutive_error_count` is reset on success AND incremented on error
- `reload_pending` flag is cleared after reload applies
- `force_next_full` flag is cleared after a full backup succeeds
- Each state transition has error, success, and timeout paths

### RC-015: Cross-task return type mismatch
**Status:** APPLIED
**Action:** Watch loop calls `backup::create()` which returns `Result<BackupManifest>`, then `upload::upload()` which returns `Result<()>`. No cross-task type mismatch risk since these are called sequentially in the same loop iteration.

### RC-016: Struct field completeness for consumer tasks
**Status:** APPLIED
**Action:** WatchConfig already has all 7 fields defined (config.rs:392-420). No new fields needed unless adding `tables` field (see design 10.3).

### RC-017: State field declaration missing
**Status:** APPLIED
**Action:** Watch state machine will need new fields. Plan must declare all fields (e.g., `consecutive_errors`, `last_full_time`, `last_incr_time`, `force_next_full`, `reload_pending`) in an explicit struct.

### RC-019: Existing implementation pattern not followed for similar code
**Status:** APPLIED
**Action:** The watch loop's create+upload pipeline MUST follow the existing `create_remote` pattern from routes.rs (lines 613-717). Copy the exact parameter passing pattern.

### RC-021: Struct/field file location assumed without verification
**Status:** APPLIED
**Action:** Verified actual locations:
- `WatchConfig`: src/config.rs:392
- `ApiConfig`: src/config.rs:426
- `AppState`: src/server/state.rs:26
- `Metrics`: src/server/metrics.rs:18
- `BackupManifest`: src/manifest.rs:19
- `BackupSummary`: src/list.rs:28

### RC-032: Adding tracking/calculation without verifying data source authority
**Status:** APPLIED
**Action:** Watch mode needs to determine "time since last full/incr backup". This comes from scanning remote backups via `list_remote()` which returns `BackupSummary` with `timestamp: Option<DateTime<Utc>>`. No custom tracking needed -- USE EXISTING source.

## Rules Not Applicable

- RC-001/RC-004/RC-020: No Kameo actors in this project (chbackup uses plain async Rust)
- RC-002: No financial data types
- RC-005: No division operations in watch loop (interval comparison only)
- RC-007: No tuple types in watch mode
- RC-008: TDD sequencing will be validated at plan writing time
- RC-012/RC-013/RC-014: No E2E tests with shared mutable state
- RC-033/RC-034: No tokio::spawn with ActorRef captures

## Summary

14 rules reviewed. 8 rules applied with specific actions noted. 6 rules not applicable.
Key risk: RC-011 (state machine flags) is the highest-relevance rule for watch mode.
