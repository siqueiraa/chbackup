# Git Context -- Phase 4e RBAC & Config Backup

## Current State

**Branch**: master
**Working tree**: clean (no uncommitted changes)

## Recent Repository History (last 20 commits)

```
467f4978 docs: Archive completed plan 2026-02-19-02-phase4d-advanced-restore
d5e176a3 docs: Mark plan as COMPLETED
05479a25 style: apply cargo fmt to Phase 4d source files
6684cb8b docs: update tracking for Task 10 CLAUDE.md completion (10/10 PASS)
7b59c959 docs(restore): update CLAUDE.md for Phase 4d advanced restore
dba458b3 feat(restore): integrate ON CLUSTER, DatabaseReplicated, and Distributed rewrite into restore orchestrator
ee753213 feat(restore): wire rm parameter through restore() and all call sites
58d2f50e feat(restore): add pending mutation re-apply after data attachment
76ee6e37 feat(restore): add ATTACH TABLE mode for Replicated engine tables
b022a6c8 test(restore): add ZK conflict resolution and replicated engine tests
b5b22bb3 feat(restore): add Mode A DROP phase, ZK conflict resolution, and DatabaseReplicated detection
774fee97 feat(restore): add reverse DROP ordering for Mode A table drops
681c0719 feat(restore): add DDL helpers for ZK params, macros, ON CLUSTER, and Distributed cluster rewrite
424b808c feat(clickhouse): add ChClient methods for Mode A, ZK resolution, ATTACH TABLE, and mutations
f9e208c4 docs: Validate plan 2026-02-19-02-phase4d-advanced-restore (Phases 8-8.6)
efd4e10e docs: Create plan 2026-02-19-02-phase4d-advanced-restore
bf2a7ddc style: apply cargo fmt to restore mod.rs
62b45a1b docs(restore): update CLAUDE.md for Phase 4c streaming engine postponement
ec926658 feat(restore): add Phase 2b execution block for postponed tables
ceeb65f3 feat(restore): populate postponed_tables in classify_restore_tables
```

## File-Specific History (files being modified)

### src/manifest.rs
Most recent modifications were in Phase 2c (S3 object disk support) which added `S3ObjectInfo`, `RbacInfo`, `disk_remote_paths`. The manifest structure has been stable since then.

### src/main.rs
Most recent modifications were in Phase 4d which wired `rm` parameter through all call sites.

### src/config.rs
Config has been stable since Phase 3d (watch mode). All RBAC/config fields were added proactively during Phase 0.

### src/backup/mod.rs
Most recent modifications were in Phase 4b (dependency population) which added `query_table_dependencies()` call.

### src/restore/mod.rs
Most recent modifications were in Phase 4d (advanced restore) which added:
- Mode A DROP phase
- ZK conflict resolution
- DatabaseReplicated detection
- ATTACH TABLE mode
- Mutation re-apply
- ON CLUSTER DDL

### src/clickhouse/client.rs
Most recent modifications were in Phase 4d which added:
- `drop_table`, `drop_database`
- `detach_table_sync`, `attach_table`, `system_restore_replica`
- `drop_replica_from_zkpath`, `check_zk_replica_exists`
- `query_database_engine`, `execute_mutation`

### src/server/routes.rs
Most recent modifications were in Phase 4d which added `rm` to RestoreRemoteRequest.

## Branch Context

**Current branch**: master
**Main branch**: This is the main development branch (no separate `main` branch exists per `git log` output).

## Phase Progression

Phase 4e is the last planned feature phase (per roadmap):
- Phase 0 (skeleton): Complete
- Phase 1 (MVP): Complete
- Phase 2a-2d (parallelism, incremental, S3 disk, resume): Complete
- Phase 3a-3e (server, metrics, retention, watch, docker): Complete
- Phase 4a (remap): Complete
- Phase 4b (dependency-aware restore): Complete
- Phase 4c (streaming engine postponement): Complete
- Phase 4d (advanced restore): Complete
- **Phase 4e (RBAC & config backup): THIS PLAN**
- Phase 4f (operational extras): Deferred/future

## Commit Style

The project uses conventional commits:
- `feat:` for new features (with scope like `restore`, `clickhouse`, `backup`)
- `fix:` for bug fixes
- `test:` for test additions
- `docs:` for documentation
- `style:` for formatting (cargo fmt)
- `refactor:` for code restructuring

Per Phase 4d pattern, commits are organized by:
1. ChClient methods first
2. Core logic modules next
3. Integration/wiring last
4. CLAUDE.md updates at the end
5. cargo fmt as separate commit if needed
