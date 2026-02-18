# MR Review: Phase 2b Incremental Backups

**Branch:** `feat/phase2b-incremental`
**Base:** `master`
**Reviewer:** execute-reviewer (Claude)
**Date:** 2026-02-17
**Verdict:** **PASS**

---

## Summary

This branch implements Phase 2b -- Incremental Backups with `--diff-from` and `--diff-from-remote` support, plus the `create_remote` compound command. The implementation is clean, well-tested, and follows existing codebase patterns faithfully. The core diff logic is a pure function with comprehensive unit tests. The integration into `create()`, `upload()`, and `create_remote` is minimal and correct.

**Files changed:** 6 files, +613 / -23 lines
**Commits:** 5 (conventional commit format, logical progression)

---

## Phase 1: Automated Verification Checks

### Check 1: Compilation
- **Status:** PASS
- `cargo build` succeeds with zero errors

### Check 2: Tests
- **Status:** PASS
- 120 library tests + 5 integration tests = 125 total, all passing

### Check 3: Clippy
- **Status:** PASS
- Zero warnings

### Check 4: Debug Markers
- **Status:** PASS
- Zero `DEBUG_MARKER` or `DEBUG_VERIFY` markers found in `src/`

### Check 5: Commit Messages
- **Status:** PASS
- All 5 commits use conventional format:
  - `feat(backup): add diff_parts() for incremental backup comparison`
  - `feat(backup): integrate --diff-from into create() for incremental backups`
  - `feat(upload): integrate --diff-from-remote into upload() for incremental uploads`
  - `feat: implement create_remote command and wire --diff-from/--diff-from-remote`
  - `docs: update CLAUDE.md for Phase 2b incremental backup changes`
- Logical progression from core module to integration to docs

### Check 6: No Secrets or Credentials
- **Status:** PASS
- No `.env`, credentials, API keys, or sensitive data in diff

### Check 7: No AI Tool References
- **Status:** PASS
- No mentions of Claude, AI, or similar in commits or code comments

### Check 8: Branch Hygiene
- **Status:** PASS
- 5 clean commits, no merge commits, no fixup commits

### Check 9: File Scope
- **Status:** PASS
- All changed files are directly relevant to the plan scope (backup/diff, backup/mod, upload/mod, main, CLAUDE.md docs)

### Check 10: No Unrelated Changes
- **Status:** PASS
- Warning message wording updated from "not implemented in Phase 1" to "not yet implemented" in the create/upload handlers is a reasonable cleanup within scope

### Check 11: Test Coverage for New Code
- **Status:** PASS
- `diff.rs`: 6 unit tests covering all key scenarios:
  - Empty base (all parts uploaded)
  - Full match (all parts carried)
  - Partial match (mix of carried and uploaded)
  - CRC64 mismatch (re-upload despite same name)
  - Multi-disk within same table
  - Extra table in base (gracefully ignored)

### Check 12: Documentation Updated
- **Status:** PASS
- `src/backup/CLAUDE.md` updated with diff pattern description and updated public API
- `src/upload/CLAUDE.md` updated with incremental upload section and updated public API

---

## Phase 2: Design Review

### Area 1: Code Correctness

**Status:** PASS

**diff_parts() logic (diff.rs:29-91):**
- HashMap lookup by `(table_key, disk_name, part_name)` is the correct composite key per design doc section 3.5
- CRC64 comparison is done correctly -- matching name + CRC64 = carried, matching name + different CRC64 = re-upload with warning
- Mutation of `current` manifest in-place is appropriate since the caller owns the manifest
- `DiffResult` counters are correctly tallied

**create() integration (backup/mod.rs:397-413):**
- Diff is applied at step 13b (after manifest construction, before save) -- correct position in the pipeline
- Base manifest loaded from local disk path, consistent with how local backups are stored
- Error context is clear: "Failed to load base backup 'X' for --diff-from"

**upload() integration (upload/mod.rs:130-160):**
- Remote base manifest fetched from S3 via `get_object` and deserialized with `from_json_bytes` -- both methods pre-exist in the codebase
- Manifest re-saved locally after diff applied -- ensures carried part metadata persists
- Carried parts skipped in work queue (line 197): `part.source.starts_with("carried:")` is the correct check

