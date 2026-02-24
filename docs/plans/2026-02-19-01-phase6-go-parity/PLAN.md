# PLAN: Phase 6 — Go Parity

Fix all gaps found from comprehensive Go clickhouse-backup comparison (8 parallel research agents).

**Design doc reference:** docs/design.md §12 (config), §3 (backup), §5 (restore), §8 (retention), §9 (API), §10 (watch), §2 (CLI)

---

## Task 1: Config Default Parity + Debug Flags

**Files:** `src/config.rs`, `src/logging.rs`, `src/clickhouse/client.rs`, `src/storage/s3.rs`

**Gap items:** Tier 1 (items 7,8) + Tier 4 (items 22-27)

**Changes:**
1. Change `default_ch_timeout()` from `"5m"` to `"30m"` (Go default)
2. Change `default_max_connections()` from `1` to `std::thread::available_parallelism().map(|n| n.get() as u32 / 2).unwrap_or(1).max(1)` (Go uses NumCPU/2)
3. Change `default_replica_path()` to include `{cluster}`: `"/clickhouse/tables/{cluster}/{shard}/{database}/{table}"`
4. Add `"_temporary_and_external_tables.*"` to `default_skip_tables()`
5. Change `check_parts_columns` default from `false` to `true`
6. Change default ACL from `""` to `"private"` (add `default_s3_acl()` function)
7. Wire `clickhouse.debug`: when true, set CH client to log all queries at DEBUG level
8. Wire `s3.debug`: when true, enable SDK-level debug logging for S3 operations

**Dependency group:** A

---

## Task 2: S3 ACL + Storage Class + Cert Verification

**Files:** `src/storage/s3.rs`, `src/config.rs`

**Gap items:** Tier 1 (items 1, 4, 20)

**Changes:**
1. Store `acl` in `S3Client` struct field
2. Apply `ObjectCannedAcl` to `put_object_with_options()`, `create_multipart_upload()`, and `copy_object()`
3. Before constructing `StorageClass` variant, uppercase the string: `storage_class.to_uppercase()` — lowercase values like `"standard"` produce `Unknown` SDK variant
4. Wire `disable_cert_verification`: when true, configure the HTTP client to skip TLS certificate verification (use `rustls` dangerous verifier or `hyper-rustls` config)

**Dependency group:** A

---

## Task 3: STS AssumeRole

**Files:** `src/storage/s3.rs`, `Cargo.toml`

**Gap items:** Tier 3 (item 15)

**Changes:**
1. Add `aws-sdk-sts` dependency to Cargo.toml
2. In `S3Client::new()`, after building base SDK config, check `config.assume_role_arn`
3. If non-empty: create STS client, call `assume_role()` with the ARN, extract temporary credentials (access_key, secret_key, session_token)
4. Use those credentials to build the S3 client instead of the base credentials
5. Log the assumed role ARN at info level

**Dependency group:** B

---

## Task 4: S3 Concurrency + Object Disk Path

**Files:** `src/storage/s3.rs`, `src/upload/mod.rs`, `src/backup/collect.rs`, `src/config.rs`

**Gap items:** Tier 1 (items 2, 3)

**Changes:**
1. **s3.concurrency**: Store in S3Client. Use as semaphore limit for concurrent multipart part uploads within a single file (currently sequential). In `upload_part()` callers, use `Arc<Semaphore>` with this concurrency limit.
2. **s3.object_disk_path**: When non-empty, use as the key prefix for S3 disk object references instead of the default path from metadata. In backup collect and upload, route S3 disk parts through this custom path.

**Dependency group:** B

---

## Task 5: Retry Jitter + Backup Retries

**Files:** `src/storage/s3.rs`, `src/upload/mod.rs`, `src/download/mod.rs`, `src/restore/mod.rs`

**Gap items:** Tier 1 (items 9, 10, 11)

**Changes:**
1. Create helper `fn effective_retries(config: &Config) -> (u32, Duration, f64)` that resolves `backup.retries_on_failure` vs `general.retries_on_failure` (backup overrides when > 0), similarly for retries_duration, retries_jitter
2. Parse `backup.retries_duration` (e.g., "10s", "1m") into `Duration`
3. Add jitter to all retry delays: `delay * (1.0 + rand::random::<f64>() * jitter_factor)` where jitter_factor comes from config
4. Apply to: `copy_object_with_retry()` backoff, upload retry loops, download CRC retry, restore retry loops
5. Add `rand` as dependency (or use simple PRNG without extra dep)

**Dependency group:** A

---

## Task 6: Freeze-by-Part + Backup Cleanup + Error Handling

**Files:** `src/backup/mod.rs`, `src/backup/freeze.rs`, `src/backup/collect.rs`

**Gap items:** Tier 1 (item 5) + Tier 3 (items 16, 17, 21)

