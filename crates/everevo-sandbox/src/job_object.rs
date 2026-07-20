//! Windows Job Objects — process-tree containment + resource limits.
#![allow(unsafe_code)] // Windows FFI requires unsafe

/// Create a Windows Job Object with KILL_ON_JOB_CLOSE.
pub struct JobObject {
    handle: windows_sys::Win32::Foundation::HANDLE,
}

impl JobObject {
    pub fn new() -> Result<Self, crate::error::SandboxError> {
        unsafe {
            let handle = windows_sys::Win32::System::JobObjects::CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() {
                return Err(crate::error::SandboxError::JobObject(format!("CreateJobObjectW failed: {}", std::io::Error::last_os_error())));
            }
            let mut info: windows_sys::Win32::System::JobObjects::JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = windows_sys::Win32::System::JobObjects::JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let ret = windows_sys::Win32::System::JobObjects::SetInformationJobObject(handle, windows_sys::Win32::System::JobObjects::JobObjectExtendedLimitInformation, &info as *const _ as *const std::ffi::c_void, std::mem::size_of_val(&info) as u32);
            if ret == 0 { windows_sys::Win32::Foundation::CloseHandle(handle); return Err(crate::error::SandboxError::JobObject(format!("SetInformationJobObject: {}", std::io::Error::last_os_error()))); }
            Ok(Self { handle })
        }
    }

    pub fn set_memory_limit(&self, bytes: usize) -> Result<(), crate::error::SandboxError> {
        unsafe {
            let mut info: windows_sys::Win32::System::JobObjects::JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = windows_sys::Win32::System::JobObjects::JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | windows_sys::Win32::System::JobObjects::JOB_OBJECT_LIMIT_JOB_MEMORY;
            info.JobMemoryLimit = bytes;
            let ret = windows_sys::Win32::System::JobObjects::SetInformationJobObject(self.handle, windows_sys::Win32::System::JobObjects::JobObjectExtendedLimitInformation, &info as *const _ as *const std::ffi::c_void, std::mem::size_of_val(&info) as u32);
            if ret == 0 { return Err(crate::error::SandboxError::JobObject(format!("set_memory_limit: {}", std::io::Error::last_os_error()))); }
            Ok(())
        }
    }

    #[allow(dead_code)]
    pub fn assign_process(&self, pid: u32) -> Result<(), crate::error::SandboxError> {
        unsafe {
            let h = windows_sys::Win32::System::Threading::OpenProcess(windows_sys::Win32::System::Threading::PROCESS_SET_QUOTA | windows_sys::Win32::System::Threading::PROCESS_TERMINATE, 0, pid);
            if h.is_null() { return Err(crate::error::SandboxError::JobObject(format!("OpenProcess: {}", std::io::Error::last_os_error()))); }
            let ret = windows_sys::Win32::System::JobObjects::AssignProcessToJobObject(self.handle, h);
            windows_sys::Win32::Foundation::CloseHandle(h);
            if ret == 0 { return Err(crate::error::SandboxError::JobObject(format!("AssignProcessToJobObject: {}", std::io::Error::last_os_error()))); }
            Ok(())
        }
    }
}

impl Drop for JobObject {
    fn drop(&mut self) { unsafe { windows_sys::Win32::Foundation::CloseHandle(self.handle); } }
}

unsafe impl Send for JobObject {}
unsafe impl Sync for JobObject {}
