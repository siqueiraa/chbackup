# Redundancy Analysis

## New Components Proposed

Phase 3e introduces new **files** (not Rust public API). These are infrastructure artifacts.

| Proposed File | Existing Equivalent | Decision | Justification |
|---------------|-------------------|----------|---------------|
| `Dockerfile` (production) | None | CREATE | No production Dockerfile exists. `Dockerfile.test` serves a different purpose (all-in-one test image, not production runtime). |
| `.github/workflows/ci.yml` | None | CREATE | No `.github/` directory exists. CI must be built from scratch. |
| `examples/kubernetes/sidecar.yaml` | None | CREATE | No K8s examples exist. Design doc sections 1.3 and 10.9 specify this. |
| `Dockerfile.test` (updates) | `Dockerfile.test` (existing) | EXTEND | Existing file works. May need minor updates for CI matrix compatibility. |
| `docker-compose.test.yml` (updates) | `docker-compose.test.yml` (existing) | EXTEND | Existing file works. May need matrix-related updates. |
| `test/run_tests.sh` (updates) | `test/run_tests.sh` (existing) | EXTEND | Existing file has smoke tests. May need seed_data.sql integration. |
| `test/fixtures/seed_data.sql` | None (only `setup.sql` exists) | CREATE | Design doc specifies deterministic data for checksum verification. |

## Rust Code Changes

| Proposed Change | Existing Code | Decision | Justification |
|----------------|---------------|----------|---------------|
| Add `WATCH_INTERVAL`/`FULL_INTERVAL` to env overlay | `apply_env_overlay()` in config.rs | EXTEND | Design doc 10.9 K8s manifest uses these env vars, but they are not in the overlay. Small addition to existing function. |

## No Public API Introduced

This phase does not introduce new `pub struct`, `pub fn`, `pub enum`, or new Rust modules. The only Rust change is extending the private `apply_env_overlay()` method with two additional env var mappings.

N/A for REPLACE/COEXIST analysis -- no new public Rust API.
