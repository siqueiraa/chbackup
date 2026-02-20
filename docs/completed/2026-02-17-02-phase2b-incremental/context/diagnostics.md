# Diagnostics -- Phase 2b Incremental Backups

## Compiler Diagnostics

**Tool**: `cargo check` (equivalent to MCP diagnostics)
**Timestamp**: 2026-02-17
**Result**: CLEAN -- zero errors, zero warnings

```
Checking chbackup v0.1.0 (/Users/rafael.siqueira/dev/personal/chbackup)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.49s
```

## Existing Warnings

None. The codebase has a zero-warnings policy and currently passes cleanly.

## Implications for Phase 2b

1. **No pre-existing issues** to work around.
2. Signature changes to `backup::create()` and `upload::upload()` must maintain zero-warning status.
3. New parameters (diff_from, diff_from_remote) must all be used -- no unused variable warnings allowed.
4. The `diff_from` and `diff_from_remote` CLI flags are already parsed (cli.rs:47, cli.rs:89, cli.rs:176) and currently trigger `warn!()` messages in main.rs. These warn blocks must be replaced with actual implementation.

## Key Observations from Document Symbols

### src/backup/mod.rs
- `create()` -- Lines 38-420. Public entry point. Current signature: `(config, ch, backup_name, table_pattern, schema_only) -> Result<BackupManifest>`. Needs `diff_from: Option<&str>` parameter added.
- `is_metadata_only_engine()` -- Lines 422-438. Unchanged.
- 4 unit tests. None test diff-from behavior.

### src/upload/mod.rs
- `upload()` -- Lines 84-381. Public entry point. Current signature: `(config, s3, backup_name, backup_dir, delete_local) -> Result<()>`. Needs `diff_from_remote: Option<&str>` parameter added OR needs to skip carried parts based on manifest source field.
- `UploadWorkItem` -- Lines 70-81. Work item struct for parallel upload queue.
- `s3_key_for_part()` -- Line 57. Generates S3 keys.
- `find_part_dir()` -- Line 383. Locates part directories in backup staging.
- `should_use_multipart()` -- Line 34. Multipart decision.
- 7 unit tests. None test carried-part skipping.

### src/main.rs
- `Command::Create` handler -- Lines 117-169. Currently ignores `diff_from`.
- `Command::Upload` handler -- Lines 171-194. Currently ignores `diff_from_remote`.
- `Command::CreateRemote` handler -- Lines 280-282. Currently a stub: `info!("create_remote: not implemented")`.

### src/manifest.rs
- `PartInfo` -- Lines 114-141. Already has `source` (String, default "uploaded"), `backup_key` (String), `checksum_crc64` (u64).
- `BackupManifest::load_from_file()` -- Line 219. Loads from local path.
- `BackupManifest::from_json_bytes()` -- Line 228. Loads from byte slice (used for S3 downloads).
- `BackupManifest::to_json_bytes()` -- Line 235. Serializes to bytes.

### src/backup/collect.rs
- `collect_parts()` -- Lines 105-237. Returns `HashMap<String, Vec<PartInfo>>`. Already computes CRC64 at line 199.
- `parse_part_name()` -- Line 43. Parses part name format.
- `url_encode_path()` -- Line 24. URL encoding for file paths.

### src/storage/s3.rs
- `S3Client::get_object()` -- Line 223. Downloads full object to `Vec<u8>`.
- `S3Client::put_object()` -- Line 160. Uploads object.
- `S3Client::full_key()` -- Line 147. Prepends S3 prefix.
