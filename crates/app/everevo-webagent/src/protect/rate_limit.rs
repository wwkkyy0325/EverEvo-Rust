//! Per-domain rate limiter — prevents triggering anti-bot rate-limit
//! thresholds on search engines.
//!
//! ## Design
//!
//! Token-bucket algorithm: each domain gets `capacity` tokens, refilled at
//! `refill_rate` tokens/sec. A request consumes 1 token. If no tokens
//! available, the caller is told to wait.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

struct Bucket {
    tokens: f64,
    capacity: f64,
    refill_rate: f64, // tokens per second
    last_refill: Instant,
}

pub struct RateLimiter {
    buckets: Mutex<HashMap<String, Bucket>>,
    default_capacity: f64,
    default_refill: f64,
}

impl RateLimiter {
    pub fn new(default_capacity: f64, default_refill_per_sec: f64) -> Self {
        Self {
            buckets: Mutex::new(HashMap::new()),
            default_capacity,
            default_refill: default_refill_per_sec,
        }
    }

    /// Check if a request to `domain` is allowed now.
    /// Returns `Ok(())` if allowed, `Err(wait_ms)` if the caller should wait.
    pub fn check(&self, domain: &str) -> Result<(), u64> {
        let mut buckets = self.buckets.lock().unwrap();
        let bucket = buckets.entry(domain.to_string()).or_insert_with(|| Bucket {
            tokens: self.default_capacity,
            capacity: self.default_capacity,
            refill_rate: self.default_refill,
            last_refill: Instant::now(),
        });

        let now = Instant::now();
        let elapsed = now.duration_since(bucket.last_refill).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * bucket.refill_rate).min(bucket.capacity);
        bucket.last_refill = now;

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Ok(())
        } else {
            let wait_secs = (1.0 - bucket.tokens) / bucket.refill_rate;
            Err((wait_secs * 1000.0) as u64)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rate_limit_allows_and_blocks() {
        let rl = RateLimiter::new(2.0, 1.0); // 2 tokens, 1/sec refill
        assert!(rl.check("test.com").is_ok());
        assert!(rl.check("test.com").is_ok());
        // Third request should be blocked
        assert!(rl.check("test.com").is_err());
    }

    #[test]
    fn test_different_domains_independent() {
        let rl = RateLimiter::new(1.0, 0.5);
        assert!(rl.check("a.com").is_ok());
        assert!(rl.check("b.com").is_ok()); // different domain, own bucket
        assert!(rl.check("a.com").is_err());
    }
}
