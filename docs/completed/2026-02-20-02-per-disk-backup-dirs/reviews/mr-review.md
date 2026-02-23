# MR Review: Per-Disk Backup Directories

**Branch:** `claude/per-disk-backup-dirs`
**Base:** `master`
**Reviewer:** Claude (fallback, Codex unavailable)
**Date:** 2026-02-20
**Verdict:** **PASS**

---

## Summary

10 commits implementing per-disk backup directories to eliminate EXDEV cross-device hardlink fallbacks on multi-NVMe ClickHouse setups. Changes span 13 source files (`src/`) and 5 documentation files (`CLAUDE.md`). The implementation adds a centralized `resolve_shadow_part_path()` fallback chain and `per_disk_backup_dir()` helper, then wires them through all four command pipelines (create, upload, download, restore) and both delete paths (list, upload).

---

## Phase 1: Automated Verification Checks

### 1. Compilation
- `cargo check`: **PASS** (zero errors)
- `cargo clippy -- -D warnings`: **PASS** (zero warnings)

### 2. Formatting
- `cargo fmt --check`: **PASS** (no diffs)

### 3. Tests
- `cargo test`: **PASS** (522 tests, 0 failures)
- New tests added: 30+ covering all modified modules

### 4. Warnings
- Zero compiler warnings: **PASS**

### 5. Debug Markers
- `grep -rcE "DEBUG_MARKER|DEBUG_VERIFY" src/`: 0 matches: **PASS**

### 6. TODO/FIXME/HACK
- `grep -rcE "TODO|FIXME|HACK|XXX" src/`: 0 matches: **PASS**

### 7. Debug Prints
- No `dbg!()` calls: **PASS**
- All `println!`/`eprintln!` are legitimate (CLI output, SIGQUIT handler): **PASS**

### 8. Leftover Test-Only Code
- No `#[cfg(test)]` blocks in non-test files that should not be there: **PASS**

### 9. New Dependencies
- No new crate dependencies added: **PASS**

### 10. Breaking API Changes
- `collect_parts()` signature changed (added `backup_name` parameter): Internal API only, not public crate API. All call sites updated. **PASS**
- `find_part_dir()` signature changed (added 3 params): Private function, all call sites updated. **PASS**
- `find_existing_part()` signature changed (added 2 params): Private function, all call sites updated. **PASS**
- `DownloadState` struct has new `disk_map` field with `#[serde(default)]`: Backward compatible. **PASS**
- `OwnedAttachParams` has new fields (`manifest_disks`, `source_db`, `source_table`): All construction sites updated. **PASS**
- `AttachParams` has new fields: All construction sites updated. **PASS**

### 11. Backward Compatibility
- Single-disk setups: `per_disk_backup_dir(data_path, name)` produces identical path to `{data_path}/backup/{name}`: **PASS** (verified by `test_per_disk_backup_dir_default_disk`)
- Old backups with legacy layout: `resolve_shadow_part_path()` falls through to legacy encoded and plain paths: **PASS** (verified by 6 unit tests)
- Old `DownloadState` files without `disk_map` field: `#[serde(default)]` ensures clean deserialization to empty HashMap: **PASS** (verified by `test_download_disk_map_backward_compat`)

### 12. Commit Hygiene
- All 10 commits use conventional commit format: **PASS**
- No AI tool mentions in commit messages: **PASS**
- Logical commit ordering (helpers first, consumers second, docs last): **PASS**
- Formatting commit (`b3036c56`) applies only `cargo fmt` changes: **PASS**

---

## Phase 2: Design Review

### A. Architecture and Design

**Assessment: Good**

The design introduces a single source of truth (`resolve_shadow_part_path()`) for shadow path resolution and consistently applies it across all pipelines. The 4-step fallback chain (per-disk encoded -> legacy encoded -> legacy plain -> None) is well-ordered and handles backward compatibility correctly.

Key design decisions:
- Download uses disk-path existence check (creating dirs) while upload/restore use part-path existence check (reading dirs). This asymmetry is correct and well-documented.
- `DownloadState.disk_map` persisted unconditionally (not gated by resume mode) ensures `delete_local()` can always find per-disk dirs. This is a defensive design that prevents orphaned directories.
- Path deduplication via `std::fs::canonicalize()` + `HashSet` prevents double-delete when symlinks or identical paths are involved.

### B. Error Handling

**Assessment: Good**

Error handling follows established project patterns consistently:
- Per-disk directory cleanup is non-fatal (warn + continue) across all three delete paths (upload delete_local, list delete_local, backup error cleanup)
- Default backup_dir deletion remains fatal (preserving existing `?` propagation semantics)
- `resolve_shadow_part_path()` returning `None` is handled appropriately: upload returns descriptive error, restore logs warning and skips part
- `canonicalize()` failures gracefully fall back to the original path (no panic)

### C. Code Quality

**Assessment: Good**

- Functions are well-documented with doc comments explaining parameters and behavior
- The `#[allow(clippy::too_many_arguments)]` on `resolve_shadow_part_path()` is acceptable -- the function is a centralized resolver that needs all context parameters
- URL encoding is consistently applied (using existing `url_encode_path` / `url_encode_component` helpers)
- `trim_end_matches('/')` is applied consistently before constructing per-disk paths

