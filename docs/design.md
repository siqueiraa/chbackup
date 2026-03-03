# chbackup — ClickHouse Backup Tool (Rust)

Drop-in replacement for Altinity/clickhouse-backup. Single static binary, ~40 config params vs Go tool's 200+. S3-only storage, non-destructive restore. Design validated against full Go tool source audit (40+ files, 12 packages).

---

## 1. Deployment

MUST run on the same host/pod as ClickHouse with filesystem access to `/var/lib/clickhouse/`. FREEZE creates hardlinks requiring local access. Cannot run remotely.

**Supported ClickHouse versions**: 21.8+ (ALTER TABLE FREEZE WITH NAME was added in this release). Older versions are not supported. Some features require newer versions: `system.parts_columns` (column type check) needs 22.3+, `DatabaseReplicated` needs 22.6+, `--skip-projections` needs projection support in FREEZE (23.3+).

**Streaming by default**: chbackup always uploads and downloads by individual data part (no full-backup archive mode). This means each part is independently compressed and uploaded as a separate S3 object. Benefits: parallel upload/download, resumable per-part, no need to assemble a giant archive. The Go tool's `upload_by_part` / `download_by_part` flags default to true since v1.3.0 — we make this the only mode.

### 1.1 Distribution Formats

```
1. Static binary     — musl-linked, zero runtime deps, ~15MB
                       Targets: linux/amd64, linux/arm64
2. Docker image      — Alpine-based, minimal (~25MB)
                       ghcr.io/user/chbackup:latest
                       ghcr.io/user/chbackup:<version>
3. Docker sidecar    — Primary K8s deployment model (same pod as ClickHouse)
```

### 1.2 Dockerfile

Multi-stage build: Rust cross-compilation → Alpine runtime. The binary is statically linked with musl — runtime image needs nothing beyond ca-certificates.

```dockerfile
# Stage 1: Build
FROM rust:1.82-alpine AS builder
RUN apk add --no-cache musl-dev openssl-dev openssl-libs-static pkgconfig
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main(){}" > src/main.rs && cargo build --release --target x86_64-unknown-linux-musl && rm -rf src
COPY src/ src/
RUN cargo build --release --target x86_64-unknown-linux-musl

# Stage 2: Runtime
FROM alpine:3.21
ARG VERSION=dev
LABEL org.opencontainers.image.title="chbackup"
LABEL org.opencontainers.image.description="Fast ClickHouse backup and restore"
LABEL org.opencontainers.image.version=${VERSION}

RUN addgroup -S -g 101 clickhouse \
    && adduser -S -h /var/lib/clickhouse -s /bin/bash -G clickhouse \
       -g "ClickHouse server" -u 101 clickhouse \
    && apk add --no-cache ca-certificates tzdata bash \
    && update-ca-certificates

COPY --from=builder /src/target/x86_64-unknown-linux-musl/release/chbackup /bin/chbackup
ENTRYPOINT ["/bin/chbackup"]
CMD ["--help"]
```

Key differences from Go tool Dockerfile: single build stage (no Go PPA/gcc/musl-tools), no `entrypoint.sh` wrapper, same ClickHouse uid/gid 101 for file ownership compatibility.

### 1.3 Kubernetes Sidecar

```yaml
containers:
  - name: clickhouse
    image: clickhouse/clickhouse-server:24.8
    volumeMounts:
      - name: data
        mountPath: /var/lib/clickhouse

  - name: chbackup
    image: ghcr.io/user/chbackup:latest
    args: ["server"]
    env:
      - name: S3_BUCKET
        value: "my-backups"
      - name: S3_ACCESS_KEY
        valueFrom:
          secretKeyRef: { name: s3-creds, key: access-key }
      - name: S3_SECRET_KEY
        valueFrom:
          secretKeyRef: { name: s3-creds, key: secret-key }
    volumeMounts:
      - name: data
        mountPath: /var/lib/clickhouse
    ports:
      - containerPort: 7171
```

### 1.4 Integration Test Environment

chbackup MUST have filesystem access to `/var/lib/clickhouse/` (FREEZE creates hardlinks). The test container runs ClickHouse server + chbackup in the same image — no shared volumes, no split-container hacks. Tests run against real S3, not MinIO.

#### 1.4.1 Test Dockerfile

```dockerfile
# Dockerfile.test — Multi-stage build: source compile + Altinity ClickHouse runtime
ARG CH_VERSION=25.3.8.10041.altinitystable

# --- Stage 1: Build static binary from source ---
FROM rust:1.93-alpine AS builder

RUN apk add --no-cache musl-dev perl make cmake g++ linux-headers openssl-dev openssl-libs-static pkgconfig

WORKDIR /build

# Cache dependencies: copy manifests first, do a dummy build
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs && echo '' > src/lib.rs \
    && cargo build --release 2>/dev/null || true \
    && rm -rf src target/release/chbackup target/release/deps/chbackup-* target/release/deps/libchbackup-*

# Copy real source and build
COPY src/ src/
RUN cargo build --release

# --- Stage 2: Test runtime ---
ARG CH_VERSION
FROM altinity/clickhouse-server:${CH_VERSION}

# Install test dependencies
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl jq netcat-openbsd python3 \
    && rm -rf /var/lib/apt/lists/*

# Copy built binary from builder stage
COPY --from=builder /build/target/release/chbackup /usr/local/bin/chbackup
RUN chmod +x /usr/local/bin/chbackup

# Copy test configs
COPY test/configs/clickhouse-config.xml /etc/clickhouse-server/config.d/test.xml
COPY test/configs/chbackup-test.yml /etc/chbackup/config.yml

# Copy S3 disk config generator (runs before ClickHouse starts)
COPY test/configs/generate-s3-disk-config.sh /usr/local/bin/generate-s3-disk-config.sh
RUN chmod +x /usr/local/bin/generate-s3-disk-config.sh

# Wrap entrypoint to generate S3 disk config before ClickHouse starts
RUN printf '#!/bin/bash\n/usr/local/bin/generate-s3-disk-config.sh\nexec /entrypoint.sh "$@"\n' \
    > /custom-entrypoint.sh && chmod +x /custom-entrypoint.sh
ENTRYPOINT ["/custom-entrypoint.sh"]

# Copy test fixtures and runner
COPY test/fixtures/ /test/fixtures/
COPY test/run_tests.sh /test/run_tests.sh
RUN chmod +x /test/run_tests.sh

EXPOSE 8123 9000 7171
```

Key design: two-stage build — `rust:1.93-alpine` compiles the static binary, then `altinity/clickhouse-server` provides the runtime with S3 disk support. A custom entrypoint runs `generate-s3-disk-config.sh` (which writes S3 disk XML from env vars) before delegating to the stock ClickHouse entrypoint. No pre-built binary required; `docker compose up --build` compiles from source.

#### 1.4.2 docker-compose.test.yml

```yaml
services:
  zookeeper:
    image: zookeeper:3.8
    restart: unless-stopped
    environment:
      ZOO_4LW_COMMANDS_WHITELIST: "ruok,srvr,stat"
    healthcheck:
      test: ["CMD-SHELL", "echo ruok | nc localhost 2181 | grep imok"]
      interval: 5s
      timeout: 3s
      retries: 10

  chbackup-test:
    build:
      context: .
      dockerfile: Dockerfile.test
      args:
        CH_VERSION: ${CH_VERSION:-25.3.8.10041.altinitystable}
    depends_on:
      zookeeper:
        condition: service_healthy
    ports:
      - "8123:8123"
      - "9000:9000"
      - "7171:7171"
    environment:
      # S3 credentials (required — compose fails fast if unset)
      S3_BUCKET: ${S3_BUCKET:?S3_BUCKET is required}
      S3_REGION: ${S3_REGION:-us-east-1}
      S3_ACCESS_KEY: ${S3_ACCESS_KEY:?S3_ACCESS_KEY is required}
      S3_SECRET_KEY: ${S3_SECRET_KEY:?S3_SECRET_KEY is required}
      S3_PREFIX: ${S3_PATH:-chbackup-test/${CH_VERSION:-25.3.8.10041.altinitystable}/${RUN_ID:-local}}
      # chbackup settings
      CHBACKUP_CONFIG: /etc/chbackup/config.yml
      LOG_LEVEL: ${LOG_LEVEL:-debug}
      # Test runner settings
      TEST_FILTER: ${TEST_FILTER:-}
      RUN_ID: ${RUN_ID:-local}
    healthcheck:
      test: ["CMD-SHELL", "clickhouse-client -q 'SELECT 1'"]
      interval: 5s
      timeout: 3s
      retries: 30
```

#### 1.4.3 Test Execution

```bash
# Run full test suite (builds from source automatically)
export S3_BUCKET=my-test-bucket S3_ACCESS_KEY=xxx S3_SECRET_KEY=xxx
docker compose -f docker-compose.test.yml up -d --build --wait
docker compose -f docker-compose.test.yml exec -T chbackup-test /test/run_tests.sh
docker compose -f docker-compose.test.yml down -v

# Run specific test
docker compose -f docker-compose.test.yml exec -T chbackup-test /test/run_tests.sh --filter "test_diff_from_crc64"

# Interactive debugging — shell into the container
docker compose -f docker-compose.test.yml exec chbackup-test bash
# Now you have: clickhouse-client, chbackup, and full /var/lib/clickhouse access

# Matrix: test across multiple ClickHouse versions
for ver in 23.8 24.3 24.8 25.1; do
  CH_VERSION=$ver docker compose -f docker-compose.test.yml up -d --build --wait
  docker compose -f docker-compose.test.yml exec -T chbackup-test /test/run_tests.sh
  docker compose -f docker-compose.test.yml down -v
done
```

#### 1.4.4 Test Suite Structure

All integration tests are bash-based, running inside a Docker container with real
ClickHouse and S3 backends. There are no Rust integration test files -- all 62 tests
(T1-T62) live in `test/run_tests.sh`.

```
test/
├── run_tests.sh                  # Integration test runner with 62 tests (T1-T62)
├── clean-s3.sh                   # S3 test data cleanup script (--dry-run supported)
├── configs/
│   ├── clickhouse-config.xml     # ZK connection, multi-disk, replicated settings
│   ├── chbackup-test.yml         # chbackup config pointing to S3 + localhost CH
│   └── generate-s3-disk-config.sh # Generates S3 disk config from env vars
├── fixtures/
│   ├── setup.sql                 # Creates test databases, tables (3 local + 2 S3 disk + 1 empty)
│   ├── seed_data.sql             # Deterministic INSERT statements for checksum validation
│   └── seed_large.sql            # Large data generator for multipart upload tests
Dockerfile.test                   # Multi-stage build (rust:alpine -> altinity/clickhouse-server)
docker-compose.test.yml           # ZooKeeper + ClickHouse/chbackup test compose
```

**Fixture: setup.sql** — Creates every table type referenced by T1-T18.

```sql
-- ============================================================
-- DATABASES
-- ============================================================
CREATE DATABASE IF NOT EXISTS default ENGINE = Atomic;
CREATE DATABASE IF NOT EXISTS logs ENGINE = Atomic;

-- ============================================================
-- default database — core test tables
-- ============================================================

-- T1, T2, T3, T4, T5: Plain MergeTree (baseline for most tests)
CREATE TABLE default.trades (
    trade_id     UInt64,
    ts           DateTime,
    symbol       LowCardinality(String),
    side         Enum8('buy' = 1, 'sell' = 2),
    price        Decimal64(8),
    qty          Decimal64(8),
    exchange     LowCardinality(String)
) ENGINE = MergeTree()
PARTITION BY toYYYYMM(ts)
ORDER BY (symbol, ts, trade_id);

-- T1: ReplacingMergeTree (tests SortPartsByMinBlock sequential ATTACH)
CREATE TABLE default.users (
    user_id      UInt64,
    updated_at   DateTime,
    name         String,
    email        String,
    tier         Enum8('free' = 0, 'pro' = 1, 'enterprise' = 2)
) ENGINE = ReplacingMergeTree(updated_at)
ORDER BY user_id;

-- T1, T9: ReplicatedMergeTree (tests ZK path handling, SYNC REPLICA)
CREATE TABLE default.events (
    event_id     UInt64,
    ts           DateTime64(3),
    user_id      UInt64,
    event_type   LowCardinality(String),
    payload      String
) ENGINE = ReplicatedMergeTree(
    '/clickhouse/tables/{database}/{table}', '{replica}'
)
PARTITION BY toYYYYMM(ts)
ORDER BY (user_id, ts, event_id);

-- T1: CollapsingMergeTree (tests SortPartsByMinBlock correctness)
CREATE TABLE default.sessions (
    user_id      UInt64,
    session_start DateTime,
    session_end  Nullable(DateTime),
    page_views   UInt32,
    sign         Int8
) ENGINE = CollapsingMergeTree(sign)
ORDER BY (user_id, session_start);

-- T7: Partitioned table (explicit partition test — backup 2 of 3 months)
CREATE TABLE default.metrics (
    metric_id    UInt64,
    ts           DateTime,
    host         LowCardinality(String),
    cpu_pct      Float32,
    mem_bytes    UInt64
) ENGINE = MergeTree()
PARTITION BY toYYYYMM(ts)
ORDER BY (host, ts);

-- Projection test (for --skip-projections flag)
CREATE TABLE default.orders (
    order_id     UInt64,
    ts           DateTime,
    user_id      UInt64,
    amount       Decimal64(2),
    status       Enum8('pending' = 0, 'filled' = 1, 'cancelled' = 2),
    PROJECTION orders_by_user (SELECT * ORDER BY user_id),
    PROJECTION orders_daily_sum (
        SELECT toDate(ts) AS day, sum(amount) AS total
        GROUP BY day
    )
) ENGINE = MergeTree()
ORDER BY (ts, order_id);

-- T1, T5: MaterializedView target table (.inner — created before MV)
CREATE TABLE default.events_hourly (
    hour         DateTime,
    event_type   LowCardinality(String),
    event_count  UInt64
) ENGINE = SummingMergeTree()
ORDER BY (hour, event_type);

-- T1, T5: MaterializedView (DDL-only — depends on events + events_hourly)
CREATE MATERIALIZED VIEW default.events_hourly_mv
TO default.events_hourly AS
SELECT
    toStartOfHour(ts) AS hour,
    event_type,
    count() AS event_count
FROM default.events
GROUP BY hour, event_type;

-- Phase 3: Dictionary (depends on default.users, tests topo sort)
CREATE DICTIONARY default.user_dict (
    user_id UInt64,
    name    String,
    tier    String
) PRIMARY KEY user_id
SOURCE(CLICKHOUSE(TABLE 'users' DB 'default'))
LAYOUT(HASHED())
LIFETIME(MIN 0 MAX 300);

-- Phase 3: Regular view (depends on default.trades)
CREATE VIEW default.recent_trades AS
SELECT * FROM default.trades
WHERE ts >= now() - INTERVAL 1 DAY;

-- Phase 3: AggregatingMergeTree + MV (tests parallel ATTACH — no dedup ordering needed)
CREATE TABLE default.symbol_stats (
    symbol       LowCardinality(String),
    trade_count  AggregateFunction(count, UInt64),
    avg_price    AggregateFunction(avg, Decimal64(8))
) ENGINE = AggregatingMergeTree()
ORDER BY symbol;

CREATE MATERIALIZED VIEW default.symbol_stats_mv
TO default.symbol_stats AS
SELECT
    symbol,
    countState(trade_id) AS trade_count,
    avgState(price) AS avg_price
FROM default.trades
GROUP BY symbol;

-- ============================================================
-- logs database — for T8 table filtering tests
-- ============================================================

CREATE TABLE logs.app_log (
    ts           DateTime64(3),
    level        Enum8('DEBUG' = 0, 'INFO' = 1, 'WARN' = 2, 'ERROR' = 3),
    service      LowCardinality(String),
    message      String,
    trace_id     UUID
) ENGINE = MergeTree()
PARTITION BY toYYYYMMDD(ts)
ORDER BY (service, ts);

CREATE TABLE logs.access_log (
    ts           DateTime,
    method       Enum8('GET' = 1, 'POST' = 2, 'PUT' = 3, 'DELETE' = 4),
    path         String,
    status       UInt16,
    latency_ms   UInt32,
    ip           IPv4
) ENGINE = MergeTree()
ORDER BY (ts);

CREATE TABLE logs.error_agg (
    hour         DateTime,
    service      LowCardinality(String),
    error_count  UInt64
) ENGINE = SummingMergeTree()
ORDER BY (hour, service);

-- ============================================================
-- Streaming engines — DDL-only mocks for T12 postponement test
-- Kafka/NATS/RabbitMQ need broker config that doesn't exist in test,
-- so we use the setting to prevent engine startup errors.
-- These test that restore CREATES them AFTER all data is attached.
-- ============================================================

-- T12: Kafka engine mock (will fail to connect — that's fine, we test DDL ordering)
-- NOTE: commented out by default since Kafka engine needs kafka_broker_list to parse.
-- Uncomment in T12 test script which handles the expected creation error.
-- CREATE TABLE default.kafka_source (
--     ts DateTime,
--     message String
-- ) ENGINE = Kafka()
-- SETTINGS kafka_broker_list = 'kafka:9092',
--          kafka_topic_list = 'test_topic',
--          kafka_group_name = 'chbackup_test',
--          kafka_format = 'JSONEachRow';

-- T12 alternative: use a simple MV with REFRESH to test postponement
-- (REFRESH MVs also need postponed activation — same Phase 2b logic)
-- Requires CH >= 24.1
-- CREATE MATERIALIZED VIEW default.refreshable_stats
-- REFRESH EVERY 1 HOUR
-- ENGINE = MergeTree() ORDER BY symbol AS
-- SELECT symbol, count() AS cnt FROM default.trades GROUP BY symbol;

-- ============================================================
-- Functions (Phase 4 restore test)
-- ============================================================
CREATE FUNCTION IF NOT EXISTS test_multiply AS (x, y) -> x * y;

-- ============================================================
-- S3 disk table (only for environments with S3 disk configured)
-- Tests are skipped if disk 's3disk' doesn't exist in system.disks.
-- ============================================================
-- CREATE TABLE default.s3_trades (
--     trade_id UInt64,
--     ts       DateTime,
--     symbol   String,
--     price    Decimal64(8)
-- ) ENGINE = MergeTree()
-- ORDER BY (symbol, ts)
-- SETTINGS storage_policy = 's3_policy';
```

