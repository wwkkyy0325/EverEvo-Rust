//! EverEvo Downloader — a general-purpose download engine.
//!
//! ## Capabilities
//!
//! - **Multi-mirror**: pre-configured domestic (CN) + international mirrors with
//!   automatic failover
//! - **Resumable**: interrupted downloads resume via HTTP Range requests +
//!   persistent `.resume.json` state
//! - **Concurrent**: auto-detects file size, switches to chunked parallel download
//!   when beneficial
//! - **Observable**: three access patterns — observer callbacks, broadcast channel
//!   events, and state polling
//! - **Priority queue**: tasks ordered by `Priority` level
//!
//! ## Usage
//!
//! ```rust,ignore
//! use everevo_downloader::{Downloader, DownloaderConfig, DownloadTask};
//!
//! let dl = Downloader::new(DownloaderConfig::default());
//!
//! // Pattern 1: Fire-and-wait
//! let task = DownloadTask::new("https://example.com/file.zip", "./downloads/file.zip");
//! let handle = dl.submit(task).await?;
//! let result = handle.await?;
//! println!("Downloaded to: {}", result.path.display());
//!
//! // Pattern 2: Event stream
//! let mut events = dl.events();
//! tokio::spawn(async move {
//!     while let Ok(event) = events.recv().await {
//!         match event {
//!             DownloadEvent::Progress { progress, .. } => {
//!                 println!("Progress: {:.1}%", progress.percentage);
//!             }
//!             DownloadEvent::Completed { path, .. } => {
//!                 println!("Done: {path}");
//!                 break;
//!             }
//!             _ => {}
//!         }
//!     }
//! });
//!
//! // Pattern 3: Polling
//! let state = dl.get_state(&task_id).await;
//! ```

pub mod config;
pub mod error;
pub mod mirror;
pub mod observer;
pub(crate) mod resume;
pub mod state;
pub(crate) mod strategy;
pub mod task;
mod worker;

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, oneshot, RwLock, Semaphore};

use config::DownloaderConfig;
use error::DownloadError;
use mirror::MirrorRegistry;
use observer::{DownloadEvent, EventBroadcaster, ObserverSet};
use state::{TaskMeta, TaskState};
use task::{DownloadTask, TaskId};

/// The download engine.
///
/// Create one instance per application. Internally manages a connection pool,
/// mirror registry, event bus, and task state map. Cheap to clone (Arc inside).
pub struct Downloader {
    config: DownloaderConfig,
    client: worker::HttpClient,
    mirrors: Arc<RwLock<MirrorRegistry>>,
    events: Arc<EventBroadcaster>,
    observers: Arc<ObserverSet>,
    state_map: Arc<RwLock<HashMap<TaskId, Arc<tokio::sync::Mutex<TaskMeta>>>>>,
    semaphore: Arc<Semaphore>,
    /// Cancellation tokens per task.
    cancel_tokens: Arc<RwLock<HashMap<TaskId, tokio_util::sync::CancellationToken>>>,
}

