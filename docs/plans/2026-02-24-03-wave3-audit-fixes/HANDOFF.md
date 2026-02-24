# Handoff: Fix 5 Wave-3 Audit Findings

## Plan Location
`docs/plans/2026-02-24-03-wave3-audit-fixes/`

## Directory Contents

| File | Purpose |
|------|---------|
| PLAN.md | Task definitions and TDD steps for 6 tasks across 3 dependency groups |
| SESSION.md | Status tracking and checklists |
| acceptance.json | 6 criteria with 4-layer verification (F001-F005, FDOC) |
| HANDOFF.md | This file -- resume context |
| context/patterns.md | 5 discovered patterns (request types, CLI overrides, DDL rewriting, resume tests, config validation) |
| context/symbols.md | Type verification table with 22 verified symbols |
| context/knowledge_graph.json | Structured JSON for symbol lookup (15 verified symbols) |
| context/affected-modules.json | Module status: 3 modules + 3 standalone files |
| context/affected-modules.md | Human-readable module summary |
| context/diagnostics.md | Baseline: zero errors, zero warnings |
| context/references.md | Symbol references and bug analysis for all 5 findings |
| context/redundancy-analysis.md | 2 new components checked (classify_backup_type, WatchStartRequest) |
| context/git-history.md | Recent git log and file-specific history |
| context/preventive-rules-applied.md | 8 rules applied (RC-004, RC-006, RC-008, RC-019, RC-021, RC-035) |

## To Resume

1. Read SESSION.md for current status
2. Check Agent Execution Status table
3. Continue from last incomplete phase/agent
4. Use `/project-execute` when planning is complete

## Key References

### Files Being Modified
- `src/restore/remap.rs` -- W3-1: Fix `&&` to `||` at line 647 in rewrite_distributed_engine guard clause
- `src/watch/mod.rs` -- W3-2: Add classify_backup_type() helper, replace lines 145-146
- `src/server/routes.rs` -- W3-3: Add WatchStartRequest struct, modify watch_start to accept optional body
- `src/config.rs` -- W3-4: Remove `if self.watch.enabled` gate at line 1400
- `src/cli.rs` -- W3-5: Add watch_interval/full_interval to Command::Server variant
- `src/main.rs` -- W3-5: Wire Server interval overrides into config at line 652

### Test Files
- `src/restore/remap.rs` (inline tests) -- 2 new regression tests for partial-match scenarios
- `src/watch/mod.rs` (inline tests) -- 5 new tests for classify_backup_type
- `src/config.rs` (inline tests) -- 1 new test for unconditional interval validation
- `src/cli.rs` (inline tests) -- 1 new test for Server CLI flag parsing

### Module Documentation
- `src/watch/CLAUDE.md` -- Add classify_backup_type to Public API section
- `src/server/CLAUDE.md` -- Add WatchStartRequest and updated watch_start docs

### Commit Log

| Task | Commit | Description |
|------|--------|-------------|
| 1 | b0ec2d68 | fix(restore): change Distributed remap guard from && to \|\| |
| 2 | cbdff02e | fix(watch): add template-aware classify_backup_type |
| 3 | 0931f6c5 | fix(config): always validate watch intervals regardless of watch.enabled |
| 4 | 9450b09b | feat(server): accept optional body in watch/start for interval overrides |
| 5 | 6dd4b671 | feat(cli): add --watch-interval and --full-interval flags to server command |

### Key Verified Facts
- Config derives Clone (`#[derive(Debug, Clone, Default, Serialize, Deserialize)]` at config.rs:8)
- Default intervals ("1h" = 3600s, "24h" = 86400s) always pass validation (86400 > 3600)
- `rewrite_distributed_engine` is private, called only by `rewrite_create_table_ddl` at line 219
- `resume_state` has 7 existing tests, all use template `"shard1-{type}-{time:%Y%m%d}"`
- `watch_start` is registered at `server/mod.rs:83` as POST route
