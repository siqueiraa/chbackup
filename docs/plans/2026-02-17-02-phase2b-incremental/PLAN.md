# Plan: Phase 2b -- Incremental Backups (`--diff-from` and `--diff-from-remote`)

## Goal

Implement incremental backup support via `--diff-from` (local base, at create time) and `--diff-from-remote` (remote base, at upload time), plus the `create_remote` command that composes `create` + `upload`. Parts matching the base backup by name+CRC64 are carried forward (referencing the original S3 key), while new/changed parts are uploaded normally. Every manifest remains self-contained and independently restorable per design doc section 3.5.

## Architecture Overview

**Components modified:**
- `backup::create()` (src/backup/mod.rs) -- Add `diff_from: Option<&str>` parameter. After `collect_parts()` completes, load base manifest from local disk and compare parts by (table_key, disk_name, part_name, checksum_crc64). Matching parts get `source = "carried:{base_name}"` and `backup_key = base_part.backup_key`.
- `upload::upload()` (src/upload/mod.rs) -- Add `diff_from_remote: Option<&str>` and `s3: &S3Client` is already available. When set, load base manifest from S3 before building work queue. Apply same diff comparison. Skip carried parts in upload work queue.
- `src/main.rs` -- Wire CLI flags to function calls. Implement `create_remote` handler as `create()` + `upload()`.

**New code:**
- `src/backup/diff.rs` -- Pure function `diff_parts()` for comparing current manifest parts against a base manifest. Returns counts and mutates manifest in-place.

**No changes to:**
- `src/manifest.rs` -- PartInfo already has `source`, `backup_key`, `checksum_crc64` fields
- `src/cli.rs` -- All flags already defined (`--diff-from`, `--diff-from-remote`, `--delete-source`)
- `src/config.rs` -- No new config params needed
- `src/storage/` -- Read-only usage via existing `S3Client::get_object()`

## Architecture Assumptions (VALIDATED)

### Component Ownership
- `BackupManifest`: Created by `backup::create()`, read/updated by `upload::upload()`, serialized to local JSON and uploaded to S3
- `PartInfo.source` field: Set to `"uploaded"` by `collect_parts()` (default), changed to `"carried:{base_name}"` by `diff_parts()` for matching parts
- `PartInfo.backup_key` field: Initially empty from `collect_parts()`, set by `upload()` for uploaded parts, set by `diff_parts()` for carried parts (copies from base)
- `S3Client`: Created once per command in `main.rs`, passed by reference to upload/download functions

### What This Plan CANNOT Do
- Cannot verify carried parts still exist in S3 (that is a GC/retention concern in Phase 3c)
- Cannot handle S3 disk parts (Phase 2c -- only local disk parts supported)
- Cannot resume interrupted incremental uploads (Phase 2d)
- Cannot validate that the base backup name actually exists before FREEZE (we load the manifest after FREEZE for local, or at upload time for remote -- if the base is missing, we bail with a clear error)

## Risk Assessment

| Risk | Level | Mitigation |
|------|-------|------------|
| Base manifest not found (local) | GREEN | Clear error message: "Base backup '{name}' not found at {path}" |
| Base manifest not found (S3) | GREEN | `S3Client::get_object()` returns `Err` which propagates cleanly |
| CRC64 mismatch (same name, different data) | GREEN | Per design 3.5: upload as new + log warning. Already handled in diff logic |
| Signature change breaks callers | GREEN | Only 2 callers for `create()`, 1 for `upload()` -- all in main.rs, updated in same plan |
| Carried part's S3 object deleted by external tool | YELLOW | Out of scope (GC safety in Phase 3c), documented in "Known Related Issues" |
| Large base manifest fetch from S3 | GREEN | Single `get_object` call, manifest is small JSON (~100KB even for 10K parts) |

## Expected Runtime Logs

| Pattern | Required | Description |
|---------|----------|-------------|
| `Starting backup creation` | yes | Create command started |
| `Loading base manifest for diff-from` | yes (if diff_from used) | Base manifest being loaded |
| `Incremental diff complete` | yes (if diff_from used) | Summary after comparison |
| `Part .* has same name but different checksum` | no (warning, conditional) | CRC64 mismatch detection |
| `Starting upload` | yes | Upload command started |
| `Loading remote base manifest for diff-from-remote` | yes (if diff_from_remote used) | Remote base being loaded |
| `Skipping carried part` | no (debug level) | Individual part skip logging |
| `Upload complete` | yes | Upload finished |
| `ERROR:` | no (forbidden) | Should NOT appear in happy path |

