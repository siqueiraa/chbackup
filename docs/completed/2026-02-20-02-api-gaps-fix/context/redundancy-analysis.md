# Redundancy Analysis

## New Public API Introduced

This plan does NOT introduce any new public structs, functions, or modules. All changes are modifications to existing code:

1. **BackupSummary** -- Adding 2 fields to existing struct (not a new struct)
2. **ListParams** -- Adding 3 fields to existing struct (not a new struct)
3. **list_backups()** -- Modifying existing handler return type (not a new function)
4. **summary_to_list_response()** -- Modifying existing function body (not a new function)
5. **post_actions()** -- Modifying existing handler body (not a new function)
6. **Signal handler** -- Adding code block in existing `start_server()` (not a new function)

## Decision

N/A -- no new public API introduced. Modifying existing code only.

| Proposed | Existing Match | Decision | Task/Acceptance | Justification |
|----------|---------------|----------|-----------------|---------------|
| (none -- no new public API) | - | N/A | - | Modifying existing code only |

## Helper Function Analysis

The plan may introduce a small private helper function for extracting the incremental base backup name from a manifest. Checking for existing equivalents:

- `collect_incremental_bases()` at list.rs:929 -- Iterates manifest parts and extracts `carried:` base names into a HashSet. This function downloads manifests from S3 and is async. For our use case (we already have the manifest loaded), we need a simpler sync extraction. This is NOT redundant because:
  - `collect_incremental_bases` is async, downloads manifests from S3
  - Our helper will operate on an already-loaded BackupManifest
  - Different return type (we need a single `String`, not `HashSet<String>`)
  - The pattern of `strip_prefix("carried:")` will be reused, not duplicated

Decision: COEXIST -- both serve different purposes (remote multi-manifest scan vs single loaded manifest extraction). No cleanup needed.
