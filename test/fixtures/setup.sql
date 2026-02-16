-- =============================================================================
-- setup.sql — Create test databases and tables
-- =============================================================================
-- Run before integration tests to set up the schema.
-- Covers table types needed for Phase 1 tests (T1, T3, T21, T22).
-- =============================================================================

-- Standard MergeTree with date partitioning (T1: basic backup/restore)
CREATE TABLE IF NOT EXISTS default.trades
(
    trade_date Date,
    trade_id   UInt64,
    symbol     String,
    price      Float64,
    quantity   UInt32
)
ENGINE = MergeTree()
PARTITION BY toYYYYMM(trade_date)
ORDER BY (symbol, trade_id);

-- Insert deterministic test data across multiple partitions
INSERT INTO default.trades VALUES
    ('2024-01-15', 1, 'AAPL', 185.50, 100),
    ('2024-01-16', 2, 'GOOG', 141.80, 50),
    ('2024-02-01', 3, 'AAPL', 190.25, 200),
    ('2024-02-15', 4, 'MSFT', 405.10, 75),
    ('2024-03-01', 5, 'GOOG', 155.00, 150);

-- ReplacingMergeTree (T3: different engine types)
CREATE TABLE IF NOT EXISTS default.users
(
    user_id    UInt64,
    username   String,
    email      String,
    updated_at DateTime
)
ENGINE = ReplacingMergeTree(updated_at)
ORDER BY user_id;

INSERT INTO default.users VALUES
    (1, 'alice', 'alice@example.com', '2024-01-01 00:00:00'),
    (2, 'bob', 'bob@example.com', '2024-01-02 00:00:00'),
    (3, 'carol', 'carol@example.com', '2024-01-03 00:00:00');

-- ReplicatedMergeTree (T21, T22: replicated table backup/restore)
CREATE TABLE IF NOT EXISTS default.events
(
    event_date Date,
    event_id   UInt64,
    event_type String,
    payload    String
)
ENGINE = ReplicatedMergeTree('/clickhouse/tables/{shard}/default/events', '{replica}')
PARTITION BY toYYYYMM(event_date)
ORDER BY (event_type, event_id);

INSERT INTO default.events VALUES
    ('2024-01-10', 1, 'click', '{"page": "/home"}'),
    ('2024-01-20', 2, 'purchase', '{"item": "widget"}'),
    ('2024-02-05', 3, 'click', '{"page": "/about"}'),
    ('2024-02-10', 4, 'signup', '{"source": "organic"}');
