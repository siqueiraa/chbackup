# =============================================================================
# Dockerfile -- Production chbackup image
# =============================================================================
# Multi-stage build: Rust cross-compilation to static musl binary, then
# Alpine runtime. Produces a minimal image (~25MB) with zero runtime deps.
#
# Build:
#   docker build -t chbackup:latest --build-arg VERSION=0.1.0 .
#
# Run:
#   docker run --rm chbackup:latest --help
#   docker run --rm -v /var/lib/clickhouse:/var/lib/clickhouse \
#     -e S3_BUCKET=my-backups chbackup:latest server --watch
# =============================================================================

# Stage 1: Build static binary
FROM rust:1.82-alpine AS builder
RUN apk add --no-cache musl-dev openssl-dev openssl-libs-static pkgconfig
WORKDIR /src

# Dependency caching: copy manifests first, build with dummy main.rs,
# then copy real source. Docker layer cache means dependency rebuild only
# happens when Cargo.toml or Cargo.lock change.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main(){}" > src/main.rs \
    && cargo build --release --target x86_64-unknown-linux-musl \
    && rm -rf src

COPY src/ src/
RUN cargo build --release --target x86_64-unknown-linux-musl

# Stage 2: Minimal Alpine runtime
FROM alpine:3.21
ARG VERSION=dev
LABEL org.opencontainers.image.title="chbackup"
LABEL org.opencontainers.image.description="Fast ClickHouse backup and restore"
LABEL org.opencontainers.image.version=${VERSION}

# Create clickhouse user/group with uid/gid 101 for file ownership compatibility
# with the official ClickHouse Docker image.
RUN addgroup -S -g 101 clickhouse \
    && adduser -S -h /var/lib/clickhouse -s /bin/bash -G clickhouse \
       -g "ClickHouse server" -u 101 clickhouse \
    && apk add --no-cache ca-certificates tzdata bash \
    && update-ca-certificates

COPY --from=builder /src/target/x86_64-unknown-linux-musl/release/chbackup /bin/chbackup

ENTRYPOINT ["/bin/chbackup"]
CMD ["--help"]
