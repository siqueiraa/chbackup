# Handoff: Phase 0 — Skeleton

## What This Plan Does

Creates the chbackup project skeleton: Cargo workspace, CLI with all 15 commands from the design doc, full config system (~40 params with env overlay), logging, PID lock, and connection wrappers for ClickHouse and S3.

## Key Design Decisions

1. **All commands defined upfront** — Even though only `default-config`, `print-config`, and `list` do anything in Phase 0, all 15 commands are defined with their full flag sets so that Phase 1+ can fill in implementations without touching CLI parsing.

2. **Config is complete** — All ~40 params from §12 are defined with defaults. This avoids incremental config additions later.

3. **Connection wrappers are thin** — `ChClient` and `S3Client` are minimal wrappers that will grow in Phase 1. They only do ping/connectivity checks for now.

4. **No mocks** — Config tests use real YAML. ClickHouse/S3 tests require real connections (deferred to integration testing).

## Design Doc References

| Task | Design Section |
|------|---------------|
| CLI | §2 Commands, flag reference table |
| Config | §12 Configuration (~40 params) |
| ClickHouse client | §11.3 Rust implementation notes |
| S3 client | §11.3 Rust implementation notes |
| PID lock | §2 lock paragraph |
| Logging | §11.4 Logging & Progress Reporting |

## Prerequisites

- Rust toolchain installed
- No external services needed for Phase 0 (ClickHouse/S3 only for integration tests)

## Commit Log

| Task | Commit | Description |
|------|--------|-------------|
| T5 | 48929e8 | feat(logging): add init_logging with text/JSON mode selection |
| T6 | a3ae11d | feat(lock): add PID lock with three-tier scope (backup/global/none) |
| T7 | d5949b6 | feat(clickhouse): add ChClient wrapper with config-driven setup and ping |
| T8 | 5cc69c9 | feat(storage): add S3Client wrapper with config-driven setup and ping |
| T9 | b70c455 | feat: add config.example.yml and wire full command flow |

## Resume Instructions

1. Read SESSION.md for current task status
2. Read PLAN.md for task details
3. Check `cargo check` for current compilation state
4. Continue from the first `pending` group
