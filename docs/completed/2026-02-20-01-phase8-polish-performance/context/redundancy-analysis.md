# Redundancy Analysis

## New Public Components Proposed

### Gap 1: rbac_size/config_size

| Proposed | Existing Match (fully qualified) | Decision | Task/Acceptance | Justification |
|----------|----------------------------------|----------|-----------------|---------------|
| `BackupManifest.rbac_size: u64` | None | N/A | - | New field on existing struct |
| `BackupManifest.config_size: u64` | None | N/A | - | New field on existing struct |
| `BackupSummary.rbac_size: u64` | None | N/A | - | New field on existing struct |
| `BackupSummary.config_size: u64` | None | N/A | - | New field on existing struct |
| `pub fn dir_size()` | `backup::collect::dir_size()` (private, src/backup/collect.rs:485) | REUSE | - | Make existing private fn public instead of creating new |

**Verified**: `dir_size()` at `src/backup/collect.rs:485` is currently `fn dir_size(path: &Path) -> Result<u64>` (private). Plan will make it `pub fn dir_size(path: &Path) -> Result<u64>` so `backup::rbac` can call `collect::dir_size()`.

### Gap 2: API tables pagination

| Proposed | Existing Match | Decision | Task/Acceptance | Justification |
|----------|---------------|----------|-----------------|---------------|
| `TablesParams.offset: Option<usize>` | None (no existing pagination in any endpoint) | N/A | - | New field on existing struct |
| `TablesParams.limit: Option<usize>` | None | N/A | - | New field on existing struct |

No new public functions or structs -- modifying existing query params struct and endpoint logic only.

### Gap 3: Remote manifest caching

| Proposed | Existing Match | Decision | Task/Acceptance | Justification |
|----------|---------------|----------|-----------------|---------------|
| `ManifestCache` struct | None | N/A | - | New struct (no equivalent exists in codebase) |
| `list_remote_cached()` | `list::list_remote()` | EXTEND | - | Add caching layer around existing function |

Decision: EXTEND `list_remote` with an optional cache parameter rather than creating a separate function. The cache struct itself is new and has no equivalent.

### Gap 4: SIGQUIT stack dump

| Proposed | Existing Match | Decision | Task/Acceptance | Justification |
|----------|---------------|----------|-----------------|---------------|
| SIGQUIT handler spawn | SIGHUP handler spawn (server/mod.rs:211-224) | Follow pattern | - | Same signal handling pattern, different signal kind |

No new public API -- just spawned background tasks following the existing SIGHUP pattern.

### Gap 5: Streaming multipart upload

| Proposed | Existing Match | Decision | Task/Acceptance | Justification |
|----------|---------------|----------|-----------------|---------------|
| `compress_part_streaming()` | `upload::stream::compress_part()` | COEXIST | - | Buffered for small parts, streaming for large |
| Streaming upload branch | Multipart branch in upload/mod.rs:518-561 | EXTEND | - | Replace buffer-then-chunk with true streaming |

**COEXIST justification**: `compress_part()` (buffered) remains optimal for small parts (<256MB uncompressed) where the overhead of pipe+thread coordination outweighs memory savings. `compress_part_streaming()` targets large parts only. Both serve distinct, permanent use cases. No cleanup deadline needed.

## Summary

- No REPLACE decisions (no existing code being superseded)
- 1 REUSE decision: make `backup::collect::dir_size()` public
- 2 EXTEND decisions: `list_remote` cache parameter, upload multipart streaming branch
- 1 COEXIST decision: buffered vs streaming compression (permanent, different size targets)
- Remaining are new fields on existing structs (no redundancy concern)
