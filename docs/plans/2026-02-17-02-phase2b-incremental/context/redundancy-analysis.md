# Redundancy Analysis -- Phase 2b Incremental Backups

## Proposed New Components

| Proposed | Existing Match (fully qualified) | Decision | Task/Acceptance | Justification |
|---|---|---|---|---|
| `backup::diff::diff_parts()` | None | N/A | - | New comparison logic, no equivalent exists |
| `backup::diff::load_base_manifest()` | `BackupManifest::load_from_file()`, `BackupManifest::from_json_bytes()` | REUSE | - | Helper will COMPOSE existing methods, not duplicate. Wraps local vs remote dispatch. |
| `create_remote` handler in main.rs | `Command::CreateRemote` variant exists (cli.rs:168) but body is stub `info!("not implemented")` | EXTEND | - | Add implementation to existing match arm |
| Upload part filtering (skip carried) | No existing filtering by source | N/A | - | New conditional in existing `upload()` |

## Analysis Details

### diff_parts() -- New Function
- Searched for: `diff`, `compare_parts`, `match_parts`, `incremental`
- No existing part comparison function found in the codebase
- Decision: Must create new. Will be a pure function taking two manifests and returning part decisions.

### load_base_manifest() -- Composition of Existing
- `BackupManifest::load_from_file(path)` exists at src/manifest.rs:219
- `BackupManifest::from_json_bytes(data)` exists at src/manifest.rs:228
- `S3Client::get_object(key)` exists at src/storage/s3.rs:223
- Decision: REUSE these. The new helper simply dispatches between local (load_from_file) and remote (get_object + from_json_bytes).

### create_remote Command
- `Command::CreateRemote` CLI variant already defined in src/cli.rs:168-208
- Current handler in main.rs:280 is a stub: `info!(backup_name = ?backup_name, "create_remote: not implemented in Phase 1")`
- Decision: EXTEND the existing stub with actual implementation

### Upload Source Filtering
- Current upload processes all parts indiscriminately (src/upload/mod.rs:154-179)
- No existing source-based filtering
- Decision: Add conditional check `if part.source != "uploaded" { skip }` inside the work item construction loop

## Summary

No REPLACE or COEXIST decisions. All additions are either genuinely new functionality or extensions of existing stubs. No cleanup tasks needed.
