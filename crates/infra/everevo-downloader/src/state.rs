//! Task state machine and progress tracking.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Instant;

/// Progress snapshot for a single task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Progress {
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    /// Percentage 0.0–100.0
    pub percentage: f32,
    /// Bytes per second
    pub speed_bytes: f64,
    /// Estimated time remaining
    pub eta_secs: Option<f64>,
}

/// The lifecycle state of a download task.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "phase", content = "data")]
pub enum TaskState {
    /// Queued, not yet started.
    Pending,

    /// Resolving mirrors for the URL.
    ResolvingMirror,

    /// Actively downloading.
    Downloading(Progress),

    /// Paused by user.
    Paused,

    /// Download complete.
    Completed {
        path: PathBuf,
        size_bytes: u64,
        duration_ms: u64,
        mirror_used: String,
    },

    /// Download failed.
    Failed {
        error_message: String,
        retries_used: u32,
        mirror_last_tried: Option<String>,
    },

    /// Cancelled by user.
    Cancelled,
}

impl TaskState {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed { .. } | Self::Failed { .. } | Self::Cancelled
        )
    }

    pub fn is_active(&self) -> bool {
        matches!(self, Self::Downloading(_) | Self::ResolvingMirror)
    }
}

/// In-memory metadata for an active task.
/// The task ID is always available from the HashMap key — no need to store it here.
pub(crate) struct TaskMeta {
    pub state: TaskState,
    pub started_at: Option<Instant>,
    /// Sampling state for speed calculation
    pub(crate) last_sample_bytes: u64,
    pub(crate) last_sample_time: Instant,
    pub(crate) speed_bytes: f64,
}

impl TaskMeta {
    pub fn new() -> Self {
        Self {
            state: TaskState::Pending,
            started_at: None,
            last_sample_bytes: 0,
            last_sample_time: Instant::now(),
            speed_bytes: 0.0,
        }
    }

    /// Update progress and compute speed via sampling.
    pub fn update_progress(&mut self, downloaded: u64, total: u64) {
        let now = Instant::now();
        let elapsed = (now - self.last_sample_time).as_secs_f64();
        if elapsed > 0.5 {
            let delta = downloaded.saturating_sub(self.last_sample_bytes);
            self.speed_bytes = delta as f64 / elapsed;
            self.last_sample_bytes = downloaded;
            self.last_sample_time = now;
        }

        let pct = if total > 0 {
            (downloaded as f64 / total as f64 * 100.0) as f32
        } else {
            0.0
        };

        let eta = if self.speed_bytes > 0.0 && total > 0 {
            Some((total - downloaded) as f64 / self.speed_bytes)
        } else {
            None
        };

        self.state = TaskState::Downloading(Progress {
            downloaded_bytes: downloaded,
            total_bytes: total,
            percentage: pct,
            speed_bytes: self.speed_bytes,
            eta_secs: eta,
        });
    }

    pub fn progress(&self) -> Option<&Progress> {
        match &self.state {
            TaskState::Downloading(p) => Some(p),
            _ => None,
        }
    }

    /// Mark as started.
    pub fn mark_started(&mut self) {
        self.started_at = Some(Instant::now());
        self.state = TaskState::ResolvingMirror;
    }

    pub fn duration_ms(&self) -> u64 {
        self.started_at
            .map(|t| t.elapsed().as_millis() as u64)
            .unwrap_or(0)
    }
}
