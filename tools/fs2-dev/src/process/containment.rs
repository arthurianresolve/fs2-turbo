use std::io;
use std::process::{Child, Command};

#[cfg(unix)]
pub(super) struct ProcessContainment {
    process_group: Option<i32>,
}

#[cfg(unix)]
impl ProcessContainment {
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
        self.process_group = Some(super::checked_process_id(child.id())?);
        Ok(())
    }

    pub(super) fn terminate(&self, child: &mut Child) -> io::Result<()> {
        let Some(process_group) = self.process_group else {
            return child.kill();
        };
        // SAFETY: the child was placed in a new process group whose ID is its
        // process ID. A negative PID targets that group without affecting the
        // repository tool's process group.
        let result = unsafe { libc::kill(-process_group, libc::SIGKILL) };
        group_termination_result((result != 0).then(io::Error::last_os_error))
    }
}

#[cfg(unix)]
fn group_termination_result(error: Option<io::Error>) -> io::Result<()> {
    match error {
        None => Ok(()),
        Some(error) if error.raw_os_error() == Some(libc::ESRCH) => Ok(()),
        Some(error) => Err(error),
    }
}

#[cfg(all(test, unix))]
mod unix_tests {
    use super::*;

    #[test]
    fn termination_accepts_only_success_or_an_absent_group() {
        group_termination_result(None).unwrap();
        group_termination_result(Some(io::Error::from_raw_os_error(libc::ESRCH))).unwrap();
        for errno in [libc::EPERM, libc::EIO, libc::EINTR] {
            let error =
                group_termination_result(Some(io::Error::from_raw_os_error(errno))).unwrap_err();
            assert_eq!(error.raw_os_error(), Some(errno));
        }
    }

    #[test]
    fn unattached_containment_terminates_only_its_owned_child() {
        let mut command = Command::new("sh");
        command.args(["-c", "exit 0"]);
        let containment = ProcessContainment::configure(&mut command).unwrap();
        let mut child = command.spawn().unwrap();
        let termination = containment.terminate(&mut child);
        let reaped = child.wait();
        termination.unwrap();
        reaped.unwrap();
    }
}

