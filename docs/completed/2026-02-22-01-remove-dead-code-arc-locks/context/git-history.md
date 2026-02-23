# Git Context (Phase 2)

## Branch Context

- **Current branch:** master
- **Main branch:** main
- **Commits ahead of main:** 0 (master is the active development branch; main diverged earlier)

## Recent Repository History (last 20 commits)

```
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
e5b07058 chore: apply cargo fmt to routes.rs
665309ba docs(server): update CLAUDE.md for API gap fixes
10472448 feat(list): add object_disk_size and required fields to BackupSummary
45855134 feat(server): add offset/limit/format params to list endpoint
90345ecd docs: fix design.md parts_to_do type and required_backups reference
ca9f7e22 feat(server): add SIGTERM handler for graceful shutdown
3351149d fix(server): wire post_actions to dispatch actual commands
```

## File-Specific History

### src/clickhouse/client.rs
```
b3036c56 chore: apply cargo fmt across all modified files
69d1f32d fix(config): change default ch_port from 9000 to 8123
4a769975 feat(backup,restore): wire freeze-by-part, partition restore, and replica sync check
1b07362d feat(config,s3): wire Go parity defaults, ACL, storage class, debug flags
ab3c364e feat(clickhouse): add check_json_columns() for Object/JSON type detection
```

The `debug` field was added in `1b07362d` (Go parity defaults). The `inner()` method was part of the original client scaffolding.

### src/storage/s3.rs
```
b3036c56 chore: apply cargo fmt across all modified files
e5af1a89 feat(storage): add PutObject/UploadPart retry with exponential backoff
44e9f076 feat(storage): add multipart CopyObject for objects exceeding 5GB
3beb3d43 feat(storage): add concurrency and object_disk_path fields to S3Client
1acafed6 feat(s3): implement STS AssumeRole for cross-account access
```

Both `concurrency` and `object_disk_path` fields were added in `3beb3d43`. The `inner()` method was part of original scaffolding.

### src/restore/attach.rs
```
b3036c56 chore: apply cargo fmt across all modified files
80eb80cc feat(backup): add resolve_shadow_part_path() helper with 4-step fallback chain
ccc99bc2 feat(retry): wire jitter into all retry paths for Go parity
76ee6e37 feat(restore): add ATTACH TABLE mode for Replicated engine tables
4300d5e4 style: apply cargo fmt formatting across all modules
```

`attach_parts()` was the original implementation. `attach_parts_owned()` was added in Phase 2a to support `tokio::spawn` boundaries. The original function has been dead since then.

### src/server/metrics.rs
```
487128b9 style: apply cargo fmt formatting
52b933a5 feat(server): instrument operation handlers with prometheus metrics
2e08025c feat(server): add Metrics struct with 14 prometheus metric definitions
```

`parts_uploaded_total` and `parts_skipped_incremental_total` were registered in `2e08025c` but never wired with `.inc()` calls.

### src/error.rs
```
b2d5d78f feat(cli): implement structured exit codes per design 11.6
1bb4bc2e feat: add Phase 1 dependencies, error variants, and module declarations
c236b4e2 feat: initialize cargo project with dependencies and error types
```

Error variants defined since Phase 0. Most variants were intended for future use but never constructed outside tests.

### src/progress.rs
```
bb6b1707 feat(progress): add indicatif dependency and ProgressTracker struct
```

Single commit added the full module. `disabled()` and `is_active()` were test helpers from day one.

### src/server/actions.rs
```
c5264fce feat(server): add ActionLog ring buffer and ActionEntry types
```

Single commit. `running()` was a test helper from the start.

## Working Tree Changes

Modified files (from git status):
- Source files: `src/config.rs`, `src/main.rs`, `src/server/mod.rs`, `src/server/routes.rs`, `src/server/state.rs`
- Docs: `CLAUDE.md`, `src/server/CLAUDE.md`
- Build artifacts: `target/` (doc output, rustc info)
- New: `.gitignore`

These working tree changes are unrelated to this dead code removal plan.
