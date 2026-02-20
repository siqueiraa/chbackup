# Plan: Phase 3e -- Docker / Deployment

## Goal

Create production Docker image, GitHub Actions CI with ClickHouse version matrix, Kubernetes sidecar example manifest, and extend the integration test suite with deterministic seed data and round-trip smoke tests. Also close the WATCH_INTERVAL/FULL_INTERVAL env var overlay gap required for K8s deployment.

## Architecture Overview

Phase 3e is an **infrastructure-only** phase. The only Rust source change is extending `apply_env_overlay()` in `src/config.rs` with two env var mappings (`WATCH_INTERVAL`, `FULL_INTERVAL`). All other deliverables are Docker, CI, and K8s manifest files.

**Deliverables mapped to design doc:**

| Deliverable | Design Section | File(s) |
|-------------|---------------|---------|
| Production Dockerfile | 1.2 | `Dockerfile` |
| Integration test enhancements | 1.4.4 | `test/fixtures/seed_data.sql`, `test/run_tests.sh` |
| CI matrix | 1.4.6 | `.github/workflows/ci.yml` |
| K8s sidecar manifest | 1.3, 10.9 | `examples/kubernetes/sidecar.yaml` |
| Env var overlay for watch | 10.9 | `src/config.rs` |

## Architecture Assumptions (VALIDATED)

### Component Ownership
- **Dockerfile**: New file. Builds static musl binary in builder stage, copies to Alpine runtime.
- **CI workflow**: New file. Builds binary, runs `docker compose` test matrix.
- **K8s manifest**: New file (example only). References production Docker image.
- **config.rs**: Existing file. `apply_env_overlay()` is private, called only from `Config::load()`.

### What This Plan CANNOT Do
- Cannot test K8s manifest in CI (no K8s cluster) -- it is an example artifact only
- Cannot test ARM64 builds (design doc mentions linux/arm64 but CI matrix runs on x86_64 only)
- Cannot test with MinIO -- design explicitly requires real S3
- Cannot install musl target on macOS development machine -- Docker builder handles this

### Verified Interface Points
- `chbackup server --watch` -- verified in `src/cli.rs:328-332` (`Command::Server { watch: bool }`)
- Config default path `/etc/chbackup/config.yml` -- verified in `src/cli.rs:9-16` (CHBACKUP_CONFIG env)
- API listen port `7171` -- verified in `src/config.rs` (`ApiConfig.listen` default `localhost:7171`)
- `watch.watch_interval` type is `String` -- verified in `src/config.rs:399-400`
- `watch.full_interval` type is `String` -- verified in `src/config.rs:403-404`
- `apply_env_overlay()` at `src/config.rs:844` -- verified, all overlay entries follow `if let Ok(v) = std::env::var(...)` pattern

## Risk Assessment

| Risk | Level | Mitigation |
|------|-------|------------|
| CI needs real S3 credentials as secrets | YELLOW | Document required GitHub secrets. Tests skip gracefully if not configured. |
| Altinity vs vanilla CH image tags for CI matrix | YELLOW | Use Altinity images (existing pattern). Document version tag format. |
| Docker build cache invalidation on src changes | GREEN | Cargo.toml+lock copied first for dependency caching (standard multi-stage pattern). |
| K8s manifest referencing env vars not in overlay | GREEN | Task 1 adds WATCH_INTERVAL/FULL_INTERVAL to overlay. |
| Rust toolchain version pinning in Dockerfile | GREEN | Use explicit rust:1.82-alpine (matches design doc). |

## Expected Runtime Logs

This phase is infrastructure-only. No `DEBUG_VERIFY` markers are needed since there is no runtime behavior change beyond the env var overlay (tested via unit test, not runtime logs).

| Pattern | Required | Description |
|---------|----------|-------------|
| `cargo check` zero errors | yes | Compilation passes after config.rs change |
| `cargo test` all pass | yes | Unit test for new env overlay vars passes |
| `docker build` success | yes (CI) | Production Dockerfile builds successfully |
| `ERROR:` | no (forbidden) | No errors during compilation or tests |

## Known Related Issues (Out of Scope)

| Issue | Reason | Future Plan |
|-------|--------|-------------|
| ARM64 Docker builds | Design doc mentions linux/arm64 but not in Phase 3e scope | Phase 4f or separate |
| MinIO local testing | Design explicitly requires real S3 | Deferred |
| Docker image publishing to ghcr.io | Requires repository configuration | Separate CI workflow |
| Integration test Rust files (test_create_local.rs etc.) | Design doc 1.4.4 lists these but they are Phase 4 integration tests | Phase 4 |

## Dependency Groups

```
Group A (Sequential -- Rust source change first):
  - Task 1: Add WATCH_INTERVAL/FULL_INTERVAL env var overlay
  - Task 2: Add unit test for watch env var overlay

Group B (Independent -- infrastructure files, no Rust changes):
  - Task 3: Create production Dockerfile
  - Task 4: Create seed_data.sql and extend run_tests.sh
  - Task 5: Create GitHub Actions CI workflow
  - Task 6: Create K8s sidecar example manifest

Group C (Final -- after all code tasks):
  - Task 7: Update root CLAUDE.md with Phase 3e changes
```

