# Handoff: Phase 3e -- Docker / Deployment

## Plan Location
`docs/plans/2026-02-18-04-phase3e-docker-deploy/`

## Directory Contents

| File | Purpose |
|------|---------|
| PLAN.md | Task definitions and TDD steps (7 tasks, 3 dependency groups) |
| SESSION.md | Status tracking and checklists |
| acceptance.json | 4-layer verification criteria (6 criteria: F001-F005, FDOC) |
| HANDOFF.md | This file -- resume context |
| context/patterns.md | Infrastructure patterns (Docker, CI, K8s) |
| context/symbols.md | Type verification (CLI, Config, Server interfaces) |
| context/diagnostics.md | Compiler and test state baseline |
| context/knowledge_graph.json | Structured JSON for symbol lookup |
| context/affected-modules.json | Machine-readable module status |
| context/affected-modules.md | Human-readable module summary |
| context/references.md | Reference analysis (config overlay, CLI, infra files) |
| context/git-history.md | Git context (Phase 3d complete, 3e is current) |
| context/redundancy-analysis.md | New files checked against existing codebase |
| context/data-authority.md | Data source verification (N/A -- infrastructure only) |
| context/preventive-rules-applied.md | Applied preventive rules |

## To Resume

1. Read SESSION.md for current status
2. Check Agent Execution Status table
3. Continue from last incomplete phase/agent
4. Use `/project-execute` when planning is complete

## Key References

### Files Being Modified
- `src/config.rs` -- Add WATCH_INTERVAL/FULL_INTERVAL to apply_env_overlay() (line ~904)
- `tests/config_test.rs` -- Add test_watch_env_overlay test + clear_config_env_vars update
- `test/run_tests.sh` -- Extend with seed data loading and round-trip smoke test

### Files Being Created
- `Dockerfile` -- Production multi-stage Docker image (design doc 1.2)
- `.github/workflows/ci.yml` -- GitHub Actions CI with CH version matrix (design doc 1.4.6)
- `examples/kubernetes/sidecar.yaml` -- K8s sidecar deployment example (design doc 1.3 + 10.9)
- `test/fixtures/seed_data.sql` -- Deterministic test data for checksum validation (design doc 1.4.4)

### Files Being Updated (no content changes needed)
- `CLAUDE.md` -- Root project documentation (Phase 3e status update)

### Design Doc Sections
- 1.2 -- Dockerfile specification
- 1.3 -- K8s sidecar manifest
- 1.4 -- Integration test environment
- 1.4.4 -- Test suite structure (seed_data.sql)
- 1.4.6 -- CI matrix (CH versions)
- 10.9 -- Watch mode in Docker (WATCH_INTERVAL/FULL_INTERVAL env vars)
- 11.5 -- Signal handling (terminationGracePeriodSeconds: 60 in K8s)

### Existing Infrastructure (reference patterns)
- `Dockerfile.test` -- Existing test image (Altinity CH + chbackup binary)
- `docker-compose.test.yml` -- Existing compose file (ZK + CH test container)
- `test/configs/chbackup-test.yml` -- Existing test config
- `test/configs/clickhouse-config.xml` -- Existing CH config overlay

## Commit Log

| Commit | Tasks | Description |
|--------|-------|-------------|
| 74b014ac | 1, 2 | feat(config): add WATCH_INTERVAL and FULL_INTERVAL env var overlay |
| b6703580 | 3, 4, 5, 6 | feat: add Dockerfile, CI workflow, integration tests, and K8s example |
| 763422a7 | 7 | docs: update CLAUDE.md for Phase 3e (docker/deploy) |

## Key Decisions

1. **Altinity vs vanilla CH**: Using Altinity images (`altinity/clickhouse-server`) consistent with existing `Dockerfile.test`. Design doc uses vanilla but project already established with Altinity.
2. **Phase 4.5 skipped**: Only 2-line Rust change to existing function; no new types/imports.
3. **No DEBUG_VERIFY markers**: Infrastructure-only phase, no runtime log verification needed.
4. **S3 prefix isolation in CI**: Each matrix job uses `chbackup-test/$ch_version/$run_id` to prevent test interference.
