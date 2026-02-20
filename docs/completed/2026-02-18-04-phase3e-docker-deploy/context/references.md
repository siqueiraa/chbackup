# Symbol and Reference Analysis

## Phase 3e Scope

Phase 3e is an **infrastructure-only** phase (Docker, CI, K8s manifests). The only Rust source change is adding two env var overlay lines to `src/config.rs`. Symbol analysis focuses on:

1. Existing infrastructure files being modified
2. The env overlay function in config.rs
3. CLI interface and config types referenced by Docker/K8s files

---

## 1. Config Environment Overlay (`src/config.rs:844-904`)

### Function: `apply_env_overlay()`

**Location:** `src/config.rs:844`
**Type:** `fn apply_env_overlay(&mut self)`
**Visibility:** Private (called from `Config::load()`)

**Currently mapped env vars (23 total):**

| Env Var | Config Field | Line |
|---------|-------------|------|
| `CHBACKUP_LOG_LEVEL` | `general.log_level` | 846 |
| `CHBACKUP_LOG_FORMAT` | `general.log_format` | 849 |
| `CLICKHOUSE_HOST` | `clickhouse.host` | 854 |
| `CLICKHOUSE_PORT` | `clickhouse.port` | 857 |
| `CLICKHOUSE_USERNAME` | `clickhouse.username` | 862 |
| `CLICKHOUSE_PASSWORD` | `clickhouse.password` | 865 |
| `CLICKHOUSE_DATA_PATH` | `clickhouse.data_path` | 868 |
| `S3_BUCKET` | `s3.bucket` | 873 |
| `S3_REGION` | `s3.region` | 876 |
| `S3_ENDPOINT` | `s3.endpoint` | 879 |
| `S3_PREFIX` | `s3.prefix` | 882 |
| `S3_ACCESS_KEY` | `s3.access_key` | 885 |
| `S3_SECRET_KEY` | `s3.secret_key` | 888 |
| `S3_ASSUME_ROLE_ARN` | `s3.assume_role_arn` | 891 |
| `S3_FORCE_PATH_STYLE` | `s3.force_path_style` | 894 |
| `API_LISTEN` | `api.listen` | 901 |

**MISSING env vars needed for K8s deployment (Phase 3e additions):**

| Env Var | Config Field | Justification |
|---------|-------------|---------------|
| `WATCH_INTERVAL` | `watch.watch_interval` | K8s sidecar needs to configure watch interval via env |
| `FULL_INTERVAL` | `watch.full_interval` | K8s sidecar needs to configure full backup interval via env |

### Callers of `apply_env_overlay()`

Only called from `Config::load()` at line 826:
```rust
config.apply_env_overlay();
```

No other code paths invoke this function. The change is safe -- adding new `if let Ok(v)` blocks at the end of the function.

---

## 2. CLI Interface Referenced by Docker/K8s Files

### `Command::Server { watch: bool }` (src/cli.rs:328-332)

```rust
Server {
    #[arg(long)]
    watch: bool,
}
```

**Referenced by:** K8s sidecar manifest (`args: ["server", "--watch"]`)
**Referenced by:** docker-compose.test.yml (implicitly, via `chbackup server --watch`)

### `Cli.config` field (src/cli.rs:9-16)

```rust
#[arg(
    short = 'c',
    long = "config",
    default_value = "/etc/chbackup/config.yml",
    env = "CHBACKUP_CONFIG",
    global = true
)]
pub config: String,
```

**Referenced by:** Dockerfile (ENTRYPOINT path), K8s manifest (config mount path)
**Default:** `/etc/chbackup/config.yml` -- this is where Docker/K8s expect the config file

---

## 3. Existing Infrastructure Files

### Dockerfile.test (project root)

**References:**
- Binary path: `target/x86_64-unknown-linux-musl/release/chbackup` (build output)
- Install path: `/usr/local/bin/chbackup`
- Base image: `altinity/clickhouse-server:${CH_VERSION}` (default `25.3.8.10041.altinitystable`)
- Config mount: `/etc/clickhouse-server/config.d/test.xml`
- Test config: `/etc/chbackup/config.yml`
- Ports: 8123, 9000, 7171

### docker-compose.test.yml (project root)

