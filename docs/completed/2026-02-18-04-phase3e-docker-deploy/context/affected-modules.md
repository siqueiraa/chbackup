# Affected Modules Analysis

## Summary

- **Source files to modify:** 1 (`src/config.rs`)
- **Infrastructure files to create:** 4
- **Infrastructure files to modify:** 3
- **Git base:** e87552ef

## Source Code Changes

| File | Change Type | Description |
|------|-------------|-------------|
| `src/config.rs` | MODIFY | Add `WATCH_INTERVAL` and `FULL_INTERVAL` env var mappings to `apply_env_overlay()` |

This is a minimal Rust change -- two additional `if let Ok(v) = std::env::var(...)` blocks in an existing function. No new modules, structs, or public API.

## Infrastructure Files to Create

| File | Design Section | Description |
|------|---------------|-------------|
| `Dockerfile` | 1.2 | Production multi-stage Docker image (builder + Alpine runtime) |
| `.github/workflows/ci.yml` | 1.4.6 | GitHub Actions: build + integration test matrix across CH versions |
| `examples/kubernetes/sidecar.yaml` | 1.3, 10.9 | K8s sidecar deployment with server --watch, env vars, volume mounts |
| `test/fixtures/seed_data.sql` | 1.4.4 | Deterministic test data for checksum-based restore verification |

## Infrastructure Files to Modify

| File | Change Type | Description |
|------|-------------|-------------|
| `Dockerfile.test` | EXTEND | Ensure compatibility with CI matrix CH version args |
| `docker-compose.test.yml` | EXTEND | Ensure CH_VERSION arg passes through correctly for CI |
| `test/run_tests.sh` | EXTEND | Add seed_data.sql loading, create+upload+download+restore round-trip test |

## CLAUDE.md Impact

No new `src/*/` directory modules are created. The only source file change is `src/config.rs` which does not have its own CLAUDE.md (it is documented in the root CLAUDE.md). No CLAUDE.md creation or updates needed.

## Risk Assessment

- **LOW**: All changes are infrastructure files except a 2-line extension to config.rs
- **LOW**: Existing Dockerfile.test and docker-compose.test.yml patterns are well-established
- **MEDIUM**: CI workflow is new and requires correct GitHub Actions syntax + secrets configuration
- **LOW**: K8s manifest is an example file, not executable infrastructure
