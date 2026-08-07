//! Canary router — automatic promote/rollback decisions based on metrics.
//!
//! ## Decision logic
//!
//! - **Promote**: canary success_rate >= stable AND p50 latency <= 1.1× stable,
//!   observed for at least promote_min_minutes with >= 100 samples
//! - **Rollback**: success_rate drop > 5% OR crash_count > 3/10min
//! - **KeepObserving**: metrics are within bounds but not clearly better
//! - **InsufficientData**: less than 100 samples
//! - **Observing**: enough samples but not enough time elapsed

use std::sync::Arc;

use super::version::VersionStore;

// ── Decision ────────────────────────────────────────────────────────────

/// The outcome of canary evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromoteDecision {
    /// Canary is clearly better → promote to stable.
    Promote,
    /// Canary is clearly worse → rollback to stable.
    Rollback,
    /// Metrics are within tolerance → continue observing.
    KeepObserving,
    /// Not enough data to make a decision.
    InsufficientData,
    /// Have enough samples but not enough time has elapsed.
    Observing,
    /// No active canary.
    NoCanary,
}

// ── Router ──────────────────────────────────────────────────────────────

/// Evaluates canary metrics and makes promote/rollback decisions.
pub struct CanaryRouter {
    store: Arc<VersionStore>,
}

impl CanaryRouter {
    pub fn new(store: Arc<VersionStore>) -> Self {
        Self { store }
    }

    /// Evaluate whether a canary should be promoted, rolled back, or kept observing.
    pub fn evaluate(&self, plugin_id: &str) -> Result<PromoteDecision, super::version::VersionError> {
        let config = self.store.load_config(plugin_id)?;
        let canary_ver = match &config.canary {
            Some(v) => v.clone(),
            None => return Ok(PromoteDecision::NoCanary),
        };

        let stable_m = config.metrics.get(&config.stable);
        let canary_m = config.metrics.get(&canary_ver);

        let (Some(sm), Some(cm)) = (stable_m, canary_m) else {
            return Ok(PromoteDecision::InsufficientData);
        };

        // Minimum sample size
        if cm.total_count < 100 {
            return Ok(PromoteDecision::InsufficientData);
        }

        // Minimum observation time
        let min_samples: u64 = (config.promote_min_minutes * 60) / 5; // ~12 samples/min at 5s/turn
        if cm.total_count < min_samples {
            return Ok(PromoteDecision::Observing);
        }

        let success_delta = cm.success_rate() - sm.success_rate();
        let avg_sm = sm.avg_latency_ms().max(1.0);
        let avg_cm = cm.avg_latency_ms().max(1.0);
        let latency_ratio = avg_cm / avg_sm;

        // ── Rollback conditions ──
        // Success rate dropped more than 5 percentage points
        if success_delta < -0.05 {
            return Ok(PromoteDecision::Rollback);
        }
        // More than 3 crashes in the observation window
        if cm.crash_count > 3 {
            return Ok(PromoteDecision::Rollback);
        }
        // Any crash with degraded success rate
        if cm.crash_count > 0 && success_delta < -0.01 {
            return Ok(PromoteDecision::Rollback);
        }

        // ── Promote conditions ──
        // Success rate not decreased AND latency within 10% of stable
        if success_delta >= 0.0 && latency_ratio <= 1.10 {
            return Ok(PromoteDecision::Promote);
        }

        // ── Keep observing ──
        Ok(PromoteDecision::KeepObserving)
    }

    /// Evaluate and automatically apply the decision.
    ///
    /// Returns the decision that was made (or NoCanary if not applicable).
    pub async fn evaluate_and_apply(&self, plugin_id: &str) -> Result<PromoteDecision, super::version::VersionError> {
        let decision = self.evaluate(plugin_id)?;

        match &decision {
            PromoteDecision::Promote => {
                tracing::info!(%plugin_id, "Auto-promoting canary to stable");
                self.store.promote(plugin_id)?;
            }
            PromoteDecision::Rollback => {
                tracing::warn!(%plugin_id, "Auto-rolling back canary");
                self.store.rollback(plugin_id)?;
            }
            PromoteDecision::NoCanary
            | PromoteDecision::KeepObserving
            | PromoteDecision::InsufficientData
            | PromoteDecision::Observing => {
                // No action needed
            }
        }

        Ok(decision)
    }
}

// ── Safety loop (background task) ───────────────────────────────────────

