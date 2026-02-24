# HANDOFF: Phase 6 — Go Parity

## Context

This plan fixes all gaps found from a comprehensive comparison of the Rust chbackup implementation against the Go clickhouse-backup source code. Eight parallel research agents compared every domain (config, backup, restore, storage, server, CLI, watch, list) and found 35 gaps organized into 6 tiers of severity.

## Key Files

| File | Purpose | Tasks |
|------|---------|-------|
| `src/config.rs` | Config definitions + defaults | 1 |
| `src/storage/s3.rs` | S3 client wrapper | 2, 3, 4, 5, 8 |
| `src/backup/mod.rs` | Backup pipeline | 6 |
| `src/backup/freeze.rs` | FREEZE logic | 6 |
| `src/restore/mod.rs` | Restore pipeline | 7 |
| `src/restore/attach.rs` | ATTACH part logic | 7 |
| `src/list.rs` | List/retention/cleanup | 9, 12 |
| `src/server/routes.rs` | API routes | 10 |
| `src/server/mod.rs` | Server lifecycle | 11 |
| `src/cli.rs` | CLI definitions | 12 |
| `src/main.rs` | Command dispatch | 12 |
| `Cargo.toml` | Dependencies | 3, 5 |

## Architectural Decisions

1. **STS AssumeRole**: Uses aws-sdk-sts to get temporary credentials, then passes them to S3 client builder. No credential refresh loop (single-use like Go).
2. **Multipart CopyObject**: Head object to check size, then branch to single CopyObject or UploadPartCopy-based multipart. Threshold: 5GB (S3 limit).
3. **Freeze-by-part**: Queries `system.parts` for partition list, iterates FREEZE per partition. Error 218 is non-fatal (warning + continue).
4. **Retry jitter**: Multiplicative jitter: `delay * (1.0 + rand * jitter_factor)`. Uses `fastrand` or `rand` crate.
5. **HTTP 423**: Direct replacement of `StatusCode::CONFLICT` with `StatusCode::LOCKED` across all route handlers.
6. **Actions dispatch**: POST /api/v1/actions parses command string, acquires operation lock, spawns async task for the command, returns operation_id immediately.

## Resume Instructions

1. Read SESSION.md for current task status
2. Find first group with `pending` status
3. Execute tasks in that group
4. After all groups complete, run MR review
5. Update CLAUDE.md (Task 13) last

## Known Risks

- **STS dependency**: Adds ~1MB to binary size (aws-sdk-sts)
- **Actions dispatch**: POST /api/v1/actions is complex — needs proper error handling and cancellation
- **Multipart copy**: UploadPartCopy API has different error modes than regular CopyObject
- **Jitter randomness**: Using `fastrand` avoids heavy `rand` dependency but is not cryptographically secure (acceptable for jitter)
