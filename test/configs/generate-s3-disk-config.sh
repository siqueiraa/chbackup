#!/bin/bash
# =============================================================================
# generate-s3-disk-config.sh — Generate ClickHouse S3 disk storage config
# =============================================================================
# Runs before ClickHouse starts to template S3 credentials into the storage
# configuration XML. Placed in /docker-entrypoint-initdb.d/ but generates
# a config.d file (initdb scripts run after config is loaded, but we write
# the file early via the Dockerfile CMD override).
# =============================================================================

set -euo pipefail

S3_BUCKET="${S3_BUCKET:-}"
S3_REGION="${S3_REGION:-eu-west-1}"
S3_ACCESS_KEY="${S3_ACCESS_KEY:-}"
S3_SECRET_KEY="${S3_SECRET_KEY:-}"

if [[ -z "$S3_BUCKET" || -z "$S3_ACCESS_KEY" || -z "$S3_SECRET_KEY" ]]; then
    echo "WARN: S3 credentials not set, skipping S3 disk config generation"
    exit 0
fi

S3_ENDPOINT="https://s3.${S3_REGION}.amazonaws.com/${S3_BUCKET}/clickhouse-disks/"

cat > /etc/clickhouse-server/config.d/s3-storage.xml <<EOF
<clickhouse>
    <storage_configuration>
        <disks>
            <s3disk>
                <type>s3</type>
                <endpoint>${S3_ENDPOINT}</endpoint>
                <access_key_id>${S3_ACCESS_KEY}</access_key_id>
                <secret_access_key>${S3_SECRET_KEY}</secret_access_key>
                <region>${S3_REGION}</region>
            </s3disk>
            <store0>
                <path>/var/lib/clickhouse/store0/</path>
            </store0>
            <store1>
                <path>/var/lib/clickhouse/store1/</path>
            </store1>
            <store2>
                <path>/var/lib/clickhouse/store2/</path>
            </store2>
        </disks>
        <policies>
            <s3_policy>
                <volumes>
                    <s3_volume>
                        <disk>s3disk</disk>
                    </s3_volume>
                </volumes>
            </s3_policy>
            <jbod_policy>
                <volumes>
                    <jbod_volume>
                        <disk>store0</disk>
                        <disk>store1</disk>
                        <disk>store2</disk>
                    </jbod_volume>
                </volumes>
            </jbod_policy>
        </policies>
    </storage_configuration>
</clickhouse>
EOF

echo "Generated S3 disk config: endpoint=${S3_ENDPOINT}"
echo "Generated JBOD policy: store0, store1, store2"
