# MR Review: Phase 4d Advanced Restore

**Branch:** `feat/phase4d-advanced-restore`
**Base:** `master`
**Reviewer:** Claude (execute-reviewer agent)
**Date:** 2026-02-19

---

## Verdict: **PASS**

---

## Phase 1: Automated Verification (12 checks)

| # | Check | Status | Details |
|---|-------|--------|---------|
| 1 | Compilation | PASS | `cargo check` succeeds with zero errors |
| 2 | Clippy | PASS | `cargo clippy --all-targets` produces zero warnings |
| 3 | Tests | PASS | 406 unit tests pass, 6 integration config tests pass, 0 failures |
| 4 | Debug markers | PASS | Zero `DEBUG_MARKER` or `DEBUG_VERIFY` strings in `src/` |
| 5 | Conventional commits | PASS | All 11 commits follow `type(scope): message` format |
| 6 | No AI mentions | PASS | No references to Claude/ChatGPT/Copilot in code or commits (CLAUDE.md is a project convention, not AI reference) |
| 7 | Zero warnings | PASS | Compilation and clippy both produce zero warnings |
| 8 | Diff stats | PASS | +2061 / -112 lines across 15 files; reasonable scope for 10 tasks |
| 9 | New file count | PASS | 1 new file (`topo.rs` additions, `remap.rs` additions); all changes within existing module structure |
| 10 | Feature flags | PASS | No new feature flags required; config fields already exist |
| 11 | Test coverage | PASS | 80+ new test assertions across 4 test modules (client.rs, mod.rs, remap.rs, schema.rs, topo.rs) |
| 12 | Config validation | PASS | 3 new config fields have defaults, env overlay support, and are properly wired |

## Phase 2: Design Review (6 areas)

### 1. Architecture & Design Consistency

**Status:** PASS

- All new ChClient methods follow the existing `log_and_execute()` pattern used by the rest of the client
- SQL helper functions are extracted as standalone `pub fn` for testing (matching `freeze_sql`, `unfreeze_sql` pattern)
- Mode A DROP follows the retry-loop pattern from `create_ddl_objects()` (max 10 rounds)
- ZK conflict resolution is non-fatal by design (warn + continue), consistent with the resilient approach throughout the codebase
- ATTACH TABLE mode has proper fallback to per-part ATTACH on failure
- Mutation re-apply is non-fatal (warn + continue per design 5.7)
- All new remap functions are pure (no async, no I/O) for easy unit testing, consistent with existing remap.rs pattern

### 2. API Contract & Backward Compatibility

**Status:** PASS

- `restore()` function signature gains 1 new parameter (`rm: bool`) inserted at the correct position
- All 5 call sites updated correctly:
  - `main.rs` restore command: passes `rm` from CLI
  - `main.rs` restore_remote command: passes `rm` from CLI
  - `routes.rs` restore_backup handler: `req.rm.unwrap_or(false)`
  - `routes.rs` restore_remote handler: `req.rm.unwrap_or(false)`
  - `state.rs` auto_resume: `false` (correct -- auto-resume should never drop)
- `RestoreRemoteRequest` gains `rm: Option<bool>` with `#[serde(default)]` for backward compatibility
- Schema creation functions gain `on_cluster`, `replicated_databases`, `macros`, `dist_cluster` parameters -- these are internal APIs only, no external contract break
- `--rm` warning removed from main.rs (was "not yet implemented, ignoring")

### 3. Error Handling & Robustness

**Status:** PASS

- `check_zk_replica_exists()` returns `Ok(false)` on query error (system.zookeeper may be unavailable)
- `query_database_engine()` returns empty string on error (database may not exist)
- `resolve_zk_conflict()` catches and warns on all failures, never aborts
- `drop_databases()` catches and warns on individual database DROP failures
- `reapply_pending_mutations()` catches and warns on individual mutation failures
- `try_attach_table_mode()` returns `Err` which the caller catches and falls back to normal ATTACH
- SYSTEM_DATABASES constant prevents accidental DROP of `system`, `information_schema`, `INFORMATION_SCHEMA`
- Mode A DROP retry loop has the same progress check as `create_ddl_objects()` (bail if zero progress after round 0)

### 4. SQL Safety

**Status:** PASS (minor observation)

- All SQL helpers use backtick-escaped identifiers for db/table names (consistent with existing codebase)
- `drop_table_sql`, `drop_database_sql` use `IF EXISTS` safety
- `check_zk_replica_exists` uses `format!` with string interpolation for ZK path and replica name -- these values come from DDL stored in the manifest (not direct user input) and are ZK paths/replica identifiers. This matches the pattern used in other system table queries. Not a blocking concern.
- `execute_mutation_sql` passes the mutation command verbatim from the manifest -- this is by design since the command comes from `system.mutations` during backup and must be replayed exactly
- `SYSTEM DROP REPLICA` uses single-quoted parameters (ClickHouse convention)

### 5. Concurrency & Performance

**Status:** PASS

- ATTACH TABLE mode runs sequentially per Replicated table (correct -- DETACH/ATTACH is table-level operation)
- Non-Replicated tables still use parallel per-part ATTACH through the existing semaphore
- `get_macros()` called once at restore start, result passed down (no N+1)
- `detect_replicated_databases()` called once, result cached in `HashSet` (no repeated queries)
- Mutation re-apply is sequential per-table with `mutations_sync=2` (waits for completion) -- correct per design
- DROP retry loop is bounded to 10 rounds maximum

### 6. Documentation & Code Quality

**Status:** PASS

- All 4 module CLAUDE.md files updated with comprehensive documentation of new APIs
- Root CLAUDE.md updated: data flow, removed Mode A from limitations
- Server CLAUDE.md updated: `rm` field on `RestoreRemoteRequest`
- Doc comments on all new public functions with SQL examples
- `#[allow(clippy::too_many_arguments)]` used where needed (create_tables with 9 params, try_attach_table_mode with 13 params)
- Module-level doc comment in mod.rs updated with full phase listing and cross-cutting features

---

## Issues Found

### Critical: 0
### Important: 0
### Minor: 1

1. **[Minor]** `try_attach_table_mode` has 13 parameters. While `#[allow(clippy::too_many_arguments)]` is applied, this function could benefit from a params struct in a future refactoring pass. Not blocking for this MR.

---

## Summary

The MR implements Phase 4d Advanced Restore cleanly across 11 commits, adding Mode A destructive restore (--rm), ZK conflict resolution, ATTACH TABLE mode for Replicated engines, mutation re-apply, ON CLUSTER DDL, DatabaseReplicated detection, and Distributed cluster rewriting. All changes follow established codebase patterns, maintain backward compatibility, and have comprehensive test coverage. Zero compilation warnings, zero clippy warnings, 406 tests passing.
