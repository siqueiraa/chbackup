# Preventive Rules Applied

## Rules Loaded
- root-causes.md: 35 rules total (32 approved, 3 proposed)
- planning-rules.md: 14 planning-specific rules

## Applicable Rules for This Plan

### RC-006: Plan code snippets use unverified APIs
**Status:** APPLIED
**Relevance:** HIGH -- plan modifies routes.rs handler functions. All API calls (backup::create, upload::upload, etc.) verified to exist in their respective modules via reading routes.rs which already calls them.

### RC-019: Existing implementation pattern not followed for similar code
**Status:** APPLIED
**Relevance:** HIGH -- BUG-1 (post_actions dispatch) must follow the EXACT pattern used by existing endpoint handlers (create_backup, upload_backup, etc.) in routes.rs. Pattern documented below:
- `try_start_op(command)` -> `tokio::spawn` -> `finish_op/fail_op`
- Each handler clones `state`, loads config/ch/s3 via `.load()`, calls command function
- Metrics instrumentation on success/failure paths

### RC-021: Struct/field file location assumed without verification
**Status:** APPLIED
**Relevance:** MEDIUM -- Verified file locations:
- `BackupSummary`: `src/list.rs:46` (not a separate file)
- `ListParams`: `src/server/routes.rs:65`
- `ListResponse`: `src/server/routes.rs:73`
- `AppState`: `src/server/state.rs:65`
- `PartInfo`: `src/manifest.rs:129`
- `BackupManifest`: `src/manifest.rs:19`

### RC-032: Adding tracking/calculation without verifying data source authority
**Status:** APPLIED
**Relevance:** HIGH -- MISSING-2 (required field) and MISSING-4 (object_disk_size) require checking if data is already available in manifest fields vs. needing new computation. Verified:
- `PartInfo.source` field already has `"carried:{base}"` -- data EXISTS, just needs extraction
- `PartInfo.s3_objects` field already has `Vec<S3ObjectInfo>` with `size` -- data EXISTS, just needs summing

### RC-008: TDD task sequencing violation
**Status:** APPLIED
**Relevance:** MEDIUM -- BackupSummary field additions (MISSING-2, MISSING-4) must precede ListResponse usage changes. Fields must be added to struct before being referenced in `summary_to_list_response()`.

## Rules Not Applicable
- RC-001, RC-004, RC-020: No Kameo actors in this project (Rust backup tool, not trading)
- RC-010: No adapter pattern in this codebase
- RC-011: No state machine flags being added
- RC-012, RC-013, RC-014: No async test state issues
- RC-033, RC-034: No tokio::spawn with ActorRef captures
