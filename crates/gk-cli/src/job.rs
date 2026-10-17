//! Kills a git process together with its children (remote helpers, hooks).
//! On Windows this uses a Job Object; elsewhere it falls back to killing the child.

use std::process::Child;

#[cfg(windows)]
pub struct Job(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
unsafe impl Send for Job {}

#[cfg(windows)]
impl Job {
    pub fn assign(child: &Child) -> Option<Job> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::{AssignProcessToJobObject, CreateJobObjectW};
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return None;
            }
            if AssignProcessToJobObject(job, child.as_raw_handle() as _) == 0 {
                windows_sys::Win32::Foundation::CloseHandle(job);
                return None;
            }
            Some(Job(job))
        }
    }

    pub fn terminate(&self) {
        unsafe {
            windows_sys::Win32::System::JobObjects::TerminateJobObject(self.0, 1);
        }
    }
}

#[cfg(windows)]
impl Drop for Job {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

#[cfg(not(windows))]
pub struct Job;

#[cfg(not(windows))]
impl Job {
    pub fn assign(_child: &Child) -> Option<Job> {
        None
    }
    pub fn terminate(&self) {}
}
