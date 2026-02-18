# Pattern Discovery -- Phase 2b Incremental Backups

## Global Pattern Registry

No global pattern files found (`docs/patterns/` is empty). All patterns discovered locally from codebase analysis.

## Component Identification

| Component Type | Components |
|---|---|
| CLI Commands | `create` (modify), `upload` (modify), `create_remote` (implement) |
| Modules | `backup/mod.rs`, `upload/mod.rs`, `main.rs`, `cli.rs` |
| Data Structures | `BackupManifest`, `PartInfo`, `TableManifest` |
| Clients | `S3Client` (read manifest from S3), `ChClient` (unchanged) |
| Config | `Config` (no changes needed -- flags already defined in CLI) |
| Tests | Unit tests in `backup/mod.rs`, `upload/mod.rs`, integration test in `tests/` |

## Reference Implementations Analyzed

### Pattern 1: Backup Create Flow (src/backup/mod.rs)

```
Signature: pub async fn create(config, ch, backup_name, table_pattern, schema_only) -> Result<BackupManifest>
Flow:
  1. Get CH version + disk info
  2. List + filter tables
  3. Check mutations + sync replicas
  4. Create backup dir
  5. Parallel FREEZE+collect (via Arc<Semaphore> + tokio::spawn + try_join_all)
  6. Unfreeze all
  7. Build manifest with tables HashMap
  8. Save metadata.json
Returns: BackupManifest
```

Key insight for diff-from: The `collect_parts()` function in step 5 builds `PartInfo` entries with `source: "uploaded"` and `checksum_crc64`. Diff-from needs to compare these CRC64s against the base manifest's parts AFTER collect, and rewrite `source`/`backup_key` for carried parts.

### Pattern 2: Upload Flow (src/upload/mod.rs)

```
Signature: pub async fn upload(config, s3, backup_name, backup_dir, delete_local) -> Result<()>
Flow:
  1. Load manifest from local {backup_dir}/metadata.json
  2. Flatten parts into Vec<UploadWorkItem>
  3. Parallel compress+upload (semaphore + tokio::spawn + try_join_all)
  4. Apply results to manifest (table_key, disk_name, updated_part, compressed_size)
  5. Upload manifest LAST
  6. Optionally delete local
```

Key insight for diff-from-remote: Step 2 needs to skip parts where `source` starts with `"carried:"` (already on S3). Only parts with `source: "uploaded"` get compressed and uploaded. The `UploadWorkItem` filtering must check part source.

### Pattern 3: Manifest Loading (src/manifest.rs)

```
BackupManifest::load_from_file(path) -> Result<Self>  -- local file
BackupManifest::from_json_bytes(data) -> Result<Self>  -- from S3 bytes
S3Client::get_object(key) -> Result<Vec<u8>>           -- fetch manifest from S3
```

Key insight: Loading base manifest from S3 uses `s3.get_object("{base_name}/metadata.json")` -> `BackupManifest::from_json_bytes()`. This pattern already exists in download module.

### Pattern 4: S3 Key Format

```
{backup_name}/data/{url_encode(db)}/{url_encode(table)}/{part_name}.tar.lz4
{backup_name}/metadata.json
```

Carried parts retain the ORIGINAL backup's S3 key. Example:
- Original: `daily-sun/data/default/trades/202401_1_50_3.tar.lz4`
- Carried in `daily-mon`: backup_key = `daily-sun/data/default/trades/202401_1_50_3.tar.lz4` (same key)

### Pattern 5: Parallel Work Queue Pattern

Used consistently in create, upload, download, restore:
1. Flatten all work items into a Vec
2. `Arc<Semaphore>::new(concurrency)`
3. `tokio::spawn` each item
4. `futures::future::try_join_all` for fail-fast
5. Collect results, apply sequentially to shared state

No new pattern needed -- diff-from filtering happens BEFORE the work queue is built.

## Pattern Summary

This feature follows existing patterns completely. No new architectural patterns needed:
- Manifest loading: existing `from_json_bytes` + `S3Client::get_object`
- Part comparison: iterate parts HashMap, match by (table_key, disk_name, part_name)
- Upload filtering: skip `UploadWorkItem` creation for carried parts
- create_remote: composition of `create()` + `upload()` with shared manifest
