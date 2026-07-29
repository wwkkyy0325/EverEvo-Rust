//! Windows Job Objects — process-tree containment + resource limits.

/// Create a Windows Job Object with KILL_ON_JOB_CLOSE.
pub struct JobObject {
    handle: windows_sys::Win32::Foundation::HANDLE,
}

impl JobObject {
    /// Create a new Job Object. All processes assigned to it are terminated
    /// when the handle is closed (KILL_ON_JOB_CLOSE).
    pub fn new() -> Result<Self, crate::error::SandboxError> {
        // SAFETY: CreateJobObjectW with null name creates an unnamed job object.
        // The returned handle is owned and closed in Drop.
        #[allow(unsafe_code)]
        unsafe {
            let handle = windows_sys::Win32::System::JobObjects::CreateJobObjectW(
                std::ptr::null(),
                std::ptr::null(),
            );
            if handle.is_null() {
                return Err(crate::error::SandboxError::JobObject(format!(
                    "CreateJobObjectW failed: {}",
                    std::io::Error::last_os_error()
                )));
            }
            let mut info: windows_sys::Win32::System::JobObjects::JOBOBJECT_EXTENDED_LIMIT_INFORMATION =
                std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags =
                windows_sys::Win32::System::JobObjects::JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: info is a valid JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            // handle is a valid job object handle from CreateJobObjectW.
            let ret = windows_sys::Win32::System::JobObjects::SetInformationJobObject(
                handle,
                windows_sys::Win32::System::JobObjects::JobObjectExtendedLimitInformation,
                &info as *const _ as *const std::ffi::c_void,
                std::mem::size_of_val(&info) as u32,
            );
            if ret == 0 {
                windows_sys::Win32::Foundation::CloseHandle(handle);
                return Err(crate::error::SandboxError::JobObject(format!(
                    "SetInformationJobObject: {}",
                    std::io::Error::last_os_error()
                )));
            }
            Ok(Self { handle })
        }
    }

    /// Set a memory limit (in bytes) on the job object.
    /// Processes exceeding this limit will be terminated by Windows.
    pub fn set_memory_limit(&self, bytes: usize) -> Result<(), crate::error::SandboxError> {
        // SAFETY: self.handle is a valid job object handle (created in new()).
        // info is a stack-allocated JOBOBJECT_EXTENDED_LIMIT_INFORMATION.
        #[allow(unsafe_code)]
        unsafe {
            let mut info: windows_sys::Win32::System::JobObjects::JOBOBJECT_EXTENDED_LIMIT_INFORMATION =
                std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags =
                windows_sys::Win32::System::JobObjects::JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                    | windows_sys::Win32::System::JobObjects::JOB_OBJECT_LIMIT_JOB_MEMORY;
            info.JobMemoryLimit = bytes;
            let ret = windows_sys::Win32::System::JobObjects::SetInformationJobObject(
                self.handle,
                windows_sys::Win32::System::JobObjects::JobObjectExtendedLimitInformation,
                &info as *const _ as *const std::ffi::c_void,
                std::mem::size_of_val(&info) as u32,
            );
            if ret == 0 {
                return Err(crate::error::SandboxError::JobObject(format!(
                    "set_memory_limit: {}",
                    std::io::Error::last_os_error()
                )));
            }
            Ok(())
        }
    }

    #[allow(dead_code)]
    pub fn assign_process(&self, pid: u32) -> Result<(), crate::error::SandboxError> {
        // SAFETY: OpenProcess with PROCESS_SET_QUOTA | PROCESS_TERMINATE opens
        // an existing process for resource management. The returned handle is
        // immediately closed after AssignProcessToJobObject.
        #[allow(unsafe_code)]
        unsafe {
            let h = windows_sys::Win32::System::Threading::OpenProcess(
                windows_sys::Win32::System::Threading::PROCESS_SET_QUOTA
                    | windows_sys::Win32::System::Threading::PROCESS_TERMINATE,
                0,
                pid,
            );
            if h.is_null() {
                return Err(crate::error::SandboxError::JobObject(format!(
                    "OpenProcess: {}",
                    std::io::Error::last_os_error()
                )));
            }
            let ret =
                windows_sys::Win32::System::JobObjects::AssignProcessToJobObject(self.handle, h);
            windows_sys::Win32::Foundation::CloseHandle(h);
            if ret == 0 {
                return Err(crate::error::SandboxError::JobObject(format!(
                    "AssignProcessToJobObject: {}",
                    std::io::Error::last_os_error()
                )));
            }
            Ok(())
        }
    }
}

impl Drop for JobObject {
    fn drop(&mut self) {
        // SAFETY: self.handle is the handle from CreateJobObjectW.
        // Closing it triggers KILL_ON_JOB_CLOSE — all assigned processes are terminated.
        #[allow(unsafe_code)]
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}

// SAFETY: JobObject owns a Windows HANDLE which is safe to send between threads.
// Windows HANDLEs are process-scoped; the Send+Sync impl is valid because all
// operations on the handle go through safe wrapper methods that take &self.
#[allow(unsafe_code)]
unsafe impl Send for JobObject {}
#[allow(unsafe_code)]
unsafe impl Sync for JobObject {}
