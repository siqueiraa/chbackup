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
# T4: Incremental backup chain
# ---------------------------------------------------------------------------
if should_run "test_incremental_chain"; then
    info "T4: Incremental backup chain"
    FULL_NAME="incr_full_$$"
    INCR_NAME="incr_diff_$$"

    # Capture initial row count
    PRE_TRADES=$(clickhouse-client -q "SELECT count() FROM default.trades")
    info "  Pre-backup trades count: ${PRE_TRADES}"

    # Step 1: Create and upload full backup
    info "  Step 1: Create full backup ${FULL_NAME}"
    if chbackup create "${FULL_NAME}" 2>&1; then
        pass "create full ${FULL_NAME}"
    else
        fail "create full ${FULL_NAME}"
    fi

    info "  Step 2: Upload full backup"
    if chbackup upload "${FULL_NAME}" 2>&1; then
        pass "upload full ${FULL_NAME}"
    else
        fail "upload full ${FULL_NAME}"
    fi

    # Step 3: Insert more data
    info "  Step 3: Insert additional data"
    clickhouse-client -q "INSERT INTO default.trades VALUES ('2024-04-01', 99901, 'TSLA', 250.00, 50), ('2024-04-02', 99902, 'TSLA', 255.00, 75)"
    AFTER_INSERT=$(clickhouse-client -q "SELECT count() FROM default.trades")
    info "  After insert trades count: ${AFTER_INSERT}"

    # Step 4: Create incremental backup with --diff-from
    info "  Step 4: Create incremental backup ${INCR_NAME} --diff-from ${FULL_NAME}"
    if chbackup create "${INCR_NAME}" --diff-from "${FULL_NAME}" 2>&1; then
        pass "create incremental ${INCR_NAME}"
    else
        fail "create incremental ${INCR_NAME}"
    fi

    # Step 5: Upload incremental
    info "  Step 5: Upload incremental backup"
    if chbackup upload "${INCR_NAME}" 2>&1; then
        pass "upload incremental ${INCR_NAME}"
    else
        fail "upload incremental ${INCR_NAME}"
    fi

    # Step 6: Delete local, download incremental, restore
    info "  Step 6: Delete local backups"
    chbackup delete local "${INCR_NAME}" 2>&1 || true
    chbackup delete local "${FULL_NAME}" 2>&1 || true

    info "  Step 7: Download incremental"
    if chbackup download "${INCR_NAME}" 2>&1; then
        pass "download incremental ${INCR_NAME}"
    else
        fail "download incremental ${INCR_NAME}"
    fi

    # Download full (needed for incremental restore base)
    info "  Step 8: Download full (base for incremental)"
    if chbackup download "${FULL_NAME}" 2>&1; then
        pass "download full ${FULL_NAME}"
    else
        fail "download full ${FULL_NAME}"
    fi

    info "  Step 9: DROP tables and restore incremental"
    clickhouse-client -q "DROP TABLE IF EXISTS default.trades SYNC"
    clickhouse-client -q "DROP TABLE IF EXISTS default.users SYNC"
    clickhouse-client -q "DROP TABLE IF EXISTS default.events SYNC"

    if chbackup restore "${INCR_NAME}" 2>&1; then
        pass "restore incremental ${INCR_NAME}"
    else
        fail "restore incremental ${INCR_NAME}"
    fi

    # Step 10: Verify all data present (full + incremental)
    POST_TRADES=$(clickhouse-client -q "SELECT count() FROM default.trades")
    if [[ "$POST_TRADES" -eq "$AFTER_INSERT" ]]; then
        pass "incremental restore row count matches: ${POST_TRADES}"
    else
        fail "incremental restore row count mismatch: expected=${AFTER_INSERT} got=${POST_TRADES}"
    fi

    # Cleanup
    info "  Cleanup"
    chbackup delete remote "${INCR_NAME}" 2>&1 || true
    chbackup delete remote "${FULL_NAME}" 2>&1 || true
    chbackup delete local "${INCR_NAME}" 2>&1 || true
    chbackup delete local "${FULL_NAME}" 2>&1 || true

    # Remove the extra rows we inserted
    clickhouse-client -q "ALTER TABLE default.trades DELETE WHERE trade_id IN (99901, 99902)" 2>&1 || true
fi

