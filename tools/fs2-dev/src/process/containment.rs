use std::io;
use std::process::{Child, Command};

#[cfg(unix)]
pub(super) struct ProcessContainment {
    process_group: Option<i32>,
}

#[cfg(unix)]
impl ProcessContainment {
    pub(super) const METHOD: &'static str = "unix-process-group";

    pub(super) fn configure(command: &mut Command) -> io::Result<Self> {
        use std::os::unix::process::CommandExt as _;

        // This is same-group lifecycle containment, not an adversarial
        // sandbox. A child can escape with setsid() or another process-group
        // change; hostile subjects require a stronger isolation primitive.
        command.process_group(0);
        Ok(Self {
            process_group: None,
        })
    }

    pub(super) fn attach(&mut self, child: &Child) -> io::Result<()> {
        self.process_group = Some(i32::try_from(child.id()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "child process id is too large")
        })?);
        Ok(())
    }

    pub(super) fn terminate(&self, child: &mut Child) -> io::Result<()> {
        let Some(process_group) = self.process_group else {
            return child.kill();
        };
        // SAFETY: the child was placed in a new process group whose ID is its
        // process ID. A negative PID targets that group without affecting the
        // benchmark runner's process group.
        let result = unsafe { libc::kill(-process_group, libc::SIGKILL) };
        if result == 0 {
            Ok(())
        } else {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                Ok(())
            } else {
                Err(error)
            }
        }
    }
}

#[cfg(windows)]
pub(super) struct ProcessContainment {
    job: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl ProcessContainment {
    pub(super) const METHOD: &'static str = "windows-job-object-suspended";

    pub(super) fn configure(command: &mut Command) -> io::Result<Self> {
        use std::mem::{size_of, zeroed};
        use std::os::windows::process::CommandExt as _;
        use std::ptr;
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::JobObjects::{
            CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };

        // The suspended child cannot create descendants before the job owns it.
        // Assignment and resume happen in `attach`.
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_SUSPENDED);

        // SAFETY: null security attributes and name request an unnamed job with
        // default security. The returned handle is owned by this value.
        let job = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        if job.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the structure is plain Windows API data and is fully sized for
        // JobObjectExtendedLimitInformation.
        let mut information: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        information.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let information_size = u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
            .map_err(|_| io::Error::other("Windows job information size exceeds u32"))?;
        // SAFETY: `job` is valid and `information` points to the declared
        // structure for the duration of the call.
        let configured = unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&raw const information).cast(),
                information_size,
            )
        };
        if configured == 0 {
            let error = io::Error::last_os_error();
            // SAFETY: `job` is an owned valid handle.
            unsafe { CloseHandle(job) };
            return Err(error);
        }
        Ok(Self { job })
    }

    pub(super) fn attach(&mut self, child: &Child) -> io::Result<()> {
        use std::os::windows::io::AsRawHandle as _;
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;

        // SAFETY: both handles are valid for the duration of the call. The job
        // remains owned by this value until process execution completes.
        if unsafe { AssignProcessToJobObject(self.job, child.as_raw_handle().cast()) } == 0 {
            Err(io::Error::last_os_error())
        } else {
            resume_process(child.id())
        }
    }

    pub(super) fn wait_for_exit(&self, timeout: std::time::Duration) -> io::Result<()> {
        let timeout_ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
        let result = unsafe {
            windows_sys::Win32::System::Threading::WaitForSingleObject(self.job, timeout_ms)
        };

        match result {
            windows_sys::Win32::Foundation::WAIT_OBJECT_0 => Ok(()),
            windows_sys::Win32::Foundation::WAIT_TIMEOUT => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "Windows Job still has associated processes after {} ms",
                    timeout.as_millis()
                ),
            )),
            windows_sys::Win32::Foundation::WAIT_FAILED => Err(io::Error::last_os_error()),
            result => Err(io::Error::other(format!(
                "unexpected Windows Job wait result {result:#x}"
            ))),
        }
    }

    pub(super) fn terminate(&self, _child: &mut Child) -> io::Result<()> {
        use windows_sys::Win32::System::JobObjects::TerminateJobObject;

        // SAFETY: `self.job` is a valid owned job handle.
        if unsafe { TerminateJobObject(self.job, 1) } == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

#[cfg(windows)]
fn resume_process(process_id: u32) -> io::Result<()> {
    use std::mem::{size_of, zeroed};
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

    // SAFETY: the snapshot handle is checked before use and closed below.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let result = (|| {
        // SAFETY: THREADENTRY32 is plain Windows API data; dwSize is set before use.
        let mut entry: THREADENTRY32 = unsafe { zeroed() };
        entry.dwSize = u32::try_from(size_of::<THREADENTRY32>())
            .map_err(|_| io::Error::other("Windows thread entry size exceeds u32"))?;
        // SAFETY: snapshot and entry are valid for enumeration.
        if unsafe { Thread32First(snapshot, &mut entry) } == 0 {
            return Err(io::Error::last_os_error());
        }
        loop {
            if entry.th32OwnerProcessID == process_id {
                // SAFETY: the thread ID came from the live snapshot entry.
                let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                if thread.is_null() {
                    return Err(io::Error::last_os_error());
                }
                // SAFETY: thread is an owned handle opened with suspend/resume access.
                let resumed = unsafe { ResumeThread(thread) };
                let resume_error = (resumed == u32::MAX).then(io::Error::last_os_error);
                // SAFETY: thread is owned and closed exactly once.
                unsafe { CloseHandle(thread) };
                return if let Some(error) = resume_error {
                    Err(error)
                } else {
                    Ok(())
                };
            }
            // SAFETY: snapshot and entry remain valid for enumeration.
            if unsafe { Thread32Next(snapshot, &mut entry) } == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "suspended child thread was not found",
                ));
            }
        }
    })();
    // SAFETY: snapshot is owned and closed exactly once.
    unsafe { CloseHandle(snapshot) };
    result
}

#[cfg(windows)]
impl Drop for ProcessContainment {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;

        // SAFETY: `self.job` is owned by this value and is closed exactly once.
        unsafe { CloseHandle(self.job) };
    }
}

#[cfg(not(any(unix, windows)))]
pub(super) struct ProcessContainment;

#[cfg(not(any(unix, windows)))]
impl ProcessContainment {
    pub(super) const METHOD: &'static str = "direct-child";

    pub(super) fn configure(_command: &mut Command) -> io::Result<Self> {
        Ok(Self)
    }

    pub(super) fn attach(&mut self, _child: &Child) -> io::Result<()> {
        Ok(())
    }

    pub(super) fn terminate(&self, child: &mut Child) -> io::Result<()> {
        child.kill()
    }
}
