//! Observer pattern for download events.
//!
//! Supports three access patterns:
//! 1. **Observer trait** — register callbacks for lifecycle events
//! 2. **Broadcast channel** — `tokio::broadcast` for async event stream
//! 3. **Polling** — query `Downloader::get_state(task_id)` directly

use std::sync::Arc;
use tokio::sync::broadcast;

use crate::state::Progress;
use crate::task::TaskId;

/// Events emitted by the download engine.
#[derive(Debug, Clone)]
pub enum DownloadEvent {
    /// A new task was submitted.
    TaskQueued {
        task_id: TaskId,
        url: String,
    },
    /// Mirror resolution started.
    ResolvingMirror {
        task_id: TaskId,
        original_url: String,
    },
    /// A mirror was selected (or original URL kept).
    MirrorSelected {
        task_id: TaskId,
        url: String,
        mirror_name: String,
    },
    /// Download progress update (emitted at most ~2 Hz).
    Progress {
        task_id: TaskId,
        progress: Progress,
    },
    /// A chunk completed (for chunked downloads).
    ChunkDone {
        task_id: TaskId,
        chunk_index: usize,
        total_chunks: usize,
    },
    /// Download paused.
    Paused {
        task_id: TaskId,
    },
    /// Download resumed.
    Resumed {
        task_id: TaskId,
    },
    /// Download completed successfully.
    Completed {
        task_id: TaskId,
        path: String,
        size_bytes: u64,
        duration_ms: u64,
        mirror_used: String,
    },
    /// Download failed.
    Failed {
        task_id: TaskId,
        error: String,
        retries_used: u32,
    },
    /// Download cancelled.
    Cancelled {
        task_id: TaskId,
    },
    /// Mirror switch occurred mid-download (e.g., chunk failed on one mirror).
    MirrorSwitched {
        task_id: TaskId,
        from_mirror: String,
        to_mirror: String,
        reason: String,
    },
    /// Retry attempt.
    Retrying {
        task_id: TaskId,
        attempt: u32,
        max_attempts: u32,
        reason: String,
    },
}

// ── Observer Trait ──────────────────────────────────────────────────────

/// Trait for observing download events asynchronously.
///
/// Implement this and register with `Downloader::observe()`.
/// All methods have default no-op implementations — override only what you need.
#[async_trait::async_trait]
pub trait DownloadObserver: Send + Sync {
    async fn on_event(&self, _event: DownloadEvent) {}
}

// ── Event Broadcaster ───────────────────────────────────────────────────

/// Internal event bus wrapping `tokio::broadcast`.
#[derive(Clone)]
pub(crate) struct EventBroadcaster {
    tx: broadcast::Sender<DownloadEvent>,
}

impl EventBroadcaster {
    /// Create a new broadcaster with the given buffer capacity.
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Send an event to all subscribers. If the channel is full, the oldest event is dropped.
    pub fn send(&self, event: DownloadEvent) {
        let _ = self.tx.send(event);
    }

    /// Create a new subscriber receiver.
    pub fn subscribe(&self) -> broadcast::Receiver<DownloadEvent> {
        self.tx.subscribe()
    }

    /// Number of active subscribers.
    #[allow(dead_code)]
    pub fn subscriber_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

// ── Composite Observer ──────────────────────────────────────────────────

/// A collection of observers — dispatches events to all registered observers.
pub(crate) struct ObserverSet {
    observers: tokio::sync::RwLock<Vec<Arc<dyn DownloadObserver>>>,
}

impl ObserverSet {
    pub fn new() -> Self {
        Self {
            observers: tokio::sync::RwLock::new(Vec::new()),
        }
    }

    pub async fn register(&self, observer: Arc<dyn DownloadObserver>) {
        self.observers.write().await.push(observer);
    }

    pub async fn notify(&self, event: DownloadEvent) {
        let observers = self.observers.read().await;
        for obs in observers.iter() {
            obs.on_event(event.clone()).await;
        }
    }
}