**Merge logic (upload/mod.rs:381-399):**
- After parallel upload completes, carried parts are preserved from the original manifest while uploaded parts get their new S3 keys
- The filter-carried + extend-uploaded pattern correctly reconstructs the full part list per disk

**create_remote (main.rs:283-346):**
- Correctly composes create() + upload() with `diff_from: None` and `diff_from_remote` passed to upload
- Per design doc, `create_remote` does NOT use `--diff-from` (local), only `--diff-from-remote` -- this is correct

### Area 2: Error Handling

**Status:** PASS

- Base manifest not found: Properly propagated via `?` with `.with_context()` giving clear error messages
- S3 base manifest download failure: Propagated with context
- S3 base manifest parse failure: Propagated with context
- All error paths use `anyhow::Result` + `.context()` consistent with codebase patterns

### Area 3: Pattern Consistency

**Status:** PASS with minor note

- `diff.rs` follows the pattern of other pure-function modules (like `checksum.rs`) -- no side effects, clear inputs/outputs
- Logging uses `tracing::{info, warn, debug}` consistently
- HashMap usage for the lookup table is idiomatic Rust
- `spawn_blocking` not needed since `diff_parts()` is in-memory computation (no I/O)
- Function signature `(&mut BackupManifest, &BackupManifest) -> DiffResult` clearly communicates intent

**Minor note:** Warning messages in `Create` and `Upload` handlers were updated from "not implemented in Phase 1" to "not yet implemented", but `Download`, `Restore`, and other handlers still use the old phrasing. This is cosmetic and does not affect functionality.

### Area 4: Security

**Status:** PASS

- No path traversal risks: base backup name is joined to a well-known path
- No user-controlled format strings
- No credential exposure

### Area 5: Performance

**Status:** PASS

- HashMap lookup for diff comparison is O(1) per part -- efficient even for large manifests
- No unnecessary cloning of large data structures (only `backup_key` strings are cloned for carried parts)
- Carried parts correctly skip compression + upload, which is the entire point of incremental
- `compressed_size` in the final manifest only counts actually uploaded bytes (carried parts excluded from `total_compressed_size`) -- correct behavior

### Area 6: Design Doc Compliance

**Status:** PASS

Checked against design doc section 3.5:

| Design Requirement | Implementation | Status |
|---|---|---|
| Load previous backup manifest | `BackupManifest::load_from_file` (local) / `from_json_bytes` (remote) | PASS |
| Compare by part name + CRC64 | `base_lookup.get(table, disk, name)` then `checksum_crc64 ==` | PASS |
| Carry forward with previous S3 key | `part.backup_key = base_part.backup_key.clone()` | PASS |
| Mark source as "carried:base_name" | `part.source = format!("carried:{}", base_name)` | PASS |
| Re-upload on CRC64 mismatch | Part stays `source = "uploaded"`, warn log emitted | PASS |
| Self-contained manifest (no chain) | All parts listed with S3 keys, no RequiredBackup pointer | PASS |
| `create_remote` uses `--diff-from-remote` not `--diff-from` | Task 4 passes `diff_from: None` to create(), `diff_from_remote` to upload() | PASS |

---

## Issues Found

### Critical
None.

### Important
None.

### Minor

1. **Inconsistent warning phrasing in main.rs:** The `Create` and `Upload` handlers use "not yet implemented" while `Download`, `Restore`, and other handlers still use "not implemented in Phase 1". This is cosmetic and pre-existing (only the touched handlers were updated). Not blocking.

2. **Self-referencing diff-from not guarded:** If a user passes `--diff-from=<same-backup-name>`, the code would attempt to load a manifest that may not exist yet (since it is being created). The `load_from_file` call would fail with a file-not-found error and propagate a reasonably clear error message, so this is a user error scenario that fails safely. Not blocking.

---

## Verdict

**PASS**

The implementation is correct, well-tested, follows design doc specifications, and is consistent with codebase patterns. No critical or important issues found. The two minor items do not warrant blocking the merge.
