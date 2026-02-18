# Plan: Phase 2d -- Resume & Reliability

## Goal

Implement resumable operations (upload, download, restore), post-download CRC64 verification, manifest atomicity, broken backup cleanup, ClickHouse TLS support, partition-level backup, disk filtering, parts column consistency check, and disk space pre-flight check. After this phase, chbackup is robust against mid-operation crashes and network failures.

## Architecture Overview

Phase 2d adds cross-cutting reliability features to the existing parallel pipeline architecture. No new modules are created; all changes fit within existing module boundaries. The key architectural additions are:

1. **Resume state files** (`*.state.json`) written alongside each operation, tracking completed parts. On `--resume`, completed parts are skipped. State file write failures are non-fatal warnings (design 16.1).
2. **Manifest atomicity**: upload to `.tmp` key, CopyObject to final key, delete `.tmp`. Crash between steps produces a "broken" backup.
3. **Post-download CRC64 verification**: reuse existing `compute_crc64` to verify downloaded parts against manifest checksums, with retry on mismatch.
4. **ChClient enhancements**: TLS certificate wiring, FREEZE PARTITION, system.parts query, parts_columns check, disk free_space query.
5. **Disk filtering**: skip parts on excluded disks during shadow walk.

## Architecture Assumptions (VALIDATED)

### Component Ownership
- **Config fields**: All Phase 2d config fields (TLS, skip_disks, check_parts_columns, use_resumable_state) already exist in `src/config.rs` -- verified at lines 91, 119-135, 147, 222-226.
- **CLI flags**: All Phase 2d CLI flags (`--resume`, `--partitions`, `--skip-check-parts-columns`) already exist in `src/cli.rs` -- verified via full read.
- **BackupSummary.is_broken**: Already defined at `src/list.rs:37` with `[BROKEN]` marker in `print_backup_table()`.
- **ChClient**: Uses `clickhouse-rs` crate HTTP interface. TLS is scheme-only (`https://`); the crate does NOT expose custom TLS certificate configuration directly. Must use `reqwest` or native TLS environment variables for custom certs.

### What This Plan CANNOT Do
- Cannot add streaming multipart upload (Phase 2a buffers, acceptable)
- Cannot implement Mode A restore (`--rm` DROP) -- Phase 4d
- Cannot implement table remap (`--as`) -- Phase 4a
- Cannot add server-mode auto-resume -- Phase 3a
- Cannot implement `--hardlink-exists-files` optimization in download -- deferred

## Risk Assessment

| Risk | Level | Mitigation |
|------|-------|------------|
| `clickhouse-rs` crate TLS cert config | YELLOW | The crate uses `reqwest` internally; custom certs may require env vars (`SSL_CERT_FILE`) or `reqwest` TLS builder. Verify crate API before implementing. |
| State file corruption on concurrent access | GREEN | PID lock prevents concurrent operations on same backup name. |
| CRC64 verification false negatives | GREEN | CRC64/XZ is a strong hash; false negatives extremely unlikely. |
| Disk space check accuracy | GREEN | Design allows 5% safety margin (`actual_free * 0.95`). |
| FREEZE PARTITION syntax across CH versions | GREEN | ALTER TABLE FREEZE PARTITION supported since CH 21.8 (our minimum). |
| Manifest atomicity CopyObject failure | GREEN | Leaves `.tmp` file which is cleaned by `clean_broken`. |

## Expected Runtime Logs

| Pattern | Required | Description |
|---------|----------|-------------|
| `Resuming upload: N/M parts already uploaded` | yes (if --resume with prior state) | Shows resume detection working |
| `Resuming download: N/M parts already downloaded` | yes (if --resume with prior state) | Shows resume detection working |
| `Resuming restore: N parts already attached` | yes (if --resume with prior state) | Shows resume detection working |
| `Failed to write resumable state:` | no (forbidden during normal ops) | State degradation warning -- should NOT appear normally |
| `Post-download CRC64 mismatch` | no (forbidden during normal ops) | CRC64 verification failure -- triggers retry |
| `Manifest uploaded atomically` | yes (on upload) | Confirms atomic manifest upload |
| `Broken backup detected` | yes (on list with broken backup) | Confirms broken detection |
| `clean_broken: deleted N broken backups` | yes (on clean_broken) | Confirms cleanup works |
| `Freezing partition` | yes (if --partitions used) | Confirms partition-level freeze |
| `Skipping disk` | yes (if skip_disks configured) | Confirms disk filtering |
| `Column type inconsistency detected` | yes (if check_parts_columns finds issues) | Confirms parts column check |
| `Disk space pre-flight` | yes (on download) | Confirms space check |
| `Building ClickHouse client.*secure=true` | yes (if clickhouse.secure configured) | Confirms TLS wiring |

