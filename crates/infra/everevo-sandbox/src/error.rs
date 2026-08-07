//! Sandbox errors.

#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error("No sandbox tier available on this system")]
    NoTierAvailable,

    #[error("Execution timeout: {0}s exceeded")]
    Timeout(u64),

    #[error("Memory limit exceeded: {0} MB")]
    MemoryLimit(u64),

    #[error("Job Object error: {0}")]
    JobObject(String),

    #[error("WSL error: {0}")]
    Wsl(String),

    #[error("Process spawn failed: {0}")]
    Spawn(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<SandboxError> for everevo_core::EverEvoError {
    fn from(e: SandboxError) -> Self {
        everevo_core::EverEvoError::Sandbox(e.to_string())
    }
}
