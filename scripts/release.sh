#!/usr/bin/env bash
# =============================================================================
# release.sh -- Tag a new release (git tag triggers Docker Hub push via CI)
# =============================================================================
#
# Usage:
#   ./scripts/release.sh 0.2.0           # Tag v0.2.0 and push tag
#   ./scripts/release.sh 0.2.0 --dry-run # Show what would happen
#
# Workflow:
#   1. Verifies Cargo.toml version matches the requested version
#   2. Verifies working tree is clean
#   3. Creates annotated git tag (v0.2.0)
#   4. Pushes the tag to origin
#   5. GitHub Actions release.yml builds multi-arch Docker image and pushes
#      to Docker Hub with tags: latest, 0.2.0, 0.2
#
# For manual Docker builds (without CI): use scripts/build-docker.sh --push
# =============================================================================

set -euo pipefail

DRY_RUN=false
VERSION=""

# Parse arguments
for arg in "$@"; do
  case "$arg" in
    --dry-run) DRY_RUN=true ;;
    --help|-h)
      echo "Usage: $0 <VERSION> [--dry-run]"
      echo ""
      echo "  VERSION    Semantic version (e.g. 0.2.0) — must match Cargo.toml"
      echo "  --dry-run  Show what would happen without making changes"
      echo ""
      echo "This script creates a git tag and pushes it. The GitHub Actions"
      echo "release workflow then builds and pushes the Docker image."
      exit 0
      ;;
    *)
      if [[ -z "$VERSION" ]]; then
        VERSION="$arg"
      else
        echo "Unknown argument: $arg"
        exit 1
      fi
      ;;
  esac
done

if [[ -z "$VERSION" ]]; then
  echo "Error: VERSION required"
  echo "Usage: $0 <VERSION> [--dry-run]"
  exit 1
fi

TAG="v${VERSION}"

# 1. Check Cargo.toml version matches
CARGO_VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/')
if [[ "$CARGO_VERSION" != "$VERSION" ]]; then
  echo "Error: Cargo.toml version ($CARGO_VERSION) != requested version ($VERSION)"
  echo ""
  echo "Update Cargo.toml first:"
  echo "  sed -i '' 's/version = \"$CARGO_VERSION\"/version = \"$VERSION\"/' Cargo.toml"
  echo "  cargo check  # update Cargo.lock"
  echo "  git add Cargo.toml Cargo.lock && git commit -m 'chore: bump version to $VERSION'"
  exit 1
fi

# 2. Check working tree is clean
if [[ -n "$(git status --porcelain)" ]]; then
  echo "Error: Working tree is not clean. Commit or stash changes first."
  git status --short
  exit 1
fi

# 3. Check tag doesn't already exist
if git tag -l "$TAG" | grep -q "$TAG"; then
  echo "Error: Tag $TAG already exists"
  exit 1
fi

# 4. Check we're on main branch
BRANCH=$(git rev-parse --abbrev-ref HEAD)
if [[ "$BRANCH" != "main" && "$BRANCH" != "master" ]]; then
  echo "Warning: You're on branch '$BRANCH', not main/master"
  read -r -p "Continue anyway? [y/N] " confirm
  if [[ "$confirm" != "y" && "$confirm" != "Y" ]]; then
    exit 1
  fi
fi

echo "=== chbackup release ==="
echo "  Version:  $VERSION"
echo "  Tag:      $TAG"
echo "  Branch:   $BRANCH"
echo "  Commit:   $(git rev-parse --short HEAD)"
echo ""

if [[ "$DRY_RUN" == "true" ]]; then
  echo "[dry-run] Would run:"
  echo "  git tag -a $TAG -m 'Release $VERSION'"
  echo "  git push origin $TAG"
  echo ""
  echo "After push, GitHub Actions will:"
  echo "  1. Build linux/amd64 + linux/arm64 Docker images"
  echo "  2. Push to Docker Hub: siqueiraa/chbackup:latest, :$VERSION, :${VERSION%.*}"
  echo "  3. Create GitHub Release with auto-generated notes"
  exit 0
fi

echo "Creating annotated tag $TAG..."
git tag -a "$TAG" -m "Release $VERSION"

echo "Pushing tag to origin..."
git push origin "$TAG"

echo ""
echo "Tag $TAG pushed. GitHub Actions will now:"
echo "  1. Build linux/amd64 + linux/arm64 Docker images"
echo "  2. Push to Docker Hub:"
echo "     - siqueiraa/chbackup:latest"
echo "     - siqueiraa/chbackup:$VERSION"
echo "     - siqueiraa/chbackup:${VERSION%.*}"
echo "  3. Create GitHub Release"
echo ""
echo "Monitor: https://github.com/siqueiraa/chbackup/actions"
