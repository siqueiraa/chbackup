# Pattern Discovery

## Global Pattern Registry

No `docs/patterns/` directory exists in the project.

## Phase 3e Pattern Analysis

Phase 3e is **infrastructure-only** (Docker, CI, K8s manifests). No Rust actors, messages, or handler patterns are involved.

### Existing Infrastructure Patterns

1. **Dockerfile.test** (already exists at project root)
   - Base: `altinity/clickhouse-server:${CH_VERSION}` (Altinity stable, not vanilla)
   - Installs: `ca-certificates curl jq netcat-openbsd`
   - Copies: pre-built static binary from `target/x86_64-unknown-linux-musl/release/chbackup`
   - Copies: test configs and fixtures
   - Exposes: 8123 (HTTP), 9000 (native), 7171 (API)

2. **docker-compose.test.yml** (already exists at project root)
   - Services: `zookeeper` (ZK 3.8) + `chbackup-test`
   - Required env vars: `S3_BUCKET`, `S3_ACCESS_KEY`, `S3_SECRET_KEY`
   - Optional: `S3_REGION`, `CH_VERSION`, `LOG_LEVEL`, `TEST_FILTER`, `RUN_ID`
   - Health checks: ZK via `ruok`, CH via `SELECT 1`
   - S3 prefix isolation: `chbackup-test/${CH_VERSION}/${RUN_ID}`

3. **test/run_tests.sh** (already exists)
   - Validates env vars, waits for CH, runs setup fixtures
   - Smoke tests: `--help`, `print-config`, `list`
   - Filter support via `--filter` or `TEST_FILTER` env

4. **test/configs/** (already exists)
   - `chbackup-test.yml`: minimal config, S3 from env vars
   - `clickhouse-config.xml`: ZK, macros, listen_host

5. **test/fixtures/** (already exists)
   - `setup.sql`: creates tables (trades, users, events)

### Design Doc Patterns to Implement

1. **Production Dockerfile** (design 1.2)
   - Multi-stage: `rust:1.82-alpine` builder, `alpine:3.21` runtime
   - ClickHouse uid/gid 101 user
   - Static musl binary
   - ENTRYPOINT + CMD pattern

2. **K8s Sidecar Manifest** (design 1.3 + 10.9)
   - Two containers sharing a volume
   - Secret references for S3 credentials
   - Watch mode via `server --watch`
   - Env var config overlay

3. **CI Matrix** (design 1.4.6)
   - GitHub Actions with matrix strategy
   - CH versions: 23.8, 24.3, 24.8, 25.1
   - Musl build + docker compose test
   - S3 test isolation per run

### Pattern: Existing vs. Design Doc Reconciliation

| Component | Existing | Design Doc | Decision |
|-----------|----------|------------|----------|
| Test base image | `altinity/clickhouse-server` | `clickhouse/clickhouse-server` | Keep Altinity -- existing and battle-tested |
| Default CH version | `25.3.8.10041.altinitystable` | `24.8` | Update compose default to Altinity equivalent, CI matrix tests multiple |
| Binary install path | `/usr/local/bin/chbackup` | `/bin/chbackup` | Keep `/usr/local/bin/` for test, `/bin/` for production Dockerfile |
| Test entrypoint | Container entrypoint from base image | Custom bash script | Existing pattern is correct |
| Seed data | `setup.sql` only (small dataset) | `setup.sql` + `seed_data.sql` + `seed_large.sql` | Add `seed_data.sql` for checksum validation (defer `seed_large.sql` to multipart test) |
