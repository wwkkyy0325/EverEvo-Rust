//! Dreaming Scheduler — triggers the memory consolidation pipeline.
//!
//! ## Triggers
//!
//! | Trigger | Phase | Frequency |
//! |---------|-------|-----------|
//! | Timer | LIGHT | Every N hours (default: 3) |
//! | Timer | REM + DEEP | Daily at 3 AM |
//! | Nudge | LIGHT | Every ~10 conversation turns |
//! | Manual | LIGHT/REM/DEEP | User `/dream` command |
//!
//! ## Architecture
//!
//! The scheduler runs as a background tokio task. It tracks:
//! - Last LIGHT run timestamp
//! - Last REM/DEEP run timestamp
//! - Conversation turn counter (for Nudge)
//!
//! Phase execution is delegated to [`DreamingEngine`](super::engine::DreamingEngine).

use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use everevo_core::EverEvoError;

use super::engine::DreamingEngine;
use super::facts::FactManager;
use super::wiki::WikiGenerator;

/// Tracks when each phase last ran.
#[derive(Debug, Clone)]
pub struct PhaseTimestamps {
    pub last_light: Arc<AtomicI64>, // Unix timestamp (seconds)
    pub last_rem: Arc<AtomicI64>,
    pub last_deep: Arc<AtomicI64>,
}

impl Default for PhaseTimestamps {
    fn default() -> Self {
        let now = chrono::Utc::now().timestamp();
        Self {
            last_light: Arc::new(AtomicI64::new(now)),
            last_rem: Arc::new(AtomicI64::new(now)),
            last_deep: Arc::new(AtomicI64::new(now)),
        }
    }
}

impl PhaseTimestamps {
    fn seconds_since_light(&self) -> i64 {
        chrono::Utc::now().timestamp() - self.last_light.load(Ordering::Relaxed)
    }
    fn seconds_since_rem(&self) -> i64 {
        chrono::Utc::now().timestamp() - self.last_rem.load(Ordering::Relaxed)
    }
    fn touch_light(&self) {
        self.last_light
            .store(chrono::Utc::now().timestamp(), Ordering::Relaxed);
    }
    fn touch_rem(&self) {
        self.last_rem
            .store(chrono::Utc::now().timestamp(), Ordering::Relaxed);
    }
    fn touch_deep(&self) {
        self.last_deep
            .store(chrono::Utc::now().timestamp(), Ordering::Relaxed);
    }
}

/// Configuration for the dreaming scheduler.
#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    /// LIGHT interval when buffer has data (hours).
    pub active_light_interval_hours: u32,
    /// LIGHT interval when buffer is empty/idle (hours).
    pub idle_light_interval_hours: u32,
    /// REM + DEEP phase interval in hours (usually daily = 24).
    pub deep_interval_hours: u32,
    /// Nudge: trigger LIGHT after this many conversation turns.
    pub nudge_turn_threshold: u32,
    /// Nudge cooldown in seconds — prevents burst triggering (default: 1800 = 30 min).
    pub nudge_cooldown_secs: i64,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            active_light_interval_hours: 3,
            idle_light_interval_hours: 12,
            deep_interval_hours: 24,
            nudge_turn_threshold: 10,
            nudge_cooldown_secs: 1800, // 30 minutes
        }
    }
}

/// The dreaming scheduler — orchestrates the pipeline timing.
/// Handles burst protection, idle detection, and session boundaries.
///
/// Phase execution is delegated to [`DreamingEngine`].
pub struct DreamingScheduler {
    config: SchedulerConfig,
    timestamps: PhaseTimestamps,
    /// Conversation turn counter for Nudge engine.
    turn_counter: Arc<AtomicU32>,
    /// Whether the background task is running.
    running: Arc<AtomicU32>, // 0 = stopped, 1 = running
    /// Unix timestamp (seconds) of the last Nudge-triggered LIGHT.
    /// Used to enforce cooldown and prevent burst triggering.
    last_nudge_ts: Arc<AtomicI64>,
    /// Mutex to prevent concurrent LIGHT phase execution.
    /// 0 = idle, 1 = running. Non-blocking: if already running, skip.
    light_running: Arc<AtomicU32>,
    /// Whether the last LIGHT found data in the buffer.
    /// Used for adaptive interval: idle vs active.
    last_light_had_data: Arc<AtomicU32>, // 0 = empty, 1 = had data
}