**Changes:**
1. **freeze_by_part**: When `config.clickhouse.freeze_by_part` is true, iterate over partitions (from `system.parts`) and FREEZE each individually instead of whole-table FREEZE. Apply `freeze_by_part_where` as SQL filter on partition selection.
2. **partition_id="all"**: When `--partitions` contains `"all"`, treat it as "select all partitions" for unpartitioned tables (MergeTree without explicit partition key).
3. **Error 218 (CANNOT_FREEZE_PARTITION)**: In freeze error handling, add code 218 alongside existing 60, 81 checks. Log warning and continue (don't fail the backup for a single unfrozen partition).
4. **Backup failure cleanup**: On `backup::create()` error, before returning: (a) call `unfreeze_all()` (already done), (b) remove the local backup directory if it was created, (c) call `clean_shadow()` for this backup name.

**Dependency group:** C

---

## Task 7: Restore Partitions + Skip Empty + Replica Check

**Files:** `src/restore/mod.rs`, `src/restore/attach.rs`, `src/restore/schema.rs`

**Gap items:** Tier 1 (item 6) + Tier 2 (items 13, 14)

**Changes:**
1. **--partitions on restore**: Filter parts during ATTACH phase — only attach parts whose `partition_id` matches the `--partitions` list. Parse partition list from CLI, match against part names.
2. **--skip-empty-tables**: After filtering, skip tables that have zero parts to attach. Don't create the table DDL if skip_empty_tables is true and no parts would be attached.
3. **check_replicas_before_attach**: When true, before attaching parts to a Replicated table, query `system.replicas` to verify the table is in sync. If not in sync, log warning and continue (non-fatal per Go behavior).

**Dependency group:** C

---

## Task 8: Multipart CopyObject >5GB

**Files:** `src/storage/s3.rs`

**Gap items:** Tier 3 (item 18)

**Changes:**
1. Add `copy_object_multipart()` method using `UploadPartCopy` API
2. In `copy_object()`, check source object size via `head_object()`
3. If size > 5GB (5,368,709,120 bytes): use multipart copy (create multipart upload, UploadPartCopy in chunks, complete)
4. If size <= 5GB: use existing single CopyObject
5. Apply same SSE/storage_class/ACL settings as regular copy
6. Update `copy_object_with_retry()` to use the size-aware copy

**Dependency group:** B

---

## Task 9: Incremental Chain Protection

**Files:** `src/list.rs`

**Gap items:** Tier 3 (item 19)

**Changes:**
1. In `retention_remote()`, before deleting a backup, check if any KEPT backup has it listed as `diff_from_remote` in its manifest
2. Load manifests of all surviving backups, collect their `diff_from_remote` references
3. If the to-be-deleted backup is referenced as a base by any surviving backup, skip its deletion and log a warning
4. This prevents orphaned incremental backups that can't be restored

**Dependency group:** D

---

## Task 10: API Parity

**Files:** `src/server/routes.rs`, `src/server/state.rs`, `src/server/actions.rs`, `src/manifest.rs`

**Gap items:** Tier 5 (items 28-33)

**Changes:**
1. **POST /api/v1/actions**: Wire actual command dispatch — parse command string and call appropriate handler (create, upload, download, restore, delete, etc.) via `AppState` methods. Return operation_id.
2. **HTTP 423**: Change all `StatusCode::CONFLICT` (409) to `StatusCode::LOCKED` (423) for concurrent operation rejection, matching Go behavior.
3. **Health JSON**: Change GET /health from `"OK"` plain text to `Json({"status": "ok"})`.
4. **List fields**: Add `desc` (boolean, true=reverse sort) support to list API. Add `named_collection_size` (u64, scan named collections dir size if present).
5. **operation_id**: Generate UUID for each operation, include in action response.
6. **rbac_size/config_size**: Calculate actual directory sizes during backup create and store in manifest. Read from manifest in list response instead of hardcoded 0.

**Dependency group:** D

---

## Task 11: Watch Is Main Process

**Files:** `src/server/mod.rs`

**Gap items:** Tier 1 (item 12)

**Changes:**
1. In the watch loop exit handler (after watch loop task finishes), check `config.api.watch_is_main_process`
2. If true: trigger server shutdown (cancel the `CancellationToken`) so the entire process exits
3. If false: current behavior (just mark status inactive)
4. Add info-level log: "Watch loop exited, watch_is_main_process={}, shutting down={}"

**Dependency group:** D

---

## Task 12: List Format + Shortcuts

**Files:** `src/list.rs`, `src/cli.rs`, `src/main.rs`

**Gap items:** Tier 6 (items 34, 35)

**Changes:**
1. **--format flag**: Add `--format` option to `list` and `list_remote` commands accepting values: `default` (current table format), `json`, `yaml`, `csv`, `tsv`. Format output accordingly.
2. **latest/previous shortcuts**: When backup name argument is `"latest"`, resolve to most recent backup. When `"previous"`, resolve to second most recent. Apply to restore, restore_remote, upload, download, delete commands.

**Dependency group:** E

---

## Task 13: CLAUDE.md Update

**Files:** `CLAUDE.md`, `src/storage/CLAUDE.md`, `src/backup/CLAUDE.md`, `src/restore/CLAUDE.md`, `src/server/CLAUDE.md`, `src/list.rs` (if list gets CLAUDE.md)

Update all affected CLAUDE.md files to reflect:
- New patterns (STS AssumeRole, multipart CopyObject, freeze-by-part, etc.)
- Updated defaults
- New API behaviors (423 status, JSON health, actions dispatch)
- Remaining limitations section updates

**Dependency group:** F (depends on all previous groups)

---

## Dependency Groups

| Group | Tasks | Sequential? | Notes |
|-------|-------|-------------|-------|
| A | 1, 2, 5 | No | Config defaults + S3 wiring + retry jitter (independent files) |
| B | 3, 4, 8 | Yes | All touch `src/storage/s3.rs` — STS first, then concurrency, then multipart copy |
| C | 6, 7 | No | Backup pipeline + restore pipeline (different files) |
| D | 9, 10, 11 | No | Retention + API + watch (different files) |
| E | 12 | No | List CLI features |
| F | 13 | No | CLAUDE.md (depends on A-E) |