**Fixture: seed_data.sql** — Deterministic data for checksum validation. Uses `numbers()` with modular arithmetic for reproducibility — same seed always produces same checksums.

```sql
-- ============================================================
-- Deterministic data: every INSERT uses numbers() so the exact
-- same data is produced on every run. Tests verify restore
-- correctness by comparing SELECT ... ORDER BY checksums.
-- ============================================================

-- default.trades — 1M rows across 3 months (Jan-Mar 2024)
INSERT INTO default.trades
SELECT
    number AS trade_id,
    toDateTime('2024-01-01 00:00:00') + (number * 7) AS ts,  -- ~3 months span
    arrayElement(['BTC/USD','ETH/USD','SOL/USD','DOGE/USD'], (number % 4) + 1) AS symbol,
    if(number % 3 = 0, 'buy', 'sell') AS side,
    toDecimal64(40000 + (number % 20000) + (number % 100) / 100, 8) AS price,
    toDecimal64((number % 1000) / 100 + 0.01, 8) AS qty,
    arrayElement(['binance','coinbase','kraken'], (number % 3) + 1) AS exchange
FROM numbers(1000000);

-- default.users — 10K rows (ReplacingMergeTree dedup test)
INSERT INTO default.users
SELECT
    number AS user_id,
    toDateTime('2024-01-01 00:00:00') + number AS updated_at,
    concat('user_', toString(number)) AS name,
    concat('user', toString(number), '@test.com') AS email,
    arrayElement(['free','pro','enterprise'], (number % 3) + 1) AS tier
FROM numbers(10000);

-- default.events — 500K rows (ReplicatedMergeTree, 3-month span)
INSERT INTO default.events
SELECT
    number AS event_id,
    toDateTime64('2024-01-01 00:00:00.000', 3) + number AS ts,
    (number % 10000) AS user_id,
    arrayElement(['click','view','purchase','signup'], (number % 4) + 1) AS event_type,
    concat('{"page":"/', toString(number % 100), '"}') AS payload
FROM numbers(500000);

-- default.sessions — 50K rows (CollapsingMergeTree)
INSERT INTO default.sessions
SELECT
    (number % 5000) AS user_id,
    toDateTime('2024-01-15 00:00:00') + (number * 60) AS session_start,
    toDateTime('2024-01-15 00:00:00') + (number * 60) + (number % 3600) AS session_end,
    (number % 50) + 1 AS page_views,
    1 AS sign
FROM numbers(50000);

-- default.metrics — 300K rows across exactly Jan/Feb/Mar 2024 (partition test)
INSERT INTO default.metrics
SELECT
    number AS metric_id,
    toDateTime('2024-01-01 00:00:00') + (number * 26) AS ts,  -- ~3 months at 26s intervals
    concat('host-', toString(number % 10)) AS host,
    (number % 100) + (number % 37) / 100 AS cpu_pct,
    ((number % 16) + 1) * 1073741824 AS mem_bytes  -- 1-16 GB
FROM numbers(300000);

-- default.orders — 100K rows (projection test)
INSERT INTO default.orders
SELECT
    number AS order_id,
    toDateTime('2024-02-01 00:00:00') + (number * 30) AS ts,
    (number % 10000) AS user_id,
    toDecimal64((number % 10000) + (number % 100) / 100, 2) AS amount,
    arrayElement(['pending','filled','cancelled'], (number % 3) + 1) AS status
FROM numbers(100000);

-- logs.app_log — 200K rows
INSERT INTO logs.app_log
SELECT
    toDateTime64('2024-01-15 00:00:00.000', 3) + number AS ts,
    arrayElement(['DEBUG','INFO','WARN','ERROR'], (number % 4) + 1) AS level,
    concat('svc-', toString(number % 5)) AS service,
    concat('Log message #', toString(number)) AS message,
    toUUID(concat(
        lpad(hex(intDiv(number, 65536)), 8, '0'), '-',
        lpad(hex(number % 65536), 4, '0'), '-4000-8000-',
        lpad(hex(number), 12, '0')
    )) AS trace_id
FROM numbers(200000);

-- logs.access_log — 200K rows
INSERT INTO logs.access_log
SELECT
    toDateTime('2024-01-15 00:00:00') + number AS ts,
    arrayElement(['GET','POST','PUT','DELETE'], (number % 4) + 1) AS method,
    concat('/api/v', toString((number % 3) + 1), '/resource/', toString(number % 100)) AS path,
    arrayElement([200, 200, 200, 201, 400, 404, 500], (number % 7) + 1) AS status,
    (number % 500) + 1 AS latency_ms,
    toIPv4(intDiv(number, 256) * 256 + number % 256) AS ip
FROM numbers(200000);

-- logs.error_agg — small summary table
INSERT INTO logs.error_agg
SELECT
    toStartOfHour(toDateTime('2024-01-15 00:00:00') + number * 3600) AS hour,
    concat('svc-', toString(number % 5)) AS service,
    (number % 100) + 1 AS error_count
FROM numbers(1000);

-- Force materialized views to process
OPTIMIZE TABLE default.events_hourly FINAL;
OPTIMIZE TABLE default.symbol_stats FINAL;

-- ============================================================
-- Checksum anchors: record expected state for restore verification.
-- Tests compare against these after restore to confirm correctness.
-- ============================================================
-- Run after all INSERTs:
--   SELECT count(), sum(cityHash64(*)) FROM default.trades
--   → Store as expected_trades_count, expected_trades_hash
-- Each test captures these BEFORE backup and asserts AFTER restore.
```

**Fixture: seed_large.sql** — T13 multipart upload test. Generates >100MB parts.

```sql
-- Generate a table with wide rows to produce parts > 100MB
-- 5M rows × ~50 bytes avg ≈ 250MB uncompressed → parts likely > 100MB compressed
CREATE TABLE IF NOT EXISTS default.large_test (
    id           UInt64,
    ts           DateTime,
    data         String,
    value        Float64
) ENGINE = MergeTree()
ORDER BY id
SETTINGS min_bytes_for_wide_part = 0;  -- force wide parts

INSERT INTO default.large_test
SELECT
    number AS id,
    toDateTime('2024-01-01') + number AS ts,
    randomPrintableASCII(200) AS data,  -- ~200 bytes per row
    rand64() / 1e18 AS value
FROM numbers(5000000);

-- Force merge into fewer large parts
OPTIMIZE TABLE default.large_test FINAL;
```

**ClickHouse test config**: `test/configs/clickhouse-config.xml`

```xml
<!-- Minimal CH config overrides for test environment -->
<clickhouse>
    <!-- ZooKeeper for ReplicatedMergeTree -->
    <zookeeper>
        <node>
            <host>zookeeper</host>
            <port>2181</port>
        </node>
    </zookeeper>

    <!-- Macros for Replicated table paths -->
    <macros>
        <shard>01</shard>
        <replica>replica-test</replica>
    </macros>

    <!-- Allow test functions -->
    <user_defined_functions_config>*_function.xml</user_defined_functions_config>
</clickhouse>
```

**Test runner**: `test/run_tests.sh`

```bash
#!/bin/bash
set -euo pipefail

FIXTURES=/test/fixtures
FILTER="${TEST_FILTER:-}"

log() { echo "[$(date +%H:%M:%S)] $*"; }
fail() { echo "FAIL: $*" >&2; exit 1; }

# Wait for ClickHouse
log "Waiting for ClickHouse..."
until clickhouse-client -q "SELECT 1" &>/dev/null; do sleep 0.5; done
log "ClickHouse ready"

# Setup: create tables and seed data
log "Creating test tables..."
clickhouse-client --multiquery < "$FIXTURES/setup.sql"

log "Seeding test data..."
clickhouse-client --multiquery < "$FIXTURES/seed_data.sql"

# Capture pre-backup checksums for restore verification
log "Capturing checksums..."
declare -A CHECKSUMS
for table in default.trades default.users default.events default.sessions \
             default.metrics default.orders logs.app_log logs.access_log; do
    CHECKSUMS[$table]=$(clickhouse-client -q \
        "SELECT count(), sum(cityHash64(*)) FROM $table FORMAT CSV")
done

export CHECKSUMS
log "Setup complete. Tables: $(clickhouse-client -q \
    "SELECT count() FROM system.tables WHERE database IN ('default','logs')")"

# Run test functions
# ... (each T1-T18 as a bash function, skip if FILTER set and doesn't match)
```

#### 1.4.5 Test Cases — Full Coverage

Authoritative source: `test/run_tests.sh`. All tests run inside the Docker container
(`Dockerfile.test`) against a real ClickHouse + S3 backend.

| ID | Name | `should_run` key | Validates |
|----|------|-------------------|-----------|
| — | Smoke: binary | `smoke_binary` | `chbackup --help` exits 0 |
| — | Smoke: config | `smoke_config` | `print-config` outputs YAML, redacts secrets |
| — | Smoke: list | `smoke_list` | `chbackup list` exits 0 |
| — | Round-trip | `test_round_trip` | create → upload → delete local → download → restore → verify row counts |
| T4 | Incremental backup chain | `test_incremental_chain` | `--diff-from`, carried parts skip re-upload, CRC64 verification |
| T5 | Schema-only backup | `test_schema_only` | `--schema-only` creates DDL but no data parts |
| T6 | Partitioned restore | `test_partitioned_restore` | `--partitions` filters parts by partition_id |
| T7 | Server API create + upload | `test_server_api_create_upload` | API create → upload → list round-trip |
| T8 | Backup name validation | `test_backup_name_validation` | Rejects reserved names, path traversal |
| T9 | Delete and list | `test_delete_and_list` | `delete local`, `delete remote`, `list` correctness |
| T10 | Clean broken | `test_clean_broken` | Detects and removes broken backups (missing metadata.json) |
| T11 | S3 object disk round-trip | `test_s3_disk_round_trip` | S3 disk create → upload → download → restore with CopyObject |
| T12 | Incremental with S3 disk | `test_s3_disk_incremental` | `--diff-from` carries S3 disk parts forward |
| T13 | S3 tables rename | `test_s3_restore_rename` | `--as` flag remap with S3 disk tables |
| T14 | S3 diff verification | `test_s3_incremental_diff` | Carried S3 parts not re-uploaded; new parts uploaded |
| T15 | Restore mode A (--rm) | `test_restore_mode_a` | DROP + recreate, extra rows removed |
| T16 | Database mapping (-m) | `test_database_mapping` | `-m src:dst` remap during restore |
| T17 | Data-only restore | `test_data_only_restore` | `--data-only` skips schema creation |
| T18 | Skip empty tables | `test_skip_empty_tables` | `--skip-empty-tables` omits CREATE for empty tables |
| T19 | Retention | `test_retention` | Local + remote retention with keep count |
| T20 | Clean shadow | `test_clean_shadow` | `chbackup clean` removes shadow directories |
| T21 | Structured exit codes | `test_exit_codes` | Exit codes 0/1/3/4 for success/error/not-found/lock |
| T22 | API full round-trip | `test_api_full_round_trip` | API download + restore + list pagination |
| T23 | API concurrent rejection | `test_api_concurrent` | HTTP 423 for overlapping operations |
| T24 | API kill | `test_api_kill` | `/api/v1/kill` cancels running operation |
| T25 | Partition-level create | `test_partitioned_create` | `--partitions` in create command |
| T26 | Skip projections | `test_skip_projections` | `--skip-projections '*'` excludes .proj dirs |
| T27 | Hardlink dedup | `test_hardlink_dedup` | `--hardlink-exists-files` deduplicates via hardlinks |
| T28 | RBAC backup and restore | `test_rbac` | `--rbac` flag backup/restore round-trip |
| T29 | Watch mode | `test_watch_mode` | Full + incremental cycle with watch loop |
| T30 | List formats | `test_list_formats` | `--format json/yaml/csv/tsv` output |
| T31 | Create remote | `test_create_remote` | One-step create + upload |
| T32 | Restore remote | `test_restore_remote` | One-step download + restore |
| T33 | Freeze by part | `test_freeze_by_part` | `freeze_by_part` config, per-partition FREEZE |
| T34 | Disk filtering | `test_disk_filtering` | `skip_disk_types` excludes disks from backup |
| T35 | Default config output | `test_default_config` | `print-config` default values match design doc |
| T36 | Schema-only restore | `test_schema_only_restore` | `--schema` restores DDL without data |
| T37 | Single table rename | `test_single_table_rename` | `--as` flag for single table remap |
| T38 | Upload --delete-local | `test_upload_delete_local` | Local backup removed after upload |
| T39 | Upload --diff-from-remote | `test_upload_diff_from_remote` | Remote-side incremental diff during upload |
| T40 | Tables command | `test_tables_command` | `-t` filter + `--all` flag |
| T41 | Tables --remote-backup | `test_tables_remote` | Tables listing from remote manifest |
| T42 | Clean broken remote | `test_clean_broken_remote` | Removes broken remote backups |
| T43 | Latest/previous shortcuts | `test_latest_previous` | `latest`/`previous` aliases in delete |
| T44 | Env var overlay | `test_env_overlay` | CLICKHOUSE_HOST, CHBACKUP_LOG_LEVEL override config |
| T45 | Config --env flag | `test_env_flag` | CLI `--env` overrides config values |
| T46 | API health/version/status | `test_api_health` | Health JSON, version, status endpoints |
| T47 | API tables pagination | `test_api_tables_pagination` | `offset`/`limit` params, `X-Total-Count` header |
| T48 | API tables remote | `test_api_tables_remote` | Tables endpoint with `remote_backup` param |
| T49 | API delete | `test_api_delete` | DELETE endpoint for local/remote backups |
| T50 | API actions dispatch | `test_api_actions` | `POST /api/v1/actions` command dispatch |
| T51 | API reload | `test_api_reload` | `/api/v1/reload` hot-reloads config |
| T52 | API restart | `test_api_restart` | `/api/v1/restart` recreates clients |
| T53 | API basic auth | `test_api_auth` | 401 for unauthenticated, 200 for authenticated |
| T54 | API clean endpoints | `test_api_clean` | API-driven clean operations |
| T55 | API Prometheus metrics | `test_api_metrics` | `/metrics` endpoint with operation counters |
| T56 | Configs backup/restore | `test_configs` | `--configs` flag backup/restore round-trip |
| T57 | Restore remote --rm | `test_restore_remote_rm` | Remote restore with destructive Mode A |
| T58 | Resume upload | `test_resume_upload` | Resumable upload via state file |
| T59 | Resume download | `test_resume_download` | Resumable download via state file |
| T60 | Print config overrides | `test_print_config_combined` | Combined config + env + CLI overrides |
| T61 | Resume restore | `test_resume_restore` | Resumable restore via state file |
| T62 | Clean --name | `test_clean_name` | Targeted shadow cleanup by backup name |

**Aspirational tests** (require infrastructure not in the single-node Docker environment):
- Replicated tables + ZK path conflict (multi-replica)
- Streaming engine postponement (Kafka/NATS broker)
- ON CLUSTER restore (multi-node cluster)
- DatabaseReplicated engine (DatabaseReplicated setup)
- ClickHouse TLS connection (self-signed cert)
- Cross-version compatibility (CI matrix provides implicit coverage)

#### 1.4.6 CI Integration (GitHub Actions)

The CI pipeline has two jobs: `check` (fast feedback: fmt, clippy, tests, coverage) and `integration` (ClickHouse version matrix with real S3). The test Dockerfile (`Dockerfile.test`) builds from source using a multi-stage build — no pre-built binary is needed.

