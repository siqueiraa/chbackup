# Git Context

## Branch Context

- **Current branch**: `master`
- **Commits ahead of main**: 0
- **Working tree**: clean (only `target/.rustc_info.json` modified, untracked)

## Recent Repository History (last 20 commits)

```
5fc70108 docs: Archive completed plan 2026-02-22-01-remove-dead-code-arc-locks
7c0b1d29 docs: Mark plan as COMPLETED
432e7829 docs: MR review PASS
fbc227e4 docs: update CLAUDE.md files to reflect dead code removal from ChClient, S3Client, and attach.rs
c6152ac8 refactor(restore): remove dead attach_parts() function superseded by attach_parts_owned()
9edc6e3c refactor(storage): remove unused inner(), concurrency(), object_disk_path() getters and dead fields from S3Client
5b912ba4 refactor(clickhouse): remove dead debug field and unused inner() getter from ChClient
33377878 fix(server): lock-order inversion in status() handler
6a8184e4 fix(server): three correctness bugs from post-merge review
a7503b3a chore: apply cargo fmt to lock.rs, main.rs, state.rs
a5774991 docs(server): update CLAUDE.md for running_ops, run_operation, kill, validation, reload changes
b4e8b6b7 test: add integration tests T4-T10 (incremental, schema-only, partitions, API, validation, delete, clean_broken)
3973d090 docs(main): clarify create --resume as intentionally deferred design decision
59f08788 feat(list): add auto-retention after upload for CLI and API handlers
c75af0f8 fix(server): make reload update AppState config+clients and watch loop clients
e24d33e2 refactor(server): extract DRY run_operation helper for all route handlers
989639b0 feat(server): wire CancellationToken into all 11 route handlers via tokio::select!
38c21233 feat(server): replace single-slot current_op with running_ops HashMap
ccbc36a1 fix(lock): eliminate TOCTOU race in PidLock::acquire via O_CREAT|O_EXCL
f13aebd0 feat(server): add backup name path traversal validation
```

## File-Specific History

### src/storage/s3.rs (last 10 commits)

```
9edc6e3c refactor(storage): remove unused inner(), concurrency(), object_disk_path() getters and dead fields from S3Client
b3036c56 chore: apply cargo fmt across all modified files
e5af1a89 feat(storage): add PutObject/UploadPart retry with exponential backoff
44e9f076 feat(storage): add multipart CopyObject for objects exceeding 5GB
3beb3d43 feat(storage): add concurrency and object_disk_path fields to S3Client
1acafed6 feat(s3): implement STS AssumeRole for cross-account access
ccc99bc2 feat(retry): wire jitter into all retry paths for Go parity
1b07362d feat(config,s3): wire Go parity defaults, ACL, storage class, debug flags
1050619b style: apply cargo fmt formatting across all modules
b90f31ff feat(storage): add copy_object, copy_object_streaming, and copy_object_with_retry to S3Client
```

### src/config.rs (last 10 commits)

```
5fc70108 docs: Archive completed plan 2026-02-22-01-remove-dead-code-arc-locks
a5de6b80 feat(server): wire ManifestCache into AppState with TTL and invalidation
52fb1f46 feat(upload): wire streaming multipart upload for large parts
e097c915 feat(config): expand env var overlay to cover 54 config fields
69d1f32d fix(config): change default ch_port from 9000 to 8123
dd2495e8 fix(config): revert Phase 6 defaults to design doc values
ccc99bc2 feat(retry): wire jitter into all retry paths for Go parity
1b07362d feat(config,s3): wire Go parity defaults, ACL, storage class, debug flags
74b014ac feat(config): add WATCH_INTERVAL and FULL_INTERVAL env var overlay
62e43dc5 refactor(config): make parse_duration_secs public and add WatchConfig.tables field
```

### src/backup/mod.rs (last 10 commits)

```
b3036c56 chore: apply cargo fmt across all modified files
2c4dca91 feat(backup): use per-disk staging dirs in collect_parts()
620a0c08 feat(backup): compute rbac_size and config_size during backup create
cfa1f44b feat(manifest): add rbac_size and config_size fields to BackupManifest and BackupSummary
a49afda4 feat(list): add --format flag and latest/previous backup shortcuts
4a769975 feat(backup,restore): wire freeze-by-part, partition restore, and replica sync check
8a4ad4ab feat(backup): implement --skip-projections flag for projection filtering
210ba7a0 feat(backup): integrate JSON column check into backup pre-flight
8053a9d9 docs: update CLAUDE.md for Phase 4e RBAC/config modules
3e5cdf08 feat(backup): add RBAC, config, and named collections backup logic
```

### src/download/mod.rs (last 10 commits)

```
b3036c56 chore: apply cargo fmt across all modified files
507d9a61 feat(download): update find_existing_part() for per-disk search
402c8b4d feat(download): write parts to per-disk backup directories
452389c0 feat(list): update delete_local() to clean per-disk backup directories
ccc99bc2 feat(retry): wire jitter into all retry paths for Go parity
4780b28e feat(upload,download): wire progress bar into parallel pipelines
c32b0b0d feat(download): implement --hardlink-exists-files for part deduplication
4a8474b4 feat(upload,download): wire compression format through pipelines
3c31f3b0 feat(upload,download): add access/ and configs/ directory transfer
4300d5e4 style: apply cargo fmt formatting across all modules
```

### src/backup/collect.rs (last 10 commits)

```
b3036c56 chore: apply cargo fmt across all modified files
2c4dca91 feat(backup): use per-disk staging dirs in collect_parts()
80eb80cc feat(backup): add resolve_shadow_part_path() helper with 4-step fallback chain
452389c0 feat(list): update delete_local() to clean per-disk backup directories
620a0c08 feat(backup): compute rbac_size and config_size during backup create
8a4ad4ab feat(backup): implement --skip-projections flag for projection filtering
4300d5e4 style: apply cargo fmt formatting across all modules
ff42860d feat(backup): add disk filtering via skip_disks and skip_disk_types
1050619b style: apply cargo fmt formatting across all modules
83a55532 feat(backup): add disk-aware shadow walk and actual disk name grouping
```

## Related Prior Changes

### Path Traversal Validation (already exists for backup names)

Commit `f13aebd0 feat(server): add backup name path traversal validation` added validation for backup names in the server API routes. This plan extends similar validation to db/table names in the path encoding layer.

### S3Client Dead Code Removal (recent)

Commit `9edc6e3c` removed `inner()`, `concurrency()`, `object_disk_path()` getters and dead fields from `S3Client`. The struct is now lean (7 fields). This is relevant for Issue 3 (mock_s3_client) since the struct shape was recently simplified.

### Config Env Overlay Expansion (Phase 7)

Commit `e097c915` expanded `apply_env_overlay()` to 54+ fields. This is the authoritative mapping that Issue 6 needs to make available for `--env` CLI overrides.
