# Docker Guide

This guide covers running chbackup alongside ClickHouse in Docker. It includes single-container setups, docker-compose examples, and common patterns for development and production.

Ready-to-use compose files are in [`examples/docker/`](../examples/docker/):
- `docker-compose.yml` -- AWS S3
- `docker-compose.minio.yml` -- MinIO (no AWS needed)
- `docker-compose.server.yml` -- API server with watch mode

## Table of contents

- [How it works in Docker](#how-it-works-in-docker)
- [Quick start with docker-compose](#quick-start-with-docker-compose)
- [Manual Docker setup](#manual-docker-setup)
- [Using MinIO instead of AWS S3](#using-minio-instead-of-aws-s3)
- [Backup and restore walkthrough](#backup-and-restore-walkthrough)
- [Scheduled backups with watch mode](#scheduled-backups-with-watch-mode)
- [API server mode](#api-server-mode)
- [Multi-container setup (separate containers)](#multi-container-setup-separate-containers)
- [Building the chbackup image](#building-the-chbackup-image)
- [Environment variables reference](#environment-variables-reference)
- [Troubleshooting](#troubleshooting)

## How it works in Docker

chbackup must have filesystem access to `/var/lib/clickhouse/` because ClickHouse FREEZE creates hardlinks in that directory. In Docker, this means chbackup and ClickHouse must share the same volume.

There are two approaches:

1. **Same container**: Run both ClickHouse and chbackup in one container (simpler, good for development)
2. **Separate containers**: Share a volume between ClickHouse and chbackup containers (better for production, mirrors the Kubernetes sidecar pattern)

## Quick start with docker-compose

This is the fastest way to get a working backup system. Create a `docker-compose.yml`:

```yaml
services:
  clickhouse:
    image: clickhouse/clickhouse-server:24.8
    ports:
      - "8123:8123"
      - "9000:9000"
    volumes:
      - clickhouse-data:/var/lib/clickhouse
    healthcheck:
      test: clickhouse-client -q "SELECT 1"
      interval: 2s
      retries: 10

  chbackup:
    image: ghcr.io/user/chbackup:latest
    depends_on:
      clickhouse:
        condition: service_healthy
    volumes:
      - clickhouse-data:/var/lib/clickhouse
    environment:
      CLICKHOUSE_HOST: clickhouse
      S3_BUCKET: my-clickhouse-backups
      S3_REGION: us-east-1
      S3_ACCESS_KEY: ${S3_ACCESS_KEY}
      S3_SECRET_KEY: ${S3_SECRET_KEY}
    # Keep container alive for running CLI commands
    entrypoint: ["sleep", "infinity"]

volumes:
  clickhouse-data:
```

Start it:

```bash
export S3_ACCESS_KEY=your-access-key
export S3_SECRET_KEY=your-secret-key
docker compose up -d
```

Wait for ClickHouse to be ready, then run backup commands:

```bash
# Create some test data
docker compose exec clickhouse clickhouse-client -q "
  CREATE TABLE default.test (id UInt64, name String)
  ENGINE = MergeTree() ORDER BY id;
  INSERT INTO default.test SELECT number, concat('user_', toString(number))
  FROM numbers(1000);
"

# Create a backup
docker compose exec chbackup chbackup create my-first-backup

# Upload to S3
docker compose exec chbackup chbackup upload my-first-backup

# List backups
docker compose exec chbackup chbackup list
```

## Manual Docker setup

If you do not use docker-compose, create a shared volume and run both containers manually:

```bash
# Create a shared volume
docker volume create clickhouse-data

# Start ClickHouse
docker run -d \
  --name clickhouse \
  -v clickhouse-data:/var/lib/clickhouse \
  -p 8123:8123 \
  clickhouse/clickhouse-server:24.8

# Wait for ClickHouse to start
until docker exec clickhouse clickhouse-client -q "SELECT 1" 2>/dev/null; do
  sleep 1
done

# Run a backup command
docker run --rm \
  -v clickhouse-data:/var/lib/clickhouse \
  -e CLICKHOUSE_HOST=clickhouse \
  -e S3_BUCKET=my-backups \
  -e S3_REGION=us-east-1 \
  -e S3_ACCESS_KEY=your-key \
  -e S3_SECRET_KEY=your-secret \
  --network container:clickhouse \
  ghcr.io/user/chbackup:latest \
  create my-backup
```

The `--network container:clickhouse` flag makes the chbackup container share the ClickHouse container's network, so `localhost:8123` reaches ClickHouse.

Alternatively, use `CLICKHOUSE_HOST=clickhouse` and a shared Docker network:

```bash
docker network create ch-net

docker run -d --name clickhouse --network ch-net \
  -v clickhouse-data:/var/lib/clickhouse \
  clickhouse/clickhouse-server:24.8

docker run --rm --network ch-net \
  -v clickhouse-data:/var/lib/clickhouse \
  -e CLICKHOUSE_HOST=clickhouse \
  -e S3_BUCKET=my-backups \
  -e S3_ACCESS_KEY=your-key \
  -e S3_SECRET_KEY=your-secret \
  ghcr.io/user/chbackup:latest \
  create my-backup
```

## Using MinIO instead of AWS S3

For local development without AWS, use MinIO as an S3-compatible store:

```yaml
services:
  clickhouse:
    image: clickhouse/clickhouse-server:24.8
    ports:
      - "8123:8123"
    volumes:
      - clickhouse-data:/var/lib/clickhouse
    healthcheck:
      test: clickhouse-client -q "SELECT 1"
      interval: 2s
      retries: 10

  minio:
    image: minio/minio:latest
    command: server /data --console-address ":9001"
    ports:
      - "9000:9000"
      - "9001:9001"   # MinIO console
    environment:
      MINIO_ROOT_USER: minioadmin
      MINIO_ROOT_PASSWORD: minioadmin
    volumes:
      - minio-data:/data
    healthcheck:
      test: mc ready local
      interval: 2s
      retries: 10

  # Create the bucket on startup
  minio-setup:
    image: minio/mc:latest
    depends_on:
      minio:
        condition: service_healthy
    entrypoint: >
      /bin/sh -c "
      mc alias set local http://minio:9000 minioadmin minioadmin;
      mc mb --ignore-existing local/clickhouse-backups;
      "

  chbackup:
    image: ghcr.io/user/chbackup:latest
    depends_on:
      clickhouse:
        condition: service_healthy
      minio-setup:
        condition: service_completed_successfully
    volumes:
      - clickhouse-data:/var/lib/clickhouse
    environment:
      CLICKHOUSE_HOST: clickhouse
      S3_BUCKET: clickhouse-backups
      S3_ENDPOINT: "http://minio:9000"
      S3_ACCESS_KEY: minioadmin
      S3_SECRET_KEY: minioadmin
      S3_FORCE_PATH_STYLE: "true"
      S3_DISABLE_SSL: "true"
    entrypoint: ["sleep", "infinity"]

volumes:
  clickhouse-data:
  minio-data:
```

Start everything:

```bash
docker compose up -d
```

Now you can run backups without any AWS credentials:

```bash
docker compose exec chbackup chbackup create my-backup
docker compose exec chbackup chbackup upload my-backup
docker compose exec chbackup chbackup list
```

The MinIO console is available at `http://localhost:9001` (login: minioadmin/minioadmin) where you can browse the backup objects.

## Backup and restore walkthrough

This is a complete example: create data, back it up, destroy it, restore it.

```bash
# 1. Start the stack
docker compose up -d

# 2. Create tables and insert data
docker compose exec clickhouse clickhouse-client -q "
  CREATE DATABASE IF NOT EXISTS mydb;

  CREATE TABLE mydb.events (
    event_id UInt64,
    ts DateTime,
    user_id UInt64,
    event_type String
  ) ENGINE = MergeTree()
  PARTITION BY toYYYYMM(ts)
  ORDER BY (user_id, ts);

  INSERT INTO mydb.events
  SELECT
    number,
    now() - toIntervalDay(number % 90),
    number % 1000,
    ['click','view','purchase','signup'][number % 4 + 1]
  FROM numbers(100000);
"

# 3. Verify the data
docker compose exec clickhouse clickhouse-client -q "
  SELECT count() FROM mydb.events;
  SELECT event_type, count() FROM mydb.events GROUP BY event_type;
"

# 4. Create and upload a backup
docker compose exec chbackup chbackup create_remote full-backup

# 5. Verify the backup exists in S3
docker compose exec chbackup chbackup list remote

# 6. Simulate disaster: drop the table
docker compose exec clickhouse clickhouse-client -q "DROP TABLE mydb.events"

# 7. Verify it is gone
docker compose exec clickhouse clickhouse-client -q "SELECT count() FROM mydb.events"
# Error: Table mydb.events does not exist

# 8. Restore from S3
docker compose exec chbackup chbackup restore_remote full-backup

# 9. Verify the data is back
docker compose exec clickhouse clickhouse-client -q "
  SELECT count() FROM mydb.events;
  SELECT event_type, count() FROM mydb.events GROUP BY event_type;
"
```

## Scheduled backups with watch mode

Run chbackup with automatic scheduled backups:

```yaml
services:
  clickhouse:
    image: clickhouse/clickhouse-server:24.8
    volumes:
      - clickhouse-data:/var/lib/clickhouse
    healthcheck:
      test: clickhouse-client -q "SELECT 1"
      interval: 2s
      retries: 10

  chbackup:
    image: ghcr.io/user/chbackup:latest
    depends_on:
      clickhouse:
        condition: service_healthy
    # Run watch mode (continuous backup loop)
    command: ["watch", "--watch-interval=1h", "--full-interval=24h"]
    volumes:
      - clickhouse-data:/var/lib/clickhouse
    environment:
      CLICKHOUSE_HOST: clickhouse
      S3_BUCKET: my-clickhouse-backups
      S3_REGION: us-east-1
      S3_ACCESS_KEY: ${S3_ACCESS_KEY}
      S3_SECRET_KEY: ${S3_SECRET_KEY}

volumes:
  clickhouse-data:
```

Watch mode will:

- Create a full backup immediately on start
- Create incremental backups every hour
- Create a new full backup every 24 hours
- Delete local backups after upload
- Run retention cleanup

## API server mode

For remote management via HTTP API:

```yaml
services:
  clickhouse:
    image: clickhouse/clickhouse-server:24.8
    volumes:
      - clickhouse-data:/var/lib/clickhouse
    healthcheck:
      test: clickhouse-client -q "SELECT 1"
      interval: 2s
      retries: 10

  chbackup:
    image: ghcr.io/user/chbackup:latest
    depends_on:
      clickhouse:
        condition: service_healthy
    command: ["server", "--watch"]
    ports:
      - "7171:7171"
    volumes:
      - clickhouse-data:/var/lib/clickhouse
    environment:
      CLICKHOUSE_HOST: clickhouse
      API_LISTEN: "0.0.0.0:7171"
      S3_BUCKET: my-clickhouse-backups
      S3_REGION: us-east-1
      S3_ACCESS_KEY: ${S3_ACCESS_KEY}
      S3_SECRET_KEY: ${S3_SECRET_KEY}
      WATCH_INTERVAL: "1h"
      FULL_INTERVAL: "24h"

volumes:
  clickhouse-data:
```

Now you can manage backups via HTTP from your host:

```bash
# Health check
curl http://localhost:7171/health

# Create a backup
curl -X POST http://localhost:7171/api/v1/create

# List backups
curl http://localhost:7171/api/v1/list

# Watch status
curl http://localhost:7171/api/v1/watch/status

# Prometheus metrics
curl http://localhost:7171/metrics
```

See the [API documentation](api.md) for all endpoints.

## Multi-container setup (separate containers)

For production-like setups where chbackup runs as a separate container:

```yaml
services:
  clickhouse:
    image: clickhouse/clickhouse-server:24.8
    volumes:
      - clickhouse-data:/var/lib/clickhouse
    ports:
      - "8123:8123"
      - "9000:9000"
    healthcheck:
      test: clickhouse-client -q "SELECT 1"
      interval: 2s
      retries: 10

  chbackup:
    image: ghcr.io/user/chbackup:latest
    depends_on:
      clickhouse:
        condition: service_healthy
    command: ["server", "--watch"]
    ports:
      - "7171:7171"
    volumes:
      # Same volume as ClickHouse -- required for FREEZE hardlinks
      - clickhouse-data:/var/lib/clickhouse
    environment:
      # Point to ClickHouse container hostname
      CLICKHOUSE_HOST: clickhouse
      CLICKHOUSE_PORT: "8123"
      # API server listens on all interfaces
      API_LISTEN: "0.0.0.0:7171"
      # S3 credentials
      S3_BUCKET: my-clickhouse-backups
      S3_REGION: us-east-1
      S3_ACCESS_KEY: ${S3_ACCESS_KEY}
      S3_SECRET_KEY: ${S3_SECRET_KEY}
      # Watch mode settings
      WATCH_INTERVAL: "1h"
      FULL_INTERVAL: "24h"
      # Keep only 1 local backup, 7 remote
      CHBACKUP_BACKUPS_TO_KEEP_LOCAL: "-1"
      CHBACKUP_BACKUPS_TO_KEEP_REMOTE: "7"
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:7171/health"]
      interval: 10s
      retries: 3

volumes:
  clickhouse-data:
```

## Building the chbackup image

Build the Docker image from source:

```bash
# Build the production image
docker build -t chbackup:local .

# Use it in docker-compose
# Replace "ghcr.io/user/chbackup:latest" with "chbackup:local" in your compose file
```

The Dockerfile uses a multi-stage build:

1. **Builder stage**: Rust compiler with musl target, produces a static binary
2. **Runtime stage**: Alpine Linux with ca-certificates, ~25 MB total

For development with faster rebuild cycles:

```bash
# Build the binary on your host
cargo build --release --target x86_64-unknown-linux-musl

# Copy into a minimal image
docker build -f Dockerfile.local-test -t chbackup:dev .
```

## Environment variables reference

The most common environment variables for Docker deployments:

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `CLICKHOUSE_HOST` | Yes (if not localhost) | `localhost` | ClickHouse hostname |
| `CLICKHOUSE_PORT` | No | `8123` | ClickHouse HTTP port |
| `CLICKHOUSE_PASSWORD` | No | _(empty)_ | ClickHouse password |
| `S3_BUCKET` | Yes | | S3 bucket name |
| `S3_REGION` | Yes (for AWS) | `us-east-1` | AWS region |
| `S3_ACCESS_KEY` | Yes (if no IAM) | | AWS access key |
| `S3_SECRET_KEY` | Yes (if no IAM) | | AWS secret key |
| `S3_ENDPOINT` | For MinIO/R2 | | Custom S3 endpoint URL |
| `S3_FORCE_PATH_STYLE` | For MinIO/R2 | `false` | Use path-style addressing |
| `S3_DISABLE_SSL` | For HTTP endpoints | `false` | Disable HTTPS |
| `API_LISTEN` | For server mode | `localhost:7171` | API listen address |
| `WATCH_INTERVAL` | For watch mode | `1h` | Backup check interval |
| `FULL_INTERVAL` | For watch mode | `24h` | Full backup interval |

See [Configuration](configuration.md) for the full list of 54+ environment variables.

## Troubleshooting

### chbackup cannot connect to ClickHouse

```
Error: ClickHouse connection failed
```

1. Verify ClickHouse is running: `docker compose exec clickhouse clickhouse-client -q "SELECT 1"`
2. Check `CLICKHOUSE_HOST` is set to the container name (e.g., `clickhouse`), not `localhost`
3. Both containers must be on the same Docker network (docker-compose handles this automatically)
4. Check the port: `CLICKHOUSE_PORT` should be `8123` (HTTP), not `9000` (native)

### "No such file or directory" during backup

```
Error: /var/lib/clickhouse/shadow/... No such file or directory
```

The chbackup container cannot see ClickHouse's data directory. Verify both containers mount the same volume:

```bash
docker compose exec clickhouse ls /var/lib/clickhouse/
docker compose exec chbackup ls /var/lib/clickhouse/
```

Both should show the same contents. If not, check your volume mounts.

### Permission denied errors

The chbackup Docker image runs as root by default, which allows it to read/write ClickHouse files. If you run as a non-root user, ensure it has the same UID/GID as the ClickHouse user (101:101).

### MinIO "bucket does not exist"

The bucket must be created before running chbackup. The `minio-setup` service in the MinIO example handles this. If you are setting it up manually:

```bash
docker compose exec minio mc alias set local http://localhost:9000 minioadmin minioadmin
docker compose exec minio mc mb local/clickhouse-backups
```

### Container exits immediately

If using `chbackup create` (CLI mode), the container exits after the command finishes. This is normal. For a long-running container, use `server` or `watch` mode, or set `entrypoint: ["sleep", "infinity"]` for ad-hoc CLI usage.

### S3 upload fails with "connection refused"

If using MinIO, verify:

1. `S3_ENDPOINT` includes the correct port: `http://minio:9000`
2. `S3_FORCE_PATH_STYLE` is `true`
3. The MinIO container is on the same Docker network
4. MinIO is healthy: `docker compose exec minio mc ready local`
