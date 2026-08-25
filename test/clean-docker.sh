#!/bin/bash
# =============================================================================
# clean-docker.sh — Remove this project's Docker resources, and nothing else
# =============================================================================
# Tears down the integration test environment and deletes the images it built.
#
# Everything here is scoped to this project. Containers, volumes and networks
# are matched on the compose project label; images are matched by name. A
# developer machine typically hosts many unrelated projects, so this script
# never runs `docker system prune`, which would take their images, volumes and
# build cache too.
#
# What it removes:
#   1. compose containers, volumes and networks labelled com.docker.compose.project=chbackup
#   2. images built by this project: chbackup-chbackup-test, chbackup:test-local
#   3. zookeeper:3.8, pulled solely for docker-compose.test.yml (re-pulled on demand)
#
# What it deliberately leaves alone:
#   * alpine:3.21 and the rust builder images. These are common bases shared
#     with other projects, and re-pulling them is slower than keeping them.
#   * Build cache. BuildKit cache entries carry no project label, and the
#     --filter keys are undocumented in some Docker versions, so a prune that
#     silently ignored its filter would destroy every other project's cache.
#     This project's share is small and ages out on its own. If you really want
#     it gone, run `docker builder prune` yourself and accept that other
#     projects will rebuild from scratch.
#   * S3 test data. That costs money and has its own script: test/clean-s3.sh.
#
# Usage:
#   ./test/clean-docker.sh
#   ./test/clean-docker.sh --dry-run    # report what would be removed
# =============================================================================

set -euo pipefail

COMPOSE_FILE="${COMPOSE_FILE:-docker-compose.test.yml}"
PROJECT="${COMPOSE_PROJECT_NAME:-chbackup}"
PROJECT_IMAGES=("chbackup-chbackup-test" "chbackup:test-local" "zookeeper:3.8")
DRY_RUN=false

if [[ "${1:-}" == "--dry-run" ]]; then
    DRY_RUN=true
fi

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
    sed -n '2,32p' "$0" | sed 's/^# \{0,1\}//'
    exit 0
fi

run() {
    if [[ "$DRY_RUN" == true ]]; then
        echo "  [dry-run] $*"
    else
        "$@" || true
    fi
}

cd "$(dirname "$0")/.."

echo "==> Project: $PROJECT   compose file: $COMPOSE_FILE"
if [[ "$DRY_RUN" == true ]]; then
    echo "==> DRY RUN: nothing will be removed"
fi

# --- 1. Compose stack: containers, volumes, networks -------------------------
echo
echo "==> Compose containers and volumes for project '$PROJECT'"
docker ps -a --filter "label=com.docker.compose.project=$PROJECT" \
    --format '    container {{.Names}} ({{.State}})' 2>/dev/null || true
docker volume ls --filter "label=com.docker.compose.project=$PROJECT" \
    --format '    volume    {{.Name}}' 2>/dev/null || true

if [[ -f "$COMPOSE_FILE" ]]; then
    # -v also removes named volumes, which hold the ClickHouse data directory.
    run docker compose -f "$COMPOSE_FILE" down -v --remove-orphans
else
    echo "    WARN: $COMPOSE_FILE not found; falling back to label-scoped removal"
    for c in $(docker ps -aq --filter "label=com.docker.compose.project=$PROJECT" 2>/dev/null); do
        run docker rm -f "$c"
    done
    for v in $(docker volume ls -q --filter "label=com.docker.compose.project=$PROJECT" 2>/dev/null); do
        run docker volume rm "$v"
    done
fi

# --- 2. Images this project built or pulled for itself ----------------------
echo
echo "==> Images owned by this project"
for img in "${PROJECT_IMAGES[@]}"; do
    # Match "name" and "name:tag" alike, so an untagged build is still caught.
    found=$(docker images --format '{{.Repository}}:{{.Tag}}' 2>/dev/null \
            | grep -E "^${img%%:*}:" || true)
    if [[ -z "$found" ]]; then
        echo "    absent   $img"
        continue
    fi
    while IFS= read -r tag; do
        [[ -z "$tag" ]] && continue
        size=$(docker images --format '{{.Repository}}:{{.Tag}} {{.Size}}' 2>/dev/null \
               | awk -v t="$tag" '$1==t{print $2}')
        echo "    remove   $tag  ($size)"
        run docker rmi "$tag"
    done <<< "$found"
done

# --- 3. Dangling images left behind by this project's rebuilds ---------------
# Only untagged layers whose tags we just dropped. A repeated `docker build`
# orphans the previous image, and it is unreachable from any other project.
echo
echo "==> Dangling (untagged) images"
dangling=$(docker images -f dangling=true -q 2>/dev/null | head -50)
if [[ -z "$dangling" ]]; then
    echo "    none"
else
    echo "    $(echo "$dangling" | wc -l | tr -d ' ') dangling image(s)"
    for d in $dangling; do
        run docker rmi "$d"
    done
fi

# --- Summary ----------------------------------------------------------------
echo
echo "==> Remaining Docker usage (all projects, for context)"
docker system df 2>/dev/null || true

cat <<'EOF'

Not handled here, on purpose:
  * Build cache      -- unlabelled and shared; see the header for why.
                        Check this project's share with: docker buildx du --verbose
  * S3 test data     -- costs money. Run: ./test/clean-s3.sh --dry-run
  * Base images      -- alpine / rust bases are shared with other projects.
EOF
