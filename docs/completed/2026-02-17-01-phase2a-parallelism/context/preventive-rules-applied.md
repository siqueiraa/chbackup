# Preventive Rules Applied

## Rules Reviewed

All 34 rules from `.claude/skills/self-healing/references/root-causes.md` were reviewed.

## Applicable Rules for Phase 2a

| Rule ID | Title | Applicability | Notes |
|---------|-------|---------------|-------|
| RC-006 | Plan code snippets use unverified APIs | HIGH | Must verify `tokio::sync::Semaphore`, `futures::future::try_join_all`, S3 multipart APIs exist before referencing in plan snippets |
| RC-008 | TDD task sequencing violation | HIGH | Semaphore infrastructure must be in preceding task before parallel command tasks reference it |
| RC-015 | Cross-task return type mismatch | MEDIUM | Parallel tasks return JoinHandle<Result<T>> -- must verify T consistency |
| RC-016 | Struct field completeness | MEDIUM | New structs (e.g., UploadTask work item) must have all fields consumer tasks need |
| RC-017 | State field declaration missing | MEDIUM | Any new fields on S3Client or Config must be declared in the correct task |
| RC-018 | TDD task missing explicit test steps | HIGH | Every parallelism change needs explicit tests |
| RC-019 | Existing pattern not followed | HIGH | Must follow existing Phase 1 patterns (spawn_blocking for sync I/O, anyhow::Result, etc.) |
| RC-021 | Struct/field file location assumed | MEDIUM | Config concurrency fields are in config.rs (verified); S3Client is in storage/s3.rs (verified) |
| RC-032 | Data source authority not verified | LOW | No new tracking/calculation -- but multipart size threshold is from config, not computed |

## Non-Applicable Rules

| Rule ID | Why Not Applicable |
|---------|-------------------|
| RC-001 | No actors in this project (not kameo-based) |
| RC-002 | No financial data types |
| RC-004 | No message handlers |
| RC-010 | No adapter stubs |
| RC-011 | No state machine flags (semaphore is acquire/release, not flag-based) |
| RC-012 | No E2E tests with shared mutable state |
| RC-013 | No std::sync::Mutex usage planned |
| RC-014 | No connection polling loops |
| RC-020 | No Kameo message types |
| RC-033 | No tokio::spawn capturing strong ActorRef |
| RC-034 | No shared state captured at spawn time |

## Verification Actions Taken

1. **RC-006**: Verified `tokio::sync::Semaphore` exists in tokio with "full" features (already in Cargo.toml). Verified `aws_sdk_s3` has `create_multipart_upload`, `upload_part`, `complete_multipart_upload`, `abort_multipart_upload` in the SDK docs.
2. **RC-008**: Planned task ordering ensures semaphore infrastructure (Task 1) precedes all parallel command tasks.
3. **RC-019**: Read all existing Phase 1 module patterns -- spawn_blocking for sync I/O, anyhow::Result<()> returns, tracing for logging.
4. **RC-021**: Verified all concurrency config fields are in `src/config.rs` (upload_concurrency, download_concurrency, max_connections, max_parts_count, chunk_size).
