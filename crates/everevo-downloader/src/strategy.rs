//! Download strategies — how the download is executed.

/// The download execution strategy for a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadStrategy {
    /// Single HTTP GET with `Range` header support for resume.
    /// Best for: small files (<10 MiB), servers without concurrent connection support.
    Simple,

    /// Split file into N chunks, download concurrently, then assemble.
    /// Best for: large files, servers supporting multiple Range connections.
    Chunked {
        /// Number of concurrent chunk workers.
        concurrency: usize,
    },
}

impl DownloadStrategy {
    /// Choose the best strategy based on file size and configuration.
    pub fn choose(
        file_size: u64,
        task_concurrency: usize,
        engine_concurrency: usize,
        chunk_threshold: u64,
    ) -> Self {
        let use_chunks = task_concurrency > 0
            || (chunk_threshold > 0 && file_size >= chunk_threshold);
        if use_chunks {
            let concurrency = if task_concurrency > 0 {
                task_concurrency
            } else {
                // Auto-scale: 1 chunk per 4 MiB, capped at engine max
                let chunks = (file_size / (4 * 1024 * 1024)).max(2) as usize;
                chunks.min(engine_concurrency)
            };
            Self::Chunked { concurrency }
        } else {
            Self::Simple
        }
    }
}
