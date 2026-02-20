# Preventive Rules Applied

## Rules Loaded From
- `.claude/skills/self-healing/references/root-causes.md` (35 rules)
- `.claude/skills/self-healing/references/planning-rules.md` (14 rules)

## Applicable Rules for This Plan

| Rule | Applicability | How Applied |
|------|--------------|-------------|
| RC-006 | HIGH - Plan will add new functions/types | Every new fn verified via grep before inclusion |
| RC-008 | HIGH - Multiple tasks with field dependencies | Task sequencing validated: fields defined before use |
| RC-015 | MEDIUM - Data flows between backup and restore | Verify TableManifest.dependencies flows correctly |
| RC-016 | HIGH - New structs (DependencyGraph) consumed by multiple tasks | All fields listed for downstream consumers |
| RC-017 | HIGH - New state fields for topo sort | Each field explicitly declared with initial value |
| RC-018 | MEDIUM - TDD tasks | Each behavior task has explicit test steps |
| RC-019 | HIGH - Adding new ChClient methods, new restore logic | Follow existing patterns exactly (list_tables, create_tables) |
| RC-021 | HIGH - Modifying multiple files | File locations verified via grep |
| RC-032 | LOW - No new tracking/calculation from data sources | N/A for this plan |

## Rules NOT Applicable

| Rule | Why |
|------|-----|
| RC-001/RC-004/RC-020 | No Kameo actors in this project |
| RC-010 | No adapter pattern |
| RC-011 | No state machine flags being added |
| RC-012/RC-013/RC-014 | No async tests with shared mutable state |
| RC-033/RC-034 | No tokio::spawn with actor refs |

## Key Preventive Checks for This Plan

1. **RC-006**: Verify `system.tables` has `dependencies_database` and `dependencies_table` columns (CH 23.3+)
2. **RC-008**: `TableManifest.dependencies` field already exists -- no sequencing issue
3. **RC-019**: New `list_tables_with_deps()` method MUST follow existing `list_tables()` pattern exactly
4. **RC-021**: Verify exact file locations: `TableManifest` in `manifest.rs`, `create_tables` in `schema.rs`, `is_metadata_only_engine` in `backup/mod.rs`
