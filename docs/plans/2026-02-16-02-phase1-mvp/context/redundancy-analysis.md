# Redundancy Analysis

## New Public Components Proposed by Phase 1

Phase 1 introduces several new modules and public types. Verified against existing codebase for redundancy.

### Search Results

| Proposed | Existing Match | Decision | Justification |
|----------|---------------|----------|---------------|
| `pub mod backup` | None | N/A | New module, no existing equivalent |
| `pub mod upload` | None | N/A | New module, no existing equivalent |
| `pub mod download` | None | N/A | New module, no existing equivalent |
| `pub mod restore` | None | N/A | New module, no existing equivalent |
| `pub mod manifest` | None | N/A | New module, no existing equivalent |
| `pub mod list` | None | N/A | New module, no existing equivalent |
| `pub mod table_filter` | None | N/A | New module, no existing equivalent |
| `pub struct BackupManifest` | None | N/A | New struct |
| `pub struct TableManifest` | None | N/A | New struct |
| `pub struct PartInfo` | None | N/A | New struct |
| `pub fn compute_crc64(path)` | None | N/A | New function |
| `pub struct FreezeGuard` | None | N/A | New struct (scopeguard for UNFREEZE) |
| `pub struct TableFilter` | None | N/A | New struct |

### Existing Components That Phase 1 Extends (not replaces)

| Component | Location | Phase 1 Action |
|-----------|----------|---------------|
| `ChClient` | src/clickhouse/client.rs | EXTEND — add query methods for FREEZE/UNFREEZE, table listing, mutation check, ATTACH PART |
| `S3Client` | src/storage/s3.rs | EXTEND — add upload, download, list, delete methods |
| `ChBackupError` | src/error.rs | EXTEND — add BackupError, RestoreError, ManifestError variants |
| `lib.rs` | src/lib.rs | EXTEND — declare new modules |
| `main.rs` | src/main.rs | EXTEND — wire command implementations |

### Conclusion

No redundancy found. Phase 1 introduces entirely new functionality. All existing code is extended, not replaced. No REPLACE or COEXIST decisions needed.
