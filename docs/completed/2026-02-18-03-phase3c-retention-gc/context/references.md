# Symbol and Reference Analysis

## Phase 1: Symbol Verification

All symbols verified via file reads, grep, and LSP hover/incomingCalls.

### Existing Functions That This Plan Depends On

| Symbol | File | Line | Signature | Verified |
|--------|------|------|-----------|----------|
| `list_local` | src/list.rs | 81 | `pub fn list_local(data_path: &str) -> Result<Vec<BackupSummary>>` | YES |
| `list_remote` | src/list.rs | 125 | `pub async fn list_remote(s3: &S3Client) -> Result<Vec<BackupSummary>>` | YES |
| `delete_local` | src/list.rs | 217 | `pub fn delete_local(data_path: &str, backup_name: &str) -> Result<()>` | YES |
| `delete_remote` | src/list.rs | 248 | `pub async fn delete_remote(s3: &S3Client, backup_name: &str) -> Result<()>` | YES |
| `clean_broken_local` | src/list.rs | 292 | `pub fn clean_broken_local(data_path: &str) -> Result<usize>` | YES |
| `clean_broken_remote` | src/list.rs | 325 | `pub async fn clean_broken_remote(s3: &S3Client) -> Result<usize>` | YES |
| `S3Client::list_objects` | src/storage/s3.rs | 314 | `pub async fn list_objects(&self, prefix: &str) -> Result<Vec<S3Object>>` | YES (LSP hover) |
| `S3Client::delete_objects` | src/storage/s3.rs | 384 | `pub async fn delete_objects(&self, keys: Vec<String>) -> Result<()>` | YES (LSP hover) |
| `S3Client::get_object` | src/storage/s3.rs | 223 | `pub async fn get_object(&self, key: &str) -> Result<Vec<u8>>` | YES |
| `S3Client::list_common_prefixes` | src/storage/s3.rs | 273 | `pub async fn list_common_prefixes(&self, prefix: &str, delimiter: &str) -> Result<Vec<String>>` | YES |
| `S3Client::prefix` | src/storage/s3.rs | 137 | `pub fn prefix(&self) -> &str` | YES |
| `BackupManifest::from_json_bytes` | src/manifest.rs | 233 | `pub fn from_json_bytes(data: &[u8]) -> Result<Self>` | YES |
| `ChClient::get_disks` | src/clickhouse/client.rs | 375 | `pub async fn get_disks(&self) -> Result<Vec<DiskRow>>` | YES |
| `lock_for_command` | src/lock.rs | 116 | `pub fn lock_for_command(command: &str, backup_name: Option<&str>) -> LockScope` | YES |

### Existing Types This Plan Uses

| Type | File | Key Fields | Verified |
|------|------|-----------|----------|
| `BackupSummary` | src/list.rs:25 | name: String, timestamp: Option<DateTime<Utc>>, is_broken: bool, size: u64, compressed_size: u64, table_count: usize | YES |
| `BackupManifest` | src/manifest.rs:19 | name: String, timestamp: DateTime<Utc>, tables: HashMap<String, TableManifest> | YES |
| `TableManifest` | src/manifest.rs:86 | parts: HashMap<String, Vec<PartInfo>> | YES |
| `PartInfo` | src/manifest.rs:121 | name: String, backup_key: String, s3_objects: Option<Vec<S3ObjectInfo>> | YES |
| `S3ObjectInfo` | src/manifest.rs:150 | path: String, backup_key: String | YES |
| `S3Object` | src/storage/s3.rs:14 | key: String, size: i64, last_modified: Option<DateTime<Utc>> | YES |
| `DiskRow` | src/clickhouse/client.rs:46 | name: String, path: String, disk_type: String, remote_path: String | YES |
| `RetentionConfig` | src/config.rs:378 | backups_to_keep_local: i32, backups_to_keep_remote: i32 | YES |
| `Location` (list.rs) | src/list.rs:18 | enum { Local, Remote } | YES |
| `LockScope` | src/lock.rs:101 | enum { Backup(String), Global, None } | YES |
| `AppState` | src/server/state.rs | config: Arc<Config>, ch: ChClient, s3: S3Client, metrics: Option<Arc<Metrics>> | YES |

