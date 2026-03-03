//! Token-bucket rate limiter for byte-level bandwidth control.
//!
//! Shared across concurrent upload/download tasks via `Arc` (the struct
//! is `Clone`). When `bytes_per_second` is 0, all operations are no-ops.

use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{Duration, Instant};
use tracing::{debug, warn};

/// Internal state for the token bucket.
struct TokenBucketState {
    /// Available tokens (bytes). Can go negative when a large consume
    /// request is served immediately but requires a longer delay next time.
    tokens: f64,
    /// Timestamp of the last token refill.
    last_refill: Instant,
    /// Maximum token generation rate in bytes per second.
    rate: f64,
}

/// A token-bucket rate limiter that can be cloned and shared across tasks.
///
/// When `bytes_per_second` is 0, the rate limiter is unlimited and `consume`
/// returns immediately without any delay.
#[derive(Clone)]
pub struct RateLimiter {
    /// None when unlimited (bytes_per_second == 0).
    inner: Option<Arc<Mutex<TokenBucketState>>>,
}

impl RateLimiter {
    /// Create a new rate limiter.
    ///
    /// - `bytes_per_second = 0`: unlimited (no-op passthrough)
    /// - `bytes_per_second > 0`: limits throughput to approximately this rate
    pub fn new(bytes_per_second: u64) -> Self {
        if bytes_per_second == 0 {
            return Self { inner: None };
        }

        let rate = bytes_per_second as f64;
        let state = TokenBucketState {
            tokens: rate, // start with a full bucket (1 second worth)
            last_refill: Instant::now(),
            rate,
        };

        Self {
            inner: Some(Arc::new(Mutex::new(state))),
        }
    }

    /// Consume `bytes` worth of tokens. If the bucket is empty, this
    /// method sleeps until enough tokens have accumulated.
    ///
    /// When the rate limiter is unlimited (bytes_per_second == 0), this
    /// returns immediately.
    pub async fn consume(&self, bytes: u64) {
        let state_lock = match &self.inner {
            Some(inner) => inner,
            None => return, // unlimited -- no-op
        };

        let sleep_duration = {
            let mut state = state_lock.lock().await;

            // Refill tokens based on elapsed time
            let now = Instant::now();
            let elapsed = now.duration_since(state.last_refill).as_secs_f64();
            state.tokens += elapsed * state.rate;
            state.last_refill = now;

            // Cap tokens at the rate (1 second of burst)
            if state.tokens > state.rate {
                state.tokens = state.rate;
            }

            // Deduct the requested bytes
            state.tokens -= bytes as f64;

            // If tokens went negative, calculate how long to sleep
            if state.tokens < 0.0 {
                let deficit = -state.tokens;
                let computed = Duration::from_secs_f64(deficit / state.rate);
                // Cap at 5 minutes to prevent indefinite hang from corrupt manifests
                // with extremely large part sizes.
                let capped = computed.min(Duration::from_secs(300));
                if capped < computed {
                    warn!(
                        computed_secs = computed.as_secs_f64(),
                        "Rate limiter sleep capped at 5m; part size may be corrupt"
                    );
                }
                capped
            } else {
                Duration::ZERO
            }
        };

        // Sleep outside the lock to avoid blocking other consumers
        if !sleep_duration.is_zero() {
            debug!(
                sleep_ms = sleep_duration.as_millis(),
                "Rate limiter sleeping"
            );
            tokio::time::sleep(sleep_duration).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_rate_limiter_unlimited() {
        let limiter = RateLimiter::new(0);

        // Should return immediately with no delay
        let start = Instant::now();
        limiter.consume(1_000_000).await;
        limiter.consume(1_000_000).await;
        limiter.consume(1_000_000).await;
        let elapsed = start.elapsed();

        // Should complete in well under 100ms (essentially instant)
        assert!(
            elapsed < Duration::from_millis(100),
            "Unlimited rate limiter took {:?}, expected < 100ms",
            elapsed
        );
    }

    #[tokio::test]
    async fn test_rate_limiter_basic() {
        // 100 bytes/sec, consume 200 bytes -> should take ~1 second
        // (first 100 bytes served from initial bucket, second 100 requires waiting)
        let limiter = RateLimiter::new(100);

        let start = Instant::now();
        limiter.consume(200).await;
        let elapsed = start.elapsed();

        // Should have taken approximately 1 second (within tolerance)
        assert!(
            elapsed >= Duration::from_millis(800),
            "Rate limiter was too fast: {:?}",
            elapsed
        );
        assert!(
            elapsed < Duration::from_millis(2000),
            "Rate limiter was too slow: {:?}",
            elapsed
        );
    }

    #[tokio::test]
    async fn test_rate_limiter_concurrent() {
        // 200 bytes/sec shared between two tasks, each consuming 200 bytes
        // Total = 400 bytes at 200 bytes/sec -> should take ~1 second total
        let limiter = RateLimiter::new(200);

        let l1 = limiter.clone();
        let l2 = limiter.clone();

        let start = Instant::now();

        let h1 = tokio::spawn(async move {
            l1.consume(200).await;
        });
        let h2 = tokio::spawn(async move {
            l2.consume(200).await;
        });

        h1.await.expect("task 1 panicked");
        h2.await.expect("task 2 panicked");

        let elapsed = start.elapsed();

        // Both tasks share 200 bytes/sec, consuming 400 bytes total
        // Should take at least ~1 second
        assert!(
            elapsed >= Duration::from_millis(800),
            "Concurrent rate limiter was too fast: {:?}",
            elapsed
        );
        assert!(
            elapsed < Duration::from_millis(3000),
            "Concurrent rate limiter was too slow: {:?}",
            elapsed
        );
    }
}