#[cfg(windows)]
pub(super) struct ProcessContainment {
    job: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl ProcessContainment {
    pub(super) fn configure(command: &mut Command) -> io::Result<Self> {
        use std::mem::{size_of, zeroed};
        use std::os::windows::process::CommandExt as _;
        use std::ptr;
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
        let job = std::ptr::NonNull::new(job)
            .ok_or_else(io::Error::last_os_error)?
            .as_ptr();
        let containment = Self { job };
        // SAFETY: the structure is plain Windows API data and is fully sized for
        // JobObjectExtendedLimitInformation.
        let mut information: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        information.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let information_size = const {
            assert!(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() <= u32::MAX as usize);
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32
        };
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
        check_win32_result(configured)?;
        Ok(containment)
    }

    pub(super) fn attach(&mut self, child: &Child) -> io::Result<()> {
        use std::os::windows::io::AsRawHandle as _;
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;

        // SAFETY: both handles are valid for the duration of the call. The job
        // remains owned by this value until process execution completes.
        check_win32_result(unsafe {
            AssignProcessToJobObject(self.job, child.as_raw_handle().cast())
        })?;
        resume_process(child.id())
    }

    pub(super) fn wait_for_exit(&self, timeout: std::time::Duration) -> io::Result<()> {
        let timeout_ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
        let result = unsafe {
            windows_sys::Win32::System::Threading::WaitForSingleObject(self.job, timeout_ms)
        };

        job_wait_result(result, timeout)
    }

    pub(super) fn terminate(&self, _child: &mut Child) -> io::Result<()> {
        use windows_sys::Win32::System::JobObjects::TerminateJobObject;

        // SAFETY: `self.job` is a valid owned job handle.
        check_win32_result(unsafe { TerminateJobObject(self.job, 1) })
    }
}

#[cfg(windows)]
fn check_win32_result(result: i32) -> io::Result<()> {
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn job_wait_result(result: u32, timeout: std::time::Duration) -> io::Result<()> {
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
    check_win32_result(i32::from(snapshot != INVALID_HANDLE_VALUE))?;
    let result = (|| {
        // SAFETY: THREADENTRY32 is plain Windows API data; dwSize is set before use.
        let mut entry: THREADENTRY32 = unsafe { zeroed() };
        entry.dwSize = const {
            assert!(size_of::<THREADENTRY32>() <= u32::MAX as usize);
            size_of::<THREADENTRY32>() as u32
        };
        // SAFETY: snapshot and entry are valid for enumeration.
        check_win32_result(unsafe { Thread32First(snapshot, &mut entry) })?;
        loop {
            if entry.th32OwnerProcessID == process_id {
                // SAFETY: the thread ID came from the live snapshot entry.
                let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                check_win32_result(i32::from(!thread.is_null()))?;
                // SAFETY: thread is an owned handle opened with suspend/resume access.
                let resumed = unsafe { ResumeThread(thread) };
                let result = check_win32_result(i32::from(resumed != u32::MAX));
                // SAFETY: thread is owned and closed exactly once. Preserve the prior error.
                unsafe { CloseHandle(thread) };
                return result;
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

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::time::Duration;
    use windows_sys::Win32::Foundation::{
        ERROR_ACCESS_DENIED, GetLastError, SetLastError, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
    };

    use wait_timeout::ChildExt as _;

    struct ReapedChild(Child);

    impl Drop for ReapedChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait_timeout(Duration::from_secs(5));
        }
    }

    #[test]
    fn job_assignment_failure_reaps_the_rejected_owned_child() {
        use std::mem::{size_of, zeroed};
        use std::process::Stdio;
        use windows_sys::Win32::System::JobObjects::{
            JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };

        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "process::tests::windows_sleep_fixture",
                "--nocapture",
            ])
            .env("FS2_DEV_SLEEP_FIXTURE_SECONDS", "30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut containment = ProcessContainment::configure(&mut command).unwrap();
        // SAFETY: this is fully initialized, fixed-size Windows API storage.
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        limits.BasicLimitInformation.LimitFlags =
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
        limits.BasicLimitInformation.ActiveProcessLimit = 1;
        let size = const {
            assert!(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() <= u32::MAX as usize);
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32
        };
        // SAFETY: only this test's owned, empty Job receives the limit.
        check_win32_result(unsafe {
            SetInformationJobObject(
                containment.job,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                size,
            )
        })
        .unwrap();

        let mut first = ReapedChild(command.spawn().unwrap());
        containment.attach(&first.0).unwrap();
        let mut rejected = ReapedChild(command.spawn().unwrap());
        let assignment = containment.attach(&rejected.0);
        let (rejected_status, cleanup_error) =
            super::super::terminate_and_reap(&containment, &mut rejected.0);
        let first_status = first.0.wait_timeout(Duration::from_secs(5));
        let drained = containment.wait_for_exit(Duration::from_secs(5));
        let outcome = format!("assignment={assignment:?}; cleanup={cleanup_error:?}");

        // All normal-path cleanup completes before checking the rejection.
        assert!(assignment.is_err(), "{outcome}");
        assert!(rejected_status.is_some(), "{outcome}");
        assert!(cleanup_error.is_none(), "{outcome}");
        assert!(first_status.unwrap().is_some());
        drained.unwrap();
    }

    #[test]
    fn native_result_checks_preserve_errors_and_missing_threads_fail_closed() {
        check_win32_result(1).unwrap();
        check_win32_result(-1).unwrap();
        // SAFETY: Windows last-error storage is local to this test thread.
        let previous = unsafe { GetLastError() };
        unsafe { SetLastError(ERROR_ACCESS_DENIED) };
        let error = check_win32_result(0).unwrap_err();
        unsafe { SetLastError(previous) };
        assert_eq!(error.raw_os_error(), Some(ERROR_ACCESS_DENIED as i32));
        assert_eq!(
            resume_process(u32::MAX).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn job_wait_result_accepts_only_verified_completion() {
        job_wait_result(WAIT_OBJECT_0, Duration::ZERO).unwrap();
        let error = job_wait_result(WAIT_TIMEOUT, Duration::from_millis(17)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(error.to_string().contains("after 17 ms"));
        let error = job_wait_result(0x42, Duration::ZERO).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert!(error.to_string().contains("0x42"));
    }

    #[test]
    fn failed_wait_preserves_the_native_error_code() {
        // SAFETY: last-error state is confined to this test thread.
        let previous = unsafe { GetLastError() };
        unsafe { SetLastError(ERROR_ACCESS_DENIED) };
        let error = job_wait_result(WAIT_FAILED, Duration::ZERO).unwrap_err();
        unsafe { SetLastError(previous) };
        assert_eq!(error.raw_os_error(), Some(ERROR_ACCESS_DENIED as i32));
    }
}