## Known Related Issues (Out of Scope)

| Issue | Reason | Future Plan |
|-------|--------|-------------|
| `--hardlink-exists-files` download optimization | Complex CRC64 dedup logic, separate feature | Phase 3 or later |
| Remote metadata caching for `list remote` | Optimization, not reliability | Phase 3a (server mode) |
| Streaming multipart upload | Phase 2a already buffers, acceptable | Deferred |
| `restore_remote` command | Combines download+restore, separate workflow | Phase 4a |
| Retry with exponential backoff for upload/download | S3Client already has retry internally | Future enhancement |
| `freeze_by_part` / `freeze_by_part_where` | Advanced partition control, rarely needed | Phase 4 |

## Dependency Groups

```
Group A (Foundation -- Sequential):
  - Task 1: ClickHouse TLS support in ChClient
  - Task 2: New ChClient query methods (freeze_partition, query_system_parts, check_parts_columns, query_disk_free_space)
  - Task 3: Disk filtering in table_filter.rs and backup collect

Group B (Resume Infrastructure -- Sequential, depends on Group A):
  - Task 4: Resume state types and serialization helpers (new file: src/resume.rs)
  - Task 5: Upload resume + manifest atomicity
  - Task 6: Download resume + post-download CRC64 verification + disk space pre-flight
  - Task 7: Restore resume with system.parts query

Group C (Independent from Group B -- depends on Group A):
  - Task 8: Broken backup detection enhancement + clean_broken implementation
  - Task 9: Partition-level backup (--partitions flag)
  - Task 10: Parts column consistency check (check_parts_columns config)

Group D (Wiring -- depends on all above):
  - Task 11: Wire --resume and --partitions flags in main.rs
  - Task 12: Update CLAUDE.md for all modified modules (MANDATORY)
```

## Tasks

### Task 1: ClickHouse TLS Support in ChClient

**Goal**: Wire existing config fields (`secure`, `skip_verify`, `tls_key`, `tls_cert`, `tls_ca`) into `ChClient::new()` so connections use HTTPS with custom certificates.

**TDD Steps:**
1. Write unit test `test_ch_client_url_scheme_secure`: construct ChClient with `secure: true`, verify URL uses `https://`
2. Write unit test `test_ch_client_url_scheme_insecure`: construct ChClient with `secure: false`, verify URL uses `http://`
3. Implement TLS certificate handling in `ChClient::new()`:
   - When `secure: true`, scheme is already `https://` (existing code at line 62)
   - When `tls_ca` is non-empty, set `SSL_CERT_FILE` env var (for `reqwest` native-tls backend) OR use `clickhouse::Client` certificate builder if available
   - When `skip_verify: true`, set `DANGER_ACCEPT_INVALID_CERTS` or equivalent
   - When `tls_key`/`tls_cert` are set, configure client certificate authentication
4. Verify tests pass: `cargo test test_ch_client_url_scheme`

**Implementation Notes:**
- The `clickhouse-rs` crate (v0.13) uses `hyper-tls` or `reqwest` internally. Check the crate's feature flags and API for TLS configuration.
- If the crate does not expose TLS cert configuration, document the limitation and use env-var approach (`SSL_CERT_FILE`, `SSL_CERT_DIR`).
- Existing code at `src/clickhouse/client.rs:62` already switches scheme based on `config.secure`.

**Files:** `src/clickhouse/client.rs`
**Acceptance:** F001

---

### Task 2: New ChClient Query Methods

**Goal**: Add methods for FREEZE PARTITION, system.parts query, parts_columns consistency check, and disk free_space query.

