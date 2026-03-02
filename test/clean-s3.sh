#!/bin/bash
# =============================================================================
# clean-s3.sh — Delete all test data from S3 to avoid costs
# =============================================================================
# Removes all objects under the test prefixes in the S3 bucket.
# Run this after integration tests to avoid accumulating S3 storage costs.
#
# Two prefixes are cleaned:
#   1. chbackup-test/     — backup manifests and compressed parts
#   2. clickhouse-disks/  — ClickHouse S3 disk objects (table data)
#
# Usage:
#   # Set credentials (or use .env / AWS profile)
#   export S3_BUCKET=chbackup-test
#   export S3_REGION=eu-west-1
#   export AWS_ACCESS_KEY_ID=xxx   # or S3_ACCESS_KEY
#   export AWS_SECRET_ACCESS_KEY=xxx  # or S3_SECRET_KEY
#
#   ./test/clean-s3.sh
#   ./test/clean-s3.sh --dry-run    # list objects without deleting
# =============================================================================

set -euo pipefail

# --- Configuration ---
S3_BUCKET="${S3_BUCKET:-}"
S3_REGION="${S3_REGION:-eu-west-1}"
AWS_ACCESS_KEY_ID="${AWS_ACCESS_KEY_ID:-${S3_ACCESS_KEY:-}}"
AWS_SECRET_ACCESS_KEY="${AWS_SECRET_ACCESS_KEY:-${S3_SECRET_KEY:-}}"
DRY_RUN=false

if [[ "${1:-}" == "--dry-run" ]]; then
    DRY_RUN=true
fi

if [[ -z "$S3_BUCKET" ]]; then
    echo "ERROR: S3_BUCKET is required"
    echo "Usage: S3_BUCKET=your-bucket ./test/clean-s3.sh"
    exit 1
fi

if [[ -z "$AWS_ACCESS_KEY_ID" || -z "$AWS_SECRET_ACCESS_KEY" ]]; then
    echo "ERROR: AWS credentials required (AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY or S3_ACCESS_KEY/S3_SECRET_KEY)"
    exit 1
fi

export AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY AWS_DEFAULT_REGION="${S3_REGION}"

# Check for aws CLI
if ! command -v aws &>/dev/null; then
    echo "ERROR: aws CLI is required. Install with: brew install awscli"
    exit 1
fi

# --- Prefixes to clean ---
PREFIXES=(
    "chbackup-test/"     # backup data (manifests, compressed parts)
    "clickhouse-disks/"  # ClickHouse S3 disk objects (table data)
)

total_deleted=0

for prefix in "${PREFIXES[@]}"; do
    echo "--- Scanning s3://${S3_BUCKET}/${prefix} ---"

    # Count objects using aws s3 ls (handles pagination correctly)
    summary=$(aws s3 ls "s3://${S3_BUCKET}/${prefix}" --recursive --summarize 2>/dev/null || true)
    count=$(echo "$summary" | grep "Total Objects:" | awk '{print $3}' || echo "0")
    size=$(echo "$summary" | grep "Total Size:" | awk '{print $3}' || echo "0")
    count=${count:-0}
    size=${size:-0}

    if [[ "$count" -eq 0 ]]; then
        echo "  No objects found"
        continue
    fi

    size_mb=$(echo "scale=2; ${size} / 1048576" | bc 2>/dev/null || echo "?")
    echo "  Found ${count} objects (${size_mb} MB)"

    if [[ "$DRY_RUN" == "true" ]]; then
        echo "  [DRY RUN] Would delete ${count} objects"
        aws s3 ls "s3://${S3_BUCKET}/${prefix}" --recursive 2>/dev/null | head -10 | awk '{print "    " $NF}'
        if [[ "$count" -gt 10 ]]; then
            echo "    ... (and $((count - 10)) more)"
        fi
    else
        echo "  Deleting all objects under ${prefix}..."
        aws s3 rm "s3://${S3_BUCKET}/${prefix}" --recursive --quiet
        echo "  Deleted ${count} objects (${size_mb} MB)"
        total_deleted=$((total_deleted + count))
    fi
done

if [[ "$DRY_RUN" == "true" ]]; then
    echo ""
    echo "=== DRY RUN complete. Use without --dry-run to delete. ==="
else
    echo ""
    echo "=== Cleanup complete. Deleted ${total_deleted} total objects. ==="
fi