## Tasks

### Task 1: Add WATCH_INTERVAL and FULL_INTERVAL to env var overlay

**TDD Steps:**
1. Write failing test first (Task 2 -- but field access pattern is clear)
2. Add two `if let Ok(v) = std::env::var(...)` blocks to `apply_env_overlay()` in `src/config.rs`
3. Add `WATCH_INTERVAL` and `FULL_INTERVAL` to the `clear_config_env_vars()` helper in `tests/config_test.rs`
4. Verify `cargo check` passes
5. Verify `cargo test` passes

**Implementation Details:**

Add to `src/config.rs` inside `apply_env_overlay()` (after the API section, before the closing brace at line 904):

```rust
// Watch
if let Ok(v) = std::env::var("WATCH_INTERVAL") {
    self.watch.watch_interval = v;
}
if let Ok(v) = std::env::var("FULL_INTERVAL") {
    self.watch.full_interval = v;
}
```

Also add to `tests/config_test.rs` `clear_config_env_vars()`:

```rust
std::env::remove_var("WATCH_INTERVAL");
std::env::remove_var("FULL_INTERVAL");
```

**Files:** `src/config.rs`, `tests/config_test.rs`
**Acceptance:** F001

---

### Task 2: Add unit test for watch env var overlay

**TDD Steps:**
1. Write test `test_watch_env_overlay` in `tests/config_test.rs`
2. Test sets `WATCH_INTERVAL=30m` and `FULL_INTERVAL=12h`
3. Load config from minimal YAML
4. Assert `config.watch.watch_interval == "30m"` and `config.watch.full_interval == "12h"`
5. Verify test passes with `cargo test test_watch_env_overlay`

**Implementation Details:**

Add to `tests/config_test.rs`:

```rust
#[test]
fn test_watch_env_overlay() {
    let _guard = ENV_LOCK.lock().unwrap();
    clear_config_env_vars();

    // Set watch env vars
    std::env::set_var("WATCH_INTERVAL", "30m");
    std::env::set_var("FULL_INTERVAL", "12h");

    let yaml = r#"
watch:
  enabled: false
"#;

    let mut tmpfile = NamedTempFile::new().expect("create temp file");
    tmpfile.write_all(yaml.as_bytes()).expect("write yaml");

    let config = Config::load(tmpfile.path(), &[]).expect("Config::load should succeed");

    // Env vars should override defaults
    assert_eq!(config.watch.watch_interval, "30m");
    assert_eq!(config.watch.full_interval, "12h");

    clear_config_env_vars();
}
```

**Files:** `tests/config_test.rs`
**Acceptance:** F001

---

### Task 3: Create production Dockerfile

**TDD Steps:**
1. Create `Dockerfile` at project root following design doc section 1.2
2. Verify Dockerfile syntax with `docker build --check .` or equivalent lint
3. Verify Dockerfile contains: multi-stage build, musl target, Alpine runtime, uid 101 clickhouse user
4. Verify binary path is `/bin/chbackup` (production) not `/usr/local/bin/chbackup` (test)

**Implementation Details:**

Create `Dockerfile` matching design doc 1.2 exactly, with these adaptations:
- Use `rust:1.82-alpine` builder stage
- `alpine:3.21` runtime stage
- Create clickhouse user/group with uid/gid 101
- Install `ca-certificates tzdata bash`
- Copy binary to `/bin/chbackup`
- ENTRYPOINT + CMD pattern

Key design decisions:
- Dependency caching: copy `Cargo.toml` + `Cargo.lock` first, build with dummy `main.rs`, then copy real src
- `VERSION` build arg for image labeling
- No `entrypoint.sh` wrapper (simpler than Go tool)

**Files:** `Dockerfile`
**Acceptance:** F002

---

### Task 4: Create seed_data.sql and extend run_tests.sh with round-trip test

**TDD Steps:**
1. Create `test/fixtures/seed_data.sql` with deterministic INSERT statements
2. Extend `test/run_tests.sh` with:
   a. Load `seed_data.sql` after `setup.sql`
   b. Capture row count and checksum before backup
   c. Add `test_round_trip` test: create -> upload -> download -> restore with checksum verification
3. Verify script is syntactically correct with `bash -n test/run_tests.sh`

**Implementation Details:**

`seed_data.sql` provides deterministic data for checksum-based verification (design doc 1.4.4). Uses fixed timestamps and values so `SELECT count(), sum(cityHash64(*))` produces repeatable results.

The round-trip test in `run_tests.sh` exercises:
1. `chbackup create` with a known backup name
2. `chbackup upload` to S3
3. Delete local backup with `chbackup delete local`
4. `chbackup download` from S3
5. DROP original table, `chbackup restore`
6. Verify row count matches pre-backup count

**Files:** `test/fixtures/seed_data.sql`, `test/run_tests.sh`
**Acceptance:** F003

---

### Task 5: Create GitHub Actions CI workflow

