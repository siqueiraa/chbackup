# Affected Modules Analysis

## Summary

- **New directories:** 4 (src/backup, src/upload, src/download, src/restore)
- **Existing directories to update:** 2 (src/clickhouse, src/storage)
- **New top-level files:** 3 (src/manifest.rs, src/list.rs, src/table_filter.rs)
- **Existing files to modify:** 4 (src/lib.rs, src/main.rs, src/error.rs, Cargo.toml)
- **Git base:** 880a640

## New Modules (CREATE)

| Module | Purpose | Key Files |
|--------|---------|-----------|
| src/backup/ | FREEZE, shadow walk, CRC64 checksum, mutations check | mod.rs, freeze.rs, mutations.rs, sync_replica.rs, checksum.rs, collect.rs |
| src/upload/ | Streaming compress + S3 upload pipeline | mod.rs, stream.rs |
| src/download/ | S3 download + streaming decompress pipeline | mod.rs, stream.rs |
| src/restore/ | Schema creation, hardlink, ATTACH PART, sort | mod.rs, schema.rs, attach.rs, sort.rs |

## Existing Modules Being Modified (UPDATE)

| Module | CLAUDE.md Status | Changes |
|--------|------------------|---------|
| src/clickhouse | MISSING | Add query methods: FREEZE/UNFREEZE, table listing, mutation check, SYSTEM SYNC REPLICA, ATTACH PART, version query. Add log_sql_queries wrapper. |
| src/storage | MISSING | Add S3 operations: upload (PutObject with ByteStream), download (GetObject streaming), list prefixes, delete objects, batch delete. |

## New Top-Level Files (CREATE)

| File | Purpose |
|------|---------|
| src/manifest.rs | BackupManifest, TableManifest, PartInfo, DatabaseInfo structs with serde serialization |
| src/list.rs | Local backup dir scanning + remote S3 prefix listing |
| src/table_filter.rs | Glob pattern matching for -t flag (db.*, *.table patterns) |

## Existing Top-Level Files Being Modified

| File | Changes |
|------|---------|
| src/lib.rs | Add `pub mod backup; pub mod upload; pub mod download; pub mod restore; pub mod manifest; pub mod list; pub mod table_filter;` |
| src/main.rs | Wire command match arms to new module entry points |
| src/error.rs | Add BackupError, RestoreError, ManifestError variants |
| Cargo.toml | Add: async-compression, tar, nix, glob, CRC64 crate |

## CLAUDE.md Tasks to Generate

1. **Create:** src/backup/CLAUDE.md (new module)
2. **Create:** src/upload/CLAUDE.md (new module)
3. **Create:** src/download/CLAUDE.md (new module)
4. **Create:** src/restore/CLAUDE.md (new module)
5. **Create:** src/clickhouse/CLAUDE.md (missing, module being extended)
6. **Create:** src/storage/CLAUDE.md (missing, module being extended)

## Architecture Assumptions (VALIDATED)

### Component Ownership

- **Config**: Created by `Config::load()` in main.rs, stored in `config` variable, passed by reference to all subsystems
- **ChClient**: Created in main.rs from `&config.clickhouse`, passed to backup/restore operations
- **S3Client**: Created in main.rs from `&config.s3`, passed to upload/download operations
- **BackupManifest**: Created by backup::create(), serialized to JSON for upload/local storage, deserialized by download/restore/list
- **PidLock**: Created in main.rs, held for duration of command, released on drop

### Data Flow

```
create: Config -> ChClient -> FREEZE -> walk shadow -> CRC64 -> UNFREEZE -> BackupManifest -> JSON file
upload: BackupManifest(JSON) -> read parts -> lz4 compress -> S3Client.put_object -> upload manifest last
download: S3Client.get_object(manifest) -> BackupManifest -> S3Client.get_object(parts) -> lz4 decompress -> local files
restore: BackupManifest(JSON) -> hardlink parts to detached/ -> ChClient.attach_part -> chown
list:    scan local dirs + S3Client.list -> display
delete:  rm local dir or S3Client.delete_objects
```

### What This Plan CANNOT Do

- **No parallel operations** — Phase 1 is sequential only. Parallelism is Phase 2.
- **No multipart upload** — Phase 1 uses single PutObject only. Multipart is Phase 2.
- **No S3 disk support** — Phase 1 handles local disk parts only. S3 disk is Phase 2c.
- **No incremental backup (--diff-from)** — Phase 1 is full backups only. Incremental is Phase 2b.
- **No resume (--resume)** — Phase 1 does not implement state files. Resume is Phase 2d.
- **No Mode A restore (--rm)** — Phase 1 implements Mode B only (non-destructive). Mode A is Phase 4d.
- **No table remap (--as)** — Phase 1 restores to original table names only. Remap is Phase 4a.
- **No RBAC/config backup** — Phase 1 skips RBAC/config. These are Phase 4e.
- **No rate limiting** — Phase 1 uploads/downloads without throttling. Rate limiting is Phase 2.
- **No partition-level freeze (--partitions)** — Phase 1 freezes whole tables. Partition-level is Phase 2d.
