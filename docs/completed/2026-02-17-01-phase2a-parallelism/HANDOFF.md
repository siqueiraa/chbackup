# Handoff: Phase 2a -- Parallelism

## Plan Location
`docs/plans/2026-02-17-01-phase2a-parallelism/`

## Directory Contents

| File | Purpose |
|------|---------|
| PLAN.md | Task definitions and TDD steps (8 tasks in 3 dependency groups) |
| SESSION.md | Status tracking and checklists |
| acceptance.json | 4-layer verification criteria (8 criteria: F001-F007, FDOC) |
| HANDOFF.md | This file -- resume context |
| context/patterns.md | Discovered patterns (7 patterns from Phase 1 codebase) |
| context/symbols.md | Type verification table (config types, core types, function signatures) |
| context/diagnostics.md | Cargo check baseline (0 errors, 0 warnings) |
| context/knowledge_graph.json | Structured JSON for symbol lookup |
| context/affected-modules.json | Module status for CLAUDE.md updates (5 modules to update) |
| context/affected-modules.md | Human-readable module summary |
| context/references.md | Reference analysis for all modified functions |
| context/git-history.md | Git context (Phase 0 + Phase 1 commits) |
| context/redundancy-analysis.md | New component checks (multipart, rate limiter are genuinely new) |
| context/preventive-rules-applied.md | Applied rules from root-causes.md |

## To Resume

1. Read SESSION.md for current status
2. Check Agent Execution Status table
3. Continue from last incomplete phase/agent
4. Use `/project-execute` when planning is complete

## Key References

### Files Being Modified
- `Cargo.toml` -- add `futures = "0.3"` dependency
- `src/lib.rs` -- add `pub mod concurrency;` and `pub mod rate_limiter;`
- `src/concurrency.rs` -- NEW: effective concurrency accessors
- `src/rate_limiter.rs` -- NEW: token-bucket rate limiter
- `src/storage/s3.rs` -- add multipart upload methods to S3Client
- `src/backup/mod.rs` -- parallelize create() with Arc<Semaphore> + tokio::spawn
- `src/upload/mod.rs` -- parallelize upload() with flat work queue
- `src/download/mod.rs` -- parallelize download() with flat work queue
- `src/restore/mod.rs` -- parallelize restore() with table-level concurrency
- `src/restore/attach.rs` -- add OwnedAttachParams, wire needs_sequential_attach

### Test Files
- Unit tests are inline (in `#[cfg(test)] mod tests` within each source file)
- Integration tests require real ClickHouse + S3 (not run in unit test suite)

### Related Documentation
- `docs/design.md` sections 3.4, 3.6, 4, 5.3, 11.1
- `docs/roadmap.md` Phase 2a section
- Module-level `CLAUDE.md` files in each `src/*/` directory

### Design Doc Quick Reference
- Section 3.4: Parallel FREEZE with max_connections semaphore, scopeguard UNFREEZE
- Section 3.6: Flat upload concurrency, multipart for >32MB, rate limiting, abort cleanup
- Section 4: Flat download concurrency, same pattern as upload
- Section 5.3: Table-parallel restore, sequential ATTACH for Replacing/Collapsing
- Section 11.1: Concurrency model summary table (semaphore per operation type)
- Section 11.2: What MUST be sequential (ATTACH for dedup engines, manifest upload)

## Commit Log

| Task | Commit | Description |
|------|--------|-------------|
| 1 | a7c0ca1 | feat(concurrency): add futures crate and effective concurrency helpers |
| 2 | d4be4a5 | feat(s3): add multipart upload methods and chunk size calculation |
| 3 | 68949f7 | feat(rate-limiter): add token-bucket rate limiter module |
| 4 | 90f9029 | feat(backup): parallelize FREEZE and collect with max_connections semaphore |
| 5 | f591535 | feat(upload): parallelize upload with flat work queue, multipart, and rate limiting |
| 6 | 63ad1b3 | feat(download): parallelize download with flat work queue and rate limiting |
| 7 | 1d44f56 | feat(restore): parallelize table restore with max_connections and engine-aware ATTACH |
| 8 | 633ea6d | docs: update CLAUDE.md for Phase 2a parallelism changes |
