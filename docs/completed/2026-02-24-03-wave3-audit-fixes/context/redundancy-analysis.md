# Redundancy Analysis

## New Public Components Proposed

| Proposed | Description |
|----------|-------------|
| `classify_backup_type(template, name) -> Option<&str>` | New helper function in watch/mod.rs to determine backup type from name |
| `WatchStartRequest` | New struct in server/routes.rs for optional JSON body |

## Search Results

### classify_backup_type

No existing function with this name or similar purpose found via grep search across src/:
- `resolve_name_template` substitutes `{type}` into a template, but does not perform the reverse operation
- `resolve_template_prefix` extracts the static prefix, but does not classify
- No `classify_*` functions exist in watch/mod.rs

**Decision: MUST IMPLEMENT** -- No equivalent exists. This is a new reverse-matching function that complements the existing `resolve_name_template`.

### WatchStartRequest

No existing watch start request type found:
- Existing request types: `CreateRequest`, `UploadRequest`, `DownloadRequest`, `RestoreRequest`, `RestoreRemoteRequest`, `CreateRemoteRequest`
- None of these are related to watch start

**Decision: MUST IMPLEMENT** -- No equivalent exists. Follows the established request type pattern.

## Summary Table

| Proposed | Existing Match | Decision | Task/Acceptance | Justification |
|----------|---------------|----------|-----------------|---------------|
| `classify_backup_type()` | None | MUST IMPLEMENT | W3-2 | Reverse of resolve_name_template; no equivalent exists |
| `WatchStartRequest` | None | MUST IMPLEMENT | W3-3 | New API input type; follows existing request type pattern |

## Notes

- W3-1, W3-4, W3-5 modify existing code only -- no new public API introduced
- Both new components follow established codebase patterns
