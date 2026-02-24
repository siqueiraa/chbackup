# Diagnostics Report (Phase 1 -- MCP/LSP Verified)

## Compiler State

**Date:** 2026-02-22
**Branch:** master
**Verification method:** `cargo check`, `cargo clippy`, LSP `findReferences`, grep cross-validation

### cargo check (default)

- **Errors:** 0
- **Warnings:** 0
- **Result:** Clean compilation

### cargo clippy

- **Errors:** 0
- **Warnings:** 0
- **Result:** Clean

### cargo check with RUSTFLAGS="-F dead_code"

- **Errors:** 2 (incompatible `#[allow(dead_code)]` vs forbid)
- Confirms exactly 2 `#[allow(dead_code)]` annotations exist in the codebase:
  1. `src/clickhouse/client.rs:23` -- `ChClient.debug` field
  2. `src/restore/attach.rs:486` -- `attach_parts()` function

### Dead Code Hidden by `pub` Visibility

Rust does not warn about unused `pub` items in binary crates. The following `pub` methods/fields have zero external callers (verified via LSP `findReferences` returning 0 results and grep returning 0 matches):

1. **`ChClient::inner()`** -- `src/clickhouse/client.rs:252` -- LSP: 0 references
2. **`S3Client::inner()`** -- `src/storage/s3.rs:220` -- LSP: 0 references
3. **`S3Client::concurrency()`** -- `src/storage/s3.rs:235` -- LSP: 0 references
4. **`S3Client::object_disk_path()`** -- `src/storage/s3.rs:243` -- LSP: 0 references
5. **`S3Client.concurrency`** field -- `src/storage/s3.rs:58` -- LSP: 4 refs, all in s3.rs (decl, constructor, dead getter, test helper)
6. **`S3Client.object_disk_path`** field -- `src/storage/s3.rs:60` -- LSP: 4 refs, all in s3.rs (decl, constructor, dead getter, test helper)

### Test-Only Methods (Not Dead -- Intentional Test Helpers)

These methods are `pub` but only referenced from `#[cfg(test)]` blocks:

1. **`ProgressTracker::disabled()`** -- `src/progress.rs:48` -- LSP: 2 refs in progress.rs tests
2. **`ProgressTracker::is_active()`** -- `src/progress.rs:67` -- LSP: 4 refs in progress.rs tests (`.is_active()` calls in `restore/` are on `RemapConfig`, not `ProgressTracker`)
3. **`ActionLog::running()`** -- `src/server/actions.rs:118` -- grep: 7 callers, all in `#[cfg(test)]` blocks (actions.rs:183-191, state.rs:678-695)

### Dead Prometheus Metrics

Two counters are registered but never incremented:

1. **`chbackup_parts_uploaded_total`** -- `src/server/metrics.rs:32` -- grep: 0 `.inc()` calls
2. **`chbackup_parts_skipped_incremental_total`** -- `src/server/metrics.rs:35` -- grep: 0 `.inc()` calls

Both are registered in the Prometheus registry and exposed via `/metrics` but always report 0.

### Unused ChBackupError Variants (Not Recommended for Removal)

| Variant | Constructed in prod? | Matched in exit_code()? | Notes |
|---------|---------------------|------------------------|-------|
| `ClickHouseError(String)` | NO | NO (falls to `_ => 1`) | Test-only |
| `S3Error(String)` | NO | NO | Test-only |
| `ConfigError(String)` | NO | NO | Test-only |
| `BackupError(String)` | NO | YES (code 3 if "not found") | Matched but never constructed |
| `RestoreError(String)` | NO | NO | Test-only |
| `ManifestError(String)` | NO | YES (code 3 if "not found") | Matched but never constructed |
| `LockError(String)` | YES (lock.rs, 4 sites) | YES (code 4) | Active |
| `IoError(io::Error)` | YES (via #[from]) | NO | Active via From trait |

These serve as a domain error taxonomy and their exit_code() arms are deliberately designed. Not recommended for removal.

### ListParams::format Field

`src/server/routes.rs:74` -- `pub format: Option<String>` is deserialized from query params but never read in production code (only in one test assertion). Comment documents this as intentional: "stored for integration table DDL compatibility." Not recommended for removal.

### unreachable_pub Warnings (Out of Scope)

4 warnings in `src/cli.rs` suggesting `pub` -> `pub(crate)` for CLI types. These are not dead code; they are cosmetic visibility suggestions. Out of scope for this plan.
