//! Resource limits for sandboxed processes.

#[derive(Debug, Clone)]
pub struct ResourceLimits {
    /// Maximum execution time in seconds.
    pub timeout_secs: u64,
    /// Maximum memory in MB.
    pub memory_mb: Option<u64>,
    /// Maximum number of child processes.
    pub max_processes: Option<u32>,
    /// Whether network is allowed (outbound).
    pub network_allowed: bool,
    /// Maximum file size in MB for writes.
    pub max_file_size_mb: Option<u64>,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            timeout_secs: 30,
            memory_mb: Some(512),
            max_processes: Some(1),
            network_allowed: true,
            max_file_size_mb: Some(100),
        }
    }
}

impl ResourceLimits {
    pub fn strict() -> Self {
        Self {
            timeout_secs: 10,
            memory_mb: Some(128),
            max_processes: Some(1),
            network_allowed: false,
            max_file_size_mb: Some(10),
        }
    }

    pub fn relaxed() -> Self {
        Self {
            timeout_secs: 300,
            memory_mb: None,
            max_processes: None,
            network_allowed: true,
            max_file_size_mb: None,
        }
    }
}
