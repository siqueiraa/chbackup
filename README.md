# chbackup

Drop-in Rust replacement for [Altinity/clickhouse-backup](https://github.com/Altinity/clickhouse-backup). Single static binary (~15 MB), S3-only storage, non-destructive restore.

## Why chbackup

- **Single static binary** -- zero runtime dependencies, musl-linked, runs anywhere Linux runs
- **Built for S3** -- no unused storage backends; AWS S3, MinIO, Ceph, and Cloudflare R2 all work out of the box
- **Parallel everything** -- backup, upload, download, and restore all run with configurable concurrency
- **Resumable** -- interrupted uploads, downloads, and restores pick up where they left off with `--resume`
- **Incremental backups** -- only upload parts that changed since the last backup
- **Kubernetes-native** -- runs as a sidecar with an HTTP API, Prometheus metrics, and scheduled watch mode
- **Drop-in compatible** -- same CLI commands and config format as clickhouse-backup

## Quick start

```bash
# Create a backup and upload it to S3
chbackup create_remote

# List all backups
chbackup list

# Download and restore from S3
chbackup restore_remote latest
```

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

**Static binary** (no dependencies):

```bash
curl -L -o /usr/local/bin/chbackup \
  https://github.com/user/chbackup/releases/latest/download/chbackup-linux-amd64
chmod +x /usr/local/bin/chbackup
```

**Docker**:

```bash
docker pull ghcr.io/user/chbackup:latest
```

**Build from source** (requires Rust 1.82+):

```bash
rustup target add x86_64-unknown-linux-musl  # one-time setup
cargo build --release --target x86_64-unknown-linux-musl
```

## Requirements

- **ClickHouse 21.8+** -- uses `ALTER TABLE FREEZE WITH NAME`
- **S3-compatible storage** -- AWS S3, MinIO, Ceph, Cloudflare R2
- **Same host as ClickHouse** -- FREEZE creates hardlinks that need local filesystem access to `/var/lib/clickhouse/`

### Compatibility matrix

| chbackup | ClickHouse | Status |
|----------|------------|--------|
| 0.1.x | 23.8, 24.3, 24.8, 25.1 | Tested in CI |
| 0.1.x | 21.8 -- 23.7 | Supported, not in CI matrix |

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
| [Example Config](config.example.yml) | Annotated config file with all parameters and defaults |

### Ready-to-use examples

| Example | Description |
|---------|-------------|
| [examples/docker/](examples/docker/) | Docker Compose files for AWS S3, MinIO, and server mode |
| [examples/kubernetes/](examples/kubernetes/) | Kubernetes sidecar Deployment manifest |

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

## License

[MIT](LICENSE)
