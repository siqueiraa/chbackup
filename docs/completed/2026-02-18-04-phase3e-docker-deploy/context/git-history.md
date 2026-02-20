# Git Context and History

## Current Branch

**Branch:** `master`
**Main branch:** `main`
**Commits ahead of main:** 0 (master and main are in sync)

## Recent Repository History (last 20 commits)

```
e87552ef style: apply cargo fmt formatting
456aa2ef docs: update CLAUDE.md for watch mode (Phase 3d)
589f0749 feat(server): replace watch/reload API stubs with real implementations
d47e4391 feat(server): spawn watch loop in server mode with SIGHUP handler
6b601112 feat(server): add WatchStatus struct and watch lifecycle fields to AppState
82ae54ad docs: update acceptance.json for tasks 5-6 (F005, F006, F007 pass)
c554b796 feat(watch): wire standalone watch command in main.rs
4b3e0da6 feat(watch): implement watch state machine loop
01a9dd0b refactor(watch): remove unwrap() calls in resume_state for safety
c4cc9ff2 feat(watch): add name template resolution and resume state logic
b933771c feat(clickhouse): add get_macros() method for system.macros query
62e43dc5 refactor(config): make parse_duration_secs public and add WatchConfig.tables field
9e889020 docs: Archive completed plan 2026-02-18-03-phase3c-retention-gc
a9ed0f10 docs: Mark plan as COMPLETED
487128b9 style: apply cargo fmt formatting
cbaea0ab docs: update CLAUDE.md for Phase 3c retention/GC/clean changes
e8e2c1fb feat: wire clean command CLI dispatch and replace clean_stub API handler
4b9cf112 feat(list): add GC-safe deletion and remote retention
83433042 feat(list): add GC key collection for safe remote backup deletion
ad1aed15 feat(list): add config resolution helpers and local retention
```

## File-Specific History

### Infrastructure files (Dockerfile.test, docker-compose.test.yml, test/)

```
383ca981 test: add Docker integration test infrastructure
```

Single commit created the entire test infrastructure. No subsequent modifications.

### src/config.rs (file being modified)

The config.rs file has been modified across many phases. The env overlay function (`apply_env_overlay`) was established in Phase 0 and has been incrementally extended.

### src/server/mod.rs (referenced but not modified)

Recent changes from Phase 3d:
- `589f0749` feat(server): replace watch/reload API stubs with real implementations
- `d47e4391` feat(server): spawn watch loop in server mode with SIGHUP handler
- `6b601112` feat(server): add WatchStatus struct and watch lifecycle fields to AppState

## Phase Completion Context

The project has completed through Phase 3d (Watch Mode). Phase 3e is the final sub-phase of Phase 3 (Operations), focused entirely on deployment infrastructure:

| Phase | Status |
|-------|--------|
| Phase 0 (Skeleton) | COMPLETE |
| Phase 1 (MVP) | COMPLETE |
| Phase 2a (Parallelism) | COMPLETE |
| Phase 2b (Incremental) | COMPLETE |
| Phase 2c (S3 Object Disk) | COMPLETE |
| Phase 2d (Resume/Reliability) | COMPLETE |
| Phase 3a (API Server) | COMPLETE |
| Phase 3b (Prometheus Metrics) | COMPLETE |
| Phase 3c (Retention/GC) | COMPLETE |
| Phase 3d (Watch Mode) | COMPLETE |
| **Phase 3e (Docker/Deployment)** | **CURRENT** |

## Base Commit for Plan

**Git base:** `e87552ef` (HEAD of master, "style: apply cargo fmt formatting")

All prior phases are merged and stable. No in-flight feature branches.

## Files That Will Be Created (New)

| File | Purpose |
|------|---------|
| `Dockerfile` | Production multi-stage Docker image |
| `.github/workflows/ci.yml` | GitHub Actions CI with CH version matrix |
| `examples/kubernetes/sidecar.yaml` | K8s sidecar deployment example |
| `test/fixtures/seed_data.sql` | Deterministic test data for checksum validation |

## Files That Will Be Modified (Existing)

| File | Last Commit | Change |
|------|-------------|--------|
| `src/config.rs` | Various (Phase 0+) | Add 2 env var mappings (WATCH_INTERVAL, FULL_INTERVAL) |
| `Dockerfile.test` | `383ca981` | Ensure CI compatibility |
| `docker-compose.test.yml` | `383ca981` | Ensure CI matrix compatibility |
| `test/run_tests.sh` | `383ca981` | Add round-trip integration test |
