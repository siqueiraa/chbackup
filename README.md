# chbackup

[![CI](https://github.com/siqueiraa/chbackup/actions/workflows/ci.yml/badge.svg)](https://github.com/siqueiraa/chbackup/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Docker Image](https://img.shields.io/docker/v/siqueiraa/chbackup?label=Docker&sort=semver)](https://hub.docker.com/r/siqueiraa/chbackup)

Drop-in Rust replacement for [Altinity/clickhouse-backup](https://github.com/Altinity/clickhouse-backup). Single static binary (~15 MB), S3-only storage, non-destructive restore.

## Why chbackup

- **Single static binary** -- zero runtime dependencies, musl-linked, runs anywhere Linux runs
- **Built for S3** -- no unused storage backends; AWS S3, MinIO, Ceph, and Cloudflare R2 all work out of the box
- **Parallel everything** -- backup, upload, download, and restore all run with configurable concurrency
- **Resumable** -- interrupted uploads, downloads, and restores pick up where they left off with `--resume`
- **Incremental backups** -- only upload parts that changed since the last backup
- **Kubernetes-native** -- runs as a sidecar with an HTTP API, Prometheus metrics, and scheduled watch mode
- **Drop-in compatible** -- same CLI commands and config format as clickhouse-backup

> **Switching from clickhouse-backup?** Same config file, same CLI, same S3 layout — just swap the binary.

### How it compares

| | chbackup | clickhouse-backup |
|---|---|---|
| Language | Rust | Go |
| Binary size | ~15 MB (static musl) | ~80 MB |
| Storage backends | S3 only | S3, GCS, Azure, SFTP, FTP, ... |
| Runtime dependencies | None | None |

## Quick start

```bash
# One-step backup and restore (recommended)
chbackup create_remote my_backup
chbackup restore_remote my_backup

# Equivalent two-step
chbackup create my_backup && chbackup upload my_backup
chbackup download my_backup && chbackup restore my_backup
```

```bash
# List all backups
chbackup list
```

```text
$ chbackup list
Local backups:
  # name                  created                  size      compressed  tables
  2025-06-01T00:00:00Z    2025-06-01 00:00:00 UTC  1.2 GiB   890 MiB    12 tables
  2025-06-02T00:00:00Z    2025-06-02 00:00:00 UTC  48 MiB    32 MiB     12 tables

Remote backups:
  # name                  created                  size      compressed  tables
  2025-06-01T00:00:00Z    2025-06-01 00:00:00 UTC  1.2 GiB   890 MiB    12 tables
  2025-06-02T00:00:00Z    2025-06-02 00:00:00 UTC  48 MiB    32 MiB     12 tables
```

### Scheduling backups

Use **watch mode** to run backups on a schedule. It alternates between full and incremental backups automatically, retries after failures, and cleans up old backups via retention.

Add to your config file (`/etc/chbackup/config.yml`):

```yaml
watch:
  watch_interval: 1d    # run an incremental backup every day
  full_interval: 7d     # run a full backup every 7 days

general:
  backups_to_keep_local: 3
  backups_to_keep_remote: 14
```

Durations accept `30s`, `1h`, `1d` formats.

Then start it:

```bash
chbackup server --watch
```

Weekly full backups limit how many incrementals depend on a single base — if a full backup is lost, every incremental in that chain is unusable. Adjust `full_interval` based on your data size and recovery requirements.

> **Need exact clock-time scheduling?** Use a Kubernetes CronJob instead — see [examples/kubernetes/cronjob.yaml](examples/kubernetes/cronjob.yaml) for a ready-to-use weekly full + daily incremental setup that dispatches commands via `system.backup_actions`.

chbackup reads its config from `/etc/chbackup/config.yml`. Override with `-c path` or the `CHBACKUP_CONFIG` env var. A minimal config:

```yaml
clickhouse:
  host: localhost
  data_path: /var/lib/clickhouse

s3:
  bucket: my-backup-bucket
  region: us-east-1
```

Credentials can come from the config file, environment variables (`S3_ACCESS_KEY`, `S3_SECRET_KEY`), or IAM roles.

## Installation

**Kubernetes** (recommended):

chbackup runs as a sidecar alongside ClickHouse in `server --watch` mode. See the [Kubernetes guide](docs/kubernetes.md) and the ready-to-use [sidecar manifest](examples/kubernetes/sidecar.yaml).

```yaml
# Add chbackup as a sidecar to your ClickHouse pod
containers:
  - name: chbackup
    image: siqueiraa/chbackup:latest
    args: ["server", "--watch"]
    volumeMounts:
      - name: clickhouse-data
        mountPath: /var/lib/clickhouse
```

**Docker**:

```bash
docker pull siqueiraa/chbackup:latest
```

See the [Docker guide](docs/docker.md) and [docker-compose examples](examples/docker/).

**Static binary** (no dependencies):

```bash
curl -L -o /usr/local/bin/chbackup \
  https://github.com/siqueiraa/chbackup/releases/latest/download/chbackup-linux-amd64
chmod +x /usr/local/bin/chbackup
```

**Build from source** (requires Rust 1.85+):

```bash
cargo build --release
```

For a static musl binary (Linux):

```bash
rustup target add x86_64-unknown-linux-musl  # one-time setup
cargo build --release --target x86_64-unknown-linux-musl
```

## Requirements

- **ClickHouse 23.8+** -- uses `ALTER TABLE FREEZE WITH NAME`
- **S3-compatible storage** -- AWS S3, MinIO, Ceph, Cloudflare R2
- **Same host as ClickHouse** -- FREEZE creates hardlinks that need local filesystem access to `/var/lib/clickhouse/`

### Compatibility matrix

| chbackup | ClickHouse | Status |
|----------|------------|--------|
| 0.1.x | 23.8, 24.3, 24.8, 25.1 | Tested in CI |
| 0.1.x | 21.8 -- 23.7 | Untested and unsupported |

## Documentation

| Topic | Description |
|-------|-------------|
| [CLI Commands](docs/commands.md) | All commands with flags, examples, and usage patterns |
| [Configuration](docs/configuration.md) | All parameters, environment variables, and config priority |
| [S3 Storage](docs/s3.md) | AWS S3, MinIO, R2, STS AssumeRole, encryption, troubleshooting |
| [Kubernetes](docs/kubernetes.md) | Sidecar deployment, health checks, Prometheus, RBAC |
| [Backup Guide](docs/backup.md) | Full and incremental backups, compression, S3 disk tables |
| [Restore Guide](docs/restore.md) | Restore modes, table rename, database remap, partitions |
| [HTTP API](docs/api.md) | All API endpoints with examples and request/response formats |
| [Docker Guide](docs/docker.md) | Running with ClickHouse in Docker, docker-compose setups |
| [Migration Guide](docs/migration.md) | Step-by-step migration from Go clickhouse-backup |
| [Example Config](config.example.yml) | Annotated config file with all parameters and defaults |

### Ready-to-use examples

| Example | Description |
|---------|-------------|
| [examples/docker/](examples/docker/) | Docker Compose files for AWS S3, MinIO, and server mode |
| [examples/kubernetes/](examples/kubernetes/) | Kubernetes sidecar Deployment + CronJob for scheduled backups |

## Roadmap

### Core
- [x] Full & incremental backups
- [x] Parallel upload/download/restore
- [x] Resumable operations
- [x] S3 ObjectStorage disk support
- [x] Watch mode (scheduled backups)
- [x] HTTP API with Prometheus metrics
- [x] Go clickhouse-backup drop-in compatibility

### Planned
- [ ] Google Cloud Storage (GCS) backend
- [ ] Azure Blob Storage backend
- [ ] Embedded BACKUP/RESTORE (CH 22.7+)
- [ ] Client-side encryption
- [ ] FTP/SFTP backends
- [ ] Custom storage via rclone integration
- [ ] Disk mapping for cross-cluster restore

Have a feature request? [Open an issue](https://github.com/siqueiraa/chbackup/issues) to start a discussion.

## Exit codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | General error |
| 2 | Invalid CLI arguments |
| 3 | Backup not found |
| 4 | Lock conflict (another operation is running) |
| 130 | Interrupted (SIGINT) |
| 143 | Terminated (SIGTERM) |

## Contributing

Contributions are welcome. Before submitting a PR:

```bash
cargo fmt --check
cargo clippy -- -D warnings
cargo test
```

Integration tests run against real ClickHouse and S3. See the [CI workflow](.github/workflows/ci.yml) for the full test matrix.

## Contact

Rafael Siqueira -- [LinkedIn](https://www.linkedin.com/in/rafael-siqueiraa/) -- rafaelsiqueira06@gmail.com

## Acknowledgements

chbackup is inspired by [Altinity/clickhouse-backup](https://github.com/Altinity/clickhouse-backup). Thanks to the Altinity team for the original design.

## License

[MIT](LICENSE)
