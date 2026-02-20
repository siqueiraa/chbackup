# Preventive Rules Applied

**Plan:** 2026-02-20-01-go-parity-gaps
**Rules source:** `.claude/skills/self-healing/references/root-causes.md`
**Rules read:** Yes (35 rules total, 32 approved, 3 proposed)

## Applicable Rules for This Plan

| Rule ID | Title | Applied? | How |
|---------|-------|----------|-----|
| RC-006 | Plan code snippets use unverified APIs | YES | All code snippets reference existing functions verified via grep. No new APIs invented. Plan describes changes at the signature level, not full implementations. |
| RC-008 | TDD task sequencing | YES | Tasks ordered so config defaults (Task 1) precede CLI flags (Task 2) which precede implementation tasks. Each task only references fields that exist or were added in a preceding task. |
| RC-015 | Cross-task return type mismatch | N/A | No cross-task data flows - each task is self-contained modification of existing code. |
| RC-016 | Struct field completeness | YES | Config struct additions in Task 1 are verified against usage in subsequent tasks. |
| RC-017 | State field declaration missing | YES | All new fields are declared in their respective tasks before being used. |
| RC-018 | TDD task missing explicit test steps | YES | Every task has explicit test names with inputs and expected assertions. |
| RC-019 | Existing pattern not followed | YES | All changes follow existing patterns in the codebase (e.g., new CLI flags follow existing clap derive patterns, env overlays follow existing apply_env_overlay pattern). |
| RC-021 | Struct/field file location assumed | YES | All struct locations verified via the actual source files. Config is in src/config.rs, CLI is in src/cli.rs, etc. |
| RC-022 | Plan file structure incomplete | YES | All required files created: PLAN.md, SESSION.md, acceptance.json, HANDOFF.md, context/*.md |
| RC-035 | cargo fmt not run before committing | YES | Noted in PLAN.md that each task must run cargo fmt before committing. |

## Rules Not Applicable

| Rule ID | Reason |
|---------|--------|
| RC-001, RC-004, RC-020 | No Kameo actors - this is a CLI/server tool, not an actor system |
| RC-002, RC-007 | No tuple types or type comments being trusted |
| RC-010 | No adapter stubs |
| RC-011 | No state machine flags being added |
| RC-012, RC-013, RC-014 | No E2E tests with shared state |
| RC-032 | No new tracking/calculation fields - modifying existing config defaults |