**TDD Steps:**
1. Write unit test `test_freeze_partition_sql`: verify SQL generation for `ALTER TABLE FREEZE PARTITION`
2. Write unit test `test_query_parts_sql`: verify SQL for `SELECT FROM system.parts`
3. Write unit test `test_check_parts_columns_sql`: verify SQL for the batch column consistency query
4. Write unit test `test_disk_free_space_sql`: verify SQL for `SELECT FROM system.disks` with free_space
5. Implement methods:
   - `freeze_partition(&self, db: &str, table: &str, partition: &str, freeze_name: &str) -> Result<()>`
   - `query_system_parts(&self, db: &str, table: &str) -> Result<Vec<PartRow>>` -- returns active parts for a table
   - `check_parts_columns(&self, targets: &[(String, String)]) -> Result<Vec<ColumnInconsistency>>` -- batch query per design 3.3
   - `query_disk_free_space(&self) -> Result<Vec<DiskSpaceRow>>` -- adds `free_space` to disk query
6. Add new row types: `PartRow { name, partition_id, active }`, `ColumnInconsistency { database, table, column, types }`, `DiskSpaceRow { name, path, free_space }`
7. Add SQL helper: `freeze_partition_sql(db, table, partition, freeze_name) -> String`
8. Verify: `cargo test` passes for all new tests

**Implementation Notes:**
- `PartRow` uses `#[derive(Debug, Clone, clickhouse::Row, serde::Deserialize)]` following existing `TableRow` pattern
- The parts_columns query from design 3.3 skips Enum/Tuple/Nullable drift:
  ```sql
  SELECT database, table, name AS column, groupUniqArray(type) AS uniq_types
  FROM system.parts_columns
  WHERE active AND (database, table) IN (VALUES)
  GROUP BY database, table, column
  HAVING length(uniq_types) > 1
  ```
- `DiskSpaceRow` is separate from `DiskRow` to avoid breaking existing `get_disks()` callers

**Files:** `src/clickhouse/client.rs`
**Acceptance:** F002

---

### Task 3: Disk Filtering in table_filter.rs and backup collect

**Goal**: Add `is_disk_excluded()` function and wire disk filtering into shadow walk.

**TDD Steps:**
1. Write unit test `test_is_disk_excluded_by_name`: verify disk name exclusion
2. Write unit test `test_is_disk_excluded_by_type`: verify disk type exclusion
3. Write unit test `test_is_disk_excluded_empty_lists`: verify empty skip lists match nothing
4. Implement `is_disk_excluded(disk_name: &str, disk_type: &str, skip_disks: &[String], skip_disk_types: &[String]) -> bool` in `src/table_filter.rs`
5. Wire disk filtering into `backup::create()`: before processing collected parts, skip parts where `is_disk_excluded(disk_name, disk_type, ...)` returns true
6. Verify: `cargo test test_is_disk_excluded`

**Implementation Notes:**
- Follow existing `is_excluded()` and `is_engine_excluded()` patterns in `src/table_filter.rs`
- `skip_disks` uses exact name match (like `skip_table_engines`)
- `skip_disk_types` uses exact type match
- Config fields already exist: `config.clickhouse.skip_disks` and `config.clickhouse.skip_disk_types`

**Files:** `src/table_filter.rs`, `src/backup/mod.rs`
**Acceptance:** F003

---

### Task 4: Resume State Types and Serialization Helpers

**Goal**: Create shared resume state infrastructure: state file types, load/save helpers with degradation pattern, and hash-based invalidation.

**TDD Steps:**
1. Write unit test `test_upload_state_roundtrip`: serialize and deserialize `UploadState`
2. Write unit test `test_download_state_roundtrip`: serialize and deserialize `DownloadState`
3. Write unit test `test_restore_state_roundtrip`: serialize and deserialize `RestoreState`
4. Write unit test `test_save_state_degradation`: verify that `save_state_file` returns Ok even when path is unwritable (simulated), and logs warning
5. Write unit test `test_state_invalidation_on_param_change`: verify that loading state with different `params_hash` returns None
6. Implement in new file `src/resume.rs`:
   - `UploadState { completed_keys: HashSet<String>, backup_name: String, params_hash: String }`
   - `DownloadState { completed_keys: HashSet<String>, backup_name: String, params_hash: String }`
   - `RestoreState { attached_parts: HashMap<String, Vec<String>>, backup_name: String }`
   - `fn save_state_file<T: Serialize>(path: &Path, state: &T) -> Result<()>` -- atomic write (write to `.tmp`, rename)
   - `fn load_state_file<T: DeserializeOwned>(path: &Path) -> Result<Option<T>>` -- returns None if file doesn't exist
   - `fn save_state_graceful<T: Serialize>(path: &Path, state: &T)` -- calls `save_state_file`, on error warns but does not propagate (design 16.1)
   - `fn compute_params_hash(params: &[&str]) -> String` -- deterministic hash of operation parameters for invalidation
   - `fn delete_state_file(path: &Path)` -- remove state file (on successful completion), warn on error