## Known Related Issues (Out of Scope)

| Issue | Reason | Future Plan |
|-------|--------|-------------|
| Safe GC for shared S3 objects | Requires retention + reference counting | Phase 3c |
| S3 disk part handling | Needs CopyObject support | Phase 2c |
| Resume interrupted incremental upload | Needs state file tracking | Phase 2d |
| Watch mode incremental scheduling | Needs scheduler + full/incremental cycle | Phase 3a |
| `--partitions` flag | Independent of diff-from | Phase 2d |

## Dependency Groups

```
Group A (Sequential -- core diff logic):
  - Task 1: Create backup::diff module with diff_parts() pure function + unit tests
  - Task 2: Integrate --diff-from into backup::create() (depends on Task 1)

Group B (Sequential -- upload side, depends on Group A):
  - Task 3: Integrate --diff-from-remote into upload::upload() + skip carried parts
  - Task 4: Implement create_remote command in main.rs (depends on Task 3)

Group C (Wiring + docs, depends on Group B):
  - Task 5: Wire --diff-from in main.rs Create handler + remove Phase 1 warnings
  - Task 6: Update CLAUDE.md for all modified modules (MANDATORY)
```

## Tasks

### Task 1: Create `backup::diff` module with `diff_parts()` pure function + unit tests

**TDD Steps:**
1. Write failing tests in `src/backup/diff.rs`:
   - `test_diff_parts_no_base` -- `diff_parts()` with empty base manifest returns 0 carried, all parts remain `source: "uploaded"`
   - `test_diff_parts_all_match` -- All parts match by name+CRC64, all become `source: "carried:{base_name}"` with copied `backup_key`
   - `test_diff_parts_partial_match` -- Some parts match, some are new. Verify correct split.
   - `test_diff_parts_crc64_mismatch` -- Same part name but different CRC64: part stays `source: "uploaded"` (re-uploaded)
   - `test_diff_parts_multi_disk` -- Parts on different disks within same table are compared correctly (disk name must match too)
   - `test_diff_parts_extra_table_in_base` -- Base has a table not in current: gracefully ignored
2. Implement `diff_parts()`:
   ```rust
   // src/backup/diff.rs
   //! Incremental diff logic for --diff-from.
   //!
   //! Compares current backup parts against a base backup manifest.
   //! Parts matching by (table_key, disk_name, part_name, checksum_crc64)
   //! are marked as carried forward, referencing the original S3 key.

   use std::collections::HashMap;
   use tracing::{info, warn};
   use crate::manifest::{BackupManifest, PartInfo};

   /// Result of an incremental diff comparison.
   pub struct DiffResult {
       /// Number of parts carried forward from base.
       pub carried: usize,
       /// Number of parts that will be uploaded (new or changed).
       pub uploaded: usize,
       /// Number of parts with CRC64 mismatch (same name, different data).
       pub crc_mismatches: usize,
   }

   /// Compare current manifest parts against a base manifest and mark matching
   /// parts as carried forward.
   ///
   /// Mutates `current.tables[*].parts[*]` in place:
   /// - Matching parts: `source = "carried:{base_name}"`, `backup_key = base_part.backup_key`
   /// - Non-matching parts: unchanged (`source = "uploaded"`)
   pub fn diff_parts(current: &mut BackupManifest, base: &BackupManifest) -> DiffResult {
       // Build lookup: (table_key, disk_name, part_name) -> &PartInfo
       let mut base_lookup: HashMap<(&str, &str, &str), &PartInfo> = HashMap::new();
       for (table_key, table_manifest) in &base.tables {
           for (disk_name, parts) in &table_manifest.parts {
               for part in parts {
                   base_lookup.insert((table_key.as_str(), disk_name.as_str(), part.name.as_str()), part);
               }
           }
       }

       let base_name = &base.name;
       let mut carried = 0usize;
       let mut uploaded = 0usize;
       let mut crc_mismatches = 0usize;

       for (table_key, table_manifest) in &mut current.tables {
           for (disk_name, parts) in &mut table_manifest.parts {
               for part in parts.iter_mut() {
                   if let Some(base_part) = base_lookup.get(&(table_key.as_str(), disk_name.as_str(), part.name.as_str())) {
                       if part.checksum_crc64 == base_part.checksum_crc64 {
                           part.source = format!("carried:{}", base_name);
                           part.backup_key = base_part.backup_key.clone();
                           carried += 1;
                       } else {
                           warn!(
                               table = %table_key,
                               part = %part.name,
                               current_crc = part.checksum_crc64,
                               base_crc = base_part.checksum_crc64,
                               "Part has same name but different checksum, will re-upload"
                           );
                           crc_mismatches += 1;
                           uploaded += 1;
                       }
                   } else {
                       uploaded += 1;
                   }
               }
           }
       }

       info!(
           base = %base_name,
           carried = carried,
           uploaded = uploaded,
           crc_mismatches = crc_mismatches,
           "Incremental diff complete"
       );

       DiffResult { carried, uploaded, crc_mismatches }
   }
   ```
