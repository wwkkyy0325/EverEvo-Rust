//! Circuit breaker — stops calling a failing endpoint repeatedly.
//!
//! ## States
//!
//! ```text
//! Closed ──consecutive_failures >= threshold──→ Open (fail-fast)
//! Open ───cooldown elapsed───────────────────→ HalfOpen
//! HalfOpen ──success────────────────────────→ Closed
//! HalfOpen ──failure────────────────────────→ Open
//! ```

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Closed,
    Open,
    HalfOpen,
}

struct Entry {
    state: State,
    failures: u32,
    last_failure: Instant,
    opened_at: Instant,
}

pub struct CircuitBreaker {
    entries: Mutex<HashMap<String, Entry>>,
    threshold: u32,        // consecutive failures to trip
    cooldown: Duration,    // time in Open before HalfOpen
}

impl CircuitBreaker {
    pub fn new(threshold: u32, cooldown_secs: u64) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            threshold,
            cooldown: Duration::from_secs(cooldown_secs),
        }
    }

    /// Check if a call to `key` is allowed. If the circuit is open,
    /// returns `Err(remaining_cooldown_ms)`.
    pub fn check(&self, key: &str) -> Result<(), u64> {
        let mut entries = self.entries.lock().unwrap();
        let entry = entries.entry(key.to_string()).or_insert(Entry {
            state: State::Closed,
            failures: 0,
            last_failure: Instant::now(),
            opened_at: Instant::now(),
        });

        match entry.state {
            State::Closed => Ok(()),
            State::HalfOpen => Ok(()),
            State::Open => {
                let elapsed = Instant::now().duration_since(entry.opened_at);
                if elapsed >= self.cooldown {
                    entry.state = State::HalfOpen;
                    Ok(())
                } else {
                    Err((self.cooldown - elapsed).as_millis() as u64)
                }
            }
        }
    }

    /// Record a successful call.
    pub fn success(&self, key: &str) {
        let mut entries = self.entries.lock().unwrap();
        if let Some(entry) = entries.get_mut(key) {
            entry.state = State::Closed;
            entry.failures = 0;
        }
    }

    /// Record a failed call. May trip the circuit.
    pub fn failure(&self, key: &str) {
        let mut entries = self.entries.lock().unwrap();
        let entry = entries.entry(key.to_string()).or_insert(Entry {
            state: State::Closed,
            failures: 0,
            last_failure: Instant::now(),
            opened_at: Instant::now(),
        });

        entry.failures += 1;
        entry.last_failure = Instant::now();

        if entry.failures >= self.threshold {
            entry.state = State::Open;
            entry.opened_at = Instant::now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_circuit_trips_and_resets() {
        let cb = CircuitBreaker::new(3, 60); // trip after 3 failures, 60s cooldown
        assert!(cb.check("api").is_ok());
        cb.failure("api");
        cb.failure("api");
        assert!(cb.check("api").is_ok()); // not tripped yet
        cb.failure("api");
        assert!(cb.check("api").is_err()); // tripped!

        cb.success("api");
        assert!(cb.check("api").is_ok()); // reset
    }
}
