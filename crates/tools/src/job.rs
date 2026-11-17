//! Windows Job Objects: every process a shell starts, however indirectly, joins the shell's job,
//! so terminating the job kills them all. `taskkill /T` can't do this for Git Bash: the MSYS
//! runtime emulates fork/exec in a way that breaks the parent-child chain it follows.

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};

pub struct Job(HANDLE);

// The handle is only used through thread-safe Win32 calls.
unsafe impl Send for Job {}
unsafe impl Sync for Job {}

impl Job {
    /// A job that also kills its processes when the last handle to it closes (our process exits).
    pub fn new() -> windows::core::Result<Self> {
        unsafe {
            let handle = CreateJobObjectW(None, None)?;
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let job = Self(handle);
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )?;
            Ok(job)
        }
    }

    pub fn assign(&self, pid: u32) -> windows::core::Result<()> {
        unsafe {
            let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, false, pid)?;
            let result = AssignProcessToJobObject(self.0, process);
            let _ = CloseHandle(process);
            result
        }
    }

    /// Puts a process we hold a handle to into the job (the sandbox's spawn assigns it while it
    /// is still suspended, so nothing it starts runs outside the job).
    pub fn assign_handle(&self, process: HANDLE) -> windows::core::Result<()> {
        // SAFETY: both handles are open: the job is ours until `Drop`, `process` the caller's.
        unsafe { AssignProcessToJobObject(self.0, process) }
    }

    pub fn terminate(&self) {
        unsafe {
            let _ = TerminateJobObject(self.0, 1);
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        unsafe {
            let _ = TerminateJobObject(self.0, 1);
            let _ = CloseHandle(self.0);
        }
    }
}
