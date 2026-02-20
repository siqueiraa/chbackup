# Handoff: Go Parity Gaps - Phase 7 (Revised)

## Plan Location
`docs/plans/2026-02-20-01-go-parity-gaps/`

## Directory Contents

| File | Purpose |
|------|---------|
| PLAN.md | 6 task definitions with TDD steps across 4 dependency groups |
| SESSION.md | Status tracking and phase checklists |
| acceptance.json | 6 criteria with 4-layer verification (all "fail" initially) |
| HANDOFF.md | This file - resume context |
| context/patterns.md | Discovered patterns (existing patterns only) |
| context/symbols.md | Type verification for config/CLI/API types |
| context/diagnostics.md | Compilation baseline (clean at 93f55e28) |
| context/references.md | Key symbol references across modules |
| context/git-history.md | Recent commit context |
| context/knowledge_graph.json | Structured JSON for symbol lookup |
| context/affected-modules.json | Machine-readable module status |
| context/*-gaps.md | 12 gap analysis files from parallel research agents |

## Revision Note

This plan was revised after a thorough audit of Phase 6 "Go parity" changes against `docs/design.md`. The original 9-task plan was copying Go behavior over intentional design decisions. The revised plan:
- Drops 5 tasks that conflicted with the design doc
- Trims Task 1 from 11 changes to 5 config reverts
- Adds a design doc update task for genuine Phase 6 improvements

## To Resume

1. Read SESSION.md for current status
2. Check Agent Execution Status table
3. Continue from last incomplete phase/agent
4. Use `/project-execute 2026-02-20-01-go-parity-gaps` when planning is complete

## Key References

### Files Being Modified
- `src/config.rs` -- Config default reverts and env var overlay (Tasks 1, 2, 3)
- `src/storage/s3.rs` -- PutObject/UploadPart retry (Task 4)
- `src/upload/mod.rs` -- Wire retry into upload pipeline (Task 4)
- `docs/design.md` -- Update for genuine Phase 6 improvements (Task 5)
- `CLAUDE.md` -- Root project documentation (Task 6)
- `src/server/CLAUDE.md` -- Fix 409->423 reference (Task 6)
- `src/storage/CLAUDE.md` -- Document retry methods (Task 6)

### Test Files
- Tests added inline to existing test modules (no new test files)
- `src/config.rs` -- `#[cfg(test)]` for config defaults and env overlay tests
- `src/storage/s3.rs` -- `#[cfg(test)]` for retry config test

### Related Documentation
- `docs/design.md` -- §2 (CLI/env), §3.4 (cleanup), §3.6 (S3 ops), §7 (layout), §8.2 (retention), §9 (API), §11.7 (timeout), §12 (config YAML)
- Per-module CLAUDE.md files in src/server/, src/storage/

## Phase 6 Audit Summary

**Reverts (Tasks 1-2):** 6 config defaults that Phase 6 changed away from design doc
**Keeps (documented in Task 5):** skip_tables, API 423, multipart CopyObject, backup cleanup, incremental chain protection
**Dropped from original plan:** /backup/* routes, named_collection_size, CLI flags, restore reordering, watch type string
