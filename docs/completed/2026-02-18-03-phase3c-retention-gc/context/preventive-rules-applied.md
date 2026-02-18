# Preventive Rules Applied

## Rules Loaded From
- `.claude/skills/self-healing/references/root-causes.md` (34 rules total)
- `.claude/skills/self-healing/references/planning-rules.md` (14 planning-specific rules)

## Applicable Rules for This Plan

| Rule ID | Title | Applicability | Check Result |
|---------|-------|---------------|--------------|
| RC-006 | Plan code snippets use unverified APIs | HIGH - new functions being added | Will verify all method references against actual codebase |
| RC-008 | TDD task sequencing violation | HIGH - multiple tasks building on each other | Will verify field/method dependencies across tasks |
| RC-015 | Cross-task return type mismatch | MEDIUM - retention functions consume list output | Verified: `list_local()` returns `Vec<BackupSummary>`, `list_remote()` returns same |
| RC-016 | Struct field completeness | MEDIUM - BackupSummary used for retention decisions | Verified: has `timestamp`, `name`, `is_broken` fields needed for sorting/filtering |
| RC-017 | State field declaration missing | LOW - no actor state, mostly standalone functions | N/A for this plan |
| RC-018 | TDD task missing explicit test steps | HIGH - every task needs tests | Will include test function names and assertions |
| RC-019 | Existing pattern not followed | HIGH - new list.rs functions should match existing patterns | Will study existing `clean_broken_local`/`clean_broken_remote` patterns |
| RC-021 | Struct/field file location assumed | HIGH - must verify where to add new functions | Verified: retention config in `src/config.rs:377-386`, list functions in `src/list.rs` |
| RC-032 | Adding tracking without verifying data source authority | MEDIUM - retention uses manifest data | See `context/data-authority.md` |

## Rules NOT Applicable

| Rule ID | Title | Reason |
|---------|-------|--------|
| RC-001 | Actor dependency wiring | No actors in this project (plain async functions) |
| RC-002 | Schema/type mismatch from comments | No complex types or enums being used |
| RC-004 | Message handler without sender | No actor message system |
| RC-010 | Adapter stub methods | No adapter pattern |
| RC-011 | State machine flags missing exit paths | No state machine flags |
| RC-020 | Kameo message derives | No Kameo actors |
| RC-029 | Async signature change without migration | Not changing existing async signatures |

## Key Decisions Based on Rules

1. **RC-019**: New retention functions in `list.rs` will follow the exact same signature and error handling pattern as `clean_broken_local()` and `clean_broken_remote()`.

2. **RC-006**: Every method call in plan code snippets will be verified against the actual codebase before inclusion. Key methods verified:
   - `list::list_local(data_path) -> Result<Vec<BackupSummary>>` (exists at list.rs:81)
   - `list::list_remote(s3) -> Result<Vec<BackupSummary>>` (exists at list.rs:125)
   - `list::delete_local(data_path, backup_name) -> Result<()>` (exists at list.rs:217)
   - `list::delete_remote(s3, backup_name) -> Result<()>` (exists at list.rs:248)
   - `S3Client::list_objects(prefix) -> Result<Vec<S3Object>>` (exists at s3.rs:314)
   - `S3Client::delete_objects(keys) -> Result<()>` (exists at s3.rs:384)
   - `S3Client::get_object(key) -> Result<Vec<u8>>` (exists at s3.rs:223)
   - `BackupManifest::from_json_bytes(data) -> Result<Self>` (exists at manifest.rs:233)
   - `ChClient::get_disks() -> Result<Vec<DiskRow>>` (exists at client.rs:375)

3. **RC-008**: Tasks will be ordered so that each task only uses types/functions that either exist in the codebase OR are defined in a preceding task.