impl DreamingScheduler {
    pub fn new(config: SchedulerConfig) -> Self {
        Self {
            config,
            timestamps: PhaseTimestamps::default(),
            turn_counter: Arc::new(AtomicU32::new(0)),
            running: Arc::new(AtomicU32::new(0)),
            last_nudge_ts: Arc::new(AtomicI64::new(0)),
            light_running: Arc::new(AtomicU32::new(0)),
            last_light_had_data: Arc::new(AtomicU32::new(0)),
        }
    }

    /// Increment the conversation turn counter.
    /// Called after each agent loop turn completes.
    pub fn increment_turn(&self) {
        self.turn_counter.fetch_add(1, Ordering::Relaxed);
    }

    /// Check if any phase is due and return what should run.
    pub fn poll(&self) -> Vec<ScheduledPhase> {
        // Skip if LIGHT is already running (burst protection)
        if self.light_running.load(Ordering::Acquire) == 1 {
            return Vec::new();
        }

        let mut phases = Vec::new();
        let now = chrono::Utc::now().timestamp();

        // Nudge check — with cooldown
        let turns = self.turn_counter.swap(0, Ordering::Relaxed);
        if turns >= self.config.nudge_turn_threshold {
            let last_nudge = self.last_nudge_ts.load(Ordering::Relaxed);
            if now - last_nudge >= self.config.nudge_cooldown_secs {
                phases.push(ScheduledPhase::Light {
                    reason: "nudge".into(),
                });
                self.last_nudge_ts.store(now, Ordering::Relaxed);
            } else {
                // Put turns back — retry after cooldown
                self.turn_counter.fetch_add(turns, Ordering::Relaxed);
            }
        }

        // Timer check — LIGHT with adaptive interval
        let had_data = self.last_light_had_data.load(Ordering::Relaxed) == 1;
        let light_interval = if had_data {
            self.config.active_light_interval_hours
        } else {
            self.config.idle_light_interval_hours
        };
        let light_secs = (light_interval as i64) * 3600;
        if self.timestamps.seconds_since_light() >= light_secs {
            phases.push(ScheduledPhase::Light {
                reason: "timer".into(),
            });
        }

        // Timer check — REM + DEEP
        let deep_secs = (self.config.deep_interval_hours as i64) * 3600;
        if self.timestamps.seconds_since_rem() >= deep_secs {
            phases.push(ScheduledPhase::RemAndDeep);
        }

        phases
    }

    /// Mark a phase as completed.
    pub fn mark_completed(&self, phase: &ScheduledPhase) {
        match phase {
            ScheduledPhase::Light { .. } => self.timestamps.touch_light(),
            ScheduledPhase::RemAndDeep => {
                self.timestamps.touch_rem();
                self.timestamps.touch_deep();
            }
            ScheduledPhase::Rem => self.timestamps.touch_rem(),
            ScheduledPhase::Deep => self.timestamps.touch_deep(),
        }
    }

    /// Mark whether the last LIGHT phase found data to process.
    /// Used for adaptive interval: active (3h) vs idle (12h).
    pub fn set_light_had_data(&self, had_data: bool) {
        self.last_light_had_data
            .store(if had_data { 1 } else { 0 }, Ordering::Relaxed);
    }