**TDD Steps:**
1. Create `.github/workflows/ci.yml` with matrix strategy
2. Verify YAML syntax (valid GitHub Actions schema)
3. Verify matrix includes CH versions: 23.8, 24.3, 24.8, 25.1
4. Verify workflow has: cargo check, cargo test, cargo clippy, Docker build, integration test steps
5. Verify S3 secrets are referenced correctly

**Implementation Details:**

CI workflow structure:
- **Trigger:** push to main, pull_request
- **Jobs:**
  1. `check` -- cargo fmt, cargo clippy, cargo test (fast feedback, no Docker)
  2. `build` -- cross-compile static musl binary
  3. `integration` -- matrix of CH versions, docker compose test

Matrix strategy for integration tests:
```yaml
strategy:
  matrix:
    ch_version:
      - "23.8.16.40.altinitystable"
      - "24.3.12.76.altinitystable"
      - "24.8.13.51.altinitystable"
      - "25.1.5.31.altinitystable"
```

Note: Uses Altinity image tags (existing pattern from Dockerfile.test). Version tags are pinned to specific Altinity stable releases.

Required GitHub secrets:
- `TEST_S3_BUCKET`
- `TEST_S3_ACCESS_KEY`
- `TEST_S3_SECRET_KEY`
- `TEST_S3_REGION` (optional, defaults to us-east-1)

S3 prefix isolation per run: `chbackup-test/${{ matrix.ch_version }}/${{ github.run_id }}`

**Files:** `.github/workflows/ci.yml`
**Acceptance:** F004

---

### Task 6: Create K8s sidecar example manifest

**TDD Steps:**
1. Create `examples/kubernetes/sidecar.yaml` following design doc sections 1.3 and 10.9
2. Verify YAML is valid Kubernetes Pod/Deployment spec
3. Verify it includes: ClickHouse container, chbackup sidecar, shared volume, env vars, secret references
4. Verify `args: ["server", "--watch"]` matches CLI interface
5. Add comments explaining each section for users

**Implementation Details:**

The K8s manifest is a complete Deployment example with:
- ClickHouse StatefulSet-style pod spec
- chbackup sidecar container with `server --watch`
- Shared `emptyDir` volume for `/var/lib/clickhouse`
- S3 credentials from Kubernetes Secret
- Watch interval configuration via env vars (WATCH_INTERVAL, FULL_INTERVAL)
- Prometheus port annotation for scraping
- `terminationGracePeriodSeconds: 60` (per design doc 11.5)
- Resource requests/limits
- Readiness/liveness probes

**Files:** `examples/kubernetes/sidecar.yaml`
**Acceptance:** F005

---

### Task 7: Update root CLAUDE.md with Phase 3e changes

**TDD Steps:**
1. Read current CLAUDE.md
2. Update "Current Implementation Status" section to mark Phase 3e complete
3. Update "Source Module Map" if any new modules were created (none expected)
4. Add note about Dockerfile, CI, K8s manifest locations
5. Verify CLAUDE.md has required sections (Parent Context not applicable for root)

**Implementation Details:**

Changes to CLAUDE.md:
- Add Phase 3e line to implementation status section
- Add infrastructure files to a new "Infrastructure" section or note
- Update env var overlay documentation to include WATCH_INTERVAL, FULL_INTERVAL

**Files:** `CLAUDE.md`
**Acceptance:** FDOC

---

## Consistency Validation Results

| Check | Status | Notes |
|-------|--------|-------|
| RC-015 | PASS | No cross-task data flow -- tasks produce independent files |
| RC-016 | PASS | No struct definitions in this plan |
| RC-017 | PASS | Task 1 adds watch fields to overlay; Task 2 tests them -- correct sequencing |
| RC-018 | PASS | Task 1 references `apply_env_overlay()` pattern verified at config.rs:844 |

## Notes

### Phase 4.5 Skip Justification
Phase 4.5 (Interface Skeleton Simulation) is **skipped** because:
- The only Rust change is adding 2 lines to an existing private function (`apply_env_overlay`)
- No new imports, types, or public API are introduced
- The existing env overlay pattern is trivially verifiable (copy-paste of existing `if let Ok(v)` blocks)
- A unit test (Task 2) directly validates the change compiles and works

### Trading Logic Checklist
N/A -- no order placement or position changes in this plan.

### External API Robustness
N/A -- no new external API calls. The env var overlay is a sync config loading operation.

### Altinity vs Vanilla ClickHouse Decision
The CI matrix uses **Altinity ClickHouse images** (`altinity/clickhouse-server`), consistent with the existing `Dockerfile.test`. This diverges from the design doc which shows `clickhouse/clickhouse-server`, but the existing infrastructure is already built around Altinity and the project name itself references Altinity/clickhouse-backup. The Altinity stable releases are pinned to specific version tags.

### Watch Env Var Gap
Design doc section 10.9 shows `WATCH_INTERVAL` and `FULL_INTERVAL` as env vars in the K8s manifest, but `src/config.rs:apply_env_overlay()` does not currently handle these. Task 1 closes this gap. The alternative of using `--env watch.watch_interval=1h` CLI flag is possible but less ergonomic for K8s deployments.