7. Add `pub mod resume;` to `src/lib.rs`
8. Verify: `cargo test resume`

**Implementation Notes:**
- All state types derive `Serialize, Deserialize, Debug, Clone`
- `params_hash` is computed from relevant parameters (table pattern, partitions, backup name) so that re-running with different params invalidates stale state
- Atomic write pattern: write to `{path}.tmp`, then `std::fs::rename` -- prevents corrupt state on crash
- State degradation per design 16.1: `save_state_graceful()` catches all errors and logs `warn!("Failed to write resumable state: {e}. Operation continues but won't be resumable.")`

**Files:** `src/resume.rs` (new), `src/lib.rs`
**Acceptance:** F004

---

### Task 5: Upload Resume + Manifest Atomicity

**Goal**: Add resume state tracking to upload pipeline and implement atomic manifest upload (`.tmp` + CopyObject + delete).

**TDD Steps:**
1. Write unit test `test_upload_skips_completed_parts`: mock state with completed keys, verify they are skipped
2. Write unit test `test_manifest_atomicity_key_format`: verify `.tmp` key generation
3. Implement in `src/upload/mod.rs`:
   a. At start of `upload()`, check for `--resume` + `use_resumable_state`:
      - Load `UploadState` from `{backup_dir}/upload.state.json`
      - Validate `params_hash` matches current params; if not, warn and ignore stale state
      - Pass `completed_keys: HashSet<String>` to work queue construction
   b. In work queue construction: skip parts whose S3 key is in `completed_keys`
   c. After each successful part upload: add key to state, call `save_state_graceful()`
   d. Replace direct manifest upload (line 634) with atomic pattern:
      - Upload to `{backup_name}/metadata.json.tmp`
      - `s3.copy_object(bucket, "{prefix}/{backup_name}/metadata.json.tmp", "{backup_name}/metadata.json")`
      - `s3.delete_object("{backup_name}/metadata.json.tmp")`
   e. On successful completion: delete `upload.state.json`
4. Update `upload()` signature: add `resume: bool` parameter
5. Verify: `cargo test test_upload_skips` and `cargo test test_manifest_atomicity`

**Implementation Notes:**
- The `resume` flag must be passed from `main.rs` (Task 11 wires this)
- `params_hash` for upload: hash of `(backup_name, table_pattern, diff_from_remote)`
- Manifest atomicity uses existing `S3Client::copy_object()` and `S3Client::delete_object()`
- The `.tmp` key uses the same S3Client prefix as the final key
- On `copy_object`, source_bucket is `s3.bucket()`, source_key is `s3.full_key("{backup_name}/metadata.json.tmp")`

**Files:** `src/upload/mod.rs`
**Acceptance:** F005

---

### Task 6: Download Resume + Post-Download CRC64 Verification + Disk Space Pre-Flight

**Goal**: Add resume state tracking, CRC64 verification after decompression, retry on mismatch, and disk space pre-flight check.

**TDD Steps:**
1. Write unit test `test_download_skips_completed_parts`: mock state, verify skip
2. Write unit test `test_crc64_verification_pass`: download part, verify CRC64 matches manifest
3. Write unit test `test_crc64_verification_mismatch_retry`: simulate mismatch, verify retry
4. Write unit test `test_disk_space_preflight_sufficient`: verify pass when enough space
5. Write unit test `test_disk_space_preflight_insufficient`: verify error when not enough space
6. Implement in `src/download/mod.rs`:
   a. Disk space pre-flight (after manifest download, before data phase):
      - Call `ch.query_disk_free_space()` (requires ChClient -- add `ch: Option<&ChClient>` param or separate pre-flight function)
      - Actually: download does NOT currently take a ChClient. The pre-flight should query disk space via `statvfs` on the local `data_path` filesystem, not via ClickHouse. Design 16.3 says "query system.disks" but download doesn't have CH access. Use `nix::sys::statvfs::statvfs()` on `data_path` instead.
      - Compare required_space vs available * 0.95
      - Log and error if insufficient
   b. Resume state tracking:
      - Load `DownloadState` from `{backup_dir}/download.state.json`
      - Skip parts in `completed_keys`
      - After each successful download+decompress: add to state, `save_state_graceful()`
   c. Post-download CRC64 verification (after decompress, before marking complete):
      - Find `checksums.txt` in the decompressed part directory
      - Call `compute_crc64(checksums_path)`
      - Compare against `part.checksum_crc64` from manifest
      - On mismatch: delete corrupted part, log error, retry (up to `retries_on_failure`)
      - On persistent mismatch after retries: propagate error
   d. On successful completion: delete `download.state.json`
