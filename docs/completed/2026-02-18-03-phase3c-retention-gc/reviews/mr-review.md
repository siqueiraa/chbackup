# MR Review: Phase 3c -- Retention / GC

**Branch:** `phase3c-retention-gc`
**Reviewer:** Claude (Codex unavailable)
**Date:** 2026-02-18
**Verdict:** **PASS**

---

## Phase 1: Automated Verification Checks

| # | Check | Status | Notes |
|---|-------|--------|-------|
| 1 | Compilation (`cargo check`) | PASS | Zero errors |
| 2 | Clippy (`cargo clippy`) | PASS | Zero warnings |
| 3 | Unit tests (`cargo test --lib`) | PASS | 284 tests passed, 0 failed |
| 4 | Debug markers (DEBUG_MARKER/DEBUG_VERIFY) | PASS | Zero markers found |
| 5 | `dbg!()` macros | PASS | None found |
| 6 | `todo!()` / `unimplemented!()` macros | PASS | None found |
| 7 | Conventional commits | PASS | All 5 commits follow `feat:` / `docs:` conventions |
| 8 | No AI mentions in commits | PASS | No "Claude", "AI", "GPT" in commit messages |
| 9 | Files changed match plan scope | PASS | 6 files: list.rs, main.rs, routes.rs, mod.rs, CLAUDE.md, server/CLAUDE.md |
| 10 | No unrelated changes | PASS | All changes directly related to Phase 3c retention/GC |
| 11 | Documentation updated | PASS | CLAUDE.md and server/CLAUDE.md updated with new patterns |
| 12 | Test coverage for new code | PASS | 13 new tests covering retention, GC, shadow cleanup |

---

## Phase 2: Design Review

### 2.1 Correctness vs Design Doc

**Design Section 8.1 (Local Retention):** PASS
- `retention_local()` correctly filters broken backups, sorts by timestamp, deletes oldest exceeding keep count
- `keep=0` returns unlimited (no action), `keep=-1` defers to upload module -- matches design

**Design Section 8.2 (Remote Retention with Safe GC):** PASS
- `gc_collect_referenced_keys()` loads ALL surviving manifests and builds union of referenced keys
- `gc_delete_backup()` correctly partitions keys into manifest vs data, filters unreferenced, deletes data first then manifest last
- `retention_remote()` calls `gc_collect_referenced_keys()` fresh per backup deletion -- satisfies design 8.2 step 3c race protection
- Manifest deleted last ensures backup becomes "broken" before being fully removed

**Design Section 8.3 (Config Resolution):** PASS
- `effective_retention_local/remote()` correctly implements: retention.* overrides general.* when non-zero

**Design Section 13 (Clean Command):** PASS
- `clean_shadow()` queries `get_disks()`, filters backup-type disks, removes `chbackup_*` prefix dirs
- Name filter uses `sanitize_name()` for proper matching
- Data path fallback check handles case where default disk is not in system.disks

### 2.2 Error Handling

| Pattern | Status | Notes |
|---------|--------|-------|
| Per-item error handling | PASS | Individual deletion failures logged as warnings, not fatal (matches clean_broken pattern) |
| GC key collection failure | PASS | Skips backup with warning, continues to next |
| Shadow dir removal failure | PASS | Logs warning, continues to next directory |
| Manifest parse failure in GC | PASS | Warns and skips, does not abort retention loop |

### 2.3 Concurrency and Safety

| Check | Status | Notes |
|-------|--------|-------|
| spawn_blocking for sync I/O | PASS | `clean_shadow_dir()` runs via `spawn_blocking` |
| Owned data across spawn boundaries | PASS | `disk_path.clone()` and `name.map(String)` for owned values |
| API handler follows operation pattern | PASS | try_start_op -> spawn -> finish_op/fail_op lifecycle |
| Metrics instrumented | PASS | Duration, success, error all recorded for "clean" label |

### 2.4 Code Quality

| Check | Status | Notes |
|-------|--------|-------|
| Documentation (doc comments) | PASS | All public functions have comprehensive doc comments |
| Naming conventions | PASS | Follows existing `clean_broken_*` / `retention_*` naming |
| Pattern consistency | PASS | All new functions follow list->filter->sort->delete->count pattern |
| Import organization | PASS | New imports properly ordered (std, external crates, internal) |
| No unnecessary allocations | PASS | Keys collected into HashSet (dedup), Vec used appropriately |

### 2.5 Test Quality

| Test | Coverage |
|------|----------|
| `test_effective_retention_local` | Config override and fallback for both local and remote |
| `test_retention_local_deletes_oldest` | 5 backups, keep 3, verify 2 oldest removed |
| `test_retention_local_skips_broken` | Broken backup excluded from retention counting |
| `test_retention_local_zero_means_unlimited` | keep=0 and keep=-1 both do nothing |
| `test_collect_referenced_keys_from_manifest` | Local parts, S3 disk parts, S3 objects all collected |
| `test_collect_keys_from_empty_manifest` | Edge case: empty manifest produces no keys |
| `test_gc_filter_unreferenced_keys` | GC filtering logic with referenced/unreferenced partitioning |
| `test_clean_shadow_removes_chbackup_dirs` | Removes chbackup_* dirs, preserves others |
| `test_clean_shadow_with_name_filter` | Name-based filtering with sanitize_name |
| `test_clean_shadow_no_shadow_dir` | Graceful handling when no shadow dir exists |
| `test_clean_shadow_empty_shadow_dir` | Empty shadow dir returns 0 |

**Note:** `gc_collect_referenced_keys()`, `gc_delete_backup()`, and `retention_remote()` are integration-test-only (require real S3). This is documented and expected.

### 2.6 Security

| Check | Status | Notes |
|-------|--------|-------|
| No credentials in code | PASS | No hardcoded secrets |
| Path traversal | PASS | Shadow cleanup only matches `chbackup_*` prefix, no user-controlled paths |
| S3 key injection | PASS | Keys come from manifest parsing and S3 listing, not user input |

---

## Issues Found

### Critical
None

### Important
None

### Minor
None

---

## Summary

The implementation is clean, well-tested, and faithfully follows the design doc. All 7 tasks are complete with 13 new unit tests. The code follows existing patterns (clean_broken_local/remote) consistently. The GC implementation correctly handles the design 8.2 race protection by collecting referenced keys fresh per deletion. The clean command properly filters backup-type disks and uses sanitize_name for name matching.

**Total lines added:** ~918 (849 in list.rs, 69 in routes/main/docs)
**Test coverage:** 13 new unit tests + existing 271 tests all passing (284 total)
