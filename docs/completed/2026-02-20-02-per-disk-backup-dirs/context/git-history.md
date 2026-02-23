# Git Context

## Recent Repository History

```
afa01dab docs: Archive completed plan 2026-02-20-01-phase8-polish-performance
6244ddf5 docs: Mark plan as COMPLETED
fbf32916 docs: update CLAUDE.md files for Phase 8 changes
a5de6b80 feat(server): wire ManifestCache into AppState with TTL and invalidation
52fb1f46 feat(upload): wire streaming multipart upload for large parts
7307bb05 feat(list): implement ManifestCache with TTL-based expiry
f707ad33 feat(server): wire rbac_size and config_size through to ListResponse
620a0c08 feat(backup): compute rbac_size and config_size during backup create
fa685435 feat(upload): add streaming compression for large part multipart upload
16773248 feat(server): add SIGQUIT handler for stack dump debugging
226e068f feat(server): add offset/limit pagination to tables endpoint
cfa1f44b feat(manifest): add rbac_size and config_size fields to BackupManifest and BackupSummary
c11d7794 chore: remove debug markers from list.rs
86f9298f docs: update CLAUDE.md files for Phase 7 changes
13d2e371 docs(design): update design doc for genuine Phase 6 improvements
e5af1a89 feat(storage): add PutObject/UploadPart retry with exponential backoff
e097c915 feat(config): expand env var overlay to cover 54 config fields
69d1f32d fix(config): change default ch_port from 9000 to 8123
dd2495e8 fix(config): revert Phase 6 defaults to design doc values
93f55e28 Merge branch 'claude/phase6-go-parity' into master
```

## File-Specific History

### src/backup/collect.rs
```
620a0c08 feat(backup): compute rbac_size and config_size during backup create
8a4ad4ab feat(backup): implement --skip-projections flag for projection filtering
4300d5e4 style: apply cargo fmt formatting across all modules
ff42860d feat(backup): add disk filtering via skip_disks and skip_disk_types
1050619b style: apply cargo fmt formatting across all modules
83a55532 feat(backup): add disk-aware shadow walk and actual disk name grouping
ad4802cb feat(backup): implement backup::create with FREEZE/shadow walk/hardlink/CRC64/UNFREEZE
```
Key insight: The disk-aware shadow walk (83a55532) was added in Phase 2c. This already iterates per-disk paths. Our change builds on this foundation.

### src/backup/mod.rs
```
620a0c08 feat(backup): compute rbac_size and config_size during backup create
4a769975 feat(backup,restore): wire freeze-by-part, partition restore, and replica sync check
ccc99bc2 feat(retry): wire jitter into all retry paths for Go parity
8a4ad4ab feat(backup): implement --skip-projections flag for projection filtering
4300d5e4 style: apply cargo fmt formatting across all modules
ff42860d feat(backup): add disk filtering via skip_disks and skip_disk_types
```

### src/upload/mod.rs
```
52fb1f46 feat(upload): wire streaming multipart upload for large parts
e5af1a89 feat(storage): add PutObject/UploadPart retry with exponential backoff
ccc99bc2 feat(retry): wire jitter into all retry paths for Go parity
4780b28e feat(upload,download): wire progress bar into parallel pipelines
4a8474b4 feat(upload,download): wire compression format through pipelines
3c31f3b0 feat(upload,download): add access/ and configs/ directory transfer
```

### src/download/mod.rs
```
ccc99bc2 feat(retry): wire jitter into all retry paths for Go parity
4780b28e feat(upload,download): wire progress bar into parallel pipelines
c32b0b0d feat(download): implement --hardlink-exists-files for part deduplication
4a8474b4 feat(upload,download): wire compression format through pipelines
3c31f3b0 feat(upload,download): add access/ and configs/ directory transfer
```

### src/restore/attach.rs + src/restore/mod.rs
```
cfa1f44b feat(manifest): add rbac_size and config_size fields
4a769975 feat(backup,restore): wire freeze-by-part, partition restore
ccc99bc2 feat(retry): wire jitter into all retry paths
76ee6e37 feat(restore): add ATTACH TABLE mode for Replicated engine tables
f345a175 feat(restore): add UUID-isolated S3 restore with same-name optimization
```

### src/list.rs
```
7307bb05 feat(list): implement ManifestCache with TTL-based expiry
c11d7794 chore: remove debug markers from list.rs
a49afda4 feat(list): add --format flag and latest/previous backup shortcuts
539ab56e feat(list): add incremental chain protection to retention_remote
```

## Branch Context

- **Current branch:** master
- **Main branch:** main
- **Commits ahead of main:** (master has diverged from main with Phase 6-8 work)
- **Last merge from feature branch:** 93f55e28 (Phase 6 Go parity merge)

## Key Historical Context for This Plan

1. **Phase 2c (83a55532)** introduced the disk-aware shadow walk that iterates ALL disk paths. This is the foundation our plan builds on -- the per-disk loop already exists, we just need to change the hardlink destination.

2. **Phase 2c (f345a175)** introduced UUID-isolated S3 restore. The S3 restore path in attach.rs reads from `backup_dir.join("shadow")` to find metadata files for rewriting. This path needs per-disk awareness.

3. **Phase 2c (1c758a5d)** introduced S3 disk metadata-only download in download/mod.rs. Downloads write S3 metadata to `backup_dir.join("shadow")`. This needs per-disk awareness too.

4. **Phase 5 (c32b0b0d)** introduced hardlink dedup in download. The `find_existing_part()` function scans `{data_path}/backup/*/shadow/` -- this needs to also scan per-disk dirs.

5. **Phase 2d (8d638446)** introduced resume state in upload. The `find_part_dir()` function needs per-disk awareness for the upload pipeline.

6. **Phase 4d (76ee6e37)** introduced ATTACH TABLE mode in restore/mod.rs. The `try_attach_table_mode()` function reads from `backup_dir.join("shadow")` -- needs per-disk path.
