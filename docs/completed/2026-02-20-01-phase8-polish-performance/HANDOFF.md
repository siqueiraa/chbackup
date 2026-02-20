# Handoff: Phase 8 -- Polish & Performance

## Plan Location
`docs/plans/2026-02-20-01-phase8-polish-performance/`

## Directory Contents

| File | Purpose |
|------|---------|
| PLAN.md | 10 task definitions with TDD steps across 6 dependency groups |
| SESSION.md | Status tracking and phase checklists |
| acceptance.json | 10-criteria 4-layer verification (F001-F009 + FDOC) |
| HANDOFF.md | This file -- resume context |
| context/patterns.md | Discovered patterns: manifest field addition, signal handler, query params, buffered upload, dir_size |
| context/symbols.md | Type verification table for all structs/functions involved |
| context/knowledge_graph.json | Structured JSON for symbol lookup with verified import paths |
| context/affected-modules.json | Machine-readable module status: 3 to update, 0 to create |
| context/affected-modules.md | Human-readable module summary |
| context/diagnostics.md | Compiler state (clean), signal handling inventory |
| context/redundancy-analysis.md | 1 REUSE (dir_size pub), 2 EXTEND, 1 COEXIST (streaming vs buffered compress) |
| context/references.md | Symbol references with call sites and construction sites for all 7 BackupSummary constructors |
| context/git-history.md | Recent 20 commits, file-specific history, branch context |
| context/preventive-rules-applied.md | 18 root-cause rules checked, findings documented |

## To Resume

1. Read SESSION.md for current status
2. Check Agent Execution Status table
3. Continue from last incomplete phase/agent
4. Use `/project-execute` when planning is complete

## Five Gaps Being Addressed

| Gap | Summary | Tasks | Key Files |
|-----|---------|-------|-----------|
| 1. rbac_size/config_size | Manifest fields, computation, API propagation | 1, 2, 3 | manifest.rs, list.rs, backup/mod.rs, backup/collect.rs, server/routes.rs |
| 2. Tables pagination | offset/limit query params on GET /api/v1/tables | 4 | server/routes.rs |
| 3. Manifest caching | TTL-based in-memory cache for remote manifest summaries | 5, 6 | list.rs, server/state.rs, server/routes.rs, config.rs |
| 4. SIGQUIT stack dump | Signal handler for debugging in server + standalone watch | 7 | server/mod.rs, main.rs |
| 5. Streaming multipart | Channel-based streaming compression for large parts | 8, 9 | upload/stream.rs, upload/mod.rs, config.rs |

## Key References

### Files Being Modified
- `src/manifest.rs` -- BackupManifest struct (add rbac_size, config_size fields)
- `src/list.rs` -- BackupSummary struct (add fields), ManifestCache struct (new), list_remote_cached (new)
- `src/backup/collect.rs` -- dir_size() visibility change (private -> pub)
- `src/backup/mod.rs` -- compute sizes after RBAC backup
- `src/server/routes.rs` -- summary_to_list_response() wire sizes, TablesParams pagination, cache usage
- `src/server/state.rs` -- AppState manifest_cache field
- `src/server/mod.rs` -- SIGQUIT handler spawn
- `src/main.rs` -- SIGQUIT handler for standalone watch
- `src/upload/stream.rs` -- compress_part_streaming() new function
- `src/upload/mod.rs` -- streaming upload branch
- `src/config.rs` -- remote_cache_ttl_secs, streaming_upload_threshold config fields

### Test Files
- `src/manifest.rs` (inline tests) -- manifest field roundtrip and backward compat
- `src/backup/collect.rs` (inline tests) -- dir_size unit tests
- `src/server/routes.rs` (inline tests) -- summary_to_list_response sizes, TablesParams deserialization
- `src/list.rs` (inline tests) -- ManifestCache basic, TTL expiry
- `src/upload/stream.rs` (inline tests) -- streaming compression roundtrip, chunk sizes
- `src/upload/mod.rs` (inline tests) -- streaming threshold test

### Related Documentation
- Design doc section 8.2 -- GC manifest caching reference
- Design doc section 8.4 -- Remote list caching with TTL (5 minutes)
- Design doc section 11.5 -- Signal handling (SIGQUIT behavior)
- `src/server/CLAUDE.md` -- Server module patterns
- `src/upload/CLAUDE.md` -- Upload module patterns
- `src/backup/CLAUDE.md` -- Backup module patterns

### Design Doc Cross-References
- rbac_size/config_size: integration table schema at client.rs:1414-1415 (`rbac_size UInt64, config_size UInt64`)
- Manifest caching: design 8.2 line 1759 ("cache manifest key-sets in memory")
- Remote list cache: design 8.4 line 1796 ("cache remote backup metadata locally...with TTL")
- SIGQUIT: design 11.5 line 2391 ("Dump all goroutine/task stacks to stderr")
