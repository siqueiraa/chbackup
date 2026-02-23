# Git History Context

## Recent Commits (master branch)

```
2c5671c9 docs: update CLAUDE.md for Phase 5 polish gaps
6cb6f1ad feat(server): implement GET /api/v1/tables endpoint
4780b28e feat(upload,download): wire progress bar into parallel pipelines
80d45475 feat(server): implement restart endpoint with ArcSwap hot-swap
bb6b1707 feat(progress): add indicatif dependency and ProgressTracker struct
```

## Completed Phases

- Phase 0 (skeleton): CLI, config, ChClient, S3Client, PidLock, logging
- Phase 1 (MVP): Single-table backup/restore
- Phase 2a (parallelism): Parallel operations, multipart upload, rate limiting
- Phase 2b (incremental): Incremental backups, diff_parts, create_remote
- Phase 2c (S3 object disk): 5 format versions, mixed disk upload/download
- Phase 2d (resume): Resumable ops, atomic manifest, CRC64, disk filtering
- Phase 3d (watch): State machine, templates, hot-reload, API endpoints
- Phase 3e (docker): Dockerfile, CI, K8s sidecar, integration tests
- Phase 5 (polish): Tables/restart API, skip-projections, progress bar, exit codes

## This Plan (Phase 6)

Fills all remaining Go parity gaps found from comprehensive source comparison.
After this phase, chbackup should be a fully compatible drop-in replacement.
