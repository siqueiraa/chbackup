#!/usr/bin/env bash
# =============================================================================
# build-docker.sh -- Build and optionally push chbackup Docker images
# =============================================================================
#
# Usage:
#   ./scripts/build-docker.sh              # Build for local arch only (--load)
#   ./scripts/build-docker.sh --push       # Build linux/amd64 + linux/arm64, push to Docker Hub
#   ./scripts/build-docker.sh --push --version 0.2.0   # Push with specific version tag
#
# Requires: docker buildx
# =============================================================================

set -euo pipefail

REPO="siqueiraa/chbackup"
PLATFORMS="linux/amd64,linux/arm64"
BUILDER_NAME="chbackup-builder"
PUSH=false
VERSION=""

# Parse arguments
while [[ $# -gt 0 ]]; do
  case "$1" in
    --push)    PUSH=true; shift ;;
    --version) VERSION="$2"; shift 2 ;;
    --help|-h)
      echo "Usage: $0 [--push] [--version X.Y.Z]"
      echo ""
      echo "  --push       Build multi-arch and push to Docker Hub"
      echo "  --version    Version tag (default: from Cargo.toml)"
      exit 0
      ;;
    *) echo "Unknown option: $1"; exit 1 ;;
  esac
done

# Resolve version from Cargo.toml if not provided
if [[ -z "$VERSION" ]]; then
  VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/')
fi

BUILD_DATE=$(date -u +%Y-%m-%dT%H:%M:%SZ)
VCS_REF=$(git rev-parse --short HEAD 2>/dev/null || echo "unknown")

echo "=== chbackup Docker build ==="
echo "  Repository:  $REPO"
echo "  Version:     $VERSION"
echo "  VCS ref:     $VCS_REF"
echo "  Build date:  $BUILD_DATE"
echo "  Push:        $PUSH"

# Tags
TAGS="-t ${REPO}:latest -t ${REPO}:${VERSION}"
echo "  Tags:        ${REPO}:latest, ${REPO}:${VERSION}"
echo ""

BUILD_ARGS="--build-arg VERSION=${VERSION} --build-arg BUILD_DATE=${BUILD_DATE} --build-arg VCS_REF=${VCS_REF}"

if [[ "$PUSH" == "true" ]]; then
  # Multi-arch build requires a docker-container builder
  if ! docker buildx inspect "$BUILDER_NAME" > /dev/null 2>&1; then
    echo "Creating buildx builder: $BUILDER_NAME"
    docker buildx create --name "$BUILDER_NAME" --driver docker-container --use
  else
    docker buildx use "$BUILDER_NAME"
  fi

  echo "Building for ${PLATFORMS} and pushing..."
  docker buildx build \
    --platform "$PLATFORMS" \
    $BUILD_ARGS \
    $TAGS \
    --push \
    .
  echo ""
  echo "Pushed:"
  echo "  docker pull ${REPO}:latest"
  echo "  docker pull ${REPO}:${VERSION}"
else
  # Local build: single arch, load into local Docker
  echo "Building for local architecture..."
  docker buildx build \
    $BUILD_ARGS \
    $TAGS \
    --load \
    .
  echo ""
  echo "Built locally. Run with:"
  echo "  docker run --rm ${REPO}:latest --help"
fi
