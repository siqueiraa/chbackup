# Symbol & Reference Analysis -- Phase 4e RBAC & Config Backup

## Key Symbols and Their Locations

### Manifest Types (src/manifest.rs)

| Symbol | Type | Line | Notes |
|--------|------|------|-------|
| `BackupManifest` | struct | 19 | Central data structure; already has `functions`, `named_collections`, `rbac` fields |
| `RbacInfo` | struct | 188 | Only has `path: String` field; may need extension for richer metadata |
| `BackupManifest::save_to_file` | method | 211 | Saves JSON to local file |
| `BackupManifest::load_from_file` | method | 224 | Loads JSON from local file |
| `BackupManifest::to_json_bytes` | method | 240 | Serialize to bytes for S3 upload |
| `BackupManifest::from_json_bytes` | method | 233 | Deserialize from S3 download bytes |

### Config Types (src/config.rs)

| Symbol | Type | Line | Notes |
|--------|------|------|-------|
| `ClickHouseConfig.restart_command` | field | 190 | Default: `"exec:systemctl restart clickhouse-server"` |
| `ClickHouseConfig.rbac_backup_always` | field | 198 | Default: `false` |
| `ClickHouseConfig.config_backup_always` | field | 202 | Default: `false` |
| `ClickHouseConfig.named_collections_backup_always` | field | 206 | Default: `false` |
| `ClickHouseConfig.rbac_resolve_conflicts` | field | 210 | Default: `"recreate"` |
| `ClickHouseConfig.config_dir` | field | 115 | Default: `"/etc/clickhouse-server"` |
| `ClickHouseConfig.data_path` | field | 112 | Default: `"/var/lib/clickhouse"` |

### CLI Flags (src/cli.rs)

| Symbol | Variant | Line | Notes |
|--------|---------|------|-------|
| `Command::Create.rbac` | bool | 59 | Already defined |
| `Command::Create.configs` | bool | 63 | Already defined |
| `Command::Create.named_collections` | bool | 67 | Already defined |
| `Command::Restore.rbac` | bool | 149 | Already defined |
| `Command::Restore.configs` | bool | 153 | Already defined |
| `Command::Restore.named_collections` | bool | 157 | Already defined |
| `Command::CreateRemote.rbac` | bool | 184 | Already defined |
| `Command::CreateRemote.configs` | bool | 188 | Already defined |
| `Command::CreateRemote.named_collections` | bool | 192 | Already defined |
| `Command::RestoreRemote.rbac` | bool | 230 | Already defined |
| `Command::RestoreRemote.configs` | bool | 235 | Already defined |
| `Command::RestoreRemote.named_collections` | bool | 239 | Already defined |

### Backup Entry Point (src/backup/mod.rs)

| Symbol | Type | Line | Signature |
|--------|------|------|-----------|
| `backup::create` | async fn | 64 | `(config, ch, backup_name, table_pattern, schema_only, diff_from, partitions, skip_check_parts_columns) -> Result<BackupManifest>` |

**Callers of `backup::create`** (4 sites):
1. `src/main.rs:154` -- Command::Create handler
2. `src/main.rs:308` -- Command::CreateRemote handler
3. `src/server/routes.rs:318` -- create_backup API handler
4. `src/server/routes.rs:652` -- create_remote API handler
5. `src/watch/mod.rs:409` -- watch loop

### Restore Entry Point (src/restore/mod.rs)

| Symbol | Type | Line | Signature |
|--------|------|------|-----------|
| `restore::restore` | async fn | 78 | `(config, ch, backup_name, table_pattern, schema_only, data_only, rm, resume, rename_as, database_mapping) -> Result<()>` |

**Callers of `restore::restore`** (4 sites):
1. `src/main.rs:260` -- Command::Restore handler
2. `src/main.rs:380` -- Command::RestoreRemote handler
3. `src/server/routes.rs:566` -- restore_backup API handler
4. `src/server/routes.rs:804` -- restore_remote API handler
5. `src/server/state.rs:386` -- auto_resume handler

### Upload Entry Point (src/upload/mod.rs)

| Symbol | Type | Line | Signature |
|--------|------|------|-----------|
| `upload::upload` | async fn | 165 | `(config, s3, backup_name, backup_dir, delete_local, diff_from_remote, resume) -> Result<()>` |

### Download Entry Point (src/download/mod.rs)

| Symbol | Type | Line | Signature |
|--------|------|------|-----------|
| `download::download` | async fn | 136 | `(config, s3, backup_name, resume) -> Result<PathBuf>` |

### ChClient Methods (src/clickhouse/client.rs)

| Symbol | Type | Line | Notes |
|--------|------|------|-------|
| `ChClient::execute_ddl` | async fn | 453 | Execute arbitrary DDL (used for functions, will be used for named collections) |
| `ChClient::get_macros` | async fn | 421 | Returns HashMap<String, String> from system.macros |

**Methods NOT yet present (need to be added for Phase 4e):**
- Query `system.users` for RBAC user objects
- Query `system.roles` for RBAC role objects
- Query `system.row_policies` for RBAC row policies
- Query `system.settings_profiles` for RBAC settings profiles
- Query `system.quotas` for RBAC quotas
- Query `system.named_collections` for named collection definitions

