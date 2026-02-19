# Preventive Rules Applied

**Read from:** `.claude/skills/self-healing/references/root-causes.md`
**Read from:** `.claude/skills/self-healing/references/planning-rules.md`
**Applied at:** 2026-02-19 (Phase 4e plan creation -- plan-writer phase)

## Rules Checked

| Rule | Applicable | Status | Notes |
|------|-----------|--------|-------|
| RC-001 | No | N/A | No actor system in this project (plain async functions) |
| RC-002 | Yes | Applied | Verified all config types via reading config.rs directly |
| RC-003 | Yes | Applied | SESSION.md has full checklist, acceptance.json has all criteria |
| RC-004 | No | N/A | No actor/message system |
| RC-005 | No | N/A | No financial calculations in this phase |
| RC-006 | Yes | Applied | All method signatures verified from source: `backup::create()` (mod.rs:64), `restore::restore()` (mod.rs:78), `upload::upload()` (mod.rs:165), `download::download()` (mod.rs:136), `create_functions()` (schema.rs:721), `ChClient::execute_ddl()` (client.rs:453), `detect_clickhouse_ownership()` (attach.rs:713). New methods documented as NEW, not assumed to exist. |
| RC-007 | No | N/A | No tuple types involved |
| RC-008 | Yes | Applied | Tasks ordered: Task 1 (ChClient methods) before Task 2 (backup logic), Task 3 (upload/download) after Task 2, Task 4 (restore) after Tasks 1+3, Task 5 (wiring) after Tasks 2+4. |
| RC-015 | Yes | Applied | Identified cross-task data flows (backup -> manifest -> upload -> download -> restore). Types consistent: Vec<String> for named_collections, Option<RbacInfo> for rbac. |
| RC-016 | Yes | Applied | Identified all struct fields needed by downstream consumers. RbacInfo already defined. No new fields needed on manifest. |
| RC-017 | Yes | Applied | Verified existing fields in config.rs, manifest.rs, cli.rs at exact line numbers. |
| RC-018 | Yes | Applied | Each task has named test functions with specific assertions. |
| RC-019 | Yes | Applied | `restore_named_collections()` follows exact `create_functions()` pattern (schema.rs:721-755). ChClient queries follow `list_tables()` pattern (client.rs:262). Upload follows existing `put_object` pattern. |
| RC-020 | No | N/A | No Kameo actors |
| RC-021 | Yes | Applied | Verified actual file locations: Config in config.rs:98, BackupManifest in manifest.rs:19, ChClient in clickhouse/client.rs:14 |
| RC-022 | Yes | Applied | All required plan files created |
| RC-023 | Yes | Applied | SESSION.md has full phase checklist with both planning and execution phases |
| RC-029 | Yes | Applied | `backup::create()` and `restore::restore()` gain 3 new bool params each. All 5 callers of each identified and listed in wiring task. No sync->async changes. |
| RC-032 | Yes | Applied | ClickHouse system tables are authoritative source for RBAC data. See context/data-authority.md |
| RC-035 | Yes | Applied | Plan notes cargo fmt requirement |

## Key Findings

1. **RC-019 Pattern Reuse**: `create_functions()` (schema.rs:715-755) is the exact template for `restore_named_collections()`. Same pattern: sequential DDL, non-fatal failures, ON CLUSTER support.

2. **RC-006 API Verification**: All existing method signatures verified. New methods are clearly marked as NEW in the plan.

3. **RC-008 TDD Sequencing**: Task dependency ordering ensures every type/method is available before use. ChClient methods (Task 1) available for backup (Task 2) and restore (Task 4). Upload/download extension (Task 3) between backup and restore.

4. **RC-029 Signature Migration**: Both `backup::create()` (5 callers) and `restore::restore()` (5 callers) need parameter changes. All callers explicitly listed with file:line references.