7. Update `download()` signature: add `resume: bool` parameter
8. Verify: `cargo test download::tests`

**Implementation Notes:**
- CRC64 verification reuses existing `compute_crc64()` from `crate::backup::checksum`
- Retry loop uses `config.general.retries_on_failure` (default 3) and `config.general.retries_pause`
- Disk space check uses `nix::sys::statvfs::statvfs` (already in dependencies via `nix` crate with `"fs"` feature)
- The `statvfs` approach is simpler and more accurate than querying ClickHouse's `system.disks` (which reports all disks, not just the backup target)
- `DownloadState.params_hash` computed from `(backup_name)`

**Files:** `src/download/mod.rs`, `src/download/stream.rs` (if CRC64 helper added)
**Acceptance:** F006

---

### Task 7: Restore Resume with system.parts Query

**Goal**: Add resume state tracking to restore pipeline, querying system.parts for already-attached parts.

**TDD Steps:**
1. Write unit test `test_restore_skips_already_attached`: mock state with attached parts, verify skip
2. Write unit test `test_restore_queries_system_parts`: verify that on resume, system.parts is queried
3. Implement in `src/restore/mod.rs` and `src/restore/attach.rs`:
   a. At start of `restore()`, check for `--resume`:
      - Load `RestoreState` from `{backup_dir}/restore.state.json`
      - For each table in manifest, query `ch.query_system_parts(db, table)` to get currently attached parts
      - Merge state file info with live system.parts data (system.parts is authoritative)
   b. In `attach_parts_owned()`: skip parts whose name is in the already-attached set
   c. After each successful ATTACH PART: add to state, `save_state_graceful()`
   d. On successful completion: delete `restore.state.json`
4. Update `restore()` signature: add `resume: bool` parameter
5. Verify: `cargo test restore::tests`

**Implementation Notes:**
- `query_system_parts()` returns `Vec<PartRow>` with `name` field
- The already-attached set is: union of (state file parts) and (system.parts active parts for target table)
- This handles the case where the state file is stale (e.g., ClickHouse dropped a table between runs)
- `RestoreState.attached_parts` is keyed by `"db.table"` -> `Vec<part_name>`

**Files:** `src/restore/mod.rs`, `src/restore/attach.rs`
**Acceptance:** F007

---

### Task 8: Broken Backup Detection Enhancement + clean_broken Implementation

**Goal**: Enhance broken backup display with reason, implement `clean_broken local` and `clean_broken remote` commands.

**TDD Steps:**
1. Write unit test `test_broken_backup_display_reason`: verify broken backups show reason (e.g., "metadata.json not found")
2. Write unit test `test_clean_broken_local`: create broken backup dir, run clean_broken, verify deleted
3. Write unit test `test_clean_broken_local_preserves_valid`: verify valid backups are not deleted
4. Implement:
   a. Add `broken_reason: Option<String>` field to `BackupSummary`
   b. Update `parse_backup_summary()` to set `broken_reason` (e.g., "metadata.json not found", "manifest parse error: {e}")
   c. Update `print_backup_table()` to show reason: `[BROKEN: {reason}]`
   d. Implement `clean_broken_local(data_path: &str) -> Result<usize>`:
      - Call `list_local()`, filter `is_broken`, delete each with `delete_local()`
      - Return count of deleted
   e. Implement `clean_broken_remote(s3: &S3Client) -> Result<usize>`:
      - Call `list_remote()`, filter `is_broken`, delete each with `delete_remote()`
      - Return count of deleted
   f. Add `pub async fn clean_broken(data_path, s3, location) -> Result<()>` that dispatches to local/remote
5. Verify: `cargo test list::tests`

