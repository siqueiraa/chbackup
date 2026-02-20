# Git Context

## Repository State

- **Current branch:** master
- **Main branch:** main
- **Commits ahead of main:** 0 (master and main are in sync)
- **Dirty state:** Modified root-causes.md (staged), various build artifacts and IDE files (unstaged)

## Recent Repository History (last 20 commits)

```
763422a7 docs: update CLAUDE.md for Phase 3e (docker/deploy)
b6703580 feat: add Dockerfile, CI workflow, integration tests, and K8s example
74b014ac feat(config): add WATCH_INTERVAL and FULL_INTERVAL env var overlay
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
```

## File-Specific History

### src/restore/ module
```
4300d5e4 style: apply cargo fmt formatting across all modules
00c9e186 docs: update CLAUDE.md for Phase 2d resume and reliability features
1e44ff68 feat(restore): add resume support with system.parts query
1050619b style: apply cargo fmt formatting across all modules
b7d410dd docs: update CLAUDE.md for Phase 2c S3 object disk support
f345a175 feat(restore): add UUID-isolated S3 restore with same-name optimization
633ea6db docs: update CLAUDE.md for Phase 2a parallelism changes
1d44f566 feat(restore): parallelize table restore with max_connections and engine-aware ATTACH
caa691cd docs: add CLAUDE.md for all Phase 1 modules and update root CLAUDE.md
33414790 feat(restore): implement restore module with schema creation and part attachment
```

Last restore module change: Phase 2d (resume support). No changes since Phase 3 work.

### src/cli.rs
```
bc9b2352 feat(cli): add all 15 subcommands with full flag sets from design doc
```

CLI flags for `--as` and `-m` were defined in the initial Phase 0 CLI skeleton. They have never been modified.

### src/main.rs
```
d47e4391 feat(server): spawn watch loop in server mode with SIGHUP handler
c554b796 feat(watch): wire standalone watch command in main.rs
e8e2c1fb feat: wire clean command CLI dispatch and replace clean_stub API handler
bc7fcd42 style: apply cargo fmt to main.rs, update Cargo.lock
6229910d feat(server): wire Command::Server to start_server in main.rs
6631e92f feat(cli): wire --resume and --partitions flags, implement clean_broken dispatch
bb1c0e3f feat(backup): add partition-level backup via --partitions flag
1e44ff68 feat(restore): add resume support with system.parts query
79a5d858 feat(download): add resume state, CRC64 verification, and disk space pre-flight
8d638446 feat(upload): add resume state tracking and atomic manifest upload
```

## Phase Completion Context

All phases through Phase 3e are complete:
- **Phase 0** (skeleton): Complete
- **Phase 1** (MVP): Complete
- **Phase 2a** (parallelism): Complete
- **Phase 2b** (incremental): Complete
- **Phase 2c** (S3 object disk): Complete
- **Phase 2d** (resume & reliability): Complete
- **Phase 3a** (API server): Complete (within Phase 3d/3e)
- **Phase 3b** (Prometheus metrics): Complete (within Phase 3d/3e)
- **Phase 3c** (retention/GC): Complete
- **Phase 3d** (watch mode): Complete
- **Phase 3e** (docker/deploy): Complete

Phase 4a (table/database remap) is the next planned work.

## Existing Stubs to Replace

The following stubs in `main.rs` will be replaced by Phase 4a:

1. **Line 234-235:** `rename_as.is_some()` warning for `--as` flag
2. **Line 237-239:** `database_mapping.is_some()` warning for `-m` flag
3. **Line 340-342:** `RestoreRemote` not-implemented stub

## Commit Convention

This project uses conventional commits:
- `feat:` for new features
- `fix:` for bug fixes
- `refactor:` for code restructuring
- `style:` for formatting
- `docs:` for documentation
- `test:` for tests
- `chore:` for maintenance

Examples from this codebase:
- `feat(restore): add UUID-isolated S3 restore with same-name optimization`
- `feat(server): replace watch/reload API stubs with real implementations`
- `docs: update CLAUDE.md for Phase 3e (docker/deploy)`