/// Spawn a background task that periodically evaluates all plugins with
/// active canaries and auto-applies promote/rollback decisions.
pub async fn spawn_canary_safety_loop(
    router: Arc<CanaryRouter>,
    plugin_ids: Vec<String>,
    interval: std::time::Duration,
) {
    tokio::spawn(async move {
        loop {
            for plugin_id in &plugin_ids {
                match router.evaluate_and_apply(plugin_id).await {
                    Ok(decision) => {
                        if decision != PromoteDecision::NoCanary
                            && decision != PromoteDecision::KeepObserving
                        {
                            tracing::info!(
                                %plugin_id,
                                ?decision,
                                "Canary safety loop: decision applied"
                            );
                        }
                    }
                    Err(e) => {
                        tracing::warn!(%plugin_id, error = %e, "Canary safety loop: evaluation failed");
                    }
                }
            }
            tokio::time::sleep(interval).await;
        }
    });
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::version::{PluginConfig, PluginMetrics};
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn make_config(
        stable: &str,
        canary: Option<&str>,
        stable_metrics: PluginMetrics,
        canary_metrics: PluginMetrics,
    ) -> PluginConfig {
        let mut metrics = HashMap::new();
        metrics.insert(stable.to_string(), stable_metrics);
        if let Some(c) = canary {
            metrics.insert(c.to_string(), canary_metrics);
        }
        PluginConfig {
            stable: stable.into(),
            canary: canary.map(|s| s.into()),
            canary_pct: if canary.is_some() { 0.1 } else { 0.0 },
            auto_promote: true,
            auto_rollback: true,
            promote_min_minutes: 30,
            metrics,
        }
    }

    fn good_metrics() -> PluginMetrics {
        PluginMetrics {
            success_count: 990,
            error_count: 10,
            total_count: 1000,
            total_latency_ms: 50000, // avg 50ms
            crash_count: 0,
            last_reset: String::new(),
        }
    }

    fn better_metrics() -> PluginMetrics {
        PluginMetrics {
            success_count: 995,
            error_count: 5,
            total_count: 1000,
            total_latency_ms: 42000, // avg 42ms
            crash_count: 0,
            last_reset: String::new(),
        }
    }

    fn degraded_metrics() -> PluginMetrics {
        PluginMetrics {
            success_count: 930,
            error_count: 70,
            total_count: 1000,
            total_latency_ms: 80000,
            crash_count: 0,
            last_reset: String::new(),
        }
    }

    fn crash_metrics() -> PluginMetrics {
        PluginMetrics {
            success_count: 990,
            error_count: 10,
            total_count: 1000,
            total_latency_ms: 50000,
            crash_count: 5,
            last_reset: String::new(),
        }
    }

    #[test]
    fn test_no_canary_is_noop() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(VersionStore::open(dir.path()).unwrap());
        let config = make_config("v1.0.0", None, good_metrics(), PluginMetrics::default());
        store.save_config("test", &config).unwrap();

        let router = CanaryRouter::new(store);
        assert_eq!(router.evaluate("test").unwrap(), PromoteDecision::NoCanary);
    }

    #[test]
    fn test_insufficient_data_with_few_samples() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(VersionStore::open(dir.path()).unwrap());
        let few = PluginMetrics {
            total_count: 5,
            ..PluginMetrics::default()
        };
        let config = make_config("v1.0.0", Some("v1.0.1"), good_metrics(), few);
        store.save_config("test", &config).unwrap();

        let router = CanaryRouter::new(store);
        assert_eq!(router.evaluate("test").unwrap(), PromoteDecision::InsufficientData);
    }

    #[test]
    fn test_promote_when_better() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(VersionStore::open(dir.path()).unwrap());
        let config = make_config("v1.0.0", Some("v1.0.1"), good_metrics(), better_metrics());
        store.save_config("test", &config).unwrap();

        let router = CanaryRouter::new(store);
        assert_eq!(router.evaluate("test").unwrap(), PromoteDecision::Promote);
    }

    #[test]
    fn test_rollback_when_degraded() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(VersionStore::open(dir.path()).unwrap());
        let config = make_config("v1.0.0", Some("v1.0.1"), good_metrics(), degraded_metrics());
        store.save_config("test", &config).unwrap();

        let router = CanaryRouter::new(store);
        assert_eq!(router.evaluate("test").unwrap(), PromoteDecision::Rollback);
    }

    #[test]
    fn test_rollback_on_crashes() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(VersionStore::open(dir.path()).unwrap());
        let config = make_config("v1.0.0", Some("v1.0.1"), good_metrics(), crash_metrics());
        store.save_config("test", &config).unwrap();

        let router = CanaryRouter::new(store);
        assert_eq!(router.evaluate("test").unwrap(), PromoteDecision::Rollback);
    }
}