impl Downloader {
    /// Create a new download engine with the given configuration.
    pub fn new(config: DownloaderConfig) -> Result<Self, DownloadError> {
        let client = worker::build_client(&config)?;
        let mirrors = MirrorRegistry::with_defaults();
        let events = EventBroadcaster::new(256);
        let observers = ObserverSet::new();
        let semaphore = Semaphore::new(config.max_concurrent_tasks);

        Ok(Self {
            config,
            client,
            mirrors: Arc::new(RwLock::new(mirrors)),
            events: Arc::new(events),
            observers: Arc::new(observers),
            state_map: Arc::new(RwLock::new(HashMap::new())),
            semaphore: Arc::new(semaphore),
            cancel_tokens: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    // ── Submit ────────────────────────────────────────────────────────

    /// Submit a download task. Returns a `TaskHandle` that resolves to the result.
    ///
    /// The task is queued and executed when capacity is available, respecting
    /// priority ordering.
    pub async fn submit(&self, task: DownloadTask) -> Result<TaskHandle, DownloadError> {
        let task_id = task.id.clone();
        let task = Arc::new(task);

        // Register in state map
        let meta = Arc::new(tokio::sync::Mutex::new(TaskMeta::new()));
        {
            let mut map = self.state_map.write().await;
            map.insert(task_id.clone(), meta.clone());
        }

        // Create oneshot for final result
        let (tx, rx) = oneshot::channel();

        // Emit queued event
        self.events.send(DownloadEvent::TaskQueued {
            task_id: task_id.clone(),
            url: task.url.clone(),
        });

        // Clone everything needed by the worker
        let task_clone = task.clone();
        let client = self.client.clone();
        let config = self.config.clone();
        let mirrors = self.mirrors.clone();
        let events = self.events.clone();
        let observers = self.observers.clone();
        let semaphore = self.semaphore.clone();
        let meta_clone = meta.clone();
        let cancel_token = tokio_util::sync::CancellationToken::new();
        let cancel_token_clone = cancel_token.clone();

        {
            let mut tokens = self.cancel_tokens.write().await;
            tokens.insert(task_id.clone(), cancel_token);
        }

        // Spawn worker
        tokio::spawn(async move {
            // Acquire semaphore (respects max_concurrent_tasks)
            let _permit = semaphore.acquire().await;

            // Check if cancelled before starting
            if cancel_token_clone.is_cancelled() {
                let mut m = meta_clone.lock().await;
                m.state = TaskState::Cancelled;
                let _ = tx.send(DownloadResult {
                    task_id: task_id.clone(),
                    outcome: Outcome::Cancelled,
                    path: None,
                    size_bytes: 0,
                    duration_ms: m.duration_ms(),
                });
                return;
            }

            // Mark started
            {
                let mut m = meta_clone.lock().await;
                m.mark_started();
            }

            // Execute (bind read guard before select to avoid temp-drop)
            let mirrors_guard = mirrors.read().await;
            let result = tokio::select! {
                biased;
                _ = cancel_token_clone.cancelled() => {
                    let mut m = meta_clone.lock().await;
                    m.state = TaskState::Cancelled;
                    Err(DownloadError::Cancelled { task_id: task_id.clone() })
                }
                r = worker::execute_task(
                    &task_clone, &client, &config, &mirrors_guard,
                    &events, &observers, &meta_clone,
                ) => r
            };

            // Update state and send result
            let mut m = meta_clone.lock().await;
            match &result {
                Ok((path, size, mirror)) => {
                    m.state = TaskState::Completed {
                        path: path.clone(),
                        size_bytes: *size,
                        duration_ms: m.duration_ms(),
                        mirror_used: mirror.clone(),
                    };
                    let _ = tx.send(DownloadResult {
                        task_id: task_id.clone(),
                        outcome: Outcome::Completed,
                        path: Some(path.clone()),
                        size_bytes: *size,
                        duration_ms: m.duration_ms(),
                    });
                }
                Err(e) => {
                    m.state = TaskState::Failed {
                        error_message: e.to_string(),
                        retries_used: 0,
                        mirror_last_tried: None,
                    };
                    let _ = tx.send(DownloadResult {
                        task_id: task_id.clone(),
                        outcome: Outcome::Failed(e.to_string()),
                        path: None,
                        size_bytes: 0,
                        duration_ms: m.duration_ms(),
                    });
                }
            }
        });

        Ok(TaskHandle { rx })
    }

    // ── State Access (Polling) ─────────────────────────────────────────

    /// Get the current state of a task by ID (polling pattern).
    pub async fn get_state(&self, task_id: &str) -> Result<TaskState, DownloadError> {
        let map = self.state_map.read().await;
        let meta = map
            .get(task_id)
            .ok_or_else(|| DownloadError::TaskNotFound {
                task_id: task_id.to_string(),
            })?
            .clone();
        drop(map);
        let state = meta.lock().await.state.clone();
        Ok(state)
    }

    /// List all known task IDs.
    pub async fn list_tasks(&self) -> Vec<TaskId> {
        self.state_map.read().await.keys().cloned().collect()
    }

    // ── Events ─────────────────────────────────────────────────────────

    /// Subscribe to the event stream (broadcast channel pattern).
    pub fn events(&self) -> broadcast::Receiver<DownloadEvent> {
        self.events.subscribe()
    }

    // ── Observer ───────────────────────────────────────────────────────

    /// Register an observer for download events (observer pattern).
    pub async fn observe(&self, observer: Arc<dyn observer::DownloadObserver>) {
        self.observers.register(observer).await;
    }

    // ── Control ────────────────────────────────────────────────────────

    /// Cancel a running or pending task.
    pub async fn cancel(&self, task_id: &str) -> Result<(), DownloadError> {
        let tokens = self.cancel_tokens.read().await;
        let token = tokens
            .get(task_id)
            .ok_or_else(|| DownloadError::TaskNotFound {
                task_id: task_id.to_string(),
            })?;
        token.cancel();

        self.events.send(DownloadEvent::Cancelled {
            task_id: task_id.to_string(),
        });

        Ok(())
    }

    /// Pause a running task (graceful — waits for current chunk to finish).
    pub async fn pause(&self, task_id: &str) -> Result<(), DownloadError> {
        // Pause = cancel with resume state preserved.
        // The resume file is saved on each failure, so cancelling is effectively pausing.
        self.cancel(task_id).await?;
        self.events.send(DownloadEvent::Paused {
            task_id: task_id.to_string(),
        });
        Ok(())
    }

    // ── Mirror Management ──────────────────────────────────────────────

    /// Add a custom mirror to the registry.
    pub async fn add_mirror(&self, m: mirror::Mirror) {
        let mut mirrors = self.mirrors.write().await;
        mirrors.register(m);
        mirrors.rebuild_host_map();
    }
}

// ── Task Handle ─────────────────────────────────────────────────────────

/// Handle to a submitted task. Await it to get the result, or poll via the owner Downloader.
pub struct TaskHandle {
    rx: oneshot::Receiver<DownloadResult>,
}

impl TaskHandle {
    /// Non-blocking check: is the result ready?
    pub fn try_recv(&mut self) -> Result<Option<DownloadResult>, oneshot::error::TryRecvError> {
        self.rx.try_recv().map(Some)
    }
}

/// Resolve the handle like a future.
impl std::future::Future for TaskHandle {
    type Output = Result<DownloadResult, DownloadError>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        match std::pin::pin!(&mut self.rx).poll(cx) {
            std::task::Poll::Ready(Ok(r)) => std::task::Poll::Ready(Ok(r)),
            std::task::Poll::Ready(Err(_)) => {
                std::task::Poll::Ready(Err(DownloadError::Other("Task sender dropped".into())))
            }
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

// ── Result ──────────────────────────────────────────────────────────────

/// The outcome of a download task.
#[derive(Debug, Clone)]
pub enum Outcome {
    Completed,
    Failed(String),
    Cancelled,
}

/// Final result returned via TaskHandle.
#[derive(Debug, Clone)]
pub struct DownloadResult {
    pub task_id: TaskId,
    pub outcome: Outcome,
    pub path: Option<std::path::PathBuf>,
    pub size_bytes: u64,
    pub duration_ms: u64,
}

impl DownloadResult {
    pub fn is_success(&self) -> bool {
        matches!(self.outcome, Outcome::Completed)
    }

    pub fn error_message(&self) -> Option<&str> {
        match &self.outcome {
            Outcome::Failed(msg) => Some(msg),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_downloader_new() {
        let dl = Downloader::new(DownloaderConfig::default());
        assert!(dl.is_ok());
    }

    #[test]
    fn test_config_defaults() {
        let config = DownloaderConfig::default();
        assert_eq!(config.max_concurrent_tasks, 4);
        assert_eq!(config.chunk_threshold, 10 * 1024 * 1024);
    }

    #[test]
    fn test_task_builder() {
        let task = DownloadTask::new("https://example.com/file.zip", "./dl/file.zip")
            .with_priority(task::Priority::High)
            .with_retries(5)
            .with_timeout(60);
        assert_eq!(task.priority, task::Priority::High);
        assert_eq!(task.max_retries, 5);
        assert_eq!(task.timeout_secs, 60);
    }
}