**References:**
- Dockerfile.test (build context)
- Required env: `S3_BUCKET`, `S3_ACCESS_KEY`, `S3_SECRET_KEY`
- Optional env: `S3_REGION`, `CH_VERSION`, `LOG_LEVEL`, `TEST_FILTER`, `RUN_ID`
- S3 prefix isolation: `chbackup-test/${CH_VERSION}/${RUN_ID}`
- Zookeeper: `zookeeper:3.8`
- Health checks: ZK `ruok`, CH `SELECT 1`

### test/run_tests.sh

**References:**
- `chbackup` binary (expected in PATH)
- `CHBACKUP_CONFIG` env (set to `/etc/chbackup/config.yml`)
- ClickHouse client (from base image)
- Test fixtures at `/test/fixtures/`

### test/configs/chbackup-test.yml

**References:**
- S3 credentials from env vars (overlay pattern)
- `api.listen: 0.0.0.0:7171` (binds all interfaces for container access)
- `clickhouse.host: localhost` (same container)

---

## 4. Design Doc to Existing Code Reconciliation

### Design Doc 1.2 (Dockerfile) vs. Existing Dockerfile.test

| Aspect | Design Doc 1.2 | Existing Dockerfile.test |
|--------|----------------|--------------------------|
| Purpose | Production image | Integration test image |
| Base (build) | `rust:1.82-alpine` | N/A (pre-built binary) |
| Base (runtime) | `alpine:3.21` | `altinity/clickhouse-server` |
| Binary path | `/bin/chbackup` | `/usr/local/bin/chbackup` |
| User | `clickhouse` (uid 101) | root (CH image user) |
| Entrypoint | `["/bin/chbackup"]` | Base image entrypoint |

**Conclusion:** Dockerfile.test is a different artifact from the production Dockerfile. Both are needed.

### Design Doc 1.3 (K8s Sidecar) vs. Existing Code

The K8s sidecar manifest in the design doc uses:
- `args: ["server"]` -- maps to `Command::Server { watch: false }`
- Env vars: `S3_BUCKET`, `S3_ACCESS_KEY`, `S3_SECRET_KEY` -- all supported by env overlay
- Volume mount: `/var/lib/clickhouse` -- matches `clickhouse.data_path` default
- Port: `7171` -- matches `api.listen` default

No code changes needed for K8s support beyond env var additions for watch config.

### Design Doc 1.4.6 (CI) vs. Existing Infrastructure

The CI workflow in the design doc uses:
- `cargo build --release --target x86_64-unknown-linux-musl` -- needs musl toolchain on Ubuntu
- `docker compose -f docker-compose.test.yml` -- already exists
- Matrix: CH versions `23.8, 24.3, 24.8, 25.1` -- existing compose supports `CH_VERSION` arg
- S3 secrets: `TEST_S3_BUCKET`, `TEST_S3_ACCESS_KEY`, `TEST_S3_SECRET_KEY`

**Note:** The design doc uses vanilla CH images (`clickhouse/clickhouse-server`) but existing Dockerfile.test uses Altinity images (`altinity/clickhouse-server`). The CI matrix needs Altinity-compatible version tags.

---

## 5. Altinity vs. Vanilla ClickHouse Image Tags

The existing infrastructure uses Altinity-flavored ClickHouse images. CI matrix versions need mapping:

| Design Doc Version | Altinity Tag Pattern |
|-------------------|---------------------|
| 23.8 | `23.8.x.x.altinitystable` |
| 24.3 | `24.3.x.x.altinitystable` |
| 24.8 | `24.8.x.x.altinitystable` |
| 25.1 | `25.1.x.x.altinitystable` (if available) |

The CI workflow should either:
1. Use Altinity tags with version resolution, OR
2. Parameterize the image registry (`altinity/clickhouse-server` vs `clickhouse/clickhouse-server`)

**Decision needed in planning phase.**

---

## 6. Server Module Entry Point

### `start_server()` (src/server/mod.rs:117-309)

**Signature:**
```rust
pub async fn start_server(
    config: Arc<Config>,
    ch: ChClient,
    s3: S3Client,
    watch: bool,
    config_path: PathBuf,
) -> Result<()>
```

**Called from:** `src/main.rs:483`
```rust
Command::Server { watch } => {
    let ch = ChClient::new(&config.clickhouse)?;
    let s3 = S3Client::new(&config.s3).await?;
    let config_path = PathBuf::from(&cli.config);
    chbackup::server::start_server(Arc::new(config), ch, s3, watch, config_path).await?;
}
```

This is the code path exercised by `chbackup server --watch` in K8s sidecar mode. No modifications needed.