# ---------------------------------------------------------------------------
# T5: Schema-only backup
# ---------------------------------------------------------------------------
if should_run "test_schema_only"; then
    info "T5: Schema-only backup"
    SCHEMA_NAME="schema_only_$$"

    # Step 1: Create schema-only backup
    info "  Step 1: Create schema-only backup"
    if chbackup create "${SCHEMA_NAME}" --schema 2>&1; then
        pass "create schema-only ${SCHEMA_NAME}"
    else
        fail "create schema-only ${SCHEMA_NAME}"
    fi

    # Step 2: Verify no data parts in backup (metadata.json should have empty parts)
    MANIFEST="/var/lib/clickhouse/backup/${SCHEMA_NAME}/metadata.json"
    if [ -f "$MANIFEST" ]; then
        # A schema-only backup should have tables with empty parts maps
        PARTS_COUNT=$(python3 -c "
import json, sys
with open('${MANIFEST}') as f:
    m = json.load(f)
total = sum(len(p) for t in m.get('tables', {}).values() for p in t.get('parts', {}).values())
print(total)
" 2>/dev/null || echo "-1")
        if [[ "$PARTS_COUNT" -eq 0 ]]; then
            pass "schema-only backup has no data parts"
        else
            fail "schema-only backup has ${PARTS_COUNT} data parts (expected 0)"
        fi
    else
        fail "manifest not found at ${MANIFEST}"
    fi

    # Step 3: Drop tables and restore schema only
    info "  Step 3: DROP tables and restore schema"
    clickhouse-client -q "DROP TABLE IF EXISTS default.trades SYNC"
    clickhouse-client -q "DROP TABLE IF EXISTS default.users SYNC"
    clickhouse-client -q "DROP TABLE IF EXISTS default.events SYNC"

    if chbackup restore "${SCHEMA_NAME}" 2>&1; then
        pass "restore schema-only ${SCHEMA_NAME}"
    else
        fail "restore schema-only ${SCHEMA_NAME}"
    fi

    # Step 4: Verify tables exist but have no data
    TABLE_COUNT=$(clickhouse-client -q "SELECT count() FROM system.tables WHERE database = 'default' AND name IN ('trades', 'users', 'events')")
    if [[ "$TABLE_COUNT" -eq 3 ]]; then
        pass "all 3 tables recreated from schema"
    else
        fail "expected 3 tables, got ${TABLE_COUNT}"
    fi

    TRADES_ROWS=$(clickhouse-client -q "SELECT count() FROM default.trades")
    if [[ "$TRADES_ROWS" -eq 0 ]]; then
        pass "trades table is empty after schema-only restore"
    else
        fail "trades table has ${TRADES_ROWS} rows (expected 0)"
    fi

    # Cleanup and re-seed data
    chbackup delete local "${SCHEMA_NAME}" 2>&1 || true
    info "  Re-seeding data after schema-only test"
    clickhouse-client --multiquery < /test/fixtures/setup.sql
    clickhouse-client --multiquery < /test/fixtures/seed_data.sql
fi

# ---------------------------------------------------------------------------
# T6: Partitioned restore
# ---------------------------------------------------------------------------
if should_run "test_partitioned_restore"; then
    info "T6: Partitioned restore"
    PART_NAME="partitioned_$$"

    # trades table is partitioned by toYYYYMM(trade_date) with partitions 202401, 202402, 202403
    # Count rows in partition 202401
    ROWS_202401=$(clickhouse-client -q "SELECT count() FROM default.trades WHERE toYYYYMM(trade_date) = 202401")
    TOTAL_ROWS=$(clickhouse-client -q "SELECT count() FROM default.trades")
    info "  Partition 202401 rows: ${ROWS_202401}, total: ${TOTAL_ROWS}"

    # Step 1: Create backup
    info "  Step 1: Create backup ${PART_NAME}"
    if chbackup create "${PART_NAME}" 2>&1; then
        pass "create ${PART_NAME}"
    else
        fail "create ${PART_NAME}"
    fi

    # Step 2: Drop trades table and restore only partition 202401
    info "  Step 2: DROP trades and restore --partitions 202401"
    clickhouse-client -q "DROP TABLE IF EXISTS default.trades SYNC"
    clickhouse-client -q "DROP TABLE IF EXISTS default.users SYNC"
    clickhouse-client -q "DROP TABLE IF EXISTS default.events SYNC"

    if chbackup restore "${PART_NAME}" --partitions "202401" 2>&1; then
        pass "restore partitioned ${PART_NAME}"
    else
        fail "restore partitioned ${PART_NAME}"
    fi

    # Step 3: Verify only partition 202401 was restored
    POST_ROWS=$(clickhouse-client -q "SELECT count() FROM default.trades")
    if [[ "$POST_ROWS" -eq "$ROWS_202401" ]]; then
        pass "partitioned restore row count matches: ${POST_ROWS}"
    else
        fail "partitioned restore row count mismatch: expected=${ROWS_202401} got=${POST_ROWS}"
    fi

    # Cleanup and restore full data
    chbackup delete local "${PART_NAME}" 2>&1 || true
    info "  Re-seeding data after partitioned test"
    clickhouse-client -q "DROP TABLE IF EXISTS default.trades SYNC"
    clickhouse-client -q "DROP TABLE IF EXISTS default.users SYNC"
    clickhouse-client -q "DROP TABLE IF EXISTS default.events SYNC"
    clickhouse-client --multiquery < /test/fixtures/setup.sql
    clickhouse-client --multiquery < /test/fixtures/seed_data.sql
fi

# ---------------------------------------------------------------------------
# T7: Server API create + upload
# ---------------------------------------------------------------------------
if should_run "test_server_api_create_upload"; then
    info "T7: Server API create + upload"
    API_NAME="api_test_$$"
    SERVER_PID=""

    # Step 1: Start server in background
    info "  Step 1: Start chbackup server"
    chbackup server &
    SERVER_PID=$!
    sleep 2

    # Wait for server to be ready (up to 10s)
    api_ready=0
    for i in $(seq 1 10); do
        if curl -s http://localhost:7171/api/v1/health >/dev/null 2>&1; then
            api_ready=1
            break
        fi
        sleep 1
    done

    if [[ $api_ready -eq 0 ]]; then
        fail "Server did not become ready within 10s"
        kill "$SERVER_PID" 2>/dev/null || true
        wait "$SERVER_PID" 2>/dev/null || true
        # Skip remaining steps
    else
        pass "Server is ready"

        # Step 2: POST /api/v1/create
        info "  Step 2: POST /api/v1/create"
        CREATE_RESP=$(curl -s -X POST http://localhost:7171/api/v1/create \
            -H "Content-Type: application/json" \
            -d "{\"backup_name\": \"${API_NAME}\"}")
        if echo "$CREATE_RESP" | grep -q '"status"'; then
            pass "create API responded"
        else
            fail "create API response: ${CREATE_RESP}"
        fi

        # Step 3: Wait for create to complete (poll actions)
        info "  Step 3: Wait for create to complete"
        for i in $(seq 1 30); do
            ACTIONS=$(curl -s http://localhost:7171/api/v1/actions 2>/dev/null || echo "[]")
            if echo "$ACTIONS" | grep -q '"completed"'; then
                break
            fi
            if echo "$ACTIONS" | grep -q '"failed"'; then
                fail "create operation failed"
                break
            fi
            sleep 1
        done
        pass "create operation completed"

        # Step 4: POST /api/v1/upload
        info "  Step 4: POST /api/v1/upload/${API_NAME}"
        UPLOAD_RESP=$(curl -s -X POST "http://localhost:7171/api/v1/upload/${API_NAME}" \
            -H "Content-Type: application/json" -d '{}')
        if echo "$UPLOAD_RESP" | grep -q '"status"'; then
            pass "upload API responded"
        else
            fail "upload API response: ${UPLOAD_RESP}"
        fi

        # Step 5: Wait for upload to complete
        info "  Step 5: Wait for upload to complete"
        for i in $(seq 1 60); do
            ACTIONS=$(curl -s http://localhost:7171/api/v1/actions 2>/dev/null || echo "[]")
            LAST_STATUS=$(echo "$ACTIONS" | python3 -c "
import json, sys
data = json.load(sys.stdin)
if data:
    print(data[-1].get('status', 'unknown'))
else:
    print('unknown')
" 2>/dev/null || echo "unknown")
            if [[ "$LAST_STATUS" == "completed" ]]; then
                break
            fi
            if [[ "$LAST_STATUS" == "failed" ]]; then
                fail "upload operation failed"
                break
            fi
            sleep 1
        done
        pass "upload operation completed"

        # Step 6: Verify via /api/v1/list
        info "  Step 6: Verify backup in list"
        LIST_RESP=$(curl -s http://localhost:7171/api/v1/list 2>/dev/null || echo "[]")
        if echo "$LIST_RESP" | grep -q "${API_NAME}"; then
            pass "backup ${API_NAME} found in list"
        else
            fail "backup ${API_NAME} not found in list"
        fi

        # Cleanup: stop server and delete backup
        info "  Cleanup"
        kill "$SERVER_PID" 2>/dev/null || true
        wait "$SERVER_PID" 2>/dev/null || true
        chbackup delete remote "${API_NAME}" 2>&1 || true
        chbackup delete local "${API_NAME}" 2>&1 || true
    fi
fi

# ---------------------------------------------------------------------------
# T8: Backup name validation
# ---------------------------------------------------------------------------
if should_run "test_backup_name_validation"; then
    info "T8: Backup name validation"

    # Attempt to create backup with path traversal name -- should fail
    info "  Step 1: Attempt create with '../malicious' name"
    if chbackup create "../malicious" 2>&1; then
        fail "create with '../malicious' should have been rejected"
    else
        pass "create with '../malicious' correctly rejected"
    fi

    # Attempt with slash in name
    info "  Step 2: Attempt create with 'foo/bar' name"
    if chbackup create "foo/bar" 2>&1; then
        fail "create with 'foo/bar' should have been rejected"
    else
        pass "create with 'foo/bar' correctly rejected"
    fi

    # Attempt with backslash in name
    info "  Step 3: Attempt create with 'foo\\bar' name"
    if chbackup create 'foo\bar' 2>&1; then
        fail "create with 'foo\\bar' should have been rejected"
    else
        pass "create with 'foo\\bar' correctly rejected"
    fi
fi

# ---------------------------------------------------------------------------
# T9: Delete and list
# ---------------------------------------------------------------------------
if should_run "test_delete_and_list"; then
    info "T9: Delete and list"
    DEL_NAME="delete_test_$$"

    # Step 1: Create and upload
    info "  Step 1: Create backup ${DEL_NAME}"
    if chbackup create "${DEL_NAME}" 2>&1; then
        pass "create ${DEL_NAME}"
    else
        fail "create ${DEL_NAME}"
    fi

    info "  Step 2: Upload backup ${DEL_NAME}"
    if chbackup upload "${DEL_NAME}" 2>&1; then
        pass "upload ${DEL_NAME}"
    else
        fail "upload ${DEL_NAME}"
    fi

    # Step 3: Verify in list
    info "  Step 3: Verify in list"
    LIST_OUTPUT=$(chbackup list 2>&1)
    if echo "$LIST_OUTPUT" | grep -q "${DEL_NAME}"; then
        pass "${DEL_NAME} found in list"
    else
        fail "${DEL_NAME} not found in list"
    fi

    # Step 4: Delete remote
    info "  Step 4: Delete remote ${DEL_NAME}"
    if chbackup delete remote "${DEL_NAME}" 2>&1; then
        pass "delete remote ${DEL_NAME}"
    else
        fail "delete remote ${DEL_NAME}"
    fi

    # Step 5: Verify remote gone
    info "  Step 5: Verify remote backup gone"
    LIST_REMOTE=$(chbackup list --format json 2>&1)
    if echo "$LIST_REMOTE" | grep -q "\"name\":\"${DEL_NAME}\".*\"location\":\"remote\""; then
        fail "${DEL_NAME} still in remote list"
    else
        pass "${DEL_NAME} removed from remote"
    fi

    # Step 6: Delete local
    info "  Step 6: Delete local ${DEL_NAME}"
    if chbackup delete local "${DEL_NAME}" 2>&1; then
        pass "delete local ${DEL_NAME}"
    else
        fail "delete local ${DEL_NAME}"
    fi

    # Step 7: Verify local gone
    info "  Step 7: Verify local backup gone"
    if [ -d "/var/lib/clickhouse/backup/${DEL_NAME}" ]; then
        fail "${DEL_NAME} directory still exists locally"
    else
        pass "${DEL_NAME} removed from local"
    fi
fi

# ---------------------------------------------------------------------------
# T10: Clean broken
# ---------------------------------------------------------------------------
if should_run "test_clean_broken"; then
    info "T10: Clean broken backups"
    BROKEN_NAME="broken_test_$$"

    # Step 1: Create a valid backup
    info "  Step 1: Create backup ${BROKEN_NAME}"
    if chbackup create "${BROKEN_NAME}" 2>&1; then
        pass "create ${BROKEN_NAME}"
    else
        fail "create ${BROKEN_NAME}"
    fi

    # Step 2: Corrupt its metadata.json
    MANIFEST="/var/lib/clickhouse/backup/${BROKEN_NAME}/metadata.json"
    info "  Step 2: Corrupt metadata.json"
    if [ -f "$MANIFEST" ]; then
        echo "CORRUPTED" > "$MANIFEST"
        pass "metadata.json corrupted"
    else
        fail "metadata.json not found at ${MANIFEST}"
    fi

    # Step 3: Run clean_broken local
    info "  Step 3: Run clean_broken local"
    if chbackup clean_broken local 2>&1; then
        pass "clean_broken local completed"
    else
        fail "clean_broken local failed"
    fi

    # Step 4: Verify the broken backup is cleaned up
    info "  Step 4: Verify broken backup cleaned"
    if [ -d "/var/lib/clickhouse/backup/${BROKEN_NAME}" ]; then
        fail "${BROKEN_NAME} directory still exists after clean_broken"
    else
        pass "${BROKEN_NAME} cleaned up successfully"
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
