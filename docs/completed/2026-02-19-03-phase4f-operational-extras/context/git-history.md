# Git Context and History

## Recent Repository History (last 20 commits)

```
8053a9d9 docs: update CLAUDE.md for Phase 4e RBAC/config modules
7099bcb0 feat(cli): wire --rbac, --configs, --named-collections flags through all call sites
5395fb15 feat(restore): add RBAC, config, named collections restore and restart_command
3c31f3b0 feat(upload,download): add access/ and configs/ directory transfer
3e5cdf08 feat(backup): add RBAC, config, and named collections backup logic
328b2a69 feat(clickhouse): add RBAC, named collections, and UDF query methods
4ab7ada3 docs: Re-validate Phase 4e plan after PLAN.md updates
88005fb4 docs: Validate plan 2026-02-19-02-phase4e-rbac-config-backup (Phases 8-8.6)
6ea741b2 docs: Create plan 2026-02-19-02-phase4e-rbac-config-backup
467f4978 docs: Archive completed plan 2026-02-19-02-phase4d-advanced-restore
d5e176a3 docs: Mark plan as COMPLETED
05479a25 style: apply cargo fmt to Phase 4d source files
6684cb8b docs: update tracking for Task 10 CLAUDE.md completion (10/10 PASS)
7b59c959 docs(restore): update CLAUDE.md for Phase 4d advanced restore
dba458b3 feat(restore): integrate ON CLUSTER, DatabaseReplicated, and Distributed rewrite into restore orchestrator
ee753213 feat(restore): wire rm parameter through restore() and all call sites
58d2f50e feat(restore): add pending mutation re-apply after data attachment
76ee6e37 feat(restore): add ATTACH TABLE mode for Replicated engine tables
b022a6c8 test(restore): add ZK conflict resolution and replicated engine tests
b5b22bb3 feat(restore): add Mode A DROP phase, ZK conflict resolution, and DatabaseReplicated detection
```

## File-Specific History

### src/main.rs (command dispatch)
```
7099bcb0 feat(cli): wire --rbac, --configs, --named-collections flags through all call sites
5395fb15 feat(restore): add RBAC, config, named collections restore and restart_command
3e5cdf08 feat(backup): add RBAC, config, and named collections backup logic
ee753213 feat(restore): wire rm parameter through restore() and all call sites
0eb22c67 style: apply cargo fmt formatting
f5120d91 feat(restore): wire CLI dispatch for --as, -m flags and restore_remote command
8fff26f1 feat(restore): integrate remap into restore pipeline
d47e4391 feat(server): spawn watch loop in server mode with SIGHUP handler
c554b796 feat(watch): wire standalone watch command in main.rs
e8e2c1fb feat: wire clean command CLI dispatch and replace clean_stub API handler
```

### src/upload/stream.rs and src/download/stream.rs (compression)
```
1050619b style: apply cargo fmt formatting across all modules
324e85fa feat(upload): implement upload with tar+LZ4 compression and S3 PutObject
ae4598de feat(download): implement download module with S3 fetch and LZ4 decompression
```
**Key observation:** These files have NOT been modified since Phase 1 (original implementation). The compression pipeline is unchanged since initial creation.

### src/list.rs (list command)
```
487128b9 style: apply cargo fmt formatting
4b9cf112 feat(list): add GC-safe deletion and remote retention
83433042 feat(list): add GC key collection for safe remote backup deletion
ad1aed15 feat(list): add config resolution helpers and local retention
e8c2c4c5 feat(deps): add axum, tower-http, base64 dependencies and derive Serialize on BackupSummary
4300d5e4 style: apply cargo fmt formatting across all modules
de994686 feat(list): add broken_reason to BackupSummary and implement clean_broken
1050619b style: apply cargo fmt formatting across all modules
97cb2844 feat(upload): add mixed disk upload with CopyObject for S3 disk parts
88af4c37 feat(list): implement list command with local dir scan and remote S3 listing
```

### src/config.rs and src/manifest.rs
```
74b014ac feat(config): add WATCH_INTERVAL and FULL_INTERVAL env var overlay
62e43dc5 refactor(config): make parse_duration_secs public and add WatchConfig.tables field
1050619b style: apply cargo fmt formatting across all modules
97cb2844 feat(upload): add mixed disk upload with CopyObject for S3 disk parts
63024eab feat(manifest): add BackupManifest and related types with serde roundtrip
1bb4bc2e feat: add Phase 1 dependencies, error variants, and module declarations
880a6408 chore: apply rustfmt formatting fixes
77d825e5 feat(config): add configuration loader with ~106 params, env overlay, and validation
```

### src/clickhouse/client.rs
```
8053a9d9 docs: update CLAUDE.md for Phase 4e RBAC/config modules
328b2a69 feat(clickhouse): add RBAC, named collections, and UDF query methods
05479a25 style: apply cargo fmt to Phase 4d source files
424b808c feat(clickhouse): add ChClient methods for Mode A, ZK resolution, ATTACH TABLE, and mutations
716a7364 feat(clickhouse): add query_table_dependencies() method to ChClient
e87552ef style: apply cargo fmt formatting
b933771c feat(clickhouse): add get_macros() method for system.macros query
816cc979 style: apply cargo fmt formatting to server module files
37666738 feat(clickhouse): add integration table DDL methods for API server
4300d5e4 style: apply cargo fmt formatting across all modules
```

### src/backup/mod.rs
```
8053a9d9 docs: update CLAUDE.md for Phase 4e RBAC/config modules
3e5cdf08 feat(backup): add RBAC, config, and named collections backup logic
e0668461 style: apply cargo fmt formatting
6250dcbf feat(backup): populate TableManifest.dependencies from system.tables query
4300d5e4 style: apply cargo fmt formatting across all modules
c9054177 feat(backup): add pre-flight parts column consistency check
bb1c0e3f feat(backup): add partition-level backup via --partitions flag
ff42860d feat(backup): add disk filtering via skip_disks and skip_disk_types
1050619b style: apply cargo fmt formatting across all modules
97cb2844 feat(upload): add mixed disk upload with CopyObject for S3 disk parts
```

## Branch Context

**Current branch:** `master`
**Commits ahead of main:** On master branch (no divergence from main)

This plan will create a new branch from master for Phase 4f implementation.