### Restore Schema Functions (src/restore/schema.rs)

| Symbol | Type | Line | Notes |
|--------|------|------|-------|
| `create_functions` | async fn | 721 | Iterates manifest.functions, executes DDL with ON CLUSTER support |

### Server Route Request Types (src/server/routes.rs)

| Symbol | Type | Line | Notes |
|--------|------|------|-------|
| `CreateRequest` | struct | 368 | Needs rbac/configs/named_collections fields |
| `CreateRemoteRequest` | struct | 735 | Needs rbac/configs/named_collections fields |
| `RestoreRequest` | struct | 615 | Needs rbac/configs/named_collections fields |
| `RestoreRemoteRequest` | struct | (after 756) | Needs rbac/configs/named_collections fields |

## Data Flow Analysis

### Backup Data Flow (create command)
```
CLI flags (--rbac, --configs, --named-collections)
  -> main.rs: currently warn and ignore
  -> backup::create(): currently does NOT receive these flags
  -> BackupManifest: functions=[], named_collections=[], rbac=None
```

**Required changes**:
1. Pass flags through to backup::create (or handle separately after create returns)
2. Query system tables for RBAC/named_collections data
3. Copy config files from config_dir
4. Store in local backup directory (access/, configs/)
5. Populate manifest fields

### Upload Data Flow
```
BackupManifest (with rbac/named_collections/configs populated)
  -> upload::upload(): reads manifest, uploads parts
  -> Currently does NOT handle access/ or configs/ directories
```

**Required changes**:
1. After uploading data parts, upload access/ directory contents
2. After uploading data parts, upload configs/ directory contents
3. Both are simple file uploads (no compression needed for small JSON/config files)

### Download Data Flow
```
download::download(): downloads manifest, then parts
  -> Currently does NOT handle access/ or configs/ directories
```

**Required changes**:
1. After downloading data parts, download access/ files
2. After downloading data parts, download configs/ files

### Restore Data Flow
```
restore::restore(): Phase 4 currently only handles functions
  -> create_functions(): executes manifest.functions DDL
```

**Required changes** (Phase 4 additions):
1. Named collections restore: CREATE NAMED COLLECTION DDL from manifest
2. RBAC restore: copy .jsonl files to CH access_data_path, create need_rebuild_lists.mark, chown
3. Config restore: copy config files to config_dir
4. Execute restart_command after RBAC/config restore

## Cross-Reference: Design Doc Sections

| Feature | Design Section | Manifest Field | Config Fields | CLI Flag |
|---------|---------------|----------------|---------------|----------|
| RBAC backup | 3.4, 5.6, 7 | `rbac` | `rbac_backup_always`, `rbac_resolve_conflicts` | `--rbac` |
| Config backup | 3.4, 5.6, 7 | implicit (configs/ dir) | `config_backup_always`, `config_dir` | `--configs` |
| Named collections | 5.6, 7 | `named_collections` | `named_collections_backup_always` | `--named-collections` |
| Restart command | 5.6, 12 | N/A | `restart_command` | N/A (config-only) |

## ClickHouse System Tables for RBAC (to be queried)

Per ClickHouse docs, these system tables contain RBAC objects:
- `system.users` -- user definitions (name, auth_type, etc.)
- `system.roles` -- role definitions
- `system.row_policies` -- row-level security policies
- `system.settings_profiles` -- settings profile definitions
- `system.quotas` -- resource quota definitions
- `system.named_collections` -- named collection definitions

Each returns the DDL via `SHOW CREATE USER/ROLE/etc.` or stored as JSON in access_data_path files.

## Access Data Path

ClickHouse stores RBAC data in `{data_path}/access/`:
- `*.jsonl` files for users, roles, policies, profiles, quotas
- `need_rebuild_lists.mark` triggers RBAC index rebuild on restart
- `*.list` files are the index files that need to be removed during restore

## File Modifications Summary

### Files That Need New Code
1. **New file**: `src/backup/rbac.rs` -- RBAC/named_collections/config backup logic
2. **New file**: `src/restore/rbac.rs` -- RBAC/named_collections/config restore logic + restart_command

### Files That Need Modification
1. `src/backup/mod.rs` -- Call RBAC/config/named_collections backup after table backup, populate manifest
2. `src/restore/mod.rs` -- Call RBAC/config/named_collections restore in Phase 4
3. `src/upload/mod.rs` -- Upload access/ and configs/ directories
4. `src/download/mod.rs` -- Download access/ and configs/ directories
5. `src/main.rs` -- Remove 12 "not yet implemented" warnings, pass flags through
6. `src/clickhouse/client.rs` -- Add RBAC/named_collections query methods
7. `src/server/routes.rs` -- Add rbac/configs/named_collections to request types
8. `src/manifest.rs` -- Potentially extend RbacInfo or add config tracking
9. `src/lib.rs` -- No changes needed (backup/restore modules already declared)
