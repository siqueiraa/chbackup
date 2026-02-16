#!/usr/bin/env bash
# =============================================================================
# run_tests.sh — Integration test runner for chbackup
# =============================================================================
# Runs inside the Docker container. Waits for ClickHouse, sets up fixtures,
# and executes smoke tests.
#
# Usage:
#   /test/run_tests.sh [--filter PATTERN]
#
# Environment:
#   S3_BUCKET      (required) S3 bucket name
#   S3_ACCESS_KEY  (required) AWS access key
#   S3_SECRET_KEY  (required) AWS secret key
#   TEST_FILTER    (optional) Run only tests matching this pattern
# =============================================================================
set -euo pipefail

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

pass() { echo -e "${GREEN}PASS${NC} $1"; }
fail() { echo -e "${RED}FAIL${NC} $1"; FAILURES=$((FAILURES + 1)); }
skip() { echo -e "${YELLOW}SKIP${NC} $1"; }
info() { echo -e "---- $1"; }

FAILURES=0
FILTER="${TEST_FILTER:-}"

# Parse --filter flag
while [[ $# -gt 0 ]]; do
    case "$1" in
        --filter) FILTER="$2"; shift 2 ;;
        *) echo "Unknown arg: $1"; exit 1 ;;
    esac
done

should_run() {
    [[ -z "$FILTER" ]] || [[ "$1" == *"$FILTER"* ]]
}

# ---------------------------------------------------------------------------
# Validate required env vars
# ---------------------------------------------------------------------------
info "Validating environment"
missing=0
for var in S3_BUCKET S3_ACCESS_KEY S3_SECRET_KEY; do
    if [[ -z "${!var:-}" ]]; then
        echo "ERROR: $var is not set"
        missing=1
    fi
done
if [[ $missing -eq 1 ]]; then
    echo "Set required S3 environment variables and try again."
    exit 1
fi
pass "Environment variables"

# ---------------------------------------------------------------------------
# Wait for ClickHouse to be ready
# ---------------------------------------------------------------------------
info "Waiting for ClickHouse to be ready"
max_wait=60
elapsed=0
until clickhouse-client -q "SELECT 1" >/dev/null 2>&1; do
    if [[ $elapsed -ge $max_wait ]]; then
        fail "ClickHouse did not start within ${max_wait}s"
        exit 1
    fi
    sleep 1
    elapsed=$((elapsed + 1))
done
pass "ClickHouse ready (${elapsed}s)"

# ---------------------------------------------------------------------------
# Print ClickHouse version
# ---------------------------------------------------------------------------
CH_VERSION=$(clickhouse-client -q "SELECT version()")
info "ClickHouse version: ${CH_VERSION}"

# ---------------------------------------------------------------------------
# Run setup fixtures
# ---------------------------------------------------------------------------
if should_run "setup"; then
    info "Running setup fixtures"
    clickhouse-client --multiquery < /test/fixtures/setup.sql
    pass "Setup fixtures loaded"

    # Verify tables were created
    TABLE_COUNT=$(clickhouse-client -q "SELECT count() FROM system.tables WHERE database = 'default' AND name IN ('trades', 'users', 'events')")
    if [[ "$TABLE_COUNT" -eq 3 ]]; then
        pass "All 3 test tables created"
    else
        fail "Expected 3 tables, got ${TABLE_COUNT}"
    fi
fi

# ---------------------------------------------------------------------------
# Smoke test: chbackup binary
# ---------------------------------------------------------------------------
if should_run "smoke_binary"; then
    info "Smoke test: chbackup --help"
    if chbackup --help >/dev/null 2>&1; then
        pass "chbackup --help"
    else
        fail "chbackup --help"
    fi
fi

# ---------------------------------------------------------------------------
# Smoke test: chbackup print-config
# ---------------------------------------------------------------------------
if should_run "smoke_config"; then
    info "Smoke test: chbackup print-config"
    if chbackup print-config >/dev/null 2>&1; then
        pass "chbackup print-config"
    else
        fail "chbackup print-config"
    fi
fi

# ---------------------------------------------------------------------------
# Smoke test: chbackup list (requires CH + S3 connectivity)
# ---------------------------------------------------------------------------
if should_run "smoke_list"; then
    info "Smoke test: chbackup list"
    if chbackup list 2>&1; then
        pass "chbackup list"
    else
        fail "chbackup list"
    fi
fi

# ---------------------------------------------------------------------------
# Results
# ---------------------------------------------------------------------------
echo ""
echo "========================================"
if [[ $FAILURES -eq 0 ]]; then
    echo -e "${GREEN}All tests passed${NC}"
    exit 0
else
    echo -e "${RED}${FAILURES} test(s) failed${NC}"
    exit 1
fi