```yaml
# .github/workflows/ci.yml (simplified)
name: CI
on:
  push:
    branches: [main]
  pull_request:

jobs:
  check:
    name: Check (fmt + clippy + test)
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with: { components: "rustfmt, clippy" }
      - run: cargo fmt -- --check
      - run: cargo clippy --all-targets -- -D warnings
      - run: cargo test

  integration:
    name: Integration (CH ${{ matrix.ch_version }})
    runs-on: ubuntu-latest
    needs: check
    strategy:
      matrix:
        ch_version:
          - "23.8.16.40.altinitystable"
          - "24.3.12.76.altinitystable"
          - "24.8.13.51.altinitystable"
          - "25.1.5.31.altinitystable"
      fail-fast: false
    steps:
      - uses: actions/checkout@v4
      - name: Start test environment
        env:
          CH_VERSION: ${{ matrix.ch_version }}
          RUN_ID: ${{ github.run_id }}-${{ matrix.ch_version }}
          S3_PATH: chbackup-test/${{ matrix.ch_version }}/${{ github.run_id }}
        run: docker compose -f docker-compose.test.yml up -d --build --wait
      - name: Run integration tests
        run: docker compose -f docker-compose.test.yml exec -T chbackup-test /test/run_tests.sh
      - name: Tear down
        if: always()
        run: docker compose -f docker-compose.test.yml down -v
```

S3 test isolation: each CI run uses `S3_PATH: chbackup-test/{ch_version}/{run_id}` — parallel matrix jobs write to different prefixes. Cleanup after each job via `chbackup delete remote`.

---

## 2. Commands

```
chbackup create          [-t db.table] [--partitions=X] [--diff-from=NAME] [--skip-projections=PATTERN]
                         [--schema] [--rbac] [--configs] [--named-collections]
                         [--skip-check-parts-columns] [backup_name]
chbackup upload          [--delete-local] [--diff-from-remote=NAME] [--resume] [backup_name]
chbackup download        [--hardlink-exists-files] [--resume] [backup_name]
chbackup restore         [-t db.table] [--as=dst_db.dst_table] [-m originDB:targetDB]
                         [--partitions=X] [--schema] [--data-only] [--rm]
                         [--rbac] [--configs] [--named-collections]
                         [--skip-empty-tables] [--resume] [backup_name]
chbackup create_remote   [-t db.table] [--diff-from-remote=NAME] [--delete-source]
                         [--rbac] [--configs] [--named-collections]
                         [--skip-check-parts-columns] [--resume] [backup_name]
                         (create + upload in one step; used by watch mode)
chbackup restore_remote  [-t db.table] [-m originDB:targetDB] [--rm]
                         [--rbac] [--configs] [--named-collections]
                         [--skip-empty-tables] [--resume] [backup_name]
                         (download + restore in one step)
chbackup list            [local|remote]
chbackup tables          [-t db.table] [--all] [--remote-backup=NAME]
                         (list tables from ClickHouse or from a remote backup)
chbackup delete          [local|remote] [backup_name]
chbackup clean           [--name=BACKUP] (remove leftover shadow/ data)
chbackup clean_broken    [local|remote] (remove broken backups with missing/corrupt metadata)
chbackup default-config  (print default config to stdout)
chbackup print-config    (print resolved config after env var overlay — useful for debugging)
chbackup watch           [--watch-interval=1h] [--full-interval=24h] [--name-template=TPL] [-t db.table]
chbackup server          [--watch] (API mode for Kubernetes, optionally with watch loop)
```

**Flag reference:**

| Flag | Commands | Description |
|------|----------|-------------|
| `-t, --tables` | create, restore, create_remote, restore_remote, watch | Table filter pattern (globs: `db.*`, `*.table`) |
| `--as` | restore, restore_remote | Rename single table: `-t db.src --as=db.dst` |
| `-m, --database-mapping` | restore, restore_remote | Bulk database remap: `-m prod:staging,logs:logs_copy` |
| `--partitions` | create, restore | Filter by partition names |
| `--diff-from` | create | Local incremental base |
| `--diff-from-remote` | upload, create_remote | Remote incremental base |
| `--schema` | create, restore | Schema only (no data) |
| `--data-only` | restore | Data only (no schema) |
| `--rm, --drop` | restore, restore_remote | DROP existing tables before restore (Mode A) |
| `--resume` | upload, download, restore | Resume interrupted operation from state file |
| `--delete-local` | upload | Delete local backup after successful upload |
| `--hardlink-exists-files` | download | Deduplicate local parts via hardlinks |
| `--skip-projections` | create, create_remote | Glob patterns for projections to skip |
| `--rbac` | create, restore, create_remote, restore_remote | Include RBAC objects (users, roles, quotas, etc.) |
| `--configs` | create, restore, create_remote, restore_remote | Include ClickHouse server config files |
| `--named-collections` | create, restore, create_remote, restore_remote | Include Named Collections |
| `--skip-check-parts-columns` | create, create_remote | Allow backup with inconsistent column types across parts |
| `--skip-empty-tables` | restore, restore_remote | Skip restoring tables that have zero data parts |
| `--delete-source` | create_remote | Delete local backup after upload (same as `--delete-local` on upload) |
| `--all` | tables | Show all tables including system |

**Global flags** (apply to all commands):

| Flag | Description |
|------|-------------|
| `--config, -c` | Config file path (default: `/etc/chbackup/config.yml`, override via `CHBACKUP_CONFIG` env) |
| `--env` | Override any config param via CLI: `--env S3_BUCKET=other-bucket --env LOG_LEVEL=debug`. Accepts both env-style keys (e.g., `S3_BUCKET=val`) and dot-notation keys (e.g., `s3.bucket=val`). Env-style keys are translated to dot-notation internally via a static lookup table matching the `apply_env_overlay()` mappings. |

**Environment variable overlay**: Every config parameter can be overridden via an environment variable. Variable names are the UPPERCASE version of the config key path with underscores. Examples: `CLICKHOUSE_HOST=10.0.0.1`, `S3_BUCKET=my-bucket`, `BACKUPS_TO_KEEP_REMOTE=14`, `LOG_LEVEL=debug`, `API_LISTEN=0.0.0.0:7171`. This is essential for Kubernetes deployments where config is injected via `env:` in pod specs. Env vars take precedence over the config file; `--env` CLI flags take precedence over both.

All mutating commands acquire a **PID lock** before execution:
- Backup-scoped operations (create, upload, download, restore): lock on `/tmp/chbackup.{backup_name}.pid`
- Global operations (clean, clean_broken, retention): lock on `/tmp/chbackup.global.pid`
- Read-only operations (list, tables, status): no lock needed

The lock records PID, command, and timestamp. On start, checks if existing PID is still alive. Prevents concurrent operations on the same backup name — essential for the API server where concurrent HTTP requests could corrupt state. The global lock prevents `delete` from running while `list` is iterating manifests (list uses a snapshot of the manifest list, not a lock).

---

## 3. Backup Flow

### 3.1 Pre-Flight: Mutation Check

ClickHouse mutations (DELETE/UPDATE) are async. A half-applied mutation means some parts have the change, some don't. FREEZE at that moment captures an inconsistent snapshot.

```
1. Batch-query all pending mutations across all target tables:
   SELECT database, table, mutation_id, command, parts_to_do_names
   FROM system.mutations
   WHERE is_done = 0 AND (database, table) IN (target_tables)
2. Classify pending mutations per table:
   - Data mutations (DELETE/UPDATE): DANGEROUS if half-applied
   - Schema mutations (ADD/DROP/MODIFY COLUMN): SAFE — ClickHouse
     tolerates schema drift between parts and table DDL during ATTACH
3. Action for data mutations:
   Default:     wait for completion (poll system.mutations, timeout 5m)
   --skip-mutation-wait:        proceed, save mutations in manifest for re-apply
   --mutation-wait-timeout=Nm:  custom timeout
4. If timeout exceeded: abort with clear error message
```

Why not just re-apply mutations on restore like the Go tool? Because:
- Re-applying can take hours on large tables
- Non-idempotent mutations (UPDATE x = x + 1) would double-apply on already-processed parts
- The Go tool's `backup_mutations` is OFF by default — most users silently get inconsistent backups

If `--skip-mutation-wait` was used, manifest stores the mutations:
```json
{
  "pending_mutations": [
    {
      "mutation_id": "0000000002",
      "command": "DELETE WHERE user_id = 5",
      "parts_to_do": ["all_0_0_0", "all_1_1_0", "all_2_2_0"]
    }
  ]
}
```
On restore, we re-apply them with a clear warning.

### 3.2 Pre-Flight: Sync Replica

For Replicated*MergeTree tables, ensure replica has all data from ZooKeeper before freezing:

```sql
SYSTEM SYNC REPLICA `db`.`table`
```

Without this, backup may capture a stale replica missing recent inserts.

**Important**: SYNC REPLICA can block for minutes on large tables. It runs BEFORE the FREEZE semaphore is acquired — not inside the per-table task. This prevents slow replicas from holding semaphore permits and starving other tables:

```
Pre-flight sync phase (bounded by max_connections — shares semaphore with FREEZE):
  For each Replicated* table:
    tokio::spawn(async {
      sync_sem.acquire().await;
      SYSTEM SYNC REPLICA `db`.`table`
    })
  Await all (with per-table timeout from mutation_wait_timeout)

FREEZE phase (same max_connections semaphore, recycled):
  Now all replicas are synced, FREEZE is fast
```

### 3.3 Pre-Flight: Parts Column Consistency Check

Before freezing, validate that all active parts across ALL target tables have consistent column types in a **single batch query**. Detects schema drift issues that could cause restore failures:

```sql
SELECT database, table, name AS column, groupUniqArray(type) AS uniq_types
FROM system.parts_columns
WHERE active AND (database, table) IN (target_tables)
GROUP BY database, table, column
HAVING length(uniq_types) > 1
```

Skip Enum, Tuple, Nullable(Enum/Tuple), Array(Tuple) types which commonly have benign drift. Configurable via `check_parts_columns: true`.

### 3.4 FREEZE and Collect (Parallel)

Tables are frozen and collected **in parallel** using a tokio semaphore bounded by `max_connections`. Each table gets a **human-readable, deterministic freeze name** so operators can easily identify and manually clean up after crashes.

**Freeze naming convention**: `chbackup_{backup_name}_{db}_{table}`

```
Example: backing up tables default.trades and logs.events as "daily_mon"

shadow/chbackup_daily_mon_default_trades/     ← immediately obvious
shadow/chbackup_daily_mon_logs_events/

Operator can manually clean up:
  ALTER TABLE default.trades UNFREEZE WITH NAME 'chbackup_daily_mon_default_trades';
  ALTER TABLE logs.events UNFREEZE WITH NAME 'chbackup_daily_mon_logs_events';

Or bulk cleanup:
  ls /var/lib/clickhouse/shadow/ | grep chbackup_daily_mon
```

Special characters in database/table names are sanitized to underscores for filesystem safety (e.g., `my-db.table(v2)` → `chbackup_daily_mon_my_db_table_v2_`). The name is unique per table within a backup — no collision between parallel freezes.

```
1. On startup, clean orphaned shadow directories:
   - Scan shadow/ for directories matching chbackup_* prefix
   - Extract backup_name from directory name
   - Check PID lock file for that backup_name:
     If PID lock exists and process is alive → SKIP (concurrent operation in progress)
     If no PID lock or process dead → safe to clean, UNFREEZE + remove

2. Spawn async tasks for each table matching --tables pattern
   (up to max_connections concurrent):

   Per-table task:
   a. Skip tables matching skip_tables, skip_table_engines, or skip_disks/skip_disk_types patterns
   b. Build freeze name: chbackup_{backup_name}_{sanitize(db)}_{sanitize(table)}
   c. FREEZE:
      - If --partitions is set: iterate partition list, execute
        ALTER TABLE `db`.`table` FREEZE PARTITION 'X' WITH NAME '{freeze_name}'
        for each partition. Merge shadow results.
      - If freeze_by_part: true with freeze_by_part_where: query system.parts with WHERE clause,
        freeze individual parts (allows fine-grained control for huge tables)
      - Default: ALTER TABLE `db`.`table` FREEZE WITH NAME '{freeze_name}'
      - **On error code 60 (UNKNOWN_TABLE) or 81 (DATABASE_NOT_FOUND)**:
        Log warning "Table {db}.{table} disappeared during backup, skipping".
        This handles tables DROPped between gathering the table list and executing FREEZE
        (common with frequent CREATE/DROP workloads). Controlled by
        `ignore_not_exists_error_during_freeze: true` (default).
   d. Walk /var/lib/clickhouse/shadow/{freeze_name}/
      - Skip frozen_metadata.txt files (issue #826)
      - Skip projection directories matching --skip-projections patterns
      - For each part directory:
        Local disk:  hardlink from shadow/ to local backup staging area
        S3 disk:     parse object metadata files → collect S3 object keys
                     Upload object disk data to backup bucket (parallel, bounded by
                     object_disk_copy_concurrency semaphore)
      - Compute CRC64 checksum of each part's checksums.txt file
   e. ALTER TABLE `db`.`table` UNFREEZE WITH NAME '{freeze_name}'
   f. Return table metadata (DDL, parts, checksums, pending mutations)

3. After all table tasks complete:
   - If zero tables were processed and `allow_empty_backups: false` (default): ERROR
   - If zero tables were processed and `allow_empty_backups: true`: create empty backup with metadata only

4. Collect RBAC and config objects (if --rbac / --configs flags or always-on config):
   - RBAC: query system.users, system.roles, system.row_policies, system.settings_profiles,
     system.quotas → serialize to access/*.jsonl files in backup directory
   - Configs: copy CH config files from config_dir to backup directory
   - Named Collections: query system.named_collections → serialize to backup
   Controlled by: `rbac_backup_always`, `config_backup_always`, `named_collections_backup_always`

5. Await all table tasks (fail-fast with guaranteed cleanup):
   On first error:
     - Signal cancellation to remaining tasks
     - But each task's UNFREEZE runs in a Drop guard / scopeguard:
       even if cancelled, UNFREEZE always executes
     - Wait for all UNFREEZE operations to complete before propagating error
   This prevents shadow directory leaks on partial failures.
6. Aggregate table metadata, save backup metadata JSON
```

Why per-table parallelism is safe: FREEZE is a ClickHouse server operation that creates hardlinks in the `shadow/` directory. Each table gets its own deterministic freeze name, so shadow directories never overlap. The shadow walk and hardlinking are filesystem I/O that benefits heavily from parallelism — a backup of 200 tables on a 16-core machine with `max_connections: 8` runs ~8x faster than sequential.

