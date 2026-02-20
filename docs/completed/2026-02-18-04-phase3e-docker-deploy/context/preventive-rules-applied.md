# Preventive Rules Applied

## Rules Checked

| Rule ID | Rule Title | Applicable? | Action Taken |
|---------|-----------|-------------|--------------|
| RC-001 | Incomplete actor dependency wiring | NO | Phase 3e is infrastructure-only (Docker, CI, K8s manifests). No actors involved. |
| RC-002 | Schema/type mismatch trusting comments | NO | No Rust code types used in Docker/CI files. |
| RC-003 | Tracking files not updated after implementation | YES | Will ensure all plan files are created. |
| RC-004 | Message handler without sender | NO | No actor messages in this phase. |
| RC-005 | Zero/null division not handled | NO | No calculation code. |
| RC-006 | Plan code snippets use unverified APIs | PARTIAL | Verified CLI flags (`server --watch`) exist in `src/cli.rs`. Verified config structure in `src/config.rs`. |
| RC-007 | Tuple/struct field order assumed | NO | No tuple types involved. |
| RC-008 | TDD task sequencing violation | NO | No TDD tasks -- infrastructure files only. |
| RC-011 | State machine flags missing exit path | NO | No state machines. |
| RC-015 | Cross-task return type mismatch | NO | No data flow between tasks -- tasks produce independent files. |
| RC-016 | Struct field completeness | NO | No struct definitions. |
| RC-017 | State field declaration missing | NO | No state fields. |
| RC-018 | TDD task missing explicit test steps | PARTIAL | Test steps are "build and run" validations, not unit tests. |
| RC-019 | Existing implementation pattern not followed | YES | Verified existing Dockerfile.test and docker-compose.test.yml patterns. New Dockerfile follows design doc 1.2 spec. |
| RC-020 | Kameo message type derives | NO | No Kameo actors. |
| RC-021 | Struct/field file location assumed | YES | Verified `Config` in `src/config.rs`, `Cli`/`Command` in `src/cli.rs`, `start_server` in `src/server/mod.rs`. |
| RC-022 | Plan file structure incomplete | YES | Will validate all required files before completion. |
| RC-023 | Phase completion not tracked | YES | Will track in SESSION.md. |
| RC-032 | Adding tracking without verifying data source authority | NO | No tracking/calculation code. |

## Special Considerations for Phase 3e

This phase is **infrastructure-only** -- Docker, CI, and K8s manifest files. Most Rust-specific preventive rules (actors, types, state machines) are not applicable. The primary risks are:

1. **Dockerfile correctness** -- Design doc specifies exact Dockerfile content in section 1.2. The existing `Dockerfile.test` uses Altinity ClickHouse images, not vanilla Alpine. Must reconcile.
2. **CI matrix alignment** -- Design doc specifies CH versions 23.8, 24.3, 24.8, 25.1 but existing `docker-compose.test.yml` defaults to `25.3.8.10041.altinitystable` (Altinity stable). Need to decide on version strategy.
3. **Existing file conflict** -- `Dockerfile.test` and `docker-compose.test.yml` already exist. Must decide whether to modify in-place or create new files.
4. **Build target** -- No `.cargo/config.toml` exists for musl cross-compilation target. The Dockerfile handles this internally via `--target x86_64-unknown-linux-musl`.