3. Add `pub mod diff;` to `src/backup/mod.rs` (after existing `pub mod sync_replica;` line)
4. Verify all 6 tests pass

**Files:**
- `src/backup/diff.rs` (NEW)
- `src/backup/mod.rs` (add `pub mod diff;`)

**Acceptance:** F001

**Implementation Notes:**
- `DiffResult` is a simple struct, no derives needed beyond default (not serialized)
- The lookup key is `(&str, &str, &str)` -- borrows from base manifest, no allocation
- `warn!()` for CRC64 mismatch matches design doc section 3.5 requirement
- Parts with `checksum_crc64 == 0` (empty checksums.txt) will match 0==0, which is correct -- both are empty parts

---

### Task 2: Integrate `--diff-from` into `backup::create()`

**TDD Steps:**
1. Modify `backup::create()` signature to add `diff_from: Option<&str>`:
   ```rust
   pub async fn create(
       config: &Config,
       ch: &ChClient,
       backup_name: &str,
       table_pattern: Option<&str>,
       schema_only: bool,
       diff_from: Option<&str>,
   ) -> Result<BackupManifest>
   ```
2. After the `table_manifests` HashMap is fully populated (after step 11 `// Build database list` in current code), and before building the final `BackupManifest`:
   - If `diff_from` is `Some(base_name)`:
     - Construct base manifest path: `{data_path}/backup/{base_name}/metadata.json`
     - Load with `BackupManifest::load_from_file()`, fail with clear error if not found
     - Call `diff::diff_parts()` on the newly built manifest (requires building manifest first, then diffing, then re-saving)
   - Actually, the diff must happen AFTER the manifest is built (step 13) but BEFORE saving (step 14). Restructure:
     - Build manifest (step 13)
     - If `diff_from` is Some: load base, call `diff_parts(&mut manifest, &base)`, log result
     - Save manifest (step 14)
3. Write unit test (no -- this requires ChClient which needs real ClickHouse. The logic is tested in Task 1 unit tests. Integration test would go in tests/ but that's not in scope for this plan.)
4. Verify `cargo check` passes with no warnings

**Files:**
- `src/backup/mod.rs` (modify `create()` signature and add diff-from logic)

**Acceptance:** F002

**Implementation Notes:**
- The `diff_from` logic is inserted between manifest construction (line ~372) and `manifest.save_to_file()` (line ~392)
- Use `use self::diff::diff_parts;` import at top of mod.rs
- Base manifest path: `PathBuf::from(&config.clickhouse.data_path).join("backup").join(base_name).join("metadata.json")`
- Error context: `.with_context(|| format!("Failed to load base backup '{}' for --diff-from", base_name))`
- Log before loading: `info!(base = %base_name, "Loading base manifest for diff-from");`

---

### Task 3: Integrate `--diff-from-remote` into `upload::upload()` + skip carried parts

**TDD Steps:**
1. Modify `upload::upload()` signature to add `diff_from_remote: Option<&str>`:
   ```rust
   pub async fn upload(
       config: &Config,
       s3: &S3Client,
       backup_name: &str,
       backup_dir: &Path,
       delete_local: bool,
       diff_from_remote: Option<&str>,
   ) -> Result<()>
   ```
2. After loading the local manifest (step 1, line ~119) but before building work items (step 2, line ~131):
   - If `diff_from_remote` is `Some(base_name)`:
     - Load remote base manifest: `let manifest_key = format!("{}/metadata.json", base_name);`
     - `let base_bytes = s3.get_object(&manifest_key).await.with_context(...)?;`
     - `let base = BackupManifest::from_json_bytes(&base_bytes).with_context(...)?;`
     - Call `diff_parts(&mut manifest, &base)` (reuse same function from Task 1)
     - Save updated manifest locally so the carried parts are recorded: `manifest.save_to_file(&manifest_path)?;`
3. In the work item construction loop (line ~154-179), add a check to skip carried parts:
   ```rust
   for part in parts {
       // Skip carried parts -- their data is already on S3 from the base backup
       if part.source.starts_with("carried:") {
           debug!(
               table = %table_key,
               part = %part.name,
               source = %part.source,
               "Skipping carried part (already on S3)"
           );
           continue;
       }
       // ... existing work item construction ...
   }
   ```
4. In the result application loop (step 4, line ~317-334), carried parts are already in the manifest with correct backup_key -- they don't need updating. But the compressed_size for carried parts should NOT be counted (they weren't uploaded by this command).
5. When uploading the final manifest (step 6), the manifest already has all parts (both uploaded and carried) with correct S3 keys.
6. Add unit test `test_skip_carried_parts_in_work_items` that verifies parts with `source: "carried:base"` are excluded from work item construction. (This can be a logic test on the filtering condition.)
7. Add `use crate::backup::diff::diff_parts;` to upload/mod.rs imports
8. Verify `cargo check` passes with no warnings