**Table path encoding**: Database and table names can contain special characters (`!@#$^&*()-+=` etc.). All filesystem paths and S3 keys use URL-encoding for these characters (matching Go tool's `TablePathEncode`). Example: `my-db.table(v2)` → `my%2Ddb/table%28v2%29`.

**Projection skipping** (`--skip-projections`): Projections are pre-computed materialized indexes stored as `.proj/` subdirectories inside parts. Skipping saves backup space when projections will be rebuilt. Pattern format: `db.table:proj_name` with glob support. During shadow walk, skip directories matching `*.proj` patterns.

**Backup failure cleanup**: On `backup::create()` failure, the partial backup directory is removed and `clean_shadow()` runs for the backup name. This prevents broken backup accumulation and shadow directory leaks when a backup fails mid-process (e.g., ClickHouse goes down during FREEZE, disk runs out of space during hardlink). The cleanup is best-effort — if removal fails, the broken backup will be detected and cleaned by the `clean_broken` command.

### 3.5 Incremental Diff (--diff-from)

Parts are immutable. Part name IS identity. Comparison is pure name matching (confirmed by Go tool source — `addRequiredPartIfNotExists` does exact string compare, no block range parsing). However, we add CRC64 checksum verification (#1307) to catch silent data corruption where part names match but content differs (can happen with mutations on different replicas):

```
1. Load previous backup manifest
2. For each part in current FREEZE:
   a. If part name exists in previous manifest for same table+disk:
      Compare CRC64(current checksums.txt) vs CRC64(previous checksums.txt)
      If CRC64 matches:
        → Mark as "carried forward", record previous S3 key
        → Do NOT upload data (already in backup bucket)
      If CRC64 differs:
        → Upload as new part (data changed despite same name)
        → Log warning: "Part {name} has same name but different checksum"
   b. If part name is new:
      → Upload data to backup bucket
3. Manifest lists ALL parts needed (self-contained, no chain references)
   Each part entry has:
     - name: "all_1_3_1"
     - backup_key: "s3://backup-bucket/chbackup/daily-mon/default/trades/s3disk/all_1_3_1/"
     - source: "uploaded" | "carried:daily-sun"
     - checksum_crc64: 1234567890   ← CRC64 of checksums.txt
```

**Critical difference from Go tool**: Go tool uses chain-based incremental where each manifest only stores its own parts and a `RequiredBackup` pointer. Download must recursively follow the chain. Deleting any backup in the chain breaks all subsequent ones (issues #907, #882). Our self-contained manifest approach means every backup is independently restorable — the manifest lists ALL parts with their S3 keys, no chain traversal needed.

**Important nuance**: "self-contained" means the manifest has complete knowledge of all parts needed. However, carried-forward parts physically reside under the original backup's S3 prefix (`source: "carried:daily-sun"` → key points to `chbackup/daily-sun/...`). The safe GC in Section 8.2 prevents deletion of these shared objects. This is a deliberate tradeoff: avoid duplicating terabytes of data in S3, while keeping the manifest complete enough that no chain walking is needed for download or restore.

### 3.6 Upload (Async Parallel)

All uploads are fully async and non-blocking. The upload pipeline uses a **flat concurrency model**: a single shared tokio semaphore (`upload_concurrency`) governs ALL concurrent S3 uploads across ALL tables simultaneously. This means parts from different tables upload in parallel — there's no sequential per-table bottleneck.

```
Upload pipeline (all async, non-blocking):

1. Collect all uploadable work items across all tables:
   work_items = []
   For each table:
     For each part with source="uploaded":
       work_items.push(UploadTask { table, disk, part, files })

2. Spawn all work items through shared semaphore (upload_concurrency):
   For each work_item:
     tokio::spawn(async {
       semaphore.acquire().await;      // bounded concurrency
       // Streaming pipeline: read → compress → upload (zero temp files)
       // file_reader | tar_stream | lz4_compress | s3_multipart_upload
       track in resumable state
       release semaphore permit
     })

3. S3 disk object parts use SEPARATE semaphore (object_disk_copy_concurrency):
   For each S3 disk part:
     tokio::spawn(async {
       obj_semaphore.acquire().await;
       CopyObject (server-side) from data bucket to backup bucket
       No compression needed
     })

4. Await all tasks (fail-fast on first error, cancel remaining)
5. Upload RBAC, Named Collections, Functions metadata
6. Upload manifest JSON last (atomic commit):
   - Upload to temp key: {prefix}/{backup_name}/metadata.json.tmp
   - CopyObject to final key: {prefix}/{backup_name}/metadata.json
   - DeleteObject temp key
   A backup is only "visible" when metadata.json exists at the final key.
   If crash between tmp upload and copy → broken backup, cleaned by clean_broken.
7. Apply retention: delete oldest remote backups exceeding backups_to_keep_remote
```

**Why flat concurrency, not nested**: The Go tool also uses this model. With 50 tables averaging 20 parts each and `upload_concurrency: 8`, the semaphore ensures exactly 8 concurrent S3 uploads at any time — whether those 8 are all from one large table or spread across 8 small tables. This gives natural load balancing: small tables finish fast and free permits for parts from larger tables.

**S3 multipart uploads**: Since upload is a streaming pipeline (no temp file), we can't know compressed size in advance. Strategy: always use multipart upload for parts where `uncompressed_size > multipart_threshold` (default 32MB), since compression ratio is typically 2-4x and the result will likely exceed S3's single PUT limit. For small parts below the threshold, use a single PutObject. This avoids the waste of starting a PutObject, discovering it's too large, and re-uploading. The multipart upload itself counts as one semaphore permit — internal chunk parallelism doesn't consume additional permits.

**Multipart cleanup on failure**: On upload cancellation or error, call `AbortMultipartUpload` for any in-progress multipart uploads. Without this, incomplete multipart uploads leak S3 storage (hidden, not visible in bucket listing, but billed). The scopeguard pattern ensures abort runs even on `fail-fast` cancellation.

**Multipart CopyObject for large objects**: S3 imposes a 5GB hard limit on single CopyObject operations. For S3 disk objects exceeding 5GB (5,368,709,120 bytes), `copy_object()` automatically switches to multipart copy using the `UploadPartCopy` API. Chunk sizes are auto-calculated based on source object size with a maximum of 10,000 parts (S3 limit). On error, the multipart upload is aborted to prevent storage leaks. If `head_object()` fails to determine size, the operation falls through to a single CopyObject attempt (which will fail for >5GB objects, caught by the retry/fallback logic).

**Rate limiting**: Applied at the byte stream level using a token bucket. The rate limit is global across all concurrent uploads, not per-upload. Implemented as an async wrapper around the S3 upload stream.

**Resumable state**: Tracked in `{backup_name}/upload.state.json`. On failure, records which files completed. On retry with `--resume`, skips already-uploaded files. State file deleted on successful completion. If parameters changed between runs (different table pattern, partitions, etc.), state is invalidated.

### 3.7 Object Disk Metadata Parsing

ClickHouse has 5 metadata format versions. Must handle all:

| Version | Name | Path Format |
|---------|------|-------------|
| 1 | VersionAbsolutePaths | Absolute S3 paths |
| 2 | VersionRelativePath | Relative to disk root |
| 3 | VersionReadOnlyFlag | v2 + ReadOnly flag |
| 4 | VersionInlineData | Small data inlined in metadata (ObjectSize=0) |
| 5 | VersionFullObjectKey | Full object key (CH 25.10+) |

Metadata file format:
```
{version}
{object_count}\t{total_size}
{obj1_size}\t{obj1_path}
{obj2_size}\t{obj2_path}
{ref_count}
{read_only}       ← only if version >= 3
{inline_data}     ← only if version >= 4
```

**InlineData** (v4+): Small objects stored directly in the metadata file rather than as separate S3 objects. These have `ObjectSize == 0`. During backup, preserve the inline data string. During restore, write it back to the metadata file. No S3 copy needed for inline objects.

**VersionFullObjectKey** (v5, CH 25.10+): Contains full absolute S3 path including disk prefix. When reading, extract the last 2 path components to get the relative path (matching Go tool's normalization for #1290). When writing during restore, prepend the destination disk's remote path prefix.

---

## 4. Download (Async Parallel)

Downloads use the same flat concurrency model as uploads — a single shared semaphore (`download_concurrency`) governs all concurrent S3 downloads across all tables. Two-phase pipeline: metadata first (needed for planning), then data.

```
Download pipeline (all async, non-blocking):

Phase 1 — Metadata (parallel, no semaphore — metadata files are tiny JSON):
  Spawn async tasks for each table:
    Download table metadata JSON from S3
    Parse to determine parts list, disk assignments, sizes
  Await all metadata tasks

Pre-flight:
  Check available disk space (query system.disks for free_space)
  Compare against total download size from metadata. Abort if insufficient.

Phase 2 — Data (parallel, flat concurrency across all tables):
  Collect all download work items across all tables:
    For each table, for each part/archive file → DownloadTask

  Spawn all tasks through shared semaphore (download_concurrency):
    tokio::spawn(async {
      semaphore.acquire().await;

      // Checksum dedup optimization (--hardlink-exists-files):
      if local backup has part with same name AND matching CRC64:
        hardlink to existing part → skip download → release permit
        return

      // Resumable check:
      if state file shows this file already downloaded:
        release permit
        return

      // Actual download:
      download compressed archive from S3 (async streaming)
      decompress → write to local backup directory
      update resumable state
      release permit
    })

  For S3 disk parts (metadata only, separate semaphore):
    CopyObject metadata files from backup bucket → local staging
    (actual S3 objects stay in backup bucket until restore)

  Await all tasks (fail-fast on first error)
```

**Streaming decompression**: Downloads use async streaming — the S3 response body is piped through a decompression stream directly to disk. No intermediate buffer for the full compressed archive. This keeps memory usage constant regardless of part size.

**Post-download verification**: After decompressing each part, compute CRC64 of the local `checksums.txt` and compare against the manifest's `checksum_crc64`. If mismatch: delete the corrupted part, log error, and retry (up to `retries_on_failure`). This catches S3 bit-rot, network corruption, and decompression errors before the backup is considered complete.

**Rate limiting**: Global token bucket across all concurrent downloads, applied at the byte stream level.

---

## 5. Restore Flow

### 5.1 Phased Restore Architecture

The Go tool mixes DDL-only objects (dictionaries, views) with data objects (MergeTree tables) in the same pipeline, causing dependency failures and wasted work. We use explicit phases:

```
Phase 1: Schema — CREATE databases
Phase 2: Data tables — MergeTree family (CREATE + ATTACH PART)
         Sorted by engine priority:
           0: Regular MergeTree tables
           1: .inner tables (MV storage targets)
Phase 2b: Postponed tables — Streaming engines (Kafka, NATS, RabbitMQ, S3Queue)
          and refreshable MVs. Created AFTER all data is attached (#1235).
          These engines start consuming immediately on CREATE, so they must
          be activated only after the target tables have their data restored.
Phase 3: DDL-only objects — topological sort by dependencies
         Sorted by engine priority:
           0: Dictionaries (may depend on Phase 2 tables)
           1: Views, MaterializedViews, LiveViews, WindowViews
           2: Distributed, Merge engine tables
Phase 4: Functions, Named Collections, RBAC
```

**Engine priority sorting** (from Go tool's `getOrderByEngine`): Within each phase, tables are sorted by engine type. This ensures MV storage tables (`.inner_id.*`/`.inner.*`) are created before the MV definitions that reference them, and streaming engines (Kafka etc.) are created after their target tables exist.

**For DROP operations, order is reversed**: Views/MVs dropped first (they depend on tables), then inner tables, then regular tables. This avoids "table has dependencies" errors during teardown.

**Distributed DDL (`restore_schema_on_cluster`)**: When restoring to a cluster, schema DDL should propagate to all replicas. If `restore_schema_on_cluster` is set to a cluster name (from `system.clusters`), all CREATE/DROP/ALTER statements are executed with `ON CLUSTER '{cluster}'` as Distributed DDL. Exception: tables inside a **Replicated Database Engine** database — DDL is automatically replicated via Keeper, so ON CLUSTER must NOT be added (it would cause errors). Detection: query `SELECT engine FROM system.databases WHERE name = '{db}'` — if engine is `Replicated`, skip ON CLUSTER for all tables in that database.

**Replicated Database Engine**: ClickHouse's `DatabaseReplicated` engine replicates DDL automatically via ZooKeeper/Keeper. When restoring to or from a Replicated database:
- Skip ON CLUSTER clause (DDL replication is implicit)
- UUIDs are auto-assigned by the engine — do not reuse backup UUIDs
- CREATE TABLE may need `SYNC` flag to wait for replication

**Distributed table cluster references (`restore_distributed_cluster`)**: When restoring a `Distributed(cluster, db, table)` engine table, the cluster name from the backup DDL may not exist in the target's `system.clusters`. When `restore_distributed_cluster` is set, rewrite the engine definition to use the specified cluster name.

### 5.2 Phase 2: Data Table Restore

Two modes:

**Mode A — Full restore (`--rm` or fresh target)**
```
DROP TABLE IF EXISTS (if --rm)
  - Drop in reverse engine priority order (views → inner → regular)
  - Retry loop for dependency failures (fallback if our ordering misses something)
CREATE TABLE from backed-up DDL
  - Replicated tables: check for ZK path conflict (see below)
  - If --as=dst_db.dst_table: rewrite DDL (table name, UUID, ZK path)
Restore data (see 5.3)
```

**Mode B — Non-destructive restore (default)**
```
If table doesn't exist: CREATE TABLE from DDL, then restore data (5.3)
If table exists: Restore data only (5.3 — attach missing parts)
  No DROP, no schema change — safe by design
```

**ATTACH TABLE mode** (`restore_as_attach: true`) for Replicated*MergeTree full restores:
```
DETACH TABLE ... SYNC
SYSTEM DROP REPLICA 'replica_name' FROM ZKPATH 'zk_path'
ATTACH TABLE ...
SYSTEM RESTORE REPLICA ...
```
This wipes ZK state and rebuilds from local parts. Faster than part-by-part ATTACH for full restores.

**Replica ZK path conflict resolution**: When creating a Replicated table, the ZooKeeper path from the backup DDL may already be occupied (e.g., restoring to a cluster that still has remnants). Before CREATE:
```
1. Parse Replicated*MergeTree parameters from DDL (regex: engine, replica_path, replica_name)
   Handle both explicit params and short syntax (empty parentheses → uses server defaults)
2. Resolve macros: {database}, {table}, {uuid}, {shard}, {replica} via system.macros
3. Check system.zookeeper: SELECT count() FROM system.zookeeper WHERE path='resolved_path/replicas/resolved_name'
4. If replica already exists:
   SYSTEM DROP REPLICA 'resolved_name' FROM ZKPATH 'resolved_path' (#1162)
   (Uses actual ZK path from backup DDL, not default_replica_path macro)
   Log warning about path conflict
5. Handle {uuid} macro: if present in replica_path, substitute from CREATE TABLE's UUID clause
```

### 5.3 Data Restore: Part Attachment (Parallel per Table)

Tables are restored in parallel, bounded by `max_connections`. Within each table, parts are attached **sequentially** in sorted order (required for correctness). S3 object disk copies within each table run in parallel.

**Critical: SortPartsByMinBlock** (from Go tool's `part_metadata.go:16`)
Parts MUST attach in min_block order within each partition for engines with dedup semantics. See detailed explanation after the pipeline.

```
Restore pipeline:

Spawn async tasks for each table (bounded by max_connections):
  tokio::spawn(async {
    table_semaphore.acquire().await;

    1. Sort parts by partition, then by min_block (second element of part name)
       Part name format: {partition}_{min_block}_{max_block}_{level}

    2. For S3 disk parts — parallel object copy (bounded by object_disk_copy_concurrency):
       Spawn async tasks for all S3 objects in this table:
         CopyObject from backup bucket → data bucket (see 5.4)
       Await all copies

    3. For each part (SEQUENTIAL for Replacing/Collapsing, parallel otherwise):
       a. Local disk: hardlink from backup to {table_data_path}/detached/{part_name}/
          Hardlink is zero-copy (instant, no I/O, no disk space).
          Fall back to copy ONLY if cross-device (different filesystem mount).
       b. S3 disk: write rewritten metadata to detached/
       c. Chown all restored files to match ClickHouse data directory ownership
       d. ALTER TABLE `db`.`table` ATTACH PART '{part_name}'

    4. For non-destructive mode:
       - Check if exact part name exists in system.parts → SKIP
       - If ATTACH fails with error 232/233 (overlapping block range) →
         data exists in different merge state → log warning, skip

    5. Re-apply pending mutations if any (5.7)

    release table_semaphore permit
  })

**Resumable restore**: Track attached parts in `{backup_name}/restore.state.json`. On `--resume`, query `system.parts` for already-attached parts and skip them. This avoids re-attaching 1000 parts when part 500 failed — only the failed part and remaining ones are retried. State file records: `{ table: "db.table", attached_parts: ["part_1", "part_2", ...] }`.

Await all table tasks (fail-fast on first error)
```

**Why ATTACH PART must be sequential for some engines**: ClickHouse issue #71009: attaching parts out of min_block order for ReplacingMergeTree/CollapsingMergeTree/VersionedCollapsingMergeTree causes incorrect deduplication behavior. For plain MergeTree, SummingMergeTree, and AggregatingMergeTree there is no ordering dependency — parts can be attached in any order. Optimization: parallelize ATTACH within a table for engines without dedup semantics, fall back to sequential sorted ATTACH for Replacing/Collapsing variants.

**Why S3 object copies CAN be parallel**: CopyObject is a server-side S3 operation with no ordering dependency. All objects for a table can be copied concurrently before the sequential ATTACH phase begins.

**Wait for replication queue**: Before starting ATTACH on Replicated tables, check `CheckReplicationInProgress` — avoids conflicts with concurrent replication operations.

**File ownership (Chown)**: After copying/hardlinking parts to ClickHouse data directories, all files must be owned by the ClickHouse process user. Detect correct uid/gid by `stat()`-ing the ClickHouse data path (typically `clickhouse:clickhouse`). Skip if not running as root. This is **critical** in containers where chbackup may run as root but ClickHouse runs as a non-root user. Without chown, ClickHouse fails to read the restored parts.

### 5.4 S3 Object Disk Restore — Object Isolation

**THE BUG IN THE GO TOOL**: When restoring `db.tableA` as `db.tableB` on S3 disk, the Go tool (pre-fix #1278) copies S3 objects to the SAME paths as the original table. Both tables share identical S3 object references. Dropping either table deletes the shared objects, silently corrupting the other.

```
How ClickHouse S3 disk works:
  Local metadata file (tiny):  points to S3 objects via ObjectPath
  S3 data object (large):      actual column data

  tableA metadata → s3://data-bucket/store/abc/def/part_1/data.bin
  tableB metadata → s3://data-bucket/store/abc/def/part_1/data.bin  ← SAME!

  DROP TABLE tableA → ClickHouse deletes store/abc/def/part_1/data.bin
  tableB is now broken — reads fail with "object not found"
```

**Go tool's fix** (#1265/#1278): appends `_backupName` suffix to paths when table/database mapping is configured. Problems:
- Only triggers with explicit mapping config — not when you rename manually after restore
- Uses backup name as suffix — restoring same backup twice to different names still collides
- Complex path rewriting logic with 5 metadata format versions

**Our approach: ALWAYS isolate S3 objects during restore.**

Every restore creates objects under a unique namespace derived from the **destination table's UUID** (assigned by ClickHouse at CREATE TABLE time):

```
Restore flow for S3 disk parts:

1. CREATE TABLE db.tableB → ClickHouse assigns UUID (e.g., 5f3a7b...)
2. Generate restore prefix: store/{uuid_hash1}/{uuid_hash2}/
   (matches ClickHouse's own path convention for this UUID)

3. For each S3 object in backup:
   a. Source: s3://backup-bucket/chbackup/.../store/abc/def/part_1/data.bin
   b. Destination: s3://data-bucket/store/5f3/a7b/part_1/data.bin
                                        ^^^^^^^^^^^^^^^^
                                        derived from tableB's UUID
   c. CopyObject (server-side, no data transfer through our tool)
   d. Retry with exponential backoff on CopyObject failure
   e. If CopyObject fails (cross-region etc.), fallback to streaming copy
      via local memory (download from src, upload to dst)

4. Rewrite local metadata files to point to new paths:
   Before: "store/abc/def/part_1/data.bin"
   After:  "store/5f3/a7b/part_1/data.bin"

5. Set RefCount=0, ReadOnly=false in metadata
   (allows ClickHouse to manage lifecycle of these objects)

6. Preserve InlineData for version 4+ metadata (no path rewriting needed)

7. Write metadata files to tableB's detached/ directory
8. Chown metadata files to ClickHouse user
9. ATTACH PART
```

This means:
- **Every restored table gets independent S3 objects** regardless of naming
- Dropping the original table has zero impact on the restored copy
- Restoring the same backup 10 times to 10 different tables creates 10 independent copies
- No collision regardless of how tables are renamed post-restore
- Cost: one S3 CopyObject per object (server-side, fast, no egress charges on same region)

**Same-name restore optimization**: When restoring to the same table name AND same UUID (disaster recovery on same cluster), check if S3 objects already exist at their original paths. Use `ListObjectsV2` with the table's store prefix to get all existing keys + sizes in one API call (not per-object HeadObject), then skip CopyObject for objects that already exist with matching size.

```
For same-name/same-UUID restore:
  existing = ListObjectsV2(prefix="store/{uuid_hash1}/{uuid_hash2}/")
  existing_map = { key: size for each object }
  For each object in backup:
    If original_path in existing_map AND sizes match → zero-copy (skip)
    If missing → CopyObject from backup bucket
```

### 5.5 Phase 3: DDL-Only Objects

Dictionaries, Views, MaterializedViews are DDL-only — no parts, no FREEZE. The Go tool:
- Never populates `DependenciesTable`/`DependenciesDatabase` fields (defined but always empty)
- Uses brute-force retry loop for dependency ordering during restore
- Routes DDL objects through the data pipeline (no-op ATTACH PART on empty parts list)

**Our approach: Dependency-aware topological sort.**

```sql
-- Query dependency info (ClickHouse 23.3+)
SELECT database, name, engine, create_table_query,
       dependencies_database, dependencies_table
FROM system.tables
WHERE database NOT IN ('system', 'INFORMATION_SCHEMA', 'information_schema')
```

Build dependency graph at backup time. Store in manifest. Topological sort at restore time:

```
Example:
  Dictionary `default.user_dict` → SOURCE from `default.users` table
  MaterializedView `default.hourly_agg` → reads from `default.events`, writes to `default.events_hourly`

Correct restore order:
  1. default.users          (MergeTree, Phase 2)
  2. default.events         (MergeTree, Phase 2)
  3. default.events_hourly  (MergeTree, Phase 2 — MV's target table)
  4. default.user_dict      (Dictionary, Phase 3 — source table exists)
  5. default.hourly_agg     (MaterializedView, Phase 3 — source + target exist)
```

MaterializedView target tables (where data is stored) are regular MergeTree tables → Phase 2 with proper part handling. The MV definition itself is just DDL.

After creating a Dictionary, ClickHouse auto-loads data from its source. No parts to restore.

**Fallback**: If dependency info is unavailable (ClickHouse < 23.3), fall back to engine-priority sorting + retry loop (matching Go tool behavior).

### 5.6 Phase 4: Functions, Named Collections, RBAC

Pure DDL, typically no table dependencies. Restore via SQL:
- `CREATE FUNCTION ...`
- `CREATE NAMED COLLECTION ...`  (supports local and keeper storage types)
- RBAC: restore `.jsonl` files from `access/` directory to ClickHouse's `access_data_path`
  - Create `need_rebuild_lists.mark` file to trigger RBAC rebuild on restart
  - Remove stale `*.list` files
  - Handle replicated user directories via ZooKeeper if configured
  - Chown all access files to ClickHouse user
  - Execute `restart_command` (default: `exec:systemctl restart clickhouse-server`)
    to apply RBAC changes. Multiple commands separated by `;`. Prefixes:
    `exec:` runs a shell command, `sql:` executes a ClickHouse query.
    All errors are logged and ignored (best-effort restart).
- Config files: copy restored configs to `config_dir`, then execute `restart_command`
- `rbac_resolve_conflicts`: when a user/role already exists:
  - `"recreate"` (default): DROP + CREATE
  - `"ignore"`: skip, log warning
  - `"fail"`: error, abort restore

### 5.7 Pending Mutation Re-application

Only if backup was taken with `--skip-mutation-wait`:

```
After all parts attached for a table:
  If manifest has pending_mutations for this table:
    WARN: "table default.trades backed up with 2 pending data mutations"
    WARN: "  mutation_id=0000000002: DELETE WHERE user_id = 5 (3 parts pending)"
    WARN: "  Re-applying mutations... this may take time."
    ALTER TABLE default.trades DELETE WHERE user_id = 5
    -- wait for completion with mutations_sync=2
```

---

## 6. Table Rename / Remap

### 6.1 Table-Level Remap (--as)

When restoring with `-t db.table --as=dst_db.dst_table`:

```
1. Rewrite DDL:
   - Table name: CREATE TABLE `dst_db`.`dst_table` ...
   - UUID: let ClickHouse assign new one (omit from CREATE)
   - ZK path for Replicated tables: generate new path
     /clickhouse/tables/{shard}/dst_db/dst_table
   - Engine params: update replica name if needed
   - Distributed tables: update underlying table reference if also remapped

2. S3 object isolation (5.4) handles data independence automatically
   (objects go under new UUID path, no shared references)

3. Manifest mapping:
   src_db.src_table → dst_db.dst_table
   All part references resolved through manifest, not by table name
```

### 6.2 Database-Level Remap (-m, --database-mapping)

Bulk database remapping: `-m originDB:targetDB` or multiple: `-m prod:staging,logs:logs_copy`

```
1. For each table in backup where database matches originDB:
   - Rewrite DDL: CREATE TABLE `targetDB`.`table_name` ...
   - Create targetDB if not exists (using originDB's engine)
   - ZK paths: replace originDB with targetDB in replica path
   - Dependencies: update cross-table references within the same mapping
2. Tables in databases NOT listed in the mapping are restored to their original database
3. Can combine with -t filter: `-m prod:staging -t prod.users` restores only prod.users → staging.users
```

This is essential for cross-environment restores (prod → staging, DR scenarios).

---

## 7. Backup Directory Layout

**Local backup** (`/var/lib/clickhouse/backup/{backup_name}/`):
```
daily-20250215/
├── metadata.json              # Backup manifest (§7.1)
├── metadata/
│   ├── default/
│   │   ├── trades.json        # Per-table metadata: DDL, parts list, checksums
│   │   └── orders.json
│   └── logs/
│       └── events.json
├── shadow/
│   ├── default/
│   │   ├── trades/
│   │   │   ├── 202401_1_50_3/    # Data part directories (hardlinks to CH data)
│   │   │   │   ├── checksums.txt
│   │   │   │   ├── columns.txt
│   │   │   │   ├── primary.cidx
│   │   │   │   └── *.bin / *.mrk3
│   │   │   └── 202402_51_100_2/
│   │   └── orders/
│   │       └── ...
│   └── logs/
│       └── events/
│           └── ...
├── access/                    # RBAC objects (if --rbac)
│   ├── users.jsonl
│   ├── roles.jsonl
│   └── ...
└── configs/                   # CH configs (if --configs)
    └── ...
```

**Remote layout** (S3 key structure):
```
s3://{bucket}/{prefix}/{backup_name}/
├── metadata.json
├── data/
│   ├── {url_encoded_db}/{url_encoded_table}/
│   │   ├── {part_name}.tar.lz4     # Compressed part archive
│   │   └── ...
│   └── ...
├── access/
│   └── ...
└── configs/
    └── ...
```

The `{prefix}` supports `{macro}` expansion from `system.macros` (e.g., `chbackup/shard-{shard}` → `chbackup/shard-1`). This ensures each shard writes to a unique prefix, preventing cross-shard deletion during retention.

**Critical**: Never change file permissions in `/var/lib/clickhouse/backup/`. This path contains hardlinks to live ClickHouse data parts. Changing permissions on backup hardlinks changes permissions on the original files too, which can corrupt the running server.

## 7.1 Manifest Format

Self-contained JSON. Every backup is independently restorable.

```json
{
  "manifest_version": 1,
  "name": "daily-2024-01-15",
  "timestamp": "2024-01-15T02:00:00Z",
  "clickhouse_version": "24.1.3.31",
  "chbackup_version": "0.1.0",
  "data_format": "lz4",
  "compressed_size": 1073741824,
  "metadata_size": 524288,
  "disks": { "default": "/var/lib/clickhouse", "s3disk": "/var/lib/clickhouse/disks/s3" },
  "disk_types": { "s3disk": "s3", "default": "local" },
  "tables": {
    "default.trades": {
      "ddl": "CREATE TABLE default.trades (...) ENGINE = ReplicatedMergeTree(...)",
      "uuid": "5f3a7b2c-...",
      "engine": "ReplicatedMergeTree",
      "total_bytes": 5368709120,
      "parts": {
        "s3disk": [
          {
            "name": "202401_1_50_3",
            "size": 134217728,
            "backup_key": "chbackup/daily-2024-01-15/default/trades/s3disk/202401_1_50_3.tar.lz4",
            "source": "uploaded",
            "checksum_crc64": 12345678901234,
            "s3_objects": [
              {
                "path": "store/abc/def/202401_1_50_3/data.bin",
                "size": 134217000,
                "backup_key": "chbackup/daily-2024-01-15/objects/store/abc/def/202401_1_50_3/data.bin"
              }
            ]
          },
          {
            "name": "202401_51_100_2",
            "size": 67108864,
            "backup_key": "chbackup/daily-2024-01-14/default/trades/s3disk/202401_51_100_2.tar.lz4",
            "source": "carried:daily-2024-01-14",
            "checksum_crc64": 98765432109876,
            "s3_objects": [
              {
                "path": "store/abc/def/202401_51_100_2/data.bin",
                "size": 67100000,
                "backup_key": "chbackup/daily-2024-01-14/objects/store/abc/def/202401_51_100_2/data.bin"
              }
            ]
          }
        ],
        "default": [
          {
            "name": "202402_1_1_0",
            "size": 4096,
            "backup_key": "chbackup/daily-2024-01-15/default/trades/default/202402_1_1_0.tar.lz4",
            "source": "uploaded",
            "checksum_crc64": 11111111111111
          }
        ]
      },
      "pending_mutations": [],
      "metadata_only": false,
      "dependencies": []
    },
    "default.user_dict": {
      "ddl": "CREATE DICTIONARY default.user_dict (...) SOURCE(CLICKHOUSE(TABLE 'users' DB 'default')) ...",
      "engine": "Dictionary",
      "metadata_only": true,
      "dependencies": ["default.users"]
    }
  },
  "databases": [
    { "name": "default", "ddl": "CREATE DATABASE default ENGINE = Atomic" }
  ],
  "functions": [],
  "named_collections": [],
  "rbac": { "path": "chbackup/daily-2024-01-15/access/" }
}
```

---

## 8. Retention / GC

### 8.1 Deleting Local Backups
Delete backup directory. Done.

### 8.2 Deleting Remote Backups — Safe GC

S3 objects in the backup bucket may be shared across incremental backups (via `source: "carried:..."`). Deleting a backup must not remove objects referenced by surviving backups.

```
1. Load ALL surviving manifests from S3 (parallel, unbounded — manifests are small JSON)
   Optimization: cache manifest key-sets in memory when running in watch/server mode.
   On each cycle, only fetch manifests created since last cache refresh.
2. Build set of all referenced backup_keys (union across all manifests)
3. For the backup being deleted:
   a. Collect all S3 keys owned by this backup
   b. Filter: keys NOT in referenced set → candidates for deletion
   c. Re-check: reload manifests created AFTER step 1 (race protection —
      a concurrent create_remote might have started referencing our keys)
   d. Batch delete confirmed-safe keys (S3 DeleteObjects API, up to 1000 per call)
4. Delete the manifest JSON last
```

**Race condition window**: Between re-check (step 3c) and delete (step 3d), a new backup could still reference our keys. This window is milliseconds. Mitigation: retention runs under the global PID lock, so concurrent retention + create_remote is serialized. The only remaining race is between retention on one host and create_remote on a different host — an edge case that doesn't apply to single-sidecar deployments. Multi-host users should run retention from only one host.

**Incremental chain protection**: In addition to key-level GC (which prevents deleting shared S3 objects), `retention_remote()` provides backup-level protection for incremental bases. For each surviving manifest, `collect_incremental_bases()` scans all `PartInfo.source` fields for `"carried:{base_name}"` prefixes and collects the referenced base backup names. A backup whose name appears in this set is protected from deletion regardless of age or count. This ensures that deleting an old backup never breaks the incremental chain — even though our manifests are self-contained (all part keys listed), the physical S3 objects still reside under the base backup's prefix. Protecting the base backup avoids the need to relocate objects during retention.

### 8.3 Auto-Retention

```yaml
retention:
  backups_to_keep_local: 0   # 0 = unlimited; -1 = delete local after upload
  backups_to_keep_remote: 7  # After successful upload, delete oldest exceeding count
```

When `backups_to_keep_local: -1`, the local backup is deleted immediately after successful upload (same as `--delete-local` flag, but automatic).

### 8.4 Broken Backup Cleanup

A backup is "broken" if its `metadata.json` is missing or unparseable — typically from a crash mid-creation or a partial upload.

```
chbackup clean_broken local    → scan local backup dirs, delete any with missing/corrupt metadata
chbackup clean_broken remote   → scan remote backup prefixes, delete any with missing/corrupt manifest
```

The `list` command shows broken backups with a `[BROKEN]` marker and the reason (e.g., "metadata.json not found", "parse error"). Broken backups are excluded from retention counting and diff-from chain resolution.

**List remote optimization**: Listing remote backups requires fetching metadata.json for every backup in S3 (can be slow with hundreds of backups). Optimization: cache remote backup metadata locally in `$TMPDIR/.chbackup.remote_cache.json` with TTL (default 5 minutes). The cache is invalidated after any mutating remote operation (upload, delete). This matches the Go tool's approach (`$TEMP/.clickhouse-backup.$REMOTE_STORAGE`).

---

## 9. API Server

HTTP API for Kubernetes sidecar and remote management:

```
# Backup operations
POST /api/v1/create          { "tables": "db.*", "diff_from": "daily-mon", "schema": false }
POST /api/v1/create_remote   { "tables": "db.*", "diff_from_remote": "daily-mon" }
POST /api/v1/upload/{name}   { "delete_local": true, "diff_from_remote": "daily-sun" }
POST /api/v1/download/{name} { "hardlink_exists_files": true }
POST /api/v1/restore/{name}  { "tables": "db.table", "database_mapping": "prod:staging", "rm": false }
POST /api/v1/restore_remote/{name}  { ... }
DELETE /api/v1/delete/{where}/{name}

# Listing & info
GET  /api/v1/list?location=remote
GET  /api/v1/tables?table=db.*
GET  /api/v1/status
GET  /api/v1/actions              (action log with timestamps and durations)
GET  /api/v1/version

# Maintenance
POST /api/v1/clean
POST /api/v1/clean/remote_broken
POST /api/v1/clean/local_broken
POST /api/v1/kill                 (cancel running operation)
POST /api/v1/reload               (re-read config file, equivalent to SIGHUP)
POST /api/v1/restart              (reload config from disk, reconnect ClickHouse/S3 clients, ping-gated atomic swap; does not rebind the TCP socket)

# Watch mode
POST /api/v1/watch/start  { "watch_interval": "1h", "full_interval": "24h" }
POST /api/v1/watch/stop
GET  /api/v1/watch/status

# Monitoring
GET  /health
GET  /metrics              (Prometheus)
```

Metrics: backup_duration_seconds, backup_size_bytes, backup_last_success_timestamp, restore_duration_seconds, parts_uploaded_total, parts_skipped_incremental_total, s3_copy_object_total, errors_total, successful_backups_total, failed_backups_total, number_backups_remote, number_backups_local, in_progress_commands, watch_state (idle/full/incremental/sleeping/error), watch_last_full_timestamp, watch_last_incremental_timestamp, watch_consecutive_errors.

**API Authentication**: When `api.username` and `api.password` are configured, all endpoints require HTTP Basic Auth. Without auth configured, the API is open (suitable for localhost-only binding in trusted K8s pods).

**API TLS**: When `api.secure: true`, the server listens on HTTPS using the configured certificate and key files. Required for non-localhost deployments where API traffic crosses a network boundary.

**Auto-resume after restart** (`api.complete_resumable_after_restart: true`): On server startup, scan for `{backup_name}/(upload|download).state.json` files. If found, queue the interrupted operation for automatic resumption in the background. This handles pod rescheduling in K8s — the watch loop doesn't need to re-create the backup, just finish uploading it.

**Parallel operations** (`api.allow_parallel: false`): When false (default), API operations are serialized — a second request while one is running returns 423 Locked. When true, concurrent operations on different backup names are allowed (same backup name still serialized via PID lock). Enable only with sufficient memory, as each operation spawns concurrent upload/download tasks.

### 9.1 ClickHouse Integration Tables

When `create_integration_tables: true` (default in server mode), chbackup registers two virtual tables inside the local ClickHouse instance using the `URL` table engine. This lets operators query backup status and trigger operations directly from `clickhouse-client` — no curl or separate tooling needed.

```yaml
api:
  create_integration_tables: true   # default: true when running in server mode
  integration_tables_host: ""       # DNS name for URL engine (default: localhost). Use K8s service name
                                    # e.g., "chi-backup-0-0.svc.cluster.local" when CH and chbackup are
                                    # in separate containers that can't reach each other via localhost.
```

On server startup, chbackup executes:

```sql
CREATE TABLE IF NOT EXISTS system.backup_list (
    name String,
    created DateTime,
    location String,
    size UInt64,
    data_size UInt64,
    object_disk_size UInt64,
    metadata_size UInt64,
    rbac_size UInt64,
    config_size UInt64,
    compressed_size UInt64,
    required String
) ENGINE = URL('http://localhost:7171/api/v1/list', 'JSONEachRow');

CREATE TABLE IF NOT EXISTS system.backup_actions (
    command String,
    start DateTime,
    finish DateTime,
    status String,
    error String
) ENGINE = URL('http://localhost:7171/api/v1/actions', 'JSONEachRow');
```

**Query backup inventory from clickhouse-client:**
```sql
SELECT name, created, location,
       formatReadableSize(size) AS size,
       formatReadableSize(compressed_size) AS compressed,
       required
FROM system.backup_list
ORDER BY created;
```

**Trigger backup operations via INSERT:**
```sql
-- Create and upload a backup
INSERT INTO system.backup_actions(command)
VALUES ('create_remote daily_2025-02-15');

-- Monitor progress
SELECT * FROM system.backup_actions WHERE status = 'in progress';

-- Delete old backup
INSERT INTO system.backup_actions(command)
VALUES ('delete remote daily_2025-02-01');
```

**Why this matters**: In production, operators live in `clickhouse-client`. Being able to `SELECT * FROM system.backup_list` alongside `SELECT * FROM system.parts` keeps backup monitoring in the same workflow as everything else. The Go tool provides this via `create_integration_tables: true` — we match the same column schema for drop-in compatibility.

On server shutdown, chbackup drops the tables to avoid stale URL engine tables pointing at a dead endpoint:
```sql
DROP TABLE IF EXISTS system.backup_list;
DROP TABLE IF EXISTS system.backup_actions;
```

---

## 10. Watch Mode (Scheduler)

Built-in scheduling loop that maintains a full + incremental backup chain automatically. This is the primary production deployment mode — the tool runs continuously and manages its own backup lifecycle.

### 10.1 Core Concept

Two-tier scheduling: periodic **full** backups establish a baseline, frequent **incremental** backups fill the gaps using `--diff-from` against the previous backup.

```
Timeline:
|--- full_interval (24h) ---|--- full_interval (24h) ---|
|                            |                            |
F  i  i  i  i  i  i  i  F  i  i  i  i  i  i  i  F
|--watch_interval (1h)--|

F = full backup (self-contained)
i = incremental (diff-from previous)
```

Each cycle: `create → upload → delete local → retention cleanup`.

### 10.2 Usage

```bash
# CLI mode — standalone watch loop
chbackup watch --watch-interval=1h --full-interval=24h

# CLI with table filter
chbackup watch --watch-interval=1h --full-interval=24h -t "default.*"

# Server mode — API + watch loop together (primary K8s deployment)
chbackup server --watch --watch-interval=1h --full-interval=24h

# All params can come from config or env vars
WATCH_INTERVAL=1h FULL_INTERVAL=24h chbackup watch
```

### 10.3 Configuration

```yaml
watch:
  enabled: false                    # Enable watch mode in server
  watch_interval: "1h"             # Interval between incremental backups
  full_interval: "24h"             # Interval between full backups
  name_template: "shard{shard}-{type}-{time:%Y%m%d_%H%M%S}"
  tables: "*.*"                    # Table filter pattern
  max_consecutive_errors: 5        # Abort after N consecutive failures
  retry_interval: "5m"            # Wait before retrying after error
  delete_local_after_upload: true  # Clean up local backup after successful upload
```

The name template supports:
- `{type}` — replaced with `full` or `incr`
- `{time:FORMAT}` — replaced with current time using strftime-style format
- `{shard}` and other ClickHouse macros — resolved via `system.macros`

Examples:
```
shard{shard}-{type}-{time:%Y%m%d_%H%M%S}   →  shard1-full-20250215_140000
daily-{type}-{time:%Y%m%d_%H%M%S}          →  daily-incr-20250215_150000
```

### 10.4 State Machine

The watch loop operates as an explicit state machine, avoiding the Go tool's ad-hoc variable tracking:

```
                    ┌──────────────────────────────────────────┐
                    │                                          │
                    ▼                                          │
              ┌──────────┐                                     │
  start ──►   │  Resume   │  scan remote backups               │
              └────┬─────┘                                     │
                   │                                           │
           ┌──────▼──────┐  time since last full               │
           │  Decide     │  > full_interval?                   │
           │  full/incr  │───────────────────┐                 │
           └──────┬──────┘                   │                 │
                  │ no                       │ yes              │
                  ▼                          ▼                  │
         ┌──────────────┐          ┌──────────────┐            │
         │  Create      │          │  Create      │            │
         │  Incremental │          │  Full        │            │
         │  (diff-from) │          │              │            │
         └──────┬───────┘          └──────┬───────┘            │
                │                         │                    │
                └────────────┬────────────┘                    │
                             │                                 │
                             ▼                                 │
                      ┌─────────────┐                          │
                      │  Upload     │                          │
                      │  to S3      │                          │
                      └──────┬──────┘                          │
                             │                                 │
                      ┌──────▼──────┐                          │
                      │ Delete local│                          │
                      │ + Retention │                          │
                      └──────┬──────┘                          │
                             │                                 │
                      ┌──────▼──────┐     watch_interval       │
                      │   Sleep     │─────────────────────────►│
                      └──────┬──────┘                          │
                             │ error                           │
                      ┌──────▼──────┐  consecutive             │
                      │   Error     │  < max_errors? ──────────┘
                      │   Backoff   │         retry_interval
                      └──────┬──────┘
                             │ >= max_errors
                             ▼
                        ┌─────────┐
                        │  Abort  │
                        └─────────┘
```

### 10.5 Resume on Restart

On startup, the watch loop scans existing remote backups to determine where to pick up:

```
1. List remote backups matching name_template pattern
2. Find the most recent full and most recent incremental
3. Calculate time elapsed since each
4. If elapsed > full_interval   → next backup is full
   If elapsed > watch_interval  → next backup is incremental (diff-from last)
   If elapsed < watch_interval  → sleep for remaining time, then incremental
```

This means the tool can be restarted (crash, upgrade, pod reschedule) without producing duplicate backups or missing windows. The Go tool does this too, but our implementation is cleaner — a single `resume_state()` function rather than scattered across `calculatePrevBackupNameAndType()`.

### 10.6 Error Handling

```
On create/upload failure:
  1. Log error with full context (table, part, S3 key)
  2. Increment consecutive_error_count
  3. If consecutive_error_count >= max_consecutive_errors → abort watch, exit non-zero
  4. Otherwise → wait retry_interval, then retry (next backup is full, to avoid
     building incremental on potentially broken base)

On success:
  consecutive_error_count = 0

On retention failure:
  Log warning, continue — retention is best-effort, doesn't affect backup chain
```

Key difference from Go tool: after an error, the next attempt is always a **full** backup (not incremental). This avoids building an incremental chain on a potentially broken base. The Go tool tries to continue with incrementals after errors, which can compound the problem.

### 10.7 Retention Integration

After each successful upload, the watch loop runs retention cleanup:

```
1. List all remote backups matching name_template
2. Apply backups_to_keep_remote policy (e.g. keep last 48)
3. Delete eligible backups using safe GC from Section 8.2:
   - Only delete S3 keys not referenced by any surviving manifest
   - Carried-forward keys shared with kept backups are preserved automatically
4. Update metrics: number_backups_remote, last_backup_size_remote
```

### 10.8 Config Hot-Reload

The watch loop re-reads config on SIGHUP:

```
SIGHUP received:
  1. Set reload_pending flag (non-blocking)
  2. Current backup/upload cycle completes normally (never interrupt mid-operation)
  3. At next sleep cycle entry point:
     a. Re-parse config file (same path as startup)
     b. Validate new watch params (full_interval > watch_interval, etc.)
     c. Apply new intervals
     d. Log: "Config reloaded: watch_interval=1h→30m, full_interval=24h→12h"
```

This allows tuning backup frequency without restarting the tool. In K8s, you update the ConfigMap and send SIGHUP via the API: `POST /api/v1/reload`.

### 10.9 Watch Mode in Docker

The primary Docker deployment runs the API server with watch enabled:

```yaml
# K8s sidecar with watch mode
containers:
  - name: chbackup
    image: ghcr.io/user/chbackup:latest
    args: ["server", "--watch"]
    env:
      - name: WATCH_INTERVAL
        value: "1h"
      - name: FULL_INTERVAL
        value: "24h"
      - name: S3_BUCKET
        value: "my-backups"
      # ... S3 credentials from secrets
    volumeMounts:
      - name: data
        mountPath: /var/lib/clickhouse
    ports:
      - containerPort: 7171    # API for manual operations + Prometheus scraping
```

This gives you: automatic scheduled backups (watch) + on-demand operations via API (manual restore, list, etc.) + Prometheus metrics — all from a single container.

---

## 11. Async Architecture

All I/O-bound operations are async and non-blocking, built on tokio. The concurrency model uses **flat semaphores** — all work items across all tables share a single concurrency limit per operation type, giving natural load balancing without nested parallelism complexity. See Sections 3.4, 3.6, 4, and 5.3 for detailed pipeline descriptions.

### 11.1 Concurrency Model (Summary)

```
┌─────────────────────────────────────────────────────────────────────┐
│ Operation          │ Semaphore                   │ What's parallel  │
├────────────────────┼─────────────────────────────┼──────────────────┤
│ pre-flight (SYNC)  │ max_connections              │ Tables           │
│ create (FREEZE)    │ max_connections              │ Tables           │
│ upload (S3 PUT)    │ upload_concurrency            │ Parts across all │
│                    │                               │  tables (flat)   │
│ upload (S3 Copy)   │ object_disk_copy_concurrency  │ Object disk      │
│ download (S3 GET)  │ download_concurrency          │ Parts across all │
│                    │                               │  tables (flat)   │
│ restore (ATTACH)   │ max_connections              │ Tables           │
│ restore (S3 Copy)  │ object_disk_copy_concurrency  │ Objects within   │
│                    │                               │  each table      │
│ delete (S3 DELETE) │ upload_concurrency            │ Batch deletes    │
│ retention (manifest│ unbounded (tiny JSON)         │ Manifest loads   │
│  loading)          │                               │                  │
└─────────────────────────────────────────────────────────────────────┘
```

### 11.2 What MUST Be Sequential

| Operation | Why Sequential |
|-----------|---------------|
| ATTACH PART within Replacing/Collapsing tables | min_block ordering for correctness (CH #71009) |
| Schema CREATE (Phase 2→3→4) | Cross-phase dependencies |
| FREEZE → shadow walk → UNFREEZE per table | Atomic per-table operation |
| Manifest upload | Must be last (atomic commit marker) |

### 11.3 Rust Implementation Notes

```rust
// Shared semaphore for upload concurrency
let upload_sem = Arc::new(Semaphore::new(config.upload_concurrency));

// Flat work queue: collect all parts across all tables
let mut tasks = Vec::new();
for table in &tables_for_upload {
    for (disk, parts) in &table.parts {
        for part in parts {
            let sem = upload_sem.clone();
            let task = tokio::spawn(async move {
                let _permit = sem.acquire().await?;
                upload_part(&s3_client, &part).await
            });
            tasks.push(task);
        }
    }
}

// Fail-fast: cancel remaining on first error
let results = futures::future::try_join_all(tasks).await?;
```

**Key crate choices**:
- `aws-sdk-s3` — native async S3 client on tokio
- `clickhouse` (v0.13) — async ClickHouse driver over HTTP protocol with connection pooling
- `tokio::fs` — async file I/O; `spawn_blocking` + `walkdir` for directory walks
- `tokio_util::codec` — wrap lz4/zstd as async stream transforms for streaming compress/decompress (no temp files, constant memory regardless of part size)

### 11.4 Logging & Progress Reporting

Clear operational logs are critical — backup runs can take hours on multi-TB databases. Operators need to see what's happening at all times, not just a final "done" or a cryptic error. Two output modes, selected automatically:

**CLI mode** (default when running interactively): Human-readable with progress bars and live counters. Uses `indicatif` crate for progress bars, `tracing-subscriber` with a human-friendly format for log lines.

**Server/JSON mode** (when running as `server` or `--log-format=json`): Structured JSON lines for log aggregation (Loki, ELK, CloudWatch). No progress bars. Each log line has: timestamp, level, operation, backup_name, table (if applicable), and structured fields.

#### Log output by operation phase

**Create (backup):**
```
2025-02-15 02:00:01 INFO  [create] Starting backup "daily-20250215" — 47 tables matching "*.*"
2025-02-15 02:00:01 INFO  [create] Pre-flight: checking mutations on 47 tables
2025-02-15 02:00:01 WARN  [create] Table default.events has 2 pending mutations, waiting up to 5m
2025-02-15 02:00:15 INFO  [create] Pre-flight: mutations clear
2025-02-15 02:00:15 INFO  [create] Syncing 12 replicated tables (max_connections=4)
2025-02-15 02:00:18 INFO  [create] Freezing 47 tables (max_connections=4)
2025-02-15 02:00:18 INFO  [create]   FREEZE default.trades (340 parts, 12.4 GB)
2025-02-15 02:00:18 INFO  [create]   FREEZE default.orders (89 parts, 3.1 GB)
2025-02-15 02:00:19 WARN  [create]   SKIP logs.tmp_table — dropped during backup (code 60)
2025-02-15 02:00:22 INFO  [create]   UNFREEZE default.trades ✓
2025-02-15 02:00:22 INFO  [create]   UNFREEZE default.orders ✓
...
2025-02-15 02:00:45 INFO  [create] Frozen 46/47 tables (1 skipped), 1842 parts, 89.3 GB
2025-02-15 02:00:45 INFO  [create] Computing CRC64 checksums for 1842 parts
2025-02-15 02:00:52 INFO  [create] Backup "daily-20250215" created — 46 tables, 1842 parts, 89.3 GB, 7.2s
```

**Upload:**
```
2025-02-15 02:01:00 INFO  [upload] Uploading "daily-20250215" to s3://my-bucket/chbackup/
2025-02-15 02:01:00 INFO  [upload] 1842 parts to upload (89.3 GB uncompressed), concurrency=4
2025-02-15 02:01:00 INFO  [upload]   default.trades: 340 parts (12.4 GB) — uploading
2025-02-15 02:01:05 INFO  [upload]   default.trades: part 202401_1_150_4 (256 MB) → multipart, 5 chunks
2025-02-15 02:01:15 INFO  [upload]   default.trades: 34/340 parts (1.2 GB / 12.4 GB) [10%]
...
2025-02-15 02:05:30 INFO  [upload]   default.trades: 340/340 parts ✓ (12.4 GB, compressed 4.1 GB, ratio 3.0x)
2025-02-15 02:12:00 INFO  [upload] Progress: 1200/1842 parts (67.4 GB / 89.3 GB) [65%] — 42 MB/s
...
2025-02-15 02:18:45 INFO  [upload] Uploading manifest (metadata.json.tmp → metadata.json)
2025-02-15 02:18:45 INFO  [upload] Upload complete — 1842 parts, 89.3 GB → 31.2 GB compressed, 17m45s, avg 34 MB/s
```

**Upload with --diff-from (incremental):**
```
2025-02-15 02:01:00 INFO  [upload] Uploading "daily-20250215" (incremental from "daily-20250214")
2025-02-15 02:01:01 INFO  [upload] Diff: 1842 parts total, 1798 unchanged (CRC64 match), 44 new/modified
2025-02-15 02:01:01 INFO  [upload] Uploading 44 parts (2.1 GB), skipping 1798 parts (87.2 GB)
...
2025-02-15 02:02:30 INFO  [upload] Upload complete — 44 parts uploaded (2.1 GB → 0.7 GB), 1798 carried, 1m30s
```

**Download:**
```
2025-02-15 03:00:00 INFO  [download] Downloading "daily-20250215" from s3://my-bucket/chbackup/
2025-02-15 03:00:01 INFO  [download] Manifest: 46 tables, 1842 parts, 31.2 GB compressed
2025-02-15 03:00:01 INFO  [download] Disk space check: need 89.3 GB, available 450 GB ✓
2025-02-15 03:00:01 INFO  [download] Downloading metadata for 46 tables
2025-02-15 03:00:02 INFO  [download] Downloading 1842 parts (31.2 GB compressed), concurrency=4
...
2025-02-15 03:08:00 INFO  [download] Progress: 1200/1842 parts [65%] — 55 MB/s
...
2025-02-15 03:15:00 INFO  [download] Verifying checksums (CRC64)
2025-02-15 03:15:05 INFO  [download] Download complete — 1842 parts, 31.2 GB → 89.3 GB decompressed, 15m, avg 38 MB/s
```

**Restore:**
```
2025-02-15 04:00:00 INFO  [restore] Restoring "daily-20250215" — 46 tables, Mode B (non-destructive)
2025-02-15 04:00:00 INFO  [restore] Phase 1: Creating 3 databases
2025-02-15 04:00:00 INFO  [restore]   CREATE DATABASE IF NOT EXISTS default (Atomic)
2025-02-15 04:00:00 INFO  [restore]   CREATE DATABASE IF NOT EXISTS logs (Atomic)
2025-02-15 04:00:01 INFO  [restore] Phase 2: Restoring 42 data tables (max_connections=4)
2025-02-15 04:00:01 INFO  [restore]   default.trades: CREATE TABLE ✓
2025-02-15 04:00:01 INFO  [restore]   default.trades: attaching 340 parts (sequential — ReplacingMergeTree)
2025-02-15 04:00:05 INFO  [restore]   default.trades: 100/340 parts attached [29%]
...
2025-02-15 04:00:30 INFO  [restore]   default.trades: 340/340 parts attached ✓ (12.4 GB, 29s)
2025-02-15 04:00:30 INFO  [restore]   default.orders: attaching 89 parts (parallel — MergeTree)
...
2025-02-15 04:02:00 INFO  [restore] Phase 2 complete: 42 tables, 1800 parts attached
2025-02-15 04:02:00 INFO  [restore] Phase 3: Creating 4 DDL-only objects (topo-sorted)
2025-02-15 04:02:00 INFO  [restore]   CREATE DICTIONARY default.geo_lookup ✓
2025-02-15 04:02:00 INFO  [restore]   CREATE MATERIALIZED VIEW default.events_hourly ✓
...
2025-02-15 04:02:01 INFO  [restore] Restore complete — 46 tables, 1842 parts, 2m1s
```

**Resumable operations:**
```
2025-02-15 02:05:00 INFO  [upload] Resuming upload "daily-20250215" from state file
2025-02-15 02:05:00 INFO  [upload] State: 800/1842 parts already uploaded, resuming remaining 1042
```

**Watch mode:**
```
2025-02-15 02:00:00 INFO  [watch] Watch started — full_interval=24h, watch_interval=1h
2025-02-15 02:00:00 INFO  [watch] Last full: 2025-02-14T02:00:00Z (24h ago), last incr: 2025-02-15T01:00:00Z (1h ago)
2025-02-15 02:00:00 INFO  [watch] Decision: FULL backup (full_interval expired)
2025-02-15 02:00:00 INFO  [watch] Creating "shard1-full-20250215_020000"
...
2025-02-15 02:20:00 INFO  [watch] Backup + upload complete. Sleeping 1h until next cycle.
2025-02-15 03:20:00 INFO  [watch] Decision: INCREMENTAL backup (diff from shard1-full-20250215_020000)
...
2025-02-15 03:22:00 INFO  [watch] Error during upload: S3 timeout. Consecutive errors: 1/5
2025-02-15 03:22:00 INFO  [watch] Retrying in 5m. Next backup will be FULL (error invalidates chain).
```

**Error logging — always includes actionable context:**
```
2025-02-15 02:05:00 ERROR [upload] Failed to upload part default.trades/202401_1_150_4: 
    S3 PutObject error: RequestTimeout after 3 retries
    Part: 256 MB, key: chbackup/daily-20250215/data/default/trades/202401_1_150_4.tar.lz4
    Action: retry with --resume, or check S3 connectivity
2025-02-15 02:05:00 ERROR [upload] Aborting multipart upload ID abc123 for part 202401_1_150_4
```

#### Progress bar (CLI mode only)

For long-running operations, show a live progress bar below the log output:

```
Uploading daily-20250215 ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━ 65% 1200/1842 parts  42 MB/s  ETA 5m
```

Controlled by `disable_progress_bar: false` in config. Automatically disabled when stdout is not a terminal (piped to file, running in Docker without `-t`). Never shown in server/JSON mode.

#### SQL query logging

When `log_sql_queries: true` (default), every SQL query sent to ClickHouse is logged at info level with execution time:

```
2025-02-15 02:00:15 INFO  [sql] ALTER TABLE `default`.`trades` FREEZE WITH NAME 'chbackup_daily_default_trades' (23ms)
2025-02-15 02:00:22 INFO  [sql] ALTER TABLE `default`.`trades` UNFREEZE WITH NAME 'chbackup_daily_default_trades' (5ms)
2025-02-15 04:00:01 INFO  [sql] CREATE TABLE `default`.`trades` (...) ENGINE = ReplacingMergeTree(...) (45ms)
2025-02-15 04:00:01 INFO  [sql] ALTER TABLE `default`.`trades` ATTACH PART '202401_1_50_3' (12ms)
```

When `false`, same queries logged at debug level only.

#### Summary statistics

Every operation prints a final summary line with the key metrics:

| Operation | Summary fields |
|-----------|---------------|
| create | tables, parts, total size, duration |
| upload | parts uploaded, parts skipped (incremental), compressed size, ratio, avg throughput, duration |
| download | parts, compressed→decompressed size, avg throughput, duration |
| restore | tables, parts attached, mode (A/B), phases completed, duration |
| retention | backups deleted (local/remote), S3 keys cleaned, duration |

#### Implementation

```rust
// Progress tracker shared across concurrent tasks
struct ProgressTracker {
    operation: String,
    total_parts: u64,
    completed_parts: AtomicU64,
    total_bytes: u64,
    completed_bytes: AtomicU64,
    start_time: Instant,
    bar: Option<indicatif::ProgressBar>,  // None in server mode
}

impl ProgressTracker {
    fn log_part_complete(&self, table: &str, part: &str, bytes: u64) {
        let completed = self.completed_parts.fetch_add(1, Ordering::Relaxed) + 1;
        self.completed_bytes.fetch_add(bytes, Ordering::Relaxed);
        // Update progress bar if present
        if let Some(bar) = &self.bar {
            bar.set_position(completed);
        }
        // Log at periodic intervals (every 10% or every 30s, whichever comes first)
        if self.should_log(completed) {
            let pct = completed * 100 / self.total_parts;
            let throughput = self.throughput_mb_s();
            info!("[{}] Progress: {}/{} parts [{}%] — {} MB/s",
                  self.operation, completed, self.total_parts, pct, throughput);
        }
    }
}
```

**Key crates**: `indicatif` (progress bars), `tracing` + `tracing-subscriber` (structured logging), `serde_json` (JSON format).

### 11.5 Signal Handling & Graceful Shutdown

Proper signal handling is essential — K8s sends SIGTERM before killing pods, and operators expect Ctrl+C to work cleanly without leaving orphaned shadow directories or partial uploads.

| Signal | CLI behavior | Server behavior |
|--------|-------------|-----------------|
| SIGINT (Ctrl+C) | Cancel current operation, run cleanup (UNFREEZE, abort multipart), exit 130 | Same as SIGTERM |
| SIGTERM | Same as SIGINT | Graceful shutdown: stop accepting new requests, wait for in-flight operations to complete (up to 30s), UNFREEZE all pending tables, DROP integration tables, exit 0 |
| SIGHUP | Ignored | Reload config file (watch interval, retention, credentials). Running operations continue with old config. |
| SIGQUIT | Dump all goroutine/task stacks to stderr (debugging), then continue | Same |

**Cleanup guarantees on cancellation:**

```
SIGTERM received during backup:
  1. Signal cancellation token (tokio::CancellationToken)
  2. Each table FREEZE task has a scopeguard:
     - UNFREEZE always runs, even if task was cancelled
     - Shadow directory cleaned for this table
  3. Wait for all UNFREEZE operations to complete (bounded: 30s timeout)
  4. Log: "Backup cancelled, all shadow directories cleaned"
  5. No partial backup left on disk (or if upload started, state file persisted for --resume)

SIGTERM received during upload:
  1. Cancel remaining upload tasks
  2. Abort in-progress S3 multipart uploads (AbortMultipartUpload)
  3. Save resumable state file with completed parts
  4. Log: "Upload interrupted, state saved. Resume with --resume"

SIGTERM received during restore:
  1. Cancel remaining ATTACH operations
  2. Already-attached parts remain (safe — partial restore is better than rollback)
  3. Log: "Restore interrupted at table {db}.{table}, {N}/{M} parts attached"

SIGTERM received in server mode:
  1. Stop accepting new HTTP connections
  2. Wait for in-flight API operations (30s grace period)
  3. If watch mode active: complete current sleep or abort current backup cleanly
  4. DROP integration tables (system.backup_list, system.backup_actions)
  5. Exit 0
```

**K8s integration**: Set `terminationGracePeriodSeconds: 60` in the pod spec to give chbackup time to clean up UNFREEZE operations. The 30s internal timeout is well within this.

### 11.6 Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | General error (configuration, connection, S3 failure) |
| 2 | Usage error (invalid flags, unknown command) |
| 3 | Backup not found (for restore/download/delete of non-existent backup) |
| 4 | Lock conflict (another operation on same backup name in progress) |
| 130 | Interrupted by SIGINT (Ctrl+C) |
| 143 | Interrupted by SIGTERM |

CLI scripts can check exit codes for automation:
```bash
chbackup create daily || {
  if [ $? -eq 4 ]; then
    echo "Another backup in progress, skipping"
    exit 0
  fi
  echo "Backup failed" >&2
  exit 1
}
```

### 11.7 Operational Notes

**Timestamps**: All timestamps in manifests, logs, and API responses are UTC (ISO 8601). The watch mode `name_template` uses UTC time for `{time:...}` formatting. This avoids timezone ambiguity in multi-region deployments.

**Memory usage**: chbackup is designed for constant memory regardless of backup size. Streaming upload/download means no part is fully buffered in memory. The primary memory consumers are: (1) the table metadata list (grows with number of tables, not data size), (2) concurrent upload/download tasks (bounded by `upload_concurrency` × chunk_size). For a 1000-table backup with `upload_concurrency: 8` and default chunk size, expect ~200MB peak RSS. Set container memory limits accordingly.

**S3 consistency**: Since December 2020, AWS S3 provides strong read-after-write consistency for all operations. chbackup relies on this: after uploading `metadata.json`, it is immediately visible to list/download operations. For S3-compatible stores that don't guarantee strong consistency (some MinIO configurations), the atomic rename via CopyObject + Delete provides the safety boundary.

**Symlinks**: ClickHouse multi-disk configurations may use symlinked data directories. During FREEZE, ClickHouse creates hardlinks in `shadow/` that resolve through symlinks. chbackup follows the resolved paths when walking shadow directories. When restoring, it writes to the target path reported by `system.parts` / `system.disks`, not the symlink source.

**ClickHouse connection failure**: If ClickHouse is unreachable at startup, chbackup exits immediately with a clear error. In server mode, the health endpoint returns unhealthy until CH is reachable. The `clickhouse.timeout` setting (default 5m) applies to individual queries, not the initial connection.

---

## 12. Configuration

~40 params vs Go tool's 200+:

```yaml
general:
  log_level: info                    # debug | info | warning | error
  log_format: text                   # text (human-readable) | json (structured, for Loki/ELK)
  disable_progress_bar: false        # auto-disabled when stdout is not a TTY
  backups_to_keep_local: 0           # 0 = unlimited; -1 = delete local after successful upload
  backups_to_keep_remote: 7          # after upload, delete oldest exceeding count
  upload_concurrency: 4              # parallel part uploads (auto-tuned: round(sqrt(CPU/2)))
  download_concurrency: 4            # parallel part downloads
  upload_max_bytes_per_second: 0     # 0 = no throttle; bytes/sec rate limit per part
  download_max_bytes_per_second: 0
  object_disk_server_side_copy_concurrency: 32
  retries_on_failure: 3              # retry count for upload/download failures
  retries_pause: "5s"                # wait between retries
  retries_jitter: 30                 # percent jitter on retries_pause (avoids thundering herd)
  use_resumable_state: true          # track progress in state files for --resume

clickhouse:
  host: localhost
  port: 8123
  username: default
  password: ""
  data_path: /var/lib/clickhouse
  config_dir: /etc/clickhouse-server
  secure: false                      # use TLS for ClickHouse connection
  skip_verify: false                 # skip TLS certificate verification
  tls_key: ""                        # TLS client key file
  tls_cert: ""                       # TLS client certificate file
  tls_ca: ""                         # TLS custom CA file
  sync_replicated_tables: true       # SYSTEM SYNC REPLICA before FREEZE
  check_replicas_before_attach: true # wait for replication queue before ATTACH
  check_parts_columns: false         # validate column type consistency before backup
  mutation_wait_timeout: 5m
  restore_as_attach: false           # use DETACH/ATTACH TABLE mode for full restores
  restore_schema_on_cluster: ""      # execute DDL with ON CLUSTER clause (cluster name from system.clusters)
  restore_distributed_cluster: ""    # rewrite Distributed engine cluster references during restore
  max_connections: 1                 # concurrent restore table operations
  log_sql_queries: true              # log SQL queries at info level (false = debug level)
  ignore_not_exists_error_during_freeze: true  # skip tables dropped during backup (CH error 60/81)
  freeze_by_part: false              # freeze individual parts instead of whole table
  freeze_by_part_where: ""           # WHERE clause for part filtering when freeze_by_part: true
  backup_mutations: true             # backup pending mutations from system.mutations
  restart_command: "exec:systemctl restart clickhouse-server"  # run after --rbac or --configs restore
  debug: false                       # verbose ClickHouse client debug logging
  rbac_backup_always: false          # always include RBAC objects in backup
  config_backup_always: false        # always include CH config files in backup
  named_collections_backup_always: false  # always include named collections in backup
  rbac_resolve_conflicts: "recreate" # on RBAC restore conflict: "recreate", "ignore", "fail"
  skip_tables:                       # glob patterns to exclude
    - "system.*"
    - "INFORMATION_SCHEMA.*"
    - "information_schema.*"
    - "_temporary_and_external_tables.*"  # ClickHouse internal temporary tables
  skip_table_engines: []             # engine names to exclude (e.g. ["Kafka", "S3Queue"])
  skip_disks: []                     # disk names to exclude from backup
  skip_disk_types: []                # disk types to exclude (e.g. ["cache", "local"])
  default_replica_path: "/clickhouse/tables/{shard}/{database}/{table}"
  default_replica_name: "{replica}"
  timeout: 5m                        # ClickHouse query timeout

s3:
  bucket: my-backup-bucket
  region: us-east-1
  endpoint: ""                       # for MinIO, R2, etc.
  prefix: chbackup                   # S3 key prefix. Supports {macro} expansion from system.macros
                                     # e.g., "chbackup/shard-{shard}" to isolate per-shard backups
  access_key: ""
  secret_key: ""
  assume_role_arn: ""                # AWS IAM role to assume
  force_path_style: false            # true for MinIO, Ceph
  disable_ssl: false                 # true for local S3-compatible stores
  disable_cert_verification: false   # forces endpoint to HTTP when true (requires explicit endpoint URL).
                                     # The AWS SDK for Rust (aws-smithy-http-client v1.1.10) has no public API
                                     # to skip TLS certificate verification, so HTTP fallback is used instead.
                                     # Separate from clickhouse.skip_verify.
  acl: ""                            # S3 ACL ("private", "bucket-owner-full-control", or "" for disabled)
  storage_class: STANDARD
  sse: ""                            # AES256 | aws:kms
  sse_kms_key_id: ""                 # KMS key for aws:kms
  max_parts_count: 10000             # S3 multipart max parts
  chunk_size: 0                      # S3 multipart chunk size (0 = auto: remote_size / max_parts_count)
  concurrency: 1                     # S3 SDK internal concurrency per upload
  object_disk_path: ""               # separate S3 prefix for object disk backup data
  allow_object_disk_streaming: false # if CopyObject fails (cross-region, incompatible), fallback to
                                     # streaming download+reupload. Warns about high network traffic.
  debug: false                       # verbose S3 SDK request/response logging

backup:
  tables: "*.*"
  allow_empty_backups: false         # true = create empty backup when no tables match filter
  compression: lz4                   # lz4 | zstd | gzip | none
  compression_level: 1
  upload_concurrency: 4
  download_concurrency: 4
  object_disk_copy_concurrency: 8
  upload_max_bytes_per_second: 0     # 0 = unlimited
  download_max_bytes_per_second: 0
  retries_on_failure: 5
  retries_duration: 10s
  retries_jitter: 0.1               # randomize retry delay by ±10%
  skip_projections: []               # patterns like "db.table:proj_name"

retention:
  backups_to_keep_local: 0           # 0 = unlimited; -1 = delete local after upload
  backups_to_keep_remote: 0          # 0 = unlimited

watch:
  enabled: false                     # enable watch loop in server mode
  watch_interval: 1h                 # interval between incremental backups
  full_interval: 24h                 # interval between full backups
  name_template: "shard{shard}-{type}-{time:%Y%m%d_%H%M%S}"
  max_consecutive_errors: 5          # abort after N consecutive failures
  retry_interval: 5m                 # wait before retrying after error
  delete_local_after_upload: true    # clean local backup after upload

api:
  listen: "localhost:7171"
  enable_metrics: true
  create_integration_tables: true   # create system.backup_list and system.backup_actions URL tables
  integration_tables_host: ""       # DNS name for URL engine (default: localhost)
  username: ""                       # basic auth username (empty = no auth)
  password: ""                       # basic auth password
  secure: false                      # use TLS for API endpoint
  certificate_file: ""               # TLS certificate file
  private_key_file: ""               # TLS private key file
  ca_cert_file: ""                   # TLS CA cert file
  allow_parallel: false              # allow concurrent operations on different backup names
  complete_resumable_after_restart: true  # auto-resume interrupted upload/download on server startup
  watch_is_main_process: false       # if watch loop dies unexpectedly, exit the server process
```

---

## 13. Clean Command

Remove leftover FREEZE data from ClickHouse shadow directories across all disks:

```
chbackup clean

1. Query system.disks to get all disk paths
2. For each disk (excluding backup-type disks):
   Remove all contents of {disk.path}/shadow/ matching chbackup_* prefix
   (preserves any non-chbackup shadow data from manual FREEZE operations)
3. Log cleaned paths with freeze names for audit trail
```

Because freeze names follow the `chbackup_{backup_name}_{db}_{table}` convention, the clean command can also target a specific backup: `chbackup clean --name daily_mon` removes only `shadow/chbackup_daily_mon_*` directories.

---

## 14. Feature Comparison: chbackup vs clickhouse-backup (Go)

### Why chbackup?

| | clickhouse-backup (Go) | chbackup (Rust) |
|---|---|---|
| **Language / Runtime** | Go 1.25, ~60MB binary, GC pauses | Rust, ~15MB static musl binary, zero GC |
| **Config complexity** | 200+ parameters | ~40 parameters |
| **Storage backends** | S3, GCS, Azure, FTP, SFTP, Custom | S3 (covers MinIO, R2, GCS-via-S3) |

### Data Safety

| Problem | Go tool | chbackup |
|---|---|---|
| Incremental backup corruption | Chains break if parent deleted — silent data loss | Self-contained manifests, every backup restorable independently |
| Same part name ≠ same data (#1307) | Trusts part name only → silent wrong data | CRC64(checksums.txt) verification → re-upload + warn |
| Restore drops tables by default | DROP first, then recreate → data window | Safe mode default (ATTACH only), DROP requires explicit `--rm` |
| Streaming engines start during restore (#1235) | Kafka/Queue start consuming mid-restore → duplicates | Created AFTER all data attached |
| S3 object shared between backups | Objects shared until #1278 fix → restore corrupts other backups | Always isolated: UUID-prefix per backup |
| DROP REPLICA uses wrong ZK path (#1162) | Uses macro template → fails cross-cluster | Extracts actual ZK path from backup DDL |

### Performance

| Area | Go tool | chbackup |
|---|---|---|
| Concurrency | goroutines + errgroup (cooperative) | tokio async + flat semaphore (non-blocking I/O) |
| Download speed | 6x slower than upload in some cases (#1163) | Async streaming decompression, pipelined I/O |
| Column type pre-check | 1 query per table → N+1 on 500 tables (#1194) | Single batch query |
| S3 retention cleanup | Sequential DeleteObject calls (#1066) | Batched DeleteObjects API |
| Diff-from upload | Re-uploads parts even if unchanged | Skips via CRC64 match |

### Operability

| Area | Go tool | chbackup |
|---|---|---|
| Crash forensics | Shadow dirs named `a1b2c3d4-e5f6-...` (UUID) | `chbackup_{backup}_{db}_{table}` — instantly identifiable |
| Manual cleanup after crash | Must find UUID → table mapping in logs | `ls shadow/ \| grep chbackup_daily_mon` then UNFREEZE by name |
| State file write fails (#1172) | Fatal — entire backup aborted | Warning — backup continues (just not resumable) |
| Part sizes in list output (#1208) | Not available | Stored in manifest: per-part + per-table totals |
| Disk space pre-check | Ignores hardlink dedup savings (#1268) | `required = total - sum(local CRC64 matches)` |
| DDL restore ordering | Brute-force retry loop | Topological sort on dependency graph |
| Mutations | Off by default, fragile re-apply | Wait before FREEZE; deterministic re-apply fallback |
| New CH disk format (#1121) | Only `<type>s3</type>` | Handles both `<type>` and `<object_storage_type>` |
| Resumable state | BoltDB (extra dependency) | Simple JSON file |
| Watch/scheduler | 20-param function, ad-hoc state tracking, continues incremental after errors | State machine, resume on restart, error→full fallback, SIGHUP config reload |

### Scope Tradeoffs

Features we intentionally skip (deferred to v2 or permanently out of scope):

| Feature | Go tool | chbackup | Rationale |
|---|---|---|---|
| GCS / Azure / FTP / SFTP | ✅ | ❌ | S3-compatible covers 95% of deployments |
| Embedded backup (CH native) | ✅ | ❌ v2 | Complex, limited adoption |
| Backup sharding | ✅ | ❌ v2 | Needs shared storage design first |
| FIPS build | ✅ | ❌ v2 | Evaluate rustls-fips when needed |

### Detailed Comparison

| Area | Go Tool | chbackup (Rust) |
|------|---------|-----------------|
| Incremental model | Chain-based (fragile) | Self-contained manifests |
| Manifest | Own parts + RequiredBackup pointer | ALL parts with S3 keys |
| Mutations | Off by default; re-apply on restore | Wait before FREEZE; fallback re-apply |
| S3 disk rename | Shared objects until fix #1278 | **Always isolated** (UUID-based paths) |
| S3 same-name restore | Always copies | Zero-copy if objects exist |
| DDL restore order | Brute-force retry loop | Topological sort on dependency graph |
| Dependencies | Fields defined but never populated | Query system.tables, store in manifest |
| Part attach order | SortPartsByMinBlock | Same — sort by min_block per partition |
| Non-destructive restore | Not supported (always DROP first) | Default — try ATTACH, catch overlaps |
| Sync replica | Configurable (off by default) | Always on for Replicated tables |
| Object disk metadata | 5 format versions | Same — must handle all 5 + InlineData |
| frozen_metadata.txt | Skip via string check | Same |
| Freeze naming | Random UUID (opaque) | `chbackup_{name}_{db}_{table}` (human-readable) |
| Replicated restore | ATTACH TABLE + RESTORE REPLICA | Same + CheckReplicationInProgress |
| Replica path conflicts | Check ZK + rewrite DDL | Same (validated against Go implementation) |
| Engine-priority sort | getOrderByEngine() | Same engine ordering + phased architecture |
| File ownership | Chown to CH user after restore | Same |
| Table path encoding | URL-encode special chars | Same |
| Checksum dedup | CRC64 of checksums.txt | Same — hardlink-exists-files optimization |
| Projection skipping | --skip-projections glob patterns | Same |
| PID lock | /tmp file with PID check | Same |
| Integration tables | `system.backup_list`, `system.backup_actions` via URL engine | Same — drop-in compatible column schema |
| Clean shadow | CLI + API command | Same |
| Resumable upload/download | BoltDB state tracking | JSON state file (simpler, no dependency) |
| Rate limiting | upload/download max bytes/sec | Same |
| Retry with backoff | Exponential + jitter | Same |
| Disk space pre-check | CheckDisksUsage before download | Same + accounts for hardlink dedup savings |
| Diff-from verification | Name match only | Name + CRC64 checksum (#1307) |
| Column type check | Per-table query before freeze | Single batch query (#1194) |
| Streaming engine restore | Created with all other tables | Postponed activation after data restore (#1235) |
| Resumable state failure | Fatal (kills backup) | Warning only, backup continues (#1172) |
| Part sizes in manifest | Not stored | Stored per-part for space estimation (#1208) |
| DROP REPLICA path | Uses default_replica_path macro | Uses actual ZK path from backup DDL (#1162) |
| Object storage disk format | type=s3 only | Handles both type=s3 and object_storage_type=s3 (#1121) |
| Retention batch delete | Sequential S3 deletes | Batched DeleteObjects for speed (#1066) |
| Config params | 200+ | ~40 |
| Concurrency model | errgroup + flat semaphore (Go) | tokio + flat semaphore (async, non-blocking) |
| Storage backends | S3, GCS, Azure, FTP, SFTP, Custom | S3 only (covers MinIO, R2, etc.) |
| Embedded backup | CH native BACKUP/RESTORE support | Deferred (v2) |
| Backup sharding | Hash-based replica assignment | Deferred (v2) |
| Watch/scheduler | Built-in cron loop (20-param function) | State machine with resume, error→full fallback, SIGHUP reload |

---

## 15. Known Go Tool Issues Addressed

| Issue | Description | Our Fix |
|-------|-------------|---------|
| #907 | Delete base backup breaks incremental chain | Self-contained manifests |
| #882 | Restore fails on chain-dependent backups | No chains |
| #826 | frozen_metadata.txt breaks restore | Skip during shadow walk |
| #1025 | Various restore edge cases | Phased restore with dependency ordering |
| #1265 | S3 object collision on table rename | Always-isolate with UUID-based paths |
| #1278 | Object disk key rewriting needed | Built-in, not opt-in |
| #1290 | CH 25.10 full path format in v5 metadata | Normalize to relative, prepend dest prefix |
| #750 | Macro resolution during restore | Resolve macros before S3 operations |
| #301 | Backup with pending mutations | Pre-flight mutation wait |
| #849 | Replica already exists during restore | Check system.zookeeper, rewrite DDL path |
| #474 | Concurrent ATTACH PART with replication | CheckReplicationInProgress before attach |
| #71009 | Wrong ATTACH order corrupts ReplacingMT | SortPartsByMinBlock per partition |
| #878 | Download fills disk | Pre-flight disk space check |
| #1307 | diff-from trusts part name only, misses data corruption | CRC64 verification during diff-from upload |
| #1268 | Free space check ignores hardlink dedup savings | Subtract deduped parts from space requirement |
| #1208 | No part/table sizes in metadata, hard to estimate restore | Store part_size and total_bytes in manifest |
| #1172 | Resumable state write failure kills entire backup | Downgrade to warning, continue backup |
| #1235 | Kafka/Queue engines consume data before restore completes | Postponed table activation (Section 5.2 Phase 2) |
| #1194 | Per-table column type queries slow on 500+ tables | Single batch query before freeze |
| #1162 | Restore uses default_replica_path, not backup's ZK path | Use actual ZK path from backup DDL for DROP REPLICA |
| #1121 | S3 tiered storage (CH 24.1+ object_storage_type) fails | Detect new disk format via system.disks metadata |
| #1163 | Download much slower than upload | Async streaming decompression + flat semaphore |

---

## 16. Implementation Notes from Issue Audit

Most Go tool issues are addressed inline in their respective design sections. This section covers cross-cutting implementation patterns not covered elsewhere.

### 16.1 Graceful Resumable State Degradation (#1172)

If writing the resumable state file fails (permissions, disk full, concurrent lock), downgrade to a warning and continue the backup/upload/download. The operation is still correct — it just won't be resumable if interrupted. The Go tool currently fatals on this, which kills a perfectly good backup because of an unrelated filesystem issue.

```rust
match save_resumable_state(&state_path, &state) {
    Ok(_) => {},
    Err(e) => {
        warn!("Failed to write resumable state: {}. Backup will continue but won't be resumable.", e);
    }
}
```

This pattern applies to ALL state file writes: upload.state.json, download.state.json, restore.state.json.

### 16.2 Tiered Storage: Mixed Local + S3 Disks (#1121)

A single table can have parts on BOTH local and S3 disks (via ClickHouse storage policies). The backup must handle both in the same pipeline:

```
For each table:
  For each part:
    Check which disk it lives on (from system.parts or shadow metadata)
    If local disk → hardlink to staging, compress + upload to backup bucket
    If S3 disk   → CopyObject to backup bucket (server-side, no compression)
  Manifest records disk name per part → restore routes each to correct disk
```

ClickHouse 24.1+ uses `<object_storage_type>s3</object_storage_type>` (new format) alongside `<type>s3</type>` (old format). Detect both via `system.disks`: `type = 's3' OR type = 'object_storage'`.

### 16.3 Smart Free Space with Hardlink Dedup (#1268)

The download pre-flight space check must account for parts that will be hardlinked (zero disk space) rather than downloaded:

```
required_space = sum(all parts in backup) - sum(parts that match local CRC64)
actual_free = query system.disks for each disk path
if required_space > actual_free * 0.95:
  error "Insufficient disk space: need {required_space}, have {actual_free}"
```

### 16.4 JSON/Object Column Type Detection (#1194)

Separate from the column consistency check (3.3), also detect column types that ClickHouse cannot FREEZE correctly. Single batch query combined with the consistency check:

```sql
-- Incompatible types (added to 3.3 batch query)
SELECT database, table, name, type
FROM system.columns
WHERE (database, table) IN (target_tables)
  AND (type LIKE '%Object%' OR type LIKE '%JSON%')
```

Tables with JSON/Object columns should be flagged as warnings — they may require special handling or exclusion.

---

## 17. Deferred Features (v2)

| Feature | Go config param(s) | Why Deferred |
|---------|-------------------|-------------|
| Embedded backup (CH BACKUP/RESTORE) | `use_embedded_backup_restore`, `embedded_backup_disk` | Completely separate code path; FREEZE works with all CH versions |
| Backup sharding across replicas | `sharded_operation_mode` | Requires replica coordination; single-node MVP first. Go tool has 4 modes: none (default), table, database, first-replica. first-replica is most common production choice — lexicographically first active replica backs up each table. |
| GCS/Azure/FTP/SFTP storage | gcs.*, azblob.*, ftp.*, sftp.* | S3 API covers most object stores (MinIO, R2, Wasabi, etc.) |
| Callback URLs | `?callback=URL` on API endpoints | API webhooks for CI/CD. POST with `{"status":"error|success","error":"..."}` |
| Disk mapping | `clickhouse.disk_mapping` | Override `system.disks` paths during cross-cluster restore; edge case for unusual disk layouts |
| Shared backup storage + merges (#1308) | — | Major architectural change; 2.8.0 milestone scope |
| Iceberg table backup (#1154) | — | New CH table type, needs separate handling |
| Non-sharded replicated restore (#1181) | — | DDL rewriting for shard→single topology |
| CPU/IO nice priority | `cpu_nice_priority`, `io_nice_priority` | `nice` / `ionice` wrappers for throttling CPU/disk-intensive operations |
| S3 object labels | `s3.object_labels` | Metadata tags on S3 objects (`{macro_name}`, `{backupName}`) |
| S3 custom storage class map | `s3.use_custom_storage_class`, `s3.custom_storage_class_map` | Per-backup-name-pattern storage class assignment |
| S3 checksum algorithm | `s3.checksum_algorithm` | For S3 object lock (`CRC32` as fastest) |
| S3 multipart download | `s3.allow_multipart_download` | Parallel download of single large parts (needs extra disk space) |
| S3 request payer | `s3.request_payer` | Requester-pays bucket support |
| S3 SSE-KMS encryption context | `s3.sse_kms_encryption_context` | Base64-encoded JSON for KMS encryption context (symmetric keys only) |
| S3 customer-managed encryption (SSE-C) | `s3.sse_customer_algorithm`, `s3.sse_customer_key`, `s3.sse_customer_key_md5` | Customer-provided encryption keys |
| max_file_size archive splitting | `max_file_size` | Split large archives into chunks (default 1GB). Irrelevant with streaming per-part upload. |
| pprof endpoint | `api.enable_pprof` | Profiling endpoint (Rust equivalent: tokio-console or pprof-rs) |
| Custom storage backend | `custom.upload_command`, `custom.download_command`, etc. | go-template commands for rclone/kopia/restic/rsync integration |
| FIPS build | — | Evaluate rustls-fips when needed |
