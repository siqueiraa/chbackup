# Git Context

## Current State
- **Branch:** master
- **Commits ahead of main:** 0 (master IS main)
- **Working tree:** Clean source (target/ artifacts modified, not relevant)

## Recent Repository History (20 commits)
```
28dff947 style: apply cargo fmt to fix line length and formatting
8e72f434 fix: implement mutation_wait_timeout polling in check_mutations (P2-B)
bcff9cd8 fix: persist restore params for auto-resume after server restart (P1-B)
2653c140 fix: scope pending_mutations to each table in backup manifest (P1-A)
94e0fd7a fix: add params_hash to RestoreState for stale-state invalidation (P2-A)
900da559 fix: remove create --resume flag (local-only operation, no state to resume from)
8fb3f058 fix: P1 correctness fixes - lock after shortcut resolution, backup collision detection
e6620807 fix: P2 correctness fixes - restore flag conflict, shortcut sort, design doc
8bdd38ff merge: 2026-02-23-01-correctness-fixes - fix 7 correctness issues from audit
5fa70173 docs: Archive completed plan 2026-02-23-01-correctness-fixes
ae2227df docs: Mark plan as COMPLETED
8ada5713 style: apply cargo fmt to fix import ordering and line length
6a04106d docs: MR review PASS
cedd2681 docs: update CLAUDE.md for path_encoding, disable_ssl/cert_verification, check_parts_columns, env-style --env
a1c8be25 fix(s3): disable_cert_verification forces HTTP endpoint
17b6f855 refactor: replace duplicated url_encode with canonical path_encoding module
9ee9b616 fix: path_encoding module, disable_ssl wiring, strict check_parts_columns, env-style --env keys
cbf1a3d2 test(storage): hermetic S3 unit tests
746648c2 docs: patch plan 2026-02-23-01-correctness-fixes per review feedback
1ebbbd34 docs: Create plan 2026-02-23-01-correctness-fixes
```

## File-Specific History

### src/restore/remap.rs
```
05479a25 style: apply cargo fmt to Phase 4d source files
681c0719 feat(restore): add DDL helpers for ZK params, macros, ON CLUSTER, and Distributed cluster rewrite
0eb22c67 style: apply cargo fmt formatting
22215712 feat(restore): add remap module for --as and -m flag DDL rewriting
```
**Note:** The `rewrite_distributed_engine` function was introduced in commit `681c0719` as part of Phase 4d. The `&&` bug has existed since the function was first written. No subsequent changes to this function.

### src/watch/mod.rs
```
c75af0f8 fix(server): make reload update AppState config+clients and watch loop clients
10472448 feat(list): add object_disk_size and required fields to BackupSummary
a5de6b80 feat(server): wire ManifestCache into AppState with TTL and invalidation
cfa1f44b feat(manifest): add rbac_size and config_size fields to BackupManifest and BackupSummary
9af8fcee feat(list): thread metadata_size through BackupSummary to API response
8a4ad4ab feat(backup): implement --skip-projections flag for projection filtering
7099bcb0 feat(cli): wire --rbac, --configs, --named-collections flags through all call sites
3e5cdf08 feat(backup): add RBAC, config, and named collections backup logic
e87552ef style: apply cargo fmt formatting
4b3e0da6 feat(watch): implement watch state machine loop
```
**Note:** `resume_state` with `name.contains("full"/"incr")` was introduced in `4b3e0da6` (the original watch implementation). Never changed since.

### src/server/routes.rs
```
5fc70108 docs: Archive completed plan 2026-02-22-01-remove-dead-code-arc-locks
33377878 fix(server): lock-order inversion in status() handler
6a8184e4 fix(server): three correctness bugs from post-merge review
59f08788 feat(list): add auto-retention after upload for CLI and API handlers
c75af0f8 fix(server): make reload update AppState config+clients and watch loop clients
e24d33e2 refactor(server): extract DRY run_operation helper for all route handlers
989639b0 feat(server): wire CancellationToken into all 11 route handlers via tokio::select!
38c21233 feat(server): replace single-slot current_op with running_ops HashMap
f13aebd0 feat(server): add backup name path traversal validation
e5b07058 chore: apply cargo fmt to routes.rs
```

### src/config.rs
```
8ada5713 style: apply cargo fmt to fix import ordering and line length
9ee9b616 fix: path_encoding module, disable_ssl wiring, strict check_parts_columns, env-style --env keys
5fc70108 docs: Archive completed plan 2026-02-22-01-remove-dead-code-arc-locks
a5de6b80 feat(server): wire ManifestCache into AppState with TTL and invalidation
52fb1f46 feat(upload): wire streaming multipart upload for large parts
e097c915 feat(config): expand env var overlay to cover 54 config fields
69d1f32d fix(config): change default ch_port from 9000 to 8123
dd2495e8 fix(config): revert Phase 6 defaults to design doc values
ccc99bc2 feat(retry): wire jitter into all retry paths for Go parity
1b07362d feat(config,s3): wire Go parity defaults, ACL, storage class, debug flags
```

### src/cli.rs
```
28dff947 style: apply cargo fmt to fix line length and formatting
900da559 fix: remove create --resume flag (local-only operation, no state to resume from)
e6620807 fix: P2 correctness fixes - restore flag conflict, shortcut sort, design doc
a49afda4 feat(list): add --format flag and latest/previous backup shortcuts
bc9b2352 feat(cli): add all 15 subcommands with full flag sets from design doc
```

## Commit Style
Repository uses conventional commits: `feat:`, `fix:`, `refactor:`, `test:`, `docs:`, `chore:`, `style:`
