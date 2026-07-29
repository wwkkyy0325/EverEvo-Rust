//! Linux/macOS resource limits — rlimit (setrlimit/getrlimit).
//!
//! Windows uses Job Objects (job_object.rs). This module provides
//! equivalent functionality on Unix via POSIX rlimit.

use crate::limits::ResourceLimits;

/// Apply resource limits to the current process.
/// On Linux: uses `setrlimit` for memory (RLIMIT_AS) and CPU (RLIMIT_CPU).
/// On macOS: same syscalls via libc.
pub fn apply_limits(limits: &ResourceLimits) -> Result<(), crate::error::SandboxError> {
    #[cfg(unix)]
    {
        use crate::error::SandboxError;

        // Memory limit (RLIMIT_AS — virtual memory)
        if let Some(mb) = limits.memory_mb {
            let bytes = mb * 1024 * 1024;
            set_rlimit(libc::RLIMIT_AS, bytes, bytes)
                .map_err(|e| SandboxError::JobObject(format!("RLIMIT_AS: {e}")))?;
        }

        // CPU time limit (RLIMIT_CPU)
        let cpu_seconds = limits.timeout_secs + 5; // 5s grace for cleanup
        set_rlimit(libc::RLIMIT_CPU, cpu_seconds, cpu_seconds)
            .map_err(|e| SandboxError::JobObject(format!("RLIMIT_CPU: {e}")))?;

        // Process limit (RLIMIT_NPROC)
        if let Some(nproc) = limits.max_processes {
            set_rlimit(libc::RLIMIT_NPROC, nproc as u64, nproc as u64)
                .map_err(|e| SandboxError::JobObject(format!("RLIMIT_NPROC: {e}")))?;
        }

        // File size limit (RLIMIT_FSIZE)
        if let Some(fsize_mb) = limits.max_file_size_mb {
            let fsize = fsize_mb * 1024 * 1024;
            set_rlimit(libc::RLIMIT_FSIZE, fsize, fsize)
                .map_err(|e| SandboxError::JobObject(format!("RLIMIT_FSIZE: {e}")))?;
        }

        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = limits;
        Ok(())
    }
}

#[cfg(unix)]
fn set_rlimit(resource: libc::__rlimit_resource_t, soft: u64, hard: u64) -> Result<(), String> {
    let rlim = libc::rlimit {
        rlim_cur: soft,
        rlim_max: hard,
    };
    let rc = unsafe { libc::setrlimit(resource, &rlim) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().to_string())
    }
}
