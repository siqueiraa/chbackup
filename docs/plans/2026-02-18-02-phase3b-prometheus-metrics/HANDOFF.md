# Handoff: Phase 3b -- Prometheus Metrics

## Plan Location
`docs/plans/2026-02-18-02-phase3b-prometheus-metrics/`

## Directory Contents

| File | Purpose |
|------|---------|
| PLAN.md | Task definitions and TDD steps (6 tasks, 3 dependency groups) |
| SESSION.md | Status tracking and checklists |
| acceptance.json | 4-layer verification criteria (5 features: F001-F004, FDOC) |
| HANDOFF.md | This file -- resume context |
| context/patterns.md | Discovered patterns (route handler, AppState extension, prometheus usage) |
| context/symbols.md | Type verification table (AppState fields, config types, list functions) |
| context/diagnostics.md | Baseline diagnostics (zero errors/warnings, prometheus not yet in deps) |
| context/knowledge_graph.json | Structured JSON for symbol lookup (23 verified symbols) |
| context/affected-modules.json | Module status (src/server: update) |
| context/affected-modules.md | Human-readable module summary |
| context/redundancy-analysis.md | Redundancy check (REPLACE: metrics_stub, COEXIST: Metrics struct, metrics.rs) |
| context/references.md | Symbol and reference analysis (metrics_stub refs, AppState refs, finish_op callers) |
| context/git-history.md | Git context (Phase 3a commits, branch state) |
| context/data-authority.md | Data source verification for all metrics |
| context/preventive-rules-applied.md | Applied preventive rules (14 checked, 8 applicable) |

## To Resume

1. Read SESSION.md for current status
2. Check Agent Execution Status table
3. Continue from last incomplete phase/agent
4. Use `/project-execute` when planning is complete

## Key References

### Files Being Modified
- `Cargo.toml` -- Add `prometheus = "0.13"` dependency
- `src/lib.rs` -- Add compile-time import test for prometheus
- `src/server/metrics.rs` (NEW) -- Metrics struct with prometheus Registry and all metric definitions
- `src/server/mod.rs` -- Add `pub mod metrics;`, update route from `metrics_stub` to `metrics`
- `src/server/state.rs` -- Add `metrics: Option<Arc<Metrics>>` field to AppState, initialize in `new()`
- `src/server/routes.rs` -- Remove `metrics_stub`, add real `metrics` handler, instrument operation handlers

### Test Files
- `src/lib.rs` (test module) -- `test_phase3b_prometheus_available`
- `src/server/metrics.rs` (test module) -- `test_metrics_new_registers_all`, `test_metrics_encode_text`, `test_metrics_counter_increment`
- `src/server/routes.rs` (test module) -- Updated `test_stub_endpoints_return_501` (remove metrics_stub assertion)

### Related Documentation
- Design doc section 9 (line 1833) -- Metrics list
- Roadmap Phase 3b section (line 306-323) -- Metric table and implementation notes
- `src/server/CLAUDE.md` -- Module documentation (will be updated in Task 6)

### Commits (Group A + B + C)
| Task | Commit | Description |
|------|--------|-------------|
| 1 | b0fa80e | Add prometheus 0.13 dependency + compile-time test |
| 2 | 2e08025 | Create Metrics struct with 14 metric definitions |
| 3 | 9c364c1 | Add metrics field to AppState with conditional creation |
| 4 | 09f1603 | Replace metrics_stub with real /metrics handler |
| 5 | 52b933a | Instrument operation handlers with prometheus metrics |
| 6 | ea47234 | Update CLAUDE.md for server module with metrics docs |

### Design Decisions
1. Custom Registry (not global) -- enables clean testing
2. `Option<Arc<Metrics>>` in AppState -- None when `enable_metrics=false`
3. Backup count gauges refreshed on scrape -- acceptable for 15-30s intervals
4. `list_local()` via `spawn_blocking` -- sync function, must not block async runtime
5. Watch metrics registered but static -- Phase 3d will update them
6. `HistogramVec` with `operation` label -- single metric covers all operation durations
