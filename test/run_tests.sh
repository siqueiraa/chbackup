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
# Load seed data for round-trip verification
# ---------------------------------------------------------------------------
if should_run "seed_data"; then
    info "Loading seed data"
    clickhouse-client --multiquery < /test/fixtures/seed_data.sql
    pass "Seed data loaded"

    # Capture row counts for post-restore verification
    TRADES_COUNT=$(clickhouse-client -q "SELECT count() FROM default.trades")
    USERS_COUNT=$(clickhouse-client -q "SELECT count() FROM default.users")
    EVENTS_COUNT=$(clickhouse-client -q "SELECT count() FROM default.events")
    info "Row counts: trades=${TRADES_COUNT}, users=${USERS_COUNT}, events=${EVENTS_COUNT}"
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
# Round-trip test: create -> upload -> delete local -> download -> restore
# ---------------------------------------------------------------------------
if should_run "test_round_trip"; then
    info "Round-trip test: create -> upload -> delete local -> download -> restore"
    BACKUP_NAME="roundtrip_test_$$"

    # Capture pre-backup row counts
    PRE_TRADES=$(clickhouse-client -q "SELECT count() FROM default.trades")
    PRE_USERS=$(clickhouse-client -q "SELECT count() FROM default.users")
    PRE_EVENTS=$(clickhouse-client -q "SELECT count() FROM default.events")
    info "Pre-backup counts: trades=${PRE_TRADES}, users=${PRE_USERS}, events=${PRE_EVENTS}"

    # Step 1: Create backup
    info "  Step 1: chbackup create ${BACKUP_NAME}"
    if chbackup create "${BACKUP_NAME}" 2>&1; then
        pass "create ${BACKUP_NAME}"
    else
        fail "create ${BACKUP_NAME}"
    fi

    # Step 2: Upload to S3
    info "  Step 2: chbackup upload ${BACKUP_NAME}"
    if chbackup upload "${BACKUP_NAME}" 2>&1; then
        pass "upload ${BACKUP_NAME}"
    else
        fail "upload ${BACKUP_NAME}"
    fi

    # Step 3: Delete local backup
    info "  Step 3: chbackup delete local ${BACKUP_NAME}"
    if chbackup delete local "${BACKUP_NAME}" 2>&1; then
        pass "delete local ${BACKUP_NAME}"
    else
        fail "delete local ${BACKUP_NAME}"
    fi

    # Step 4: Download from S3
    info "  Step 4: chbackup download ${BACKUP_NAME}"
    if chbackup download "${BACKUP_NAME}" 2>&1; then
        pass "download ${BACKUP_NAME}"
    else
        fail "download ${BACKUP_NAME}"
    fi

    # Step 5: DROP tables and restore
    info "  Step 5: DROP tables and restore"
    clickhouse-client -q "DROP TABLE IF EXISTS default.trades SYNC"
    clickhouse-client -q "DROP TABLE IF EXISTS default.users SYNC"
    clickhouse-client -q "DROP TABLE IF EXISTS default.events SYNC"

    if chbackup restore "${BACKUP_NAME}" 2>&1; then
        pass "restore ${BACKUP_NAME}"
    else
        fail "restore ${BACKUP_NAME}"
    fi

    # Step 6: Verify row counts match pre-backup state
    info "  Step 6: Verify row counts"
    POST_TRADES=$(clickhouse-client -q "SELECT count() FROM default.trades")
    POST_USERS=$(clickhouse-client -q "SELECT count() FROM default.users")
    POST_EVENTS=$(clickhouse-client -q "SELECT count() FROM default.events")

    if [[ "$POST_TRADES" -eq "$PRE_TRADES" ]]; then
        pass "trades row count matches: ${POST_TRADES}"
    else
        fail "trades row count mismatch: expected=${PRE_TRADES} got=${POST_TRADES}"
    fi

    if [[ "$POST_USERS" -eq "$PRE_USERS" ]]; then
        pass "users row count matches: ${POST_USERS}"
    else
        fail "users row count mismatch: expected=${PRE_USERS} got=${POST_USERS}"
    fi

    if [[ "$POST_EVENTS" -eq "$PRE_EVENTS" ]]; then
        pass "events row count matches: ${POST_EVENTS}"
    else
        fail "events row count mismatch: expected=${PRE_EVENTS} got=${POST_EVENTS}"
    fi

    # Cleanup: delete remote backup
    info "  Cleanup: delete remote ${BACKUP_NAME}"
    chbackup delete remote "${BACKUP_NAME}" 2>&1 || true
    chbackup delete local "${BACKUP_NAME}" 2>&1 || true
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
