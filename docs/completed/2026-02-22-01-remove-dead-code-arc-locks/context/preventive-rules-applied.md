# Preventive Rules Applied

## Rules Loaded
- Root causes: `.claude/skills/self-healing/references/root-causes.md` (35 rules)
- Planning rules: `.claude/skills/self-healing/references/planning-rules.md` (14 rules)

## Applicable Rules for This Plan

| Rule | Relevance | Applied |
|------|-----------|---------|
| RC-006 | Plan code snippets must use verified APIs | N/A -- this plan removes code, no new code snippets |
| RC-019 | Follow existing patterns for similar code | N/A -- removing code, not adding |
| RC-021 | Verify struct/field file locations | YES -- verified all dead code locations with grep |
| RC-022 | Plan file structure | YES -- creating all required context files |
| RC-035 | Run cargo fmt before commits | YES -- will include in plan tasks |

## Inapplicable Rules (with reason)
- RC-001 through RC-005: Actor/message patterns -- no actors in this codebase
- RC-007, RC-008: TDD sequencing -- no new test code
- RC-010, RC-020: Kameo/adapter -- no actors
- RC-011: State machine flags -- not modifying state machines
- RC-012 through RC-014: E2E test patterns -- not modifying tests
- RC-015, RC-016, RC-017: Cross-task types -- single removal plan
- RC-032: Data authority -- not adding tracking fields
