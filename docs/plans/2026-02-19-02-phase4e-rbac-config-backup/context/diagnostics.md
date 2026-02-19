# Diagnostics Report -- Phase 4e RBAC & Config Backup

## Compiler State

**Timestamp**: 2026-02-19
**Branch**: master
**Tool**: `cargo check`

### Results

```
Finished `dev` profile [unoptimized + debuginfo] target(s) in 41.74s
```

**Errors**: 0
**Warnings**: 0

The codebase compiles cleanly with zero warnings and zero errors. This is the baseline state before Phase 4e implementation.

## Existing Warning Flags in main.rs

The following `warn!()` calls mark Phase 4e flags as "not yet implemented":

| File | Line | Flag | Warning Message |
|------|------|------|-----------------|
| `src/main.rs` | 137 | `--rbac` (create) | `"--rbac flag is not yet implemented, ignoring"` |
| `src/main.rs` | 140 | `--configs` (create) | `"--configs flag is not yet implemented, ignoring"` |
| `src/main.rs` | 143 | `--named-collections` (create) | `"--named-collections flag is not yet implemented, ignoring"` |
| `src/main.rs` | 239 | `--rbac` (restore) | `"--rbac flag is not yet implemented, ignoring"` |
| `src/main.rs` | 242 | `--configs` (restore) | `"--configs flag is not yet implemented, ignoring"` |
| `src/main.rs` | 245 | `--named-collections` (restore) | `"--named-collections flag is not yet implemented, ignoring"` |
| `src/main.rs` | 291 | `--rbac` (create_remote) | `"--rbac flag is not yet implemented, ignoring"` |
| `src/main.rs` | 294 | `--configs` (create_remote) | `"--configs flag is not yet implemented, ignoring"` |
| `src/main.rs` | 297 | `--named-collections` (create_remote) | `"--named-collections flag is not yet implemented, ignoring"` |
| `src/main.rs` | 354 | `--rbac` (restore_remote) | `"--rbac flag is not yet implemented, ignoring"` |
| `src/main.rs` | 357 | `--configs` (restore_remote) | `"--configs flag is not yet implemented, ignoring"` |
| `src/main.rs` | 360 | `--named-collections` (restore_remote) | `"--named-collections flag is not yet implemented, ignoring"` |

These 12 warning stubs will be replaced by actual implementations in Phase 4e.

## Config Fields Already Present

All Phase 4e config fields are already defined in `src/config.rs`:

| Config Path | Type | Default | Line |
|-------------|------|---------|------|
| `clickhouse.restart_command` | `String` | `"exec:systemctl restart clickhouse-server"` | 190 |
| `clickhouse.rbac_backup_always` | `bool` | `false` | 198 |
| `clickhouse.config_backup_always` | `bool` | `false` | 202 |
| `clickhouse.named_collections_backup_always` | `bool` | `false` | 206 |
| `clickhouse.rbac_resolve_conflicts` | `String` | `"recreate"` | 210 |
| `clickhouse.config_dir` | `String` | `"/etc/clickhouse-server"` | 115 |

All config fields are already wired in `apply_cli_env_overrides()` (set_field match arms exist).

## Manifest Fields Already Present

The manifest structs already have the Phase 4e fields:

| Field | Type | Location | Line |
|-------|------|----------|------|
| `BackupManifest.functions` | `Vec<String>` | `src/manifest.rs` | 73 |
| `BackupManifest.named_collections` | `Vec<String>` | `src/manifest.rs` | 77 |
| `BackupManifest.rbac` | `Option<RbacInfo>` | `src/manifest.rs` | 81 |
| `RbacInfo.path` | `String` | `src/manifest.rs` | 190 |

## CLI Flags Already Defined

All Phase 4e CLI flags are already defined in `src/cli.rs`:

| Flag | Commands | Lines |
|------|----------|-------|
| `--rbac` | Create, Restore, CreateRemote, RestoreRemote | 59, 149, 184, 230 |
| `--configs` | Create, Restore, CreateRemote, RestoreRemote | 63, 153, 188, 235 |
| `--named-collections` | Create, Restore, CreateRemote, RestoreRemote | 67, 157, 192, 239 |

## Pre-existing Implementation

### Already Implemented in Current Codebase
- Phase 4 (functions) restore: `create_functions()` in `src/restore/schema.rs:721` -- iterates `manifest.functions` and executes DDL
- Config validation for `rbac_resolve_conflicts` ("recreate", "ignore", "fail") in `config.rs:1246`

### NOT Yet Implemented (Phase 4e scope)
1. RBAC backup: query system.users/roles/row_policies/settings_profiles/quotas -> serialize to access/*.jsonl
2. Config backup: copy CH config files from config_dir to backup dir
3. Named collections backup: query system.named_collections -> serialize
4. RBAC restore: copy .jsonl files to access_data_path, create need_rebuild_lists.mark, remove stale .list files, chown
5. Config restore: copy configs to config_dir
6. Named collections restore: CREATE NAMED COLLECTION SQL
7. restart_command execution: parse "exec:" and "sql:" prefixes, execute after RBAC/config restore
8. Upload/download: upload access/ and configs/ directories to/from S3
9. Wire flags through main.rs, server routes, and watch mode (remove "not yet implemented" warnings)
10. Populate manifest.rbac, manifest.named_collections, manifest.functions during create