### D. Test Coverage

**Assessment: Good**

Comprehensive test coverage for all major scenarios:
- **collect.rs**: `test_collect_parts_per_disk_staging_dir` (multi-disk staging), `test_per_disk_backup_dir_*` (2 tests), `test_resolve_shadow_part_path_*` (6 tests covering per-disk, legacy encoded, legacy plain, no-disk-in-manifest, plain-skipped-when-same, not-found)
- **upload/mod.rs**: `test_find_part_dir_per_disk`, `test_find_part_dir_fallback_default`, `test_find_part_dir_old_backup_with_manifest_disks`, `test_upload_delete_local_cleans_per_disk_dirs`
- **download/mod.rs**: `test_download_per_disk_dir_construction`, `test_download_per_disk_fallback_*` (2 tests), `test_download_disk_map_*` (2 tests), `test_find_existing_part_per_disk*` (3 tests)
- **list.rs**: `test_delete_local_cleans_per_disk_dirs`, `test_delete_local_no_manifest_uses_download_state`, `test_delete_local_no_manifest_no_state_fallback`, `test_delete_local_symlink_dedup`
- **restore/attach.rs**: `test_attach_source_dir_per_disk`, `test_attach_source_dir_remap_uses_source_names`, `test_attach_source_dir_old_backup_fallback`
- **restore/mod.rs**: `test_attach_table_mode_per_disk_shadow`
- **backup/mod.rs**: `test_create_error_cleanup_per_disk`

### E. Performance

**Assessment: Good, one minor observation**

No performance regressions expected:
- Path resolution adds at most 3 filesystem `exists()` checks per part (cheap syscalls)
- `canonicalize()` is called only during cleanup (not in hot paths)
- `HashSet` dedup is O(n) in disk count (typically 2-8 disks)
- `find_existing_part()` search adds per-disk backup dirs but uses same O(backups) scan pattern

### F. Documentation

**Assessment: Good**

- Root `CLAUDE.md` updated with comprehensive per-disk backup directories entry
- `src/backup/CLAUDE.md` updated with Per-Disk Backup Directory section, Per-Disk Error Cleanup section, updated Public API signatures, updated Backup Directory Layout
- `src/download/CLAUDE.md` updated with Per-Disk Download Target Directories and Per-Disk Disk Map Persistence sections
- `src/restore/CLAUDE.md` updated with per-disk fields in OwnedAttachParams and ATTACH TABLE mode
- `src/upload/CLAUDE.md` updated with Per-Disk Part Lookup and Per-Disk Delete Local Cleanup sections

---

## Issues Found

### Minor (informational, non-blocking)

1. **Verbose per-part logging in `collect_parts()`** (`src/backup/collect.rs:376-379`)
   - The `info!("staging per-disk backup dir")` log is inside the per-part inner loop, meaning it fires once for EVERY local disk part processed
   - For a backup with 10,000 parts across 2 disks, this generates 10,000 log lines all saying "staging per-disk backup dir"
   - Consider moving this log outside the per-part loop (e.g., log once per disk encountered) or demoting to `debug!`
   - Severity: Minor -- does not affect correctness, only log verbosity

2. **`backup_dir` suppressed as unused** (`src/backup/collect.rs:413`)
   - After switching to per-disk staging, `backup_dir` is no longer used in `collect_parts()` for local disk parts. The `let _ = backup_dir;` suppression is fine but the parameter could be removed in a future cleanup if it's only used for the legacy fallback in `resolve_shadow_part_path()` (which is not called from `collect_parts()`).
   - Severity: Minor -- cosmetic, no functional impact

3. **Formatting-only changes in unrelated files** (`src/clickhouse/client.rs`, `src/storage/s3.rs`, `src/download/stream.rs`)
   - Commit `b3036c56` applies `cargo fmt` across all modified files, which includes reformatting lines that were not functionally changed by this plan
   - This is acceptable (the commit is clearly labeled `chore: apply cargo fmt`) but slightly inflates the diff
   - Severity: Minor -- cosmetic

---

## Checks Summary

| # | Check | Result |
|---|-------|--------|
| 1 | Compilation | PASS |
| 2 | Formatting | PASS |
| 3 | Tests (522) | PASS |
| 4 | Zero warnings | PASS |
| 5 | No debug markers | PASS |
| 6 | No TODO/FIXME | PASS |
| 7 | No debug prints | PASS |
| 8 | No test-only code leaks | PASS |
| 9 | No new dependencies | PASS |
| 10 | API compatibility | PASS |
| 11 | Backward compatibility | PASS |
| 12 | Commit hygiene | PASS |
| 13 | Architecture | PASS |
| 14 | Error handling | PASS |
| 15 | Code quality | PASS |
| 16 | Test coverage | PASS |
| 17 | Performance | PASS |
| 18 | Documentation | PASS |

**Critical issues:** 0
**Important issues:** 0
**Minor issues:** 3 (informational, non-blocking)

---

## Verdict: **PASS**

All 18 checks pass. The implementation is correct, well-tested, backward compatible, and properly documented. The three minor issues noted are informational and do not require changes before merge.
