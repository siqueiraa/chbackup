# References -- Phase 2b Incremental Backups

## Symbol and Reference Analysis

### 1. `backup::create()` -- src/backup/mod.rs:41

**Current signature:**
```rust
pub async fn create(
    config: &Config,
    ch: &ChClient,
    backup_name: &str,
    table_pattern: Option<&str>,
    schema_only: bool,
) -> Result<BackupManifest>
```

**Callers:**
- `src/main.rs:159` -- `Command::Create` handler:
  ```rust
  let _manifest = backup::create(&config, &ch, &name, tables.as_deref(), schema).await?;
  ```
- Future: `Command::CreateRemote` handler (currently stub at main.rs:280)

**What it calls (outgoing):**
- `ch.get_version()` -- ClickHouse version
- `ch.get_disks()` -- Disk info
- `ch.list_tables()` -- Table listing
- `mutations::check_mutations()` -- Pre-flight check
- `sync_replica::sync_replicas()` -- Replica sync
- `ch.freeze_table()` -- Per-table FREEZE
- `collect_parts()` -- Shadow walk + hardlink + CRC64
- `freeze_guard.unfreeze_all()` -- Cleanup
- `manifest.save_to_file()` -- Save metadata.json

**Required change:** Add `diff_from: Option<&str>` parameter. After `collect_parts()` returns, if `diff_from` is Some, load base manifest and compare parts by (table_key, disk_name, part_name, checksum_crc64). Rewrite matching parts' `source` and `backup_key` fields.

**Impact of signature change:**
- main.rs:159 -- Must pass `diff_from` value from CLI
- create_remote handler -- Must pass `None` for diff_from (create_remote uses diff_from_remote on the upload side)

---

### 2. `upload::upload()` -- src/upload/mod.rs:96

**Current signature:**
```rust
pub async fn upload(
    config: &Config,
    s3: &S3Client,
    backup_name: &str,
    backup_dir: &Path,
    delete_local: bool,
) -> Result<()>
```

**Callers:**
- `src/main.rs:191` -- `Command::Upload` handler:
  ```rust
  upload::upload(&config, &s3, &name, &backup_dir, delete_local).await?;
  ```
- Future: `Command::CreateRemote` handler

**What it calls (outgoing):**
- `BackupManifest::load_from_file()` -- Load manifest
- `find_part_dir()` -- Locate part directories
- `s3_key_for_part()` -- Generate S3 keys
- `stream::compress_part()` -- Tar + LZ4 compress
- `s3.put_object()` / `s3.create_multipart_upload()` -- Upload
- `rate_limiter.consume()` -- Rate limiting
- `s3.put_object_with_options()` -- Upload manifest

**Required change:** Two approaches:
- **Option A (preferred):** No signature change to `upload()`. The diff comparison already happened in `create()`, which set `source: "carried:..."` on matching parts. The upload function already reads the manifest from disk. Simply add a filter in the work item construction loop (line 154-179) to skip parts where `part.source.starts_with("carried:")`. The backup_key for carried parts is already set by `create()`.
- **Option B:** Add `diff_from_remote: Option<&str>` parameter and load remote manifest inside `upload()`. This would be needed if `--diff-from-remote` on the `upload` command needs to work WITHOUT a preceding `create --diff-from`. Per the design doc, `--diff-from-remote` is on `upload` and `create_remote`.

**Design decision needed:** Per design doc section 2 flag table:
- `--diff-from` is only on `create` (local base, comparison at create time)
- `--diff-from-remote` is on `upload` and `create_remote` (remote base, comparison at upload time)

This means `upload` DOES need diff-from-remote support -- it loads the remote base manifest from S3 and compares parts at upload time (since the local `create` may not have had access to S3).

**Impact:** `upload()` signature needs `diff_from_remote: Option<&str>` OR accept an optional pre-loaded base manifest.

---

### 3. `collect_parts()` -- src/backup/collect.rs:105

**Current signature:**
```rust
pub fn collect_parts(
    data_path: &str,
    freeze_name: &str,
    backup_dir: &Path,
    tables: &[TableRow],
) -> Result<HashMap<String, Vec<PartInfo>>>
```

**Callers:**
- `src/backup/mod.rs:270` -- Inside `tokio::spawn` in create():
  ```rust
  collect_parts(&data_path, &fname_for_collect, &backup_dir_clone, &tables_for_collect)
  ```

**No changes needed.** `collect_parts` already computes CRC64 for every part (line 199). The diff comparison happens after collect_parts returns, at the manifest level.

---

### 4. `Command::CreateRemote` -- src/cli.rs:168

**CLI definition:**
```rust
CreateRemote {
    tables: Option<String>,
    diff_from_remote: Option<String>,  // Already defined!
    delete_source: bool,
    rbac: bool,
    configs: bool,
    named_collections: bool,
    skip_check_parts_columns: bool,
    skip_projections: Option<String>,
    resume: bool,
    backup_name: Option<String>,
}
```