**Files:**
- `src/upload/mod.rs` (modify `upload()` signature, add diff-from-remote logic, add carried part skip)

**Acceptance:** F003

**Implementation Notes:**
- The `has_parts` flag should only be set for parts that will actually be uploaded (not carried)
- `table_count` should count tables with at least one uploaded part
- The total_parts log should distinguish: `info!("Uploading {} parts ({} carried, skipped) across {} tables", upload_count, carried_count, table_count)`
- Import: `use crate::backup::diff::diff_parts;`
- Import: add `use crate::manifest::BackupManifest;` -- already imported! No change needed.

---

### Task 4: Implement `create_remote` command + wire `--diff-from` in Create handler

**TDD Steps:**
1. Replace the `Command::CreateRemote { .. }` stub in main.rs with actual implementation:
   ```rust
   Command::CreateRemote {
       tables,
       diff_from_remote,
       delete_source,
       rbac,
       configs,
       named_collections,
       skip_check_parts_columns,
       skip_projections,
       resume,
       backup_name,
   } => {
       // Warn about unimplemented Phase 2+ flags
       if rbac { warn!("--rbac flag is not yet implemented, ignoring"); }
       if configs { warn!("--configs flag is not yet implemented, ignoring"); }
       if named_collections { warn!("--named-collections flag is not yet implemented, ignoring"); }
       if skip_check_parts_columns { warn!("--skip-check-parts-columns flag is not yet implemented, ignoring"); }
       if skip_projections.is_some() { warn!("--skip-projections flag is not yet implemented, ignoring"); }
       if resume { warn!("--resume flag is not yet implemented, ignoring"); }

       let name = resolve_backup_name(backup_name);
       let ch = ChClient::new(&config.clickhouse)?;
       let s3 = S3Client::new(&config.s3).await?;

       // Step 1: Create local backup (no local diff-from for create_remote)
       let _manifest = backup::create(
           &config,
           &ch,
           &name,
           tables.as_deref(),
           false, // schema_only
           None,  // diff_from (create_remote uses diff_from_remote on upload side)
       ).await?;

       // Step 2: Upload to S3 (with optional diff-from-remote)
       let backup_dir = PathBuf::from(&config.clickhouse.data_path)
           .join("backup")
           .join(&name);

       upload::upload(
           &config,
           &s3,
           &name,
           &backup_dir,
           delete_source,
           diff_from_remote.as_deref(),
       ).await?;

       info!(backup_name = %name, "CreateRemote command complete");
   }
   ```
2. Update `Command::Create` handler to pass `diff_from` to `backup::create()`:
   - Change `backup::create(&config, &ch, &name, tables.as_deref(), schema)` to `backup::create(&config, &ch, &name, tables.as_deref(), schema, diff_from.as_deref())`
   - Remove the `if diff_from.is_some() { warn!("--diff-from flag is not implemented..."); }` block
3. Update `Command::Upload` handler to pass `diff_from_remote` to `upload::upload()`:
   - Change `upload::upload(&config, &s3, &name, &backup_dir, delete_local)` to `upload::upload(&config, &s3, &name, &backup_dir, delete_local, diff_from_remote.as_deref())`
   - Remove the `if diff_from_remote.is_some() { warn!("--diff-from-remote flag is not implemented..."); }` block
