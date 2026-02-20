# Diagnostics Report

## Compiler State

**Command:** `cargo check`
**Result:** Clean build -- zero errors, zero warnings

```
Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.77s
```

## Current Codebase Health

| Check | Result |
|-------|--------|
| `cargo check` | PASS (0 errors, 0 warnings) |
| Existing SIGQUIT handler | NONE -- no `SignalKind::quit` usage anywhere in codebase |
| Existing rbac_size/config_size data flow | PARTIAL -- `ListResponse` has fields (hardcoded 0), `BackupManifest` and `BackupSummary` lack fields |
| Existing manifest caching | NONE -- every `list_remote()` call downloads all manifests |
| Existing tables pagination | NONE -- `TablesParams` lacks offset/limit fields |
| Existing streaming upload | NONE -- all parts buffered into `Vec<u8>` before upload |

## Known TODOs in Code

Found via grep for TODO comments in affected files:

1. `src/server/routes.rs:326` -- `rbac_size: 0, // TODO: requires scanning access/ directory sizes`
2. `src/server/routes.rs:327` -- `config_size: 0, // TODO: requires adding config_size to BackupManifest`

## Pre-Existing Conditions

No pre-existing compiler errors or warnings. The codebase is in a clean state following Phase 7 completion (commit c11d7794).

## Dependency Versions (Relevant)

| Crate | Version | Feature |
|-------|---------|---------|
| `tokio` | 1 | `full` (includes `signal::unix` with `SignalKind::quit()`) |
| `tokio-util` | 0.7 | `codec` |
| `aws-sdk-s3` | 1 | S3 multipart API |
| `walkdir` | 2 | Directory traversal for `dir_size()` |
| `lz4_flex` | 0.11 | LZ4 compression |
| `axum` | 0.7 | HTTP API server |

## Signal Handling Inventory

| Signal | Server (mod.rs) | Standalone Watch (main.rs) |
|--------|----------------|---------------------------|
| SIGINT/Ctrl+C | Yes (line 281) | Yes (line 577) |
| SIGHUP | Yes (line 211-224) | Yes (line 584-598) |
| SIGQUIT | **NOT HANDLED** | **NOT HANDLED** |
| SIGTERM | Implied via tokio (exit 143) | Implied via tokio (exit 143) |
