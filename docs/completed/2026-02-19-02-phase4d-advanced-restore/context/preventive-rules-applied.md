# Preventive Rules Applied

## Rules Loaded From
- `.claude/skills/self-healing/references/root-causes.md` (35 rules total)
- `.claude/skills/self-healing/references/planning-rules.md` (14 planning rules)

## Rules Applicable to Phase 4d

| Rule | Applies? | How Applied |
|------|----------|-------------|
| RC-002 (Schema/type mismatch) | YES | Verified all config field types, MutationInfo fields, TableManifest fields via source code reading |
| RC-006 (Unverified APIs in plan) | YES | All ChClient methods verified by reading client.rs; new methods flagged as "MUST ADD" |
| RC-008 (TDD sequencing) | YES | Dependencies noted -- new ChClient methods must precede consumer tasks |
| RC-015 (Cross-task return type mismatch) | YES | Data flows between classify/schema/attach verified |
| RC-016 (Struct field completeness) | YES | OwnedAttachParams fields verified, new fields needed for Mode A noted |
| RC-019 (Existing pattern not followed) | YES | Existing `create_tables()` and `execute_ddl()` patterns documented for extension |
| RC-021 (File location assumed) | YES | All struct locations verified via source: Config in config.rs, ChClient in clickhouse/client.rs, RestorePhases in restore/topo.rs |
| RC-032 (Data authority) | YES | MutationInfo already in manifest; ON CLUSTER/DatabaseReplicated info from system.databases |

## Rules NOT Applicable

| Rule | Why Not |
|------|---------|
| RC-001 (Actor wiring) | No actors in chbackup -- pure async functions |
| RC-004 (Message handler) | No actor message system |
| RC-010 (Adapter stubs) | No adapter pattern |
| RC-012-014 (E2E test) | Not writing E2E tests in this discovery phase |
| RC-020 (Kameo derives) | No Kameo |
| RC-033-034 (tokio::spawn refs) | No actor lifecycle concerns |