4. Verify `cargo check` passes with no warnings
5. Verify `cargo test` passes (existing tests still work)

**Files:**
- `src/main.rs` (wire all three command handlers)

**Acceptance:** F004

**Implementation Notes:**
- `create_remote` = `create()` + `upload()` composition, per design doc section 2
- `create_remote` does NOT use `--diff-from` (local base). It uses `--diff-from-remote` on the upload side, because the whole point of create_remote is to go directly to S3
- `delete_source` maps to `delete_local` parameter of `upload()`
- `schema_only` is hardcoded `false` for create_remote (not available as a CLI flag per design doc)

---

### Task 5: Update CLAUDE.md for all modified modules (MANDATORY)

**Purpose:** Ensure all module documentation reflects code changes from this plan.

**Modules to update:** `src/backup`, `src/upload`

**TDD Steps:**

1. Update `src/backup/CLAUDE.md`:
   - Add `diff.rs` to Directory Structure
   - Add "Incremental Diff Pattern" to Key Patterns section:
     - `diff_parts(current, base) -> DiffResult`: pure function, compares by (table_key, disk_name, part_name, crc64)
     - Carried parts: `source = "carried:{base_name}"`, `backup_key` copied from base
     - CRC64 mismatch: re-upload + `warn!()` log
   - Update Public API section:
     - Add `diff_parts(current, base) -> DiffResult` -- Incremental comparison
   - Update `create()` signature to include `diff_from: Option<&str>`

2. Update `src/upload/CLAUDE.md`:
   - Add "Incremental Upload (--diff-from-remote)" to Key Patterns section:
     - Upload loads remote base manifest from S3 when `diff_from_remote` is set
     - Carried parts skipped in work queue construction (`.source.starts_with("carried:")`)
     - Manifest uploaded last still applies -- carried parts are already in manifest
   - Update Public API section:
     - Update `upload()` signature to include `diff_from_remote: Option<&str>`

3. Regenerate directory trees using `tree -L 2` for accuracy

4. Validate:
   - Both CLAUDE.md files have: Parent Context, Directory Structure, Key Patterns, Parent Rules sections

**Files:**
- `src/backup/CLAUDE.md`
- `src/upload/CLAUDE.md`

**Acceptance:** FDOC

---

## Consistency Validation Results

| Check | Status | Notes |
|-------|--------|-------|
| RC-006 | PASS | All APIs verified: `BackupManifest::load_from_file`, `BackupManifest::from_json_bytes`, `S3Client::get_object`, `diff_parts` (new, defined in Task 1) |
| RC-008 | PASS | Task 2 uses `diff_parts` from Task 1 (preceding). Task 3 uses `diff_parts` from Task 1 (preceding). Task 4 uses signatures from Tasks 2+3 (preceding). |
| RC-015 | PASS | `diff_parts()` takes `&mut BackupManifest` and `&BackupManifest`, returns `DiffResult`. Consumers in Tasks 2 and 3 use this correctly. |
| RC-016 | PASS | `DiffResult` has `carried: usize`, `uploaded: usize`, `crc_mismatches: usize`. Only used for logging -- fields sufficient. |
| RC-017 | N/A | No actor state fields. |
| RC-018 | PASS | Task 1 has 6 named tests with specific inputs and assertions. Tasks 2-4 are integration-level changes verified by compilation and existing tests. |
| RC-019 | PASS | Remote manifest loading follows exact pattern from `src/download/mod.rs:82-89`. |
| RC-021 | PASS | All file locations verified: `PartInfo` at `src/manifest.rs:116`, `BackupManifest` at `src/manifest.rs:19`, `create()` at `src/backup/mod.rs:41`, `upload()` at `src/upload/mod.rs:96`. |

## Notes

**Phase 4.5 (Interface Skeleton) -- SKIPPED:**
Skip reason: All types used already exist in the codebase. The only new type (`DiffResult`) is defined within the plan. No new imports from external crates. All existing function signatures verified via source reading. A skeleton stub would not catch any errors beyond what `cargo check` validates after each task.

**Anti-Overengineering Notes:**
- No new config params needed
- No new CLI flags needed (all already defined)
- No manifest schema changes
- `DiffResult` is a plain struct, not an enum or trait -- simplest possible return type
- `diff_parts()` is a pure function (no I/O), easily testable
