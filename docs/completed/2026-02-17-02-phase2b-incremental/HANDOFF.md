# Handoff: Phase 2b -- Incremental Backups

## Plan Location
`docs/plans/2026-02-17-02-phase2b-incremental/`

## Directory Contents

| File | Purpose |
|------|---------|
| PLAN.md | Task definitions and TDD steps (5 tasks, 3 dependency groups) |
| SESSION.md | Status tracking and checklists |
| acceptance.json | 4-layer verification criteria (5 features: F001-F004, FDOC) |
| HANDOFF.md | This file -- resume context |
| context/patterns.md | Discovered patterns (existing codebase patterns reused) |
| context/symbols.md | Type verification (30+ symbols verified against source) |
| context/knowledge_graph.json | Structured JSON for symbol lookup |
| context/affected-modules.json | Module status (src/backup: update, src/upload: update) |
| context/affected-modules.md | Human-readable module summary |
| context/diagnostics.md | Compiler diagnostics baseline (zero errors, zero warnings) |
| context/references.md | Reference analysis for key symbols and callers |
| context/git-history.md | Git context (current branch: master) |
| context/redundancy-analysis.md | New components checked (no REPLACE decisions) |
| context/preventive-rules-applied.md | Applied rules verification |
| context/data-authority.md | Data source authority (CRC64 from existing PartInfo) |

## To Resume

1. Read SESSION.md for current status
2. Check Agent Execution Status table
3. Continue from last incomplete phase/agent
4. Use `/project-execute` when planning is complete

## Key References

### Files Being Modified
- `src/backup/diff.rs` (NEW) -- Pure diff_parts() function for incremental comparison
- `src/backup/mod.rs` -- Add `pub mod diff;`, modify `create()` signature to accept `diff_from: Option<&str>`
- `src/upload/mod.rs` -- Modify `upload()` signature to accept `diff_from_remote: Option<&str>`, add carried part skip logic
- `src/main.rs` -- Wire --diff-from to create(), --diff-from-remote to upload(), implement create_remote handler

### Files NOT Modified (verified)
- `src/manifest.rs` -- PartInfo already has source, backup_key, checksum_crc64
- `src/cli.rs` -- All flags already defined (--diff-from, --diff-from-remote, --delete-source)
- `src/config.rs` -- No new config params
- `src/storage/` -- Read-only usage

### Test Files
- `src/backup/diff.rs` (inline #[cfg(test)]) -- 6 unit tests for diff_parts() function

### Design Doc References
- Design doc section 3.5: Incremental Diff (--diff-from) -- Name+CRC64 comparison, self-contained manifests
- Design doc section 2: Flag reference table -- --diff-from on create, --diff-from-remote on upload/create_remote
- Design doc section 3.6: Upload -- Manifest uploaded last for atomicity
- Roadmap section 2b: Incremental Backups component list

## Commit History

| Task | Commit | Description |
|------|--------|-------------|
| 1 | 2e505b2 | feat(backup): add diff_parts() for incremental backup comparison |
| 2 | eb3e1cf | feat(backup): integrate --diff-from into create() for incremental backups |
| 3 | fcc701e | feat(upload): integrate --diff-from-remote into upload() for incremental uploads |
| 4 | dfe3541 | feat: implement create_remote command and wire --diff-from/--diff-from-remote |
| 5 | b241320 | docs: update CLAUDE.md for Phase 2b incremental backup changes |

## Architecture Summary

```
--diff-from (local base, create time):
  main.rs -> backup::create(diff_from=Some("base")) -> diff::diff_parts(manifest, base_manifest)
  Base loaded from: {data_path}/backup/{base_name}/metadata.json

--diff-from-remote (remote base, upload time):
  main.rs -> upload::upload(diff_from_remote=Some("base")) -> diff::diff_parts(manifest, base_manifest)
  Base loaded from: S3 {base_name}/metadata.json

create_remote (composition):
  main.rs -> backup::create(diff_from=None) -> upload::upload(diff_from_remote=Some("base"))
```
