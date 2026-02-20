# Preventive Rules Applied

## Rules Loaded From
- `.claude/skills/self-healing/references/root-causes.md` (35 rules)
- `.claude/skills/self-healing/references/planning-rules.md` (14 planning-specific rules)

## Applicable Rules for This Plan

| Rule | Applies? | How Addressed |
|------|----------|---------------|
| RC-006 (Unverified APIs) | YES | All APIs verified with grep: `classify_restore_tables`, `RestorePhases`, `create_tables`, `create_ddl_objects`, `engine_restore_priority`, `is_metadata_only_engine` |
| RC-008 (TDD sequencing) | YES | Engine detection function must be defined BEFORE classification uses it |
| RC-015 (Cross-task return type) | YES | `classify_restore_tables` returns `RestorePhases` -- postponed_tables is already `Vec<String>`, consistent type |
| RC-016 (Struct field completeness) | LOW | `RestorePhases.postponed_tables` already exists, no new struct needed |
| RC-018 (Explicit test steps) | YES | Each task must name test functions and assertions |
| RC-019 (Follow existing patterns) | YES | Must follow `engine_restore_priority()` match pattern, `classify_restore_tables()` loop pattern |
| RC-021 (Verify file locations) | YES | Verified: `RestorePhases` in `src/restore/topo.rs:51`, `classify_restore_tables` in `src/restore/topo.rs:61`, restore orchestration in `src/restore/mod.rs:62` |
| RC-032 (Data authority) | LOW | No tracking/calculation being added -- this is DDL ordering logic |

## Rules NOT Applicable

| Rule | Why |
|------|-----|
| RC-001 (Actor dependencies) | No actors in chbackup |
| RC-002 (Trust comments) | Pure function logic, no opaque types |
| RC-004 (Message handler) | No message system |
| RC-005 (Division by zero) | No division operations |
| RC-007 (Tuple field order) | No tuples involved |
| RC-010-014 (Kameo-specific) | No Kameo actors |
| RC-020 (Kameo derives) | No Kameo |
| RC-033-034 (tokio::spawn refs) | No actor refs |
