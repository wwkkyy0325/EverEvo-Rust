//! Linux/macOS resource limits — cgroups + rlimit.
//! Stub — Windows is the primary target for Phase 1-2.

use crate::limits::ResourceLimits;

pub fn apply_limits(_limits: &ResourceLimits) -> Result<(), crate::error::SandboxError> {
    // TODO (Phase 3): cgroups v2 memory/CPU limits on Linux
    Ok(())
}
