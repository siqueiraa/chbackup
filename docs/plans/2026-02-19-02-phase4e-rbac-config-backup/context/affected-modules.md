# Affected Modules Analysis

## Summary

- **Modules to update:** 6
- **Modules to create:** 0
- **Root files to modify:** 4 (main.rs, manifest.rs, config.rs, cli.rs -- though config.rs and cli.rs need no changes)
- **Git base:** master

## Modules Being Modified

| Module | CLAUDE.md Status | Triggers | Action | Changes |
|--------|------------------|----------|--------|---------|
| src/backup | EXISTS | new_patterns | UPDATE | Add RBAC/config/named-collections collection to create() flow |
| src/restore | EXISTS | new_patterns | UPDATE | Add Phase 4 RBAC/config/named-collections restore |
| src/clickhouse | EXISTS | new_patterns | UPDATE | Add system table query methods for RBAC and named collections |
| src/upload | EXISTS | new_patterns | UPDATE | Upload access/ and configs/ directories to S3 |
| src/download | EXISTS | new_patterns | UPDATE | Download access/ and configs/ directories from S3 |
| src/server | EXISTS | new_patterns | UPDATE | Update rbac_size in list API response |

## Root Files Being Modified

| File | Changes |
|------|---------|
| src/main.rs | Wire rbac/configs/named_collections flags through to backup::create and restore::restore |
| src/manifest.rs | Possibly extend RbacInfo or add ConfigInfo struct for configs backup metadata |

## Files That Need NO Changes (Already Implemented)

| File | Why |
|------|-----|
| src/cli.rs | All CLI flags (--rbac, --configs, --named-collections) already defined |
| src/config.rs | All config fields (rbac_backup_always, config_backup_always, named_collections_backup_always, rbac_resolve_conflicts, restart_command) already defined and validated |

## Architecture Assumptions (VALIDATED)

### Component Ownership
- **BackupManifest**: Created by `backup::create()`, serialized to JSON by `save_to_file()`, uploaded by `upload::upload()`, downloaded by `download::download()`, consumed by `restore::restore()`
- **Config**: Loaded by `Config::load()` in main.rs, passed by reference to all command functions
- **ChClient**: Created in main.rs per-command, passed by reference to backup/restore functions (Clone for tokio::spawn)
- **S3Client**: Created in main.rs per-command, passed by reference to upload/download functions

### Data Flow for RBAC/Config Backup
```
create (backup/mod.rs):
  Query system.users/roles/etc -> serialize to access/*.jsonl files in backup_dir
  Copy config files from config_dir -> configs/ directory in backup_dir
  Query system.named_collections -> store DDL in manifest.named_collections
  Set manifest.rbac = Some(RbacInfo { path: ... })

upload (upload/mod.rs):
  Read manifest -> find access/ and configs/ directories in backup_dir
  Upload each file in access/ and configs/ to S3 (simple PutObject)
  Upload manifest.json last (existing atomic pattern)

download (download/mod.rs):
  Download manifest.json
  Download access/ and configs/ files from S3
  Write to local backup_dir

restore (restore/mod.rs):
  Phase 4 (after functions):
    If manifest.named_collections is non-empty: CREATE NAMED COLLECTION for each
    If manifest.rbac is Some: restore access files, create marker, chown, restart
    If configs/ exists in backup: copy to config_dir, restart
```

### What This Plan CANNOT Do
- Cannot restore RBAC to a remote ClickHouse (requires local filesystem access to access_data_path)
- Cannot guarantee restart_command will succeed (best-effort, errors logged and ignored per design)
- Cannot handle replicated RBAC via ZooKeeper (out of scope per design doc, which mentions it but doesn't require it)
- Cannot back up RBAC from ClickHouse versions that don't have system.users/roles/etc tables (very old versions)

## CLAUDE.md Tasks to Generate

1. **Update:** src/backup/CLAUDE.md (RBAC/config/named-collections collection pattern)
2. **Update:** src/restore/CLAUDE.md (Phase 4 RBAC/config/named-collections restore)
3. **Update:** src/clickhouse/CLAUDE.md (new query methods)
4. **Update:** src/upload/CLAUDE.md (access/ and configs/ upload)
5. **Update:** src/download/CLAUDE.md (access/ and configs/ download)
6. **Update:** src/server/CLAUDE.md (rbac_size in API response)