**Implementation Notes:**
- `BackupSummary.broken_reason` is `Option<String>` -- None for valid backups, Some("...") for broken
- `list_remote()` already marks backups as broken when metadata.json is missing or unparseable
- `clean_broken` reuses existing `delete_local()` and `delete_remote()` functions
- Design 8.4: "Broken backups are excluded from retention counting and diff-from chain resolution" -- this is already the case since broken backups have no valid manifest

**Files:** `src/list.rs`
**Acceptance:** F008

---

### Task 9: Partition-Level Backup (--partitions Flag)

**Goal**: Wire `--partitions` flag to use `ALTER TABLE FREEZE PARTITION` per partition instead of whole-table FREEZE.

**TDD Steps:**
1. Write unit test `test_partition_list_parsing`: verify comma-separated partition names are parsed correctly
2. Write unit test `test_freeze_partition_called_for_each`: verify that with partitions set, `freeze_partition()` is called once per partition
3. Implement:
   a. Parse `--partitions` flag value as comma-separated partition IDs
   b. In `backup::create()`, pass partitions to the freeze phase
   c. In the per-table freeze task within `backup::mod.rs`: if partitions is set, call `ch.freeze_partition(db, table, partition, freeze_name)` for each partition instead of `ch.freeze_table()`
   d. Shadow walk proceeds identically (frozen parts end up in same shadow directory regardless of FREEZE vs FREEZE PARTITION)
4. Update `create()` signature: add `partitions: Option<&str>` parameter
5. Verify: `cargo test test_partition`

**Implementation Notes:**
- Per design 3.4: FREEZE PARTITION merges results into the same shadow directory as whole-table FREEZE
- The freeze_name is the same regardless of whether whole-table or per-partition
- Multiple partitions are frozen sequentially within a single table task (partition-level parallelism not needed)
- The `partitions` parameter format: comma-separated partition IDs (e.g., `"202401,202402"`)

**Files:** `src/backup/mod.rs`, `src/backup/freeze.rs`
**Acceptance:** F009

---

### Task 10: Parts Column Consistency Check

**Goal**: Wire `check_parts_columns` config to run pre-flight column consistency check before backup.

**TDD Steps:**
1. Write unit test `test_parts_columns_check_disabled`: verify check is skipped when config is false
2. Write unit test `test_parts_columns_check_skip_benign_types`: verify Enum/Tuple/Nullable drift is filtered out
3. Implement in `src/backup/mod.rs`:
   a. After listing tables, if `config.clickhouse.check_parts_columns` is true AND `!skip_check_parts_columns` CLI flag:
      - Build targets list: `Vec<(String, String)>` of (database, table) pairs
      - Call `ch.check_parts_columns(&targets)`
      - Filter out benign drift: Enum variants, Tuple element names, Nullable wrappers
      - If inconsistencies remain: log warning per table/column, optionally error (configurable)
   b. Wire `skip_check_parts_columns` flag through `create()` signature
4. Update `create()` signature: add `skip_check_parts_columns: bool` parameter
5. Verify: `cargo test test_parts_columns`

**Implementation Notes:**
- The check runs BEFORE FREEZE to avoid wasting time freezing tables that will fail on restore
- Design 3.3: "Skip Enum, Tuple, Nullable(Enum/Tuple), Array(Tuple) types"
- The filter is implemented in the caller, not in the SQL query, for clarity
- When inconsistencies are found: warn but continue (matching Go tool behavior). The `--skip-check-parts-columns` flag bypasses the check entirely.

**Files:** `src/backup/mod.rs`
**Acceptance:** F010

---

### Task 11: Wire --resume and --partitions Flags in main.rs

**Goal**: Remove "not yet implemented" warnings and pass flags to implementation functions.

**TDD Steps:**
1. Verify compilation: `cargo check` passes after wiring
2. Verify no remaining "not yet implemented" warnings for Phase 2d flags
3. Implement in `src/main.rs`:
   a. For `Command::Create`: pass `partitions`, `skip_check_parts_columns`, `resume` to `backup::create()` (resume not applicable to create in Phase 2d)
   b. For `Command::Upload`: pass `resume` to `upload::upload()`
   c. For `Command::Download`: pass `resume` to `download::download()`
   d. For `Command::Restore`: pass `resume` to `restore::restore()`
   e. For `Command::CleanBroken`: replace stub with `list::clean_broken()` call
   f. For `Command::CreateRemote`: pass `resume`, `partitions`, `skip_check_parts_columns` through
4. Verify: `cargo check` and `cargo test`

