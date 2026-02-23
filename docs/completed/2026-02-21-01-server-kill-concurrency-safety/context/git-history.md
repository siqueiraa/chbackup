# Git Context

## Current Branch

- **Branch:** master
- **Commits ahead of main:** 0 (master = main)

## Recent Repository History (last 25 commits)

```
e5b07058 chore: apply cargo fmt to routes.rs
665309ba docs(server): update CLAUDE.md for API gap fixes
10472448 feat(list): add object_disk_size and required fields to BackupSummary
45855134 feat(server): add offset/limit/format params to list endpoint
90345ecd docs: fix design.md parts_to_do type and required_backups reference
ca9f7e22 feat(server): add SIGTERM handler for graceful shutdown
3351149d fix(server): wire post_actions to dispatch actual commands
44af3ad1 docs: fix CLAUDE.md param counts and phantom references
3a6e946d docs: update CLAUDE.md files for per-disk backup directory changes
b3036c56 chore: apply cargo fmt across all modified files
8ff5f120 feat(restore): update ATTACH TABLE mode for per-disk shadow paths
507d9a61 feat(download): update find_existing_part() for per-disk search
dd386046 feat(upload): update upload delete_local to clean per-disk dirs
8fb18c0c feat(upload): update find_part_dir() to use resolve_shadow_part_path()
2c4dca91 feat(backup): use per-disk staging dirs in collect_parts()
402c8b4d feat(download): write parts to per-disk backup directories
80eb80cc feat(backup): add resolve_shadow_part_path() helper with 4-step fallback chain
452389c0 feat(list): update delete_local() to clean per-disk backup directories
afa01dab docs: Archive completed plan 2026-02-20-01-phase8-polish-performance
6244ddf5 docs: Mark plan as COMPLETED
fbf32916 docs: update CLAUDE.md files for Phase 8 changes
a5de6b80 feat(server): wire ManifestCache into AppState with TTL and invalidation
52fb1f46 feat(upload): wire streaming multipart upload for large parts
7307bb05 feat(list): implement ManifestCache with TTL-based expiry
f707ad33 feat(server): wire rbac_size and config_size through to ListResponse
```

## File-Specific History

### src/server/state.rs, src/server/routes.rs, src/server/mod.rs

```
e5b07058 chore: apply cargo fmt to routes.rs
10472448 feat(list): add object_disk_size and required fields to BackupSummary
45855134 feat(server): add offset/limit/format params to list endpoint
ca9f7e22 feat(server): add SIGTERM handler for graceful shutdown
3351149d fix(server): wire post_actions to dispatch actual commands
a5de6b80 feat(server): wire ManifestCache into AppState with TTL and invalidation
f707ad33 feat(server): wire rbac_size and config_size through to ListResponse
16773248 feat(server): add SIGQUIT handler for stack dump debugging
226e068f feat(server): add offset/limit pagination to tables endpoint
ee18f3e6 feat(server): exit process when watch loop ends and watch_is_main_process is set
```

### src/lock.rs

```
880a6408 chore: apply rustfmt formatting fixes
a3ae11d7 feat(lock): add PID lock with three-tier scope (backup/global/none)
```

**Note:** lock.rs has only been modified twice since creation -- once for the initial implementation and once for formatting. No functional changes since Phase 0.

## Relevant Design Doc Sections

The plan touches these design doc sections (verified from docs/design.md):
- Section 2: CLI commands and flags (lock scope mapping)
- Section 3.6: Upload pipeline (retention after upload -- step 7)
- Section 9: API server endpoints (kill, actions, reload, restart)
- Section 10.8: Config hot-reload semantics
- Section 16.1: Resume state graceful degradation

## Integration Test Coverage

Current integration tests in `test/run_tests.sh`:
- T1: Setup fixtures (3 tables)
- T2: Seed data (row counts)
- T3: Round-trip (create -> upload -> delete local -> download -> restore -> verify)

Missing test coverage relevant to this plan:
- T4: Server mode API endpoints (create, upload, download via HTTP)
- T5: Kill endpoint (requires server mode)
- T6: Concurrent operations (allow_parallel=true)
- T7: Backup name validation (path traversal attempts)
- T8: Config reload (server mode)
- T9-T28: Various API endpoints and edge cases

## Commit Convention

Repository uses conventional commits: `feat:`, `fix:`, `refactor:`, `test:`, `docs:`, `chore:`
with module scope prefixes: `feat(server):`, `fix(lock):`, etc.

## Working Directory State

Modified files in working tree (excluding target/):
- `.claude/skills/self-healing/references/root-causes.md` (staged, modified)
- No other source file modifications

No untracked source files except `.gitignore`.
