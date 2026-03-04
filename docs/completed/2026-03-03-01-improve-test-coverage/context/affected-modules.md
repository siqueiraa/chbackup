# Affected Modules Analysis

## Summary

- **Modules modified:** 4 (src/main.rs, src/backup, src/download, src/restore)
- **Modules to create CLAUDE.md:** 0
- **Modules to update CLAUDE.md:** 0
- **Git base:** 421cc296

## Rationale for No CLAUDE.md Updates

All changes in this plan are strictly test-code additions inside existing `#[cfg(test)] mod tests` blocks and a one-line CI threshold change. Test modules do not affect the public API, architectural patterns, or data flow documented in CLAUDE.md files. CLAUDE.md updates are only warranted when new patterns, APIs, or tree changes occur.

## Files Being Modified

| File | Module | Change Type | Notes |
|------|--------|-------------|-------|
| src/main.rs | (root) | Add `#[cfg(test)] mod tests` | New test module for pure helper functions |
| src/backup/mod.rs | src/backup | Extend existing `mod tests` | New tests for normalize_uuid, parse_partition_list, etc. |
| src/download/mod.rs | src/download | Extend existing `mod tests` | New tests for sanitize_relative_path |
| src/restore/attach.rs | src/restore | Extend existing or add `mod tests` | New tests for uuid_s3_prefix, is_attach_warning, etc. |
| .github/workflows/ci.yml | CI | Change threshold value | 35 -> 55 |

## CLAUDE.md Tasks

None. No CLAUDE.md files need creation or update for test-only changes.
