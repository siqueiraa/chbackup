-- =============================================================================
-- seed_large.sql -- Large dataset for streaming multipart upload tests
-- =============================================================================
-- Generates enough uncompressed data to exceed the 32 MiB multipart upload
-- threshold (MULTIPART_THRESHOLD = 32 * 1024 * 1024 bytes) so that tests
-- exercise the multipart S3 upload path instead of single PutObject.
--
-- Uses a wide String column populated with ~200-byte payloads to produce
-- large uncompressed parts even with modest row counts.
--
-- Tables populated (must exist from setup.sql):
--   default.trades  -- MergeTree, date-partitioned
--
-- Target: ~40 MiB uncompressed in a single partition for deterministic
-- multipart triggering.  200_000 rows * ~200 bytes/row ≈ 40 MiB.
-- =============================================================================

-- Widen the trades table with a large payload column for this test.
-- Uses a separate table to avoid interfering with the baseline schema.
CREATE TABLE IF NOT EXISTS default.trades_large
(
    trade_date  Date,
    trade_id    UInt64,
    symbol      String,
    price       Float64,
    quantity    UInt32,
    -- ~200-byte deterministic string payload to pad each row
    payload     String
)
ENGINE = MergeTree()
PARTITION BY toYYYYMM(trade_date)
ORDER BY (symbol, trade_id);

-- Insert 200 000 rows into a single partition (2024-01) so the part is large
-- enough to trigger multipart upload.  All rows are deterministic.
INSERT INTO default.trades_large
SELECT
    toDate('2024-01-01') + toUInt32(number % 28) AS trade_date,
    number AS trade_id,
    arrayElement(['AAPL', 'GOOG', 'MSFT', 'AMZN', 'TSLA', 'NVDA', 'META', 'NFLX'],
                 toUInt8(number % 8) + 1) AS symbol,
    toFloat64(50 + (number % 950)) + toFloat64(number % 100) / 100.0 AS price,
    toUInt32((number % 9999) + 1) AS quantity,
    -- Deterministic ~200-byte payload: concat of fixed strings and row-specific values
    concat(
        'payload-row-',
        leftPad(toString(number), 10, '0'),
        '-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
        '-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
        '-cccccccccccccccccccccccccccccccccccccccc',
        '-dddddddddddddddddddddddddddddddddddddddd',
        '-end'
    ) AS payload
FROM numbers(200000);