    /// Acquire the LIGHT mutex. Returns true if acquired (proceed), false if already running.
    pub fn try_acquire_light(&self) -> bool {
        self.light_running
            .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    /// Release the LIGHT mutex.
    pub fn release_light(&self) {
        self.light_running.store(0, Ordering::Release);
    }

    /// Start the background scheduler task.
    /// Runs LIGHT every N hours, REM+DEEP daily.
    /// Returns a handle that can be used to stop the task.
    pub fn start_background(
        self: &Arc<Self>,
        engine: Arc<DreamingEngine>,
        fact_manager: Arc<FactManager>,
        wiki_generator: Arc<WikiGenerator>,
        persona_profile: Option<std::path::PathBuf>,
    ) -> tokio::task::JoinHandle<()> {
        self.running.store(1, Ordering::Relaxed);
        let this = Arc::clone(self);
        let check_interval = Duration::from_secs(60); // check every minute

        tokio::spawn(async move {
            loop {
                if this.running.load(Ordering::Relaxed) == 0 {
                    break;
                }
                tokio::time::sleep(check_interval).await;

                let phases = this.poll();
                for phase in &phases {
                    let is_light = matches!(phase, ScheduledPhase::Light { .. });

                    // Burst protection: only one LIGHT at a time
                    if is_light && !this.try_acquire_light() {
                        tracing::debug!("LIGHT already running — skipping");
                        continue;
                    }

                    tracing::info!(?phase, "Dreaming phase triggered");
                    let result = engine.run_full_pipeline(phase).await;
                    match result {
                        Ok(()) => {
                            // Track whether LIGHT found data for adaptive interval
                            if is_light {
                                let had_data = !engine.drain_messages().is_empty();
                                this.set_light_had_data(had_data);
                                this.release_light();
                            }
                            this.mark_completed(phase);
                            // After DEEP: generate wiki + update persona
                            if matches!(phase, ScheduledPhase::Deep | ScheduledPhase::RemAndDeep) {
                                // Wiki generation
                                if let Err(e) =
                                    wiki_generator.generate_from_facts(&fact_manager).await
                                {
                                    tracing::warn!(error = %e, "Wiki generation failed");
                                }
                                // Persona auto-update from accumulated facts
                                if let Some(ref path) = persona_profile {
                                    let facts = fact_manager.load_all().unwrap_or_default();
                                    crate::stages::persona::update_persona_from_facts(
                                        path, &facts,
                                    );
                                }
                            }
                            tracing::info!(?phase, "Dreaming phase completed");
                        }
                        Err(e) => {
                            if is_light {
                                this.release_light();
                            }
                            tracing::warn!(?phase, error = %e, "Dreaming phase failed");
                        }
                    }
                }
            }
        })
    }

    /// Stop the background task.
    pub fn stop(&self) {
        self.running.store(0, Ordering::Relaxed);
    }

    /// Get the turn counter for external use.
    pub fn turn_counter(&self) -> &Arc<AtomicU32> {
        &self.turn_counter
    }

    /// Whether the background loop is running.
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed) == 1
    }

    /// Unix timestamp (seconds) of the last LIGHT phase.
    pub fn last_light_ts(&self) -> i64 {
        self.timestamps.last_light.load(Ordering::Relaxed)
    }

    /// Unix timestamp (seconds) of the last REM phase.
    pub fn last_rem_ts(&self) -> i64 {
        self.timestamps.last_rem.load(Ordering::Relaxed)
    }

    /// Unix timestamp (seconds) of the last DEEP phase.
    pub fn last_deep_ts(&self) -> i64 {
        self.timestamps.last_deep.load(Ordering::Relaxed)
    }

    /// Current conversation turn counter value.
    pub fn turn_count(&self) -> u32 {
        self.turn_counter.load(Ordering::Relaxed)
    }

    /// Manually trigger a phase (runs immediately, bypassing the timer).
    /// Marks the phase as completed on success.
    pub async fn trigger_phase(
        &self,
        phase: &ScheduledPhase,
        engine: &DreamingEngine,
    ) -> Result<(), EverEvoError> {
        let result = engine.run_full_pipeline(phase).await;
        if result.is_ok() {
            self.mark_completed(phase);
        }
        result
    }
}

// ── Phase Type ─────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum ScheduledPhase {
    /// LIGHT phase: trim raw SQLite messages -> diary entries.
    Light { reason: String },
    /// REM phase: extract themes from recent diary files.
    Rem,
    /// DEEP phase: score themes + promote to facts.
    Deep,
    /// Combined REM + DEEP (daily run).
    RemAndDeep,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scheduler_poll_on_startup() {
        let sched = DreamingScheduler::new(SchedulerConfig::default());
        // No turns yet, timestamps just set — should not trigger anything
        let phases = sched.poll();
        assert!(phases.is_empty());
    }

    #[test]
    fn test_nudge_triggers_light() {
        let sched = DreamingScheduler::new(SchedulerConfig {
            nudge_turn_threshold: 5,
            ..Default::default()
        });
        // Simulate 5 turns
        for _ in 0..5 {
            sched.increment_turn();
        }
        let phases = sched.poll();
        assert!(!phases.is_empty());
        assert!(matches!(phases[0], ScheduledPhase::Light { .. }));
    }

    #[test]
    fn test_mark_completed_updates_timestamps() {
        let sched = DreamingScheduler::new(SchedulerConfig::default());
        // Set before to a known-old value
        sched.timestamps.last_light.store(1000, Ordering::Relaxed);
        let before = sched.timestamps.last_light.load(Ordering::Relaxed);
        sched.mark_completed(&ScheduledPhase::Light {
            reason: "test".into(),
        });
        let after = sched.timestamps.last_light.load(Ordering::Relaxed);
        assert!(
            after > before,
            "Timestamp should update after mark_completed"
        );
    }
}
