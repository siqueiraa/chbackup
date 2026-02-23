# Affected Modules Analysis

## Summary

- **Modules to update:** 5 (src/backup, src/download, src/upload, src/restore, src/storage)
- **Files to modify:** 1 (src/config.rs)
- **Files to create:** 1 (src/path_encoding.rs)
- **New CLAUDE.md needed:** 0 (path_encoding.rs is a single file, not a directory module)
- **Total affected:** 7

## Modules Being Modified

| Module | CLAUDE.md Status | Issues | Action |
|--------|------------------|--------|--------|
| src/backup | EXISTS | #5 (check_parts_columns), #7 (DRY) | UPDATE |
| src/download | EXISTS | #1 (path traversal), #7 (DRY) | UPDATE |
| src/upload | EXISTS | #7 (DRY) | UPDATE |
| src/restore | EXISTS | #1 (path traversal), #7 (DRY) | UPDATE |
| src/storage | EXISTS | #2 (cert verify), #3 (tests), #4 (disable_ssl) | UPDATE |
| src/config.rs | N/A (root file) | #6 (--env format) | MODIFY |

## New Files

| File | Purpose |
|------|---------|
| src/path_encoding.rs | Canonical path component encoder with sanitization (Issues 1+7) |
| src/lib.rs | Add `pub mod path_encoding;` declaration |

## CLAUDE.md Update Tasks

After implementation, update these CLAUDE.md files to document:

1. **src/backup/CLAUDE.md** -- Note check_parts_columns strict-fail behavior change
2. **src/download/CLAUDE.md** -- Note path sanitization via `path_encoding` module
3. **src/upload/CLAUDE.md** -- Note url_encode_component replaced by `path_encoding::encode_path_component`
4. **src/restore/CLAUDE.md** -- Note url_encode replaced by `path_encoding::encode_path_component`
5. **src/storage/CLAUDE.md** -- Note disable_cert_verification proper TLS config, disable_ssl wiring, hermetic tests