**Implementation Notes:**
- Some flags need to be passed through multiple layers (e.g., `resume` from main.rs through upload() signature)
- The `resume` behavior is gated by BOTH `--resume` CLI flag AND `config.general.use_resumable_state` (both must be true)
- `clean_broken` needs both `data_path` and `S3Client` -- create S3Client in the match arm

**Files:** `src/main.rs`
**Acceptance:** F011

---

### Task 12: Update CLAUDE.md for All Modified Modules (MANDATORY)

**Purpose**: Ensure all module documentation reflects code changes from this plan.

**Modules to update:** src/backup, src/clickhouse, src/download, src/restore, src/storage, src/upload

**TDD Steps:**
1. Read affected-modules.json for module list
2. For each module, regenerate directory tree
3. Detect and add new patterns:
   - Resume state tracking pattern
   - Manifest atomicity pattern
   - CRC64 post-download verification pattern
   - Disk filtering pattern
   - Partition-level freeze pattern
   - Parts column consistency check
4. Update public API sections with new function signatures
5. Validate all CLAUDE.md files have required sections: Parent Context, Directory Structure, Key Patterns, Parent Rules

**Files:**
- `src/backup/CLAUDE.md`
- `src/clickhouse/CLAUDE.md`
- `src/download/CLAUDE.md`
- `src/restore/CLAUDE.md`
- `src/storage/CLAUDE.md`
- `src/upload/CLAUDE.md`
- Root `CLAUDE.md` (update "Current Implementation Status" section)

**Acceptance:** FDOC

---

## Consistency Validation Results

| Check | Status | Notes |
|-------|--------|-------|
| RC-006 | PASS | All APIs verified: `ChClient::freeze_table`, `S3Client::copy_object`, `S3Client::delete_object`, `S3Client::put_object_with_options`, `compute_crc64`, `BackupManifest::to_json_bytes`, `list_local`, `list_remote`, `delete_local`, `delete_remote` |
| RC-008 | PASS | Task 4 (resume types) precedes Tasks 5-7 (resume consumers). Task 2 (ChClient methods) precedes Tasks 7, 9, 10 (consumers). |
| RC-011 | PASS | State files: created at start, updated per-part, deleted on success, left for resume on failure, write failures non-fatal |
| RC-015 | PASS | Task 4 defines UploadState/DownloadState/RestoreState consumed by Tasks 5-7. HashSet<String> is consistent. |
| RC-016 | PASS | UploadState.completed_keys used in Task 5 as HashSet<String>. DownloadState.completed_keys used in Task 6. RestoreState.attached_parts used in Task 7 as HashMap<String, Vec<String>>. |
| RC-017 | PASS | No self.X references -- this is not an actor system. All new fields are on new structs defined in Task 4. |
| RC-018 | PASS | Every task has named test functions with specific assertions. |
| RC-019 | PASS | Resume follows existing flat semaphore + try_join_all pattern. New ChClient methods follow existing TableRow/DiskRow derive pattern. Disk filtering follows existing is_excluded/is_engine_excluded pattern. |
| RC-021 | PASS | All config fields verified at exact line numbers in src/config.rs. ChClient methods added to src/clickhouse/client.rs. Disk filtering in src/table_filter.rs. |

## Notes

### Phase 4.5 Skip Justification
Phase 4.5 (Interface Skeleton Simulation) is skipped because:
- All changes are within existing functions (no new crate-level imports needed)
- The only new file is `src/resume.rs` with standard serde types
- All types used (HashSet, HashMap, Path, etc.) are stdlib/standard crate types already imported elsewhere
- New ChClient methods follow exact same pattern as existing methods (same derives, same crate)

### Edge Case Analysis (Async Operations)

| Operation | Failure Mode | Handling |
|-----------|-------------|----------|
| State file write failure | Disk full, permissions | warn + continue (design 16.1) |
| Manifest .tmp upload succeeds, CopyObject fails | Network error | Backup marked broken, cleaned by clean_broken |
| CRC64 mismatch after download | Corruption | Delete part, retry up to retries_on_failure |
| system.parts query fails during resume | CH down | Log error, fall back to state file only |
| FREEZE PARTITION on non-existent partition | CH error | Follows existing ignore_not_exists pattern |
| Disk space check on remote filesystem | statvfs may not work on NFS | Log warning, continue (best-effort) |