**Current handler (main.rs:280):**
```rust
Command::CreateRemote { backup_name, .. } => {
    info!(backup_name = ?backup_name, "create_remote: not implemented in Phase 1");
}
```

**Required change:** Replace stub with actual implementation: `create()` + `upload()` composition. The `diff_from_remote` flag gets passed to `upload()`.

---

### 5. `PartInfo` -- src/manifest.rs:116

**Fields relevant to diff-from:**
```rust
pub struct PartInfo {
    pub name: String,              // Part identity -- matched by name
    pub size: u64,                 // Uncompressed size
    pub backup_key: String,        // S3 key -- carried parts keep original backup's key
    pub source: String,            // "uploaded" or "carried:{base_backup_name}"
    pub checksum_crc64: u64,       // CRC64 of checksums.txt -- used for corruption detection
    pub s3_objects: Option<Vec<S3ObjectInfo>>,
}
```

**No changes needed.** All fields required for incremental diff are already present.

---

### 6. `BackupManifest` Loading -- src/manifest.rs

**Methods for base manifest loading:**
- `BackupManifest::load_from_file(path: &Path) -> Result<Self>` -- Line 219. For `--diff-from` (local base).
- `BackupManifest::from_json_bytes(data: &[u8]) -> Result<Self>` -- Line 228. For `--diff-from-remote` (S3 base, after `s3.get_object()`).

**Reference usage in download module:**
```rust
// src/download/mod.rs:82-89
let manifest_key = format!("{}/metadata.json", backup_name);
let manifest_bytes = s3.get_object(&manifest_key).await?;
let manifest = BackupManifest::from_json_bytes(&manifest_bytes)?;
```

This exact pattern will be reused for loading the remote base manifest.

---

### 7. `S3Client::get_object()` -- src/storage/s3.rs:223

**Signature:**
```rust
pub async fn get_object(&self, key: &str) -> Result<Vec<u8>>
```

**Callers in codebase:**
- `src/download/mod.rs:83` -- Download manifest
- `src/download/mod.rs:169` -- Download part data

**New caller:** `upload::upload()` when `diff_from_remote` is Some -- to load the base backup's manifest from S3.

---

### 8. `s3_key_for_part()` -- src/upload/mod.rs:60

**Signature:**
```rust
fn s3_key_for_part(backup_name: &str, db: &str, table: &str, part_name: &str) -> String
```

**Usage:** Called at line 168 to generate S3 keys for new parts. Carried parts do NOT need new keys -- they retain the original backup's key.

---

## Cross-Module Data Flow for Incremental

### Flow 1: `create --diff-from=base_name`
```
1. main.rs: parse diff_from from CLI
2. backup::create(): load base manifest from {data_path}/backup/{base_name}/metadata.json
3. After collect_parts(): compare current vs base parts by (table.disk.part_name, crc64)
4. For matches: set part.source = "carried:{base_name}", part.backup_key = base_part.backup_key
5. Save manifest (contains both uploaded and carried parts)
6. upload: skip parts with source.starts_with("carried:")
```

### Flow 2: `upload --diff-from-remote=base_name`
```
1. main.rs: parse diff_from_remote from CLI
2. upload::upload(): load base manifest from S3: s3.get_object("{base_name}/metadata.json")
3. In work item construction: compare current manifest parts vs base parts
4. For matches: set source = "carried:{base_name}", backup_key = base_part.backup_key
5. Skip carried parts in upload work queue
6. Upload only "uploaded" parts + updated manifest
```

### Flow 3: `create_remote --diff-from-remote=base_name`
```
1. main.rs: parse diff_from_remote from CLI
2. backup::create(): runs with diff_from=None (no local comparison needed)
3. upload::upload(): receives diff_from_remote, loads remote base, does comparison
4. Same as Flow 2 from step 3 onwards
```

---

## Functions That Need Signature Changes

| Function | File | Current Params | New Param | Callers to Update |
|---|---|---|---|---|
| `backup::create()` | src/backup/mod.rs:41 | config, ch, backup_name, table_pattern, schema_only | + `diff_from: Option<&str>` | main.rs:159, create_remote handler |
| `upload::upload()` | src/upload/mod.rs:96 | config, s3, backup_name, backup_dir, delete_local | + `diff_from_remote: Option<&str>` + `s3` already available | main.rs:191, create_remote handler |

## Functions That Need Logic Changes (No Signature Change)

| Function | File | Change |
|---|---|---|
| Work item construction loop | src/upload/mod.rs:154-179 | Skip parts where `source.starts_with("carried:")` |
| Manifest result application | src/upload/mod.rs:317-334 | Carried parts already in manifest -- no update needed |
| `Command::CreateRemote` handler | src/main.rs:280-282 | Replace stub with create() + upload() composition |
| `Command::Create` handler | src/main.rs:117-169 | Pass diff_from to create(), remove warn() |
| `Command::Upload` handler | src/main.rs:171-194 | Pass diff_from_remote to upload(), remove warn() |
