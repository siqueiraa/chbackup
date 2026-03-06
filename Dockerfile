# =============================================================================
# Dockerfile -- Production chbackup image
# =============================================================================
# Multi-stage build: Rust compilation to static musl binary, then Alpine
# runtime. Produces a minimal image (~25MB) with zero runtime deps.
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
FROM rust:1.91-alpine AS builder
RUN apk add --no-cache musl-dev openssl-dev openssl-libs-static pkgconfig gcc
WORKDIR /src

# Dependency caching: copy manifests first, build with dummy main.rs,
# then copy real source. Docker layer cache means dependency rebuild only
# happens when Cargo.toml or Cargo.lock change.
COPY Cargo.toml Cargo.lock build.rs ./
RUN mkdir src && echo "fn main(){}" > src/main.rs \
    && cargo build --release \
    && rm -rf src

ARG VCS_REF
COPY src/ src/
RUN VCS_REF=${VCS_REF} cargo build --release

# Stage 2: Minimal Alpine runtime
FROM alpine:3.21
ARG VERSION=dev
ARG BUILD_DATE
ARG VCS_REF
LABEL org.opencontainers.image.title="chbackup"
LABEL org.opencontainers.image.description="ClickHouse backup and restore to S3. Single static binary, Kubernetes sidecar, incremental, resumable."
LABEL org.opencontainers.image.version=${VERSION}
LABEL org.opencontainers.image.created=${BUILD_DATE}
LABEL org.opencontainers.image.revision=${VCS_REF}
LABEL org.opencontainers.image.source="https://github.com/siqueiraa/chbackup"
LABEL org.opencontainers.image.documentation="https://github.com/siqueiraa/chbackup/tree/master/docs"
LABEL org.opencontainers.image.licenses="MIT"
LABEL org.opencontainers.image.vendor="Rafael Siqueira"

# Create clickhouse user/group with uid/gid 101 for file ownership compatibility
# with the official ClickHouse Docker image.
RUN addgroup -S -g 101 clickhouse \
    && adduser -S -h /var/lib/clickhouse -s /bin/bash -G clickhouse \
       -g "ClickHouse server" -u 101 clickhouse \
    && apk add --no-cache ca-certificates tzdata bash \
    && update-ca-certificates

COPY --from=builder /src/target/release/chbackup /bin/chbackup

ENTRYPOINT ["/bin/chbackup"]
CMD ["--help"]
