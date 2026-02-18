# Redundancy Analysis

## New Public Components Proposed

| Proposed | Description |
|----------|-------------|
| `Metrics` struct | Wrapper holding all prometheus metric instances and the registry |
| `metrics_handler()` fn | Axum handler replacing `metrics_stub()` |
| `src/server/metrics.rs` | New module file |

## Search Results

### `Metrics` struct

No existing `Metrics` struct found:
- `grep -rn 'struct Metrics' src/` -- no results
- `grep -rn 'prometheus' src/` -- no results
- `grep -rn 'prometheus' Cargo.toml` -- no results (not yet added)

**Decision: COEXIST** -- No existing equivalent. This is a genuinely new component.

### `metrics_handler()` / `metrics_stub()`

`metrics_stub()` exists at `src/server/routes.rs:900` -- returns 501 Not Implemented.

**Decision: REPLACE** -- `metrics_stub()` will be removed and replaced by the new `metrics_handler()` (or renamed to `metrics()`).
- Removal: routes.rs line 900 -- remove `metrics_stub` function
- Route update: mod.rs line 83 -- change `routes::metrics_stub` to `routes::metrics` (or `metrics::metrics_handler`)
- Test update: routes.rs test `test_stub_endpoints_return_501` line 1109 -- remove metrics_stub assertion

### `src/server/metrics.rs` module

No existing `metrics.rs` file in `src/server/`:
- `ls src/server/` shows: mod.rs, routes.rs, actions.rs, auth.rs, state.rs

**Decision: COEXIST** -- New module, no conflict.

## Summary Table

| Proposed | Existing Match | Decision | Task/Acceptance | Justification |
|----------|----------------|----------|-----------------|---------------|
| `Metrics` struct | (none) | COEXIST | - | New component, no equivalent exists |
| `metrics()` handler | `metrics_stub()` | REPLACE | Task that adds handler / remove stub | Stub is placeholder for real implementation |
| `src/server/metrics.rs` | (none) | COEXIST | - | New module, no conflict |

## REPLACE Details

- **Old code**: `metrics_stub()` at routes.rs:900 + route registration at mod.rs:83 + test assertion at routes.rs:1109
- **New code**: Real `metrics()` handler in metrics.rs or routes.rs
- **Acceptance criteria**: After implementation, `grep -n 'metrics_stub' src/` returns zero results