### Config Retention Fields

Both `general` and `retention` sections have retention fields:

| Field | Section | File:Line | Type | Default |
|-------|---------|-----------|------|---------|
| `backups_to_keep_local` | general | config.rs:51 | i32 | 0 |
| `backups_to_keep_remote` | general | config.rs:55 | i32 | 7 (via default fn) |
| `backups_to_keep_local` | retention | config.rs:381 | i32 | 0 |
| `backups_to_keep_remote` | retention | config.rs:385 | i32 | 0 |

**Resolution rule:** retention.* overrides general.* when non-zero.

## Phase 1.5: Call Hierarchy Analysis (LSP)

### Callers of `list_local` (8 incoming calls)

| Caller | File | Context |
|--------|------|---------|
| `list()` | src/list.rs:53 | Public list command |
| `clean_broken_local()` | src/list.rs:293 | Clean broken pattern -- template for retention_local |
| `list_backups()` | src/server/routes.rs:245 | API list endpoint |
| `refresh_backup_counts()` | src/server/routes.rs:1046 | Metrics scrape refresh |
| 4 test functions | src/list.rs | Unit tests |

### Callers of `list_remote` (4 incoming calls)

| Caller | File | Context |
|--------|------|---------|
| `list()` | src/list.rs:64 | Public list command |
| `clean_broken_remote()` | src/list.rs:326 | Clean broken pattern -- template for retention_remote |
| `list_backups()` | src/server/routes.rs:258 | API list endpoint |
| `refresh_backup_counts()` | src/server/routes.rs:1053 | Metrics scrape refresh |

### Callers of `delete_local` (5 incoming calls)

| Caller | File | Context |
|--------|------|---------|
| `delete()` | src/list.rs:209 | Public delete dispatcher |
| `clean_broken_local()` | src/list.rs:303 | Per-item deletion with warn on failure |
| `delete_backup()` | src/server/routes.rs:825 | API delete endpoint (via spawn_blocking) |
| 2 test functions | src/list.rs | Unit tests |

### Callers of `delete_remote` (3 incoming calls)

| Caller | File | Context |
|--------|------|---------|
| `delete()` | src/list.rs:210 | Public delete dispatcher |
| `clean_broken_remote()` | src/list.rs:336 | Per-item deletion with warn on failure |
| `delete_backup()` | src/server/routes.rs:829 | API delete endpoint |

### Key Observations for Plan

1. **`list_local` is sync, `list_remote` is async** -- retention_local can be sync, retention_remote must be async.

2. **`clean_broken_local` is the closest pattern** for `retention_local` -- same list-filter-delete loop, returns `Result<usize>`.

3. **`delete_remote` does naive delete** -- it lists all objects under the prefix and batch-deletes them. For GC-safe remote retention, we need a new function that only deletes unreferenced keys.

4. **`strip_s3_prefix()` is private** in list.rs (no `pub`) -- the GC function will be in the same module so it can access it directly. Confirmed at list.rs:435.

5. **Route wiring for `clean_stub`** is at `src/server/mod.rs:77`:
   ```rust
   .route("/api/v1/clean", post(routes::clean_stub))
   ```
   This will change to `post(routes::clean)`.

6. **Lock scope for `clean`** is already correct at lock.rs:124:
   ```rust
   "clean" | "clean_broken" | "delete" => LockScope::Global,
   ```

7. **CLI `Command::Clean`** already exists at cli.rs:286-290 with `--name` optional arg. The main.rs handler at line 370 is a stub that needs implementation.

## Verified Method Signatures for GC Key Collection

For the GC algorithm (design 8.2), we need to iterate all backup_key values from manifests:

```
BackupManifest.tables: HashMap<String, TableManifest>
  -> TableManifest.parts: HashMap<String, Vec<PartInfo>>
    -> PartInfo.backup_key: String
    -> PartInfo.s3_objects: Option<Vec<S3ObjectInfo>>
      -> S3ObjectInfo.backup_key: String
```

The manifest JSON key path for the backup being deleted: `"{backup_name}/metadata.json"`

All keys confirmed via file reads of manifest.rs.
