-- =============================================================================
-- seed_data.sql -- Deterministic INSERT statements for checksum validation
-- =============================================================================
-- Generates reproducible data using numbers() with modular arithmetic.
-- Every run produces identical rows, enabling SELECT count() / sum(cityHash64(*))
-- checksum comparisons before and after backup/restore.
--
-- Tables populated (must exist from setup.sql):
--   default.trades  -- MergeTree, date-partitioned
--   default.users   -- ReplacingMergeTree
--   default.events  -- ReplicatedMergeTree, date-partitioned
-- =============================================================================

-- default.trades -- 10000 deterministic rows across Jan-Mar 2024
INSERT INTO default.trades
SELECT
    toDate('2024-01-01') + toUInt32(number % 90) AS trade_date,
    number + 100 AS trade_id,
    arrayElement(['AAPL', 'GOOG', 'MSFT', 'AMZN'], toUInt8(number % 4) + 1) AS symbol,
    toFloat64(100 + (number % 500)) + toFloat64(number % 100) / 100.0 AS price,
    toUInt32((number % 1000) + 1) AS quantity
FROM numbers(10000);

-- default.users -- 1000 deterministic rows
INSERT INTO default.users
SELECT
    number + 100 AS user_id,
    concat('user_', toString(number)) AS username,
    concat('user', toString(number), '@test.com') AS email,
    toDateTime('2024-01-01 00:00:00') + toUInt32(number * 3600) AS updated_at
FROM numbers(1000);

-- default.events -- 5000 deterministic rows across Jan-Feb 2024
INSERT INTO default.events
SELECT
    toDate('2024-01-01') + toUInt32(number % 60) AS event_date,
    number + 100 AS event_id,
    arrayElement(['click', 'view', 'purchase', 'signup'], toUInt8(number % 4) + 1) AS event_type,
    concat('{"page":"/', toString(number % 100), '"}') AS payload
FROM numbers(5000);

-- default.s3_orders -- 3000 deterministic rows on S3 disk, across Jan-Mar 2024
INSERT INTO default.s3_orders
SELECT
    toDate('2024-01-01') + toUInt32(number % 90) AS order_date,
    number + 100 AS order_id,
    arrayElement(['alice', 'bob', 'carol', 'dave'], toUInt8(number % 4) + 1) AS customer,
    toFloat64(10 + (number % 500)) + toFloat64(number % 100) / 100.0 AS amount,
    arrayElement(['completed', 'pending', 'cancelled'], toUInt8(number % 3) + 1) AS status
FROM numbers(3000);

-- default.jbod_orders -- 5000 deterministic rows across Jan-Mar 2024 on JBOD disks
-- Multiple small inserts to force part spread via least_used balancing
INSERT INTO default.jbod_orders
SELECT
    toDate('2024-01-01') + toUInt32(number % 90) AS order_date,
    number + 100 AS order_id,
    arrayElement(['alice', 'bob', 'carol', 'dave', 'eve'], toUInt8(number % 5) + 1) AS customer,
    toFloat64(10 + (number % 1000)) + toFloat64(number % 100) / 100.0 AS amount,
    arrayElement(['us-east', 'us-west', 'eu-west', 'ap-south'], toUInt8(number % 4) + 1) AS region
FROM numbers(1000);
INSERT INTO default.jbod_orders
SELECT
    toDate('2024-01-01') + toUInt32(number % 90) AS order_date,
    number + 1100 AS order_id,
    arrayElement(['alice', 'bob', 'carol', 'dave', 'eve'], toUInt8(number % 5) + 1) AS customer,
    toFloat64(10 + (number % 1000)) + toFloat64(number % 100) / 100.0 AS amount,
    arrayElement(['us-east', 'us-west', 'eu-west', 'ap-south'], toUInt8(number % 4) + 1) AS region
FROM numbers(1000);
INSERT INTO default.jbod_orders
SELECT
    toDate('2024-01-01') + toUInt32(number % 90) AS order_date,
    number + 2100 AS order_id,
    arrayElement(['alice', 'bob', 'carol', 'dave', 'eve'], toUInt8(number % 5) + 1) AS customer,
    toFloat64(10 + (number % 1000)) + toFloat64(number % 100) / 100.0 AS amount,
    arrayElement(['us-east', 'us-west', 'eu-west', 'ap-south'], toUInt8(number % 4) + 1) AS region
FROM numbers(1000);
INSERT INTO default.jbod_orders
SELECT
    toDate('2024-01-01') + toUInt32(number % 90) AS order_date,
    number + 3100 AS order_id,
    arrayElement(['alice', 'bob', 'carol', 'dave', 'eve'], toUInt8(number % 5) + 1) AS customer,
    toFloat64(10 + (number % 1000)) + toFloat64(number % 100) / 100.0 AS amount,
    arrayElement(['us-east', 'us-west', 'eu-west', 'ap-south'], toUInt8(number % 4) + 1) AS region
FROM numbers(1000);
INSERT INTO default.jbod_orders
SELECT
    toDate('2024-01-01') + toUInt32(number % 90) AS order_date,
    number + 4100 AS order_id,
    arrayElement(['alice', 'bob', 'carol', 'dave', 'eve'], toUInt8(number % 5) + 1) AS customer,
    toFloat64(10 + (number % 1000)) + toFloat64(number % 100) / 100.0 AS amount,
    arrayElement(['us-east', 'us-west', 'eu-west', 'ap-south'], toUInt8(number % 4) + 1) AS region
FROM numbers(1000);

-- default.app_log -- 500 deterministic rows (matches .*_log skip pattern)
INSERT INTO default.app_log
SELECT
    toDate('2024-01-01') + toUInt32(number % 60) AS log_date,
    number + 100 AS log_id,
    arrayElement(['INFO', 'WARN', 'ERROR', 'DEBUG'], toUInt8(number % 4) + 1) AS level,
    concat('Event #', toString(number), ' occurred') AS message
FROM numbers(500);

-- tmpdata.scratch -- 200 deterministic rows (matches tmpdata.* skip pattern)
INSERT INTO tmpdata.scratch
SELECT
    number + 100 AS id,
    concat('scratch_', toString(number)) AS data,
    toDateTime('2024-01-01 00:00:00') + toUInt32(number * 60) AS ts
FROM numbers(200);

-- default.s3_metrics -- 2000 deterministic rows on S3 disk, across Jan-Feb 2024
INSERT INTO default.s3_metrics
SELECT
    toDate('2024-01-01') + toUInt32(number % 60) AS metric_date,
    number + 100 AS metric_id,
    arrayElement(['cpu_usage', 'mem_usage', 'disk_io', 'net_rx'], toUInt8(number % 4) + 1) AS name,
    toFloat64(number % 100) + toFloat64(number % 10) / 10.0 AS value,
    concat('{"host":"srv', toString(number % 5), '"}') AS tags
FROM numbers(2000);
