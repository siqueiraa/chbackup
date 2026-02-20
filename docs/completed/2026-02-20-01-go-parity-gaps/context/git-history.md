# Git History

**Plan:** 2026-02-20-01-go-parity-gaps

## Recent Commits (last 20)

```
93f55e28 Merge branch 'claude/phase6-go-parity' into master
af5c64b0 chore: update Cargo.lock for aws-sdk-sts dependency
db6337b7 docs: update CLAUDE.md for Phase 6 Go parity
44e9f076 feat(storage): add multipart CopyObject for objects exceeding 5GB
3beb3d43 feat(storage): add concurrency and object_disk_path fields to S3Client
a49afda4 feat(list): add --format flag and latest/previous backup shortcuts
4a769975 feat(backup,restore): wire freeze-by-part, partition restore, and replica sync check
ee18f3e6 feat(server): exit process when watch loop ends and watch_is_main_process is set
2725a395 feat(server): API parity with Go clickhouse-backup
539ab56e feat(list): add incremental chain protection to retention_remote
1acafed6 feat(s3): implement STS AssumeRole for cross-account access
ccc99bc2 feat(retry): wire jitter into all retry paths for Go parity
1b07362d feat(config,s3): wire Go parity defaults, ACL, storage class, debug flags
2c5671c9 docs: update CLAUDE.md for Phase 5 polish gaps
6cb6f1ad feat(server): implement GET /api/v1/tables endpoint
4780b28e feat(upload,download): wire progress bar into parallel pipelines
80d45475 feat(server): implement restart endpoint with ArcSwap hot-swap
bb6b1707 feat(progress): add indicatif dependency and ProgressTracker struct
9af8fcee feat(list): thread metadata_size through BackupSummary to API response
b2d5d78f feat(cli): implement structured exit codes per design 11.6
```

## Branch Status
- Current branch: `master`
- Main branch: `main`
- Last merge: `claude/phase6-go-parity` into master

## Commit Style
- Conventional commits: `feat:`, `fix:`, `refactor:`, `test:`, `docs:`, `chore:`
- Scoped: `feat(module): description`
