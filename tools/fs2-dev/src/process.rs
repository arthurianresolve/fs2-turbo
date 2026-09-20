use std::ffi::{OsStr, OsString};
#[cfg(all(test, unix))]
use std::fs;
#[cfg(windows)]
use std::fs::File;
use std::io::{Read as _, Seek as _};
#[cfg(all(test, unix))]
use std::path::Path;
#[cfg(windows)]
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Output, Stdio};
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

use serde::Serialize;
use sha2::{Digest as _, Sha256};
use wait_timeout::ChildExt;

use crate::{Result, invalid_data, lower_hex};

mod containment;
#[cfg(windows)]
mod windows_security;

use containment::ProcessContainment;

const DEFAULT_PROCESS_TIMEOUT: Duration = Duration::from_secs(3_600);
const MAX_PROCESS_TIMEOUT_SECONDS: u64 = 86_400;
const TERMINATION_REAP_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(unix)]
const PROCESS_GROUP_EXIT_POLL_INTERVAL: Duration = Duration::from_millis(10);

pub(crate) fn cargo() -> Command {
    Command::new(cargo_program(std::env::var_os("CARGO")))
}

fn cargo_program(value: Option<OsString>) -> OsString {
    match value {
        Some(program) => program,
        None => OsString::from("cargo"),
    }
}

pub(crate) fn run(command: &mut Command, label: &str) -> Result<()> {
    println!("+ {label}");
    let execution = execute(command, process_timeout()?);
    if execution.outcome.succeeded() {
        Ok(())
    } else {
        Err(invalid_data(format!(
            "{label} failed: {}",
            execution.outcome.description()
        )))
    }
}

#[cfg(windows)]
fn capture_root() -> Result<(PathBuf, Vec<File>)> {
    let mut candidates = ["USERPROFILE", "LOCALAPPDATA"]
        .into_iter()
        .filter_map(std::env::var_os)
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    if let Ok(executable) = std::env::current_exe()
        && let Some(parent) = executable.parent()
    {
        candidates.push(parent.to_path_buf());
    }

    capture_root_from(candidates)
}

#[cfg(windows)]
fn capture_root_from(
    candidates: impl IntoIterator<Item = PathBuf>,
) -> Result<(PathBuf, Vec<File>)> {
    let mut rejected = Vec::new();
    for root in candidates {
        // Reject UNC and mapped roots before dereferencing the candidate path.
        if !root.is_absolute() || !windows_security::is_local_fixed_drive(&root) {
            continue;
        }
        let mut guard = match windows_security::guard_directory_ancestry(&root) {
            Ok(guard) => guard,
            Err(error) => {
                rejected.push(format!("{}: {error}", root.display()));
                continue;
            }
        };
        let temporary_root = root.join(".fs2-secure-capture");
        match windows_security::create_or_open_private_directory(&temporary_root) {
            Ok(parent) => {
                guard.push(parent);
                return Ok((temporary_root, guard));
            }
            Err(error) => rejected.push(format!("{}: {error}", temporary_root.display())),
        }
    }

    Err(invalid_data(format!(
        "unable to bind capture temp ancestry to a trusted per-user or executable directory: {}",
        rejected.join("; ")
    )))
}

pub(crate) fn capture(command: &mut Command, label: &str) -> Result<Output> {
    capture_with_backend(command, label, &mut NativeCaptureBackend)
}

struct CaptureDirectory {
    // Fields drop in declaration order: release authority handles before removing the directory.
    #[cfg(windows)]
    _guard: Vec<std::fs::File>,
    temporary: tempfile::TempDir,
}

trait CaptureBackend {
    #[cfg(windows)]
    fn root(&mut self) -> Result<(std::path::PathBuf, Vec<std::fs::File>)>;
    #[cfg(windows)]
    fn temporary_in(&mut self, root: &std::path::Path) -> Result<tempfile::TempDir>;
    #[cfg(windows)]
    fn harden(&mut self, path: &std::path::Path) -> Result<std::fs::File>;
    #[cfg(not(windows))]
    fn temporary(&mut self) -> Result<tempfile::TempDir>;
    fn file(&mut self, path: &std::path::Path) -> Result<std::fs::File>;
    fn clone_file(&mut self, file: &std::fs::File) -> std::io::Result<std::fs::File>;
    fn rewind(&mut self, file: &mut std::fs::File) -> std::io::Result<()>;
    fn read_to_end(
        &mut self,
        file: &mut std::fs::File,
        bytes: &mut Vec<u8>,
    ) -> std::io::Result<usize>;
}

struct NativeCaptureBackend;

impl CaptureBackend for NativeCaptureBackend {
    #[cfg(windows)]
    fn root(&mut self) -> Result<(std::path::PathBuf, Vec<std::fs::File>)> {
        capture_root()
    }

    #[cfg(windows)]
    fn temporary_in(&mut self, root: &std::path::Path) -> Result<tempfile::TempDir> {
        tempfile::Builder::new()
            .prefix("fs2-capture-")
            .tempdir_in(root)
            .map_err(Into::into)
    }

    #[cfg(windows)]
    fn harden(&mut self, path: &std::path::Path) -> Result<std::fs::File> {
        windows_security::harden_new_private_directory(path)
    }

    #[cfg(not(windows))]
    fn temporary(&mut self) -> Result<tempfile::TempDir> {
        tempfile::tempdir().map_err(Into::into)
    }

    fn file(&mut self, path: &std::path::Path) -> Result<std::fs::File> {
        tempfile::tempfile_in(path).map_err(Into::into)
    }

    fn clone_file(&mut self, file: &std::fs::File) -> std::io::Result<std::fs::File> {
        file.try_clone()
    }

    fn rewind(&mut self, file: &mut std::fs::File) -> std::io::Result<()> {
        file.rewind()
    }

    fn read_to_end(
        &mut self,
        file: &mut std::fs::File,
        bytes: &mut Vec<u8>,
    ) -> std::io::Result<usize> {
        file.read_to_end(bytes)
    }
}

fn capture_directory(backend: &mut dyn CaptureBackend) -> Result<CaptureDirectory> {
    #[cfg(windows)]
    {
        let (root, guard) = backend.root()?;
        let temporary = capture_stage(backend.temporary_in(&root), "create capture directory")?;
        let mut directory = CaptureDirectory {
            _guard: guard,
            temporary,
        };
        // The parent is already private; validate and retain the randomized child before file I/O.
        directory._guard.push(capture_stage(
            backend.harden(directory.temporary.path()),
            "harden capture directory",
        )?);
        Ok(directory)
    }
    #[cfg(not(windows))]
    {
        Ok(CaptureDirectory {
            temporary: backend.temporary()?,
        })
    }
}

fn capture_with_backend(
    command: &mut Command,
    label: &str,
    backend: &mut dyn CaptureBackend,
) -> Result<Output> {
    let directory = capture_directory(backend)?;
    let mut stdout_file = capture_stage(
        backend.file(directory.temporary.path()),
        "create secure stdout capture",
    )?;
    let mut stderr_file = capture_stage(
        backend.file(directory.temporary.path()),
        "create secure stderr capture",
    )?;
    let result = (|| {
        command
            .stdout(Stdio::from(backend.clone_file(&stdout_file)?))
            .stderr(Stdio::from(backend.clone_file(&stderr_file)?));
        let execution = execute(command, process_timeout()?);
        backend.rewind(&mut stdout_file)?;
        backend.rewind(&mut stderr_file)?;
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        backend.read_to_end(&mut stdout_file, &mut stdout)?;
        backend.read_to_end(&mut stderr_file, &mut stderr)?;
        captured_output(execution, label, stdout, stderr)
    })();
    // Command retains its configured handles after spawn, including on failure.
    // Release those copies before the files and private directory are dropped.
    command.stdout(Stdio::null()).stderr(Stdio::null());
    result
}

fn capture_stage<T>(result: Result<T>, operation: &str) -> Result<T> {
    match result {
        Ok(value) => Ok(value),
        Err(error) => Err(invalid_data(format!("unable to {operation}: {error}"))),
    }
}

fn captured_output(
    execution: Execution,
    label: &str,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
) -> Result<Output> {
    if execution.outcome.succeeded() {
        let Some(status) = execution.status else {
            return Err(invalid_data("successful process has no exit status"));
        };
        Ok(Output {
            status,
            stdout,
            stderr,
        })
    } else {
        Err(invalid_data(format!(
            "{label} failed: {}\nstdout:\n{}\nstderr:\n{}",
            execution.outcome.description(),
            String::from_utf8_lossy(&stdout).trim(),
            String::from_utf8_lossy(&stderr).trim()
        )))
    }
}

pub(crate) fn toolchain_key() -> Result<String> {
    let identity = if let Some(toolchain) = std::env::var_os("RUSTUP_TOOLCHAIN") {
        display_os(&toolchain)
    } else {
        let output = capture(Command::new("rustc").arg("-vV"), "rustc -vV")?;
        String::from_utf8(output.stdout)?
    };
    Ok(toolchain_key_for_identity(&identity))
}

fn toolchain_key_for_identity(identity: &str) -> String {
    lower_hex(Sha256::digest(identity.as_bytes()))
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub(crate) enum ProcessOutcome {
    Exited {
        code: i32,
    },
    Terminated {
        detail: String,
    },
    TimedOut {
        timeout_ms: u128,
        reaped: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        kill_error: Option<String>,
    },
    SpawnFailed {
        error: String,
    },
    ContainmentFailed {
        error: String,
    },
    RunnerFailed {
        error: String,
    },
}

impl ProcessOutcome {
    fn succeeded(&self) -> bool {
        matches!(self, Self::Exited { code: 0 })
    }

    fn description(&self) -> String {
        match self {
            Self::Exited { code } => format!("native exit {code}"),
            Self::Terminated { detail } => format!("process terminated: {detail}"),
            Self::TimedOut {
                timeout_ms,
                reaped,
                kill_error,
            } => match kill_error {
                Some(error) => {
                    format!("process timed out after {timeout_ms} ms; kill failed: {error}")
                }
                None => format!("process timed out after {timeout_ms} ms; reaped={reaped}"),
            },
            Self::SpawnFailed { error } => format!("process spawn failed: {error}"),
            Self::ContainmentFailed { error } => {
                format!("process containment failed: {error}")
            }
            Self::RunnerFailed { error } => format!("process runner failed: {error}"),
        }
    }
}

struct Execution {
    outcome: ProcessOutcome,
    status: Option<ExitStatus>,
}

enum ChildObservation {
    Exited(Option<ExitStatus>),
    TimedOut,
}

trait ProcessBackend {
    type Containment;
    type Child;

    fn configure(&mut self, command: &mut Command) -> std::io::Result<Self::Containment>;
    fn spawn(&mut self, command: &mut Command) -> std::io::Result<Self::Child>;
    fn attach(
        &mut self,
        containment: &mut Self::Containment,
        child: &Self::Child,
    ) -> std::io::Result<()>;
    fn observe(
        &mut self,
        child: &mut Self::Child,
        timeout: Duration,
    ) -> std::io::Result<ChildObservation>;
    fn complete(
        &mut self,
        containment: &Self::Containment,
        child: &mut Self::Child,
        status: Option<ExitStatus>,
    ) -> (Option<ExitStatus>, Option<String>);
    fn cleanup(
        &mut self,
        containment: &Self::Containment,
        child: &mut Self::Child,
    ) -> (Option<ExitStatus>, Option<String>);
}

struct NativeProcessBackend;

impl ProcessBackend for NativeProcessBackend {
    type Containment = ProcessContainment;
    type Child = std::process::Child;

    fn configure(&mut self, command: &mut Command) -> std::io::Result<Self::Containment> {
        ProcessContainment::configure(command)
    }

    fn spawn(&mut self, command: &mut Command) -> std::io::Result<Self::Child> {
        command.spawn()
    }

    fn attach(
        &mut self,
        containment: &mut Self::Containment,
        child: &Self::Child,
    ) -> std::io::Result<()> {
        containment.attach(child)
    }

    fn observe(
        &mut self,
        child: &mut Self::Child,
        timeout: Duration,
    ) -> std::io::Result<ChildObservation> {
        observe_child_exit(child, timeout)
    }

    fn complete(
        &mut self,
        containment: &Self::Containment,
        child: &mut Self::Child,
        status: Option<ExitStatus>,
    ) -> (Option<ExitStatus>, Option<String>) {
        complete_observed_exit(containment, child, status)
    }

    fn cleanup(
        &mut self,
        containment: &Self::Containment,
        child: &mut Self::Child,
    ) -> (Option<ExitStatus>, Option<String>) {
        terminate_and_reap(containment, child)
    }
}

fn execute(command: &mut Command, timeout: Duration) -> Execution {
    execute_with(command, timeout, &mut NativeProcessBackend)
}

fn execute_with(
    command: &mut Command,
    timeout: Duration,
    backend: &mut impl ProcessBackend,
) -> Execution {
    let mut containment = match backend.configure(command) {
        Ok(containment) => containment,
        Err(error) => {
            return Execution {
                outcome: ProcessOutcome::ContainmentFailed {
                    error: error.to_string(),
                },
                status: None,
            };
        }
    };
    let mut child = match backend.spawn(command) {
        Ok(child) => child,
        Err(error) => {
            return Execution {
                outcome: ProcessOutcome::SpawnFailed {
                    error: error.to_string(),
                },
                status: None,
            };
        }
    };
    if let Err(error) = backend.attach(&mut containment, &child) {
        let (status, cleanup_error) = backend.cleanup(&containment, &mut child);
        let error = match cleanup_error {
            Some(cleanup) => format!("{error}; cleanup failed: {cleanup}"),
            None => error.to_string(),
        };
        return Execution {
            outcome: ProcessOutcome::ContainmentFailed { error },
            status,
        };
    }
    match backend.observe(&mut child, timeout) {
        Ok(ChildObservation::Exited(observed_status)) => {
            let (status, cleanup_error) =
                backend.complete(&containment, &mut child, observed_status);
            finished_execution(status, cleanup_error)
        }
        Ok(ChildObservation::TimedOut) => {
            let (status, kill_error) = backend.cleanup(&containment, &mut child);
            Execution {
                outcome: ProcessOutcome::TimedOut {
                    timeout_ms: timeout.as_millis(),
                    reaped: status.is_some(),
                    kill_error,
                },
                status,
            }
        }
        Err(error) => {
            let (status, cleanup_error) = backend.cleanup(&containment, &mut child);
            let error = match cleanup_error {
                Some(cleanup) => format!("{error}; cleanup failed: {cleanup}"),
                None => error.to_string(),
            };
            Execution {
                outcome: ProcessOutcome::RunnerFailed { error },
                status,
            }
        }
    }
}

fn finished_execution(status: Option<ExitStatus>, cleanup_error: Option<String>) -> Execution {
    let Some(status) = status else {
        return Execution {
            outcome: ProcessOutcome::ContainmentFailed {
                error: match cleanup_error {
                    Some(error) => error,
                    None => "direct child status was unavailable after observed exit".to_owned(),
                },
            },
            status: None,
        };
    };
    let outcome = status_outcome(status);
    if let Some(error) = cleanup_error {
        return Execution {
            outcome: ProcessOutcome::ContainmentFailed {
                error: format!("{}; cleanup failed: {error}", outcome.description()),
            },
            status: Some(status),
        };
    }
    Execution {
        outcome,
        status: Some(status),
    }
}

#[cfg(unix)]
fn observe_child_exit(
    child: &mut std::process::Child,
    timeout: Duration,
) -> std::io::Result<ChildObservation> {
    waitid_observe_child_exit(child, timeout)
}

#[cfg(not(unix))]
fn observe_child_exit(
    child: &mut std::process::Child,
    timeout: Duration,
) -> std::io::Result<ChildObservation> {
    child.wait_timeout(timeout).map(|status| match status {
        Some(status) => ChildObservation::Exited(Some(status)),
        None => ChildObservation::TimedOut,
    })
}

#[cfg(all(
    unix,
    any(
        target_os = "android",
        target_os = "freebsd",
        target_os = "illumos",
        target_os = "ios",
        target_os = "linux",
        target_os = "macos",
        target_os = "netbsd",
        target_os = "tvos",
        target_os = "visionos",
        target_os = "watchos"
    )
))]
fn waitid_observe_child_exit(
    child: &std::process::Child,
    timeout: Duration,
) -> std::io::Result<ChildObservation> {
    let pid = process_group_id(child)?;
    let started = Instant::now();
    loop {
        let mut information = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        let result = unsafe {
            // SAFETY: `information` is writable output storage. WNOWAIT keeps
            // the direct child waitable so its PID/PGID cannot be reused before
            // group termination and the later `Child::wait` reap.
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                information.as_mut_ptr(),
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let information = unsafe {
            // SAFETY: waitid returned success and initialized siginfo_t.
            information.assume_init()
        };
        if unsafe { information.si_pid() } != 0 {
            return Ok(ChildObservation::Exited(None));
        }

        let elapsed = started.elapsed();
        if elapsed >= timeout {
            return Ok(ChildObservation::TimedOut);
        }
        std::thread::sleep(PROCESS_GROUP_EXIT_POLL_INTERVAL.min(timeout - elapsed));
    }
}

#[cfg(all(
    unix,
    not(any(
        target_os = "android",
        target_os = "freebsd",
        target_os = "illumos",
        target_os = "ios",
        target_os = "linux",
        target_os = "macos",
        target_os = "netbsd",
        target_os = "tvos",
        target_os = "visionos",
        target_os = "watchos"
    ))
))]
fn waitid_observe_child_exit(
    _child: &std::process::Child,
    _timeout: Duration,
) -> std::io::Result<ChildObservation> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "this Unix target lacks a supported non-reaping child observation primitive",
    ))
}

#[cfg(unix)]
fn complete_observed_exit(
    containment: &ProcessContainment,
    child: &mut std::process::Child,
    observed_status: Option<ExitStatus>,
) -> (Option<ExitStatus>, Option<String>) {
    complete_observed_exit_with(containment, child, observed_status, |containment, child| {
        containment.terminate(child)
    })
}

#[cfg(unix)]
fn complete_observed_exit_with(
    containment: &ProcessContainment,
    child: &mut std::process::Child,
    observed_status: Option<ExitStatus>,
    terminate: impl FnOnce(&ProcessContainment, &mut std::process::Child) -> std::io::Result<()>,
) -> (Option<ExitStatus>, Option<String>) {
    debug_assert!(observed_status.is_none());
    let process_group = process_group_id(child);
    let termination_error = terminate(containment, child).err();
    let status = child.wait();
    let verification = process_group
        .and_then(|group| wait_for_process_group_exit(group, TERMINATION_REAP_TIMEOUT));
    completed_unix_group(status, termination_error, verification)
}

#[cfg(unix)]
fn completed_unix_group(
    status: std::io::Result<ExitStatus>,
    termination_error: Option<std::io::Error>,
    verification: std::io::Result<()>,
) -> (Option<ExitStatus>, Option<String>) {
    let mut errors = Vec::new();
    let status = match status {
        Ok(status) => Some(status),
        Err(error) => {
            errors.push(format!("unable to reap observed direct child: {error}"));
            None
        }
    };
    if let (Err(_), Some(error)) = (&verification, termination_error) {
        errors.push(format!("containment termination failed: {error}"));
    }
    cleanup_report(status, errors, verification, "process-group")
}

#[cfg(windows)]
fn complete_observed_exit(
    containment: &ProcessContainment,
    child: &mut std::process::Child,
    observed_status: Option<ExitStatus>,
) -> (Option<ExitStatus>, Option<String>) {
    let termination_error = containment.terminate(child).err();
    let wait_result = containment.wait_for_exit(TERMINATION_REAP_TIMEOUT);
    completed_windows_job(observed_status, termination_error, wait_result)
}

#[cfg(windows)]
fn completed_windows_job(
    observed_status: Option<ExitStatus>,
    termination_error: Option<std::io::Error>,
    wait_result: std::io::Result<()>,
) -> (Option<ExitStatus>, Option<String>) {
    match wait_result {
        Ok(()) => (observed_status, None),
        Err(wait_error) => {
            let cleanup_error = match termination_error {
                Some(termination_error) => format!(
                    "containment termination failed: {termination_error}; Windows Job cleanup could not be verified: {wait_error}"
                ),
                None => format!("Windows Job cleanup could not be verified: {wait_error}"),
            };
            (observed_status, Some(cleanup_error))
        }
    }
}

#[cfg(not(any(unix, windows)))]
fn complete_observed_exit(
    _containment: &ProcessContainment,
    _child: &mut std::process::Child,
    observed_status: Option<ExitStatus>,
) -> (Option<ExitStatus>, Option<String>) {
    (observed_status, None)
}

#[cfg(unix)]
fn process_group_id(child: &std::process::Child) -> std::io::Result<i32> {
    checked_process_id(child.id())
}

#[cfg(unix)]
fn checked_process_id(pid: u32) -> std::io::Result<i32> {
    let Ok(pid) = i32::try_from(pid) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "child process id is too large",
        ));
    };
    Ok(pid)
}

#[cfg(unix)]
trait GroupWaitBackend {
    fn probe_group(&mut self, process_group: i32) -> std::io::Result<()>;
    fn elapsed(&self) -> Duration;
    fn sleep(&mut self, delay: Duration);
}

#[cfg(unix)]
struct NativeGroupWait {
    started: Instant,
}

#[cfg(unix)]
impl GroupWaitBackend for NativeGroupWait {
    fn probe_group(&mut self, process_group: i32) -> std::io::Result<()> {
        // SAFETY: signal zero performs an existence/permission check only. The
        // negative ID targets the dedicated process group configured at spawn.
        let result = unsafe { libc::kill(-process_group, 0) };
        if result == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    fn sleep(&mut self, delay: Duration) {
        std::thread::sleep(delay);
    }
}

#[cfg(unix)]
fn wait_for_process_group_exit(process_group: i32, timeout: Duration) -> std::io::Result<()> {
    wait_for_process_group_exit_with(
        process_group,
        timeout,
        &mut NativeGroupWait {
            started: Instant::now(),
        },
    )
}

#[cfg(unix)]
fn wait_for_process_group_exit_with(
    process_group: i32,
    timeout: Duration,
    backend: &mut impl GroupWaitBackend,
) -> std::io::Result<()> {
    loop {
        let permission_error = match backend.probe_group(process_group) {
            Ok(()) => None,
            Err(error) => match error.raw_os_error() {
                Some(libc::ESRCH) => return Ok(()),
                // Darwin reports EPERM when any group member cannot be probed.
                // During teardown that can be transient, so use the existing
                // bounded grace period rather than failing on the first probe.
                Some(libc::EPERM) => Some(error),
                _ => {
                    return Err(std::io::Error::new(
                        error.kind(),
                        format!("unable to query process group {process_group}: {error}"),
                    ));
                }
            },
        };

        let elapsed = backend.elapsed();
        if elapsed >= timeout {
            if let Some(error) = permission_error {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!("cannot verify that process group {process_group} exited: {error}"),
                ));
            }
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!(
                    "process group {process_group} still exists after {} ms",
                    timeout.as_millis()
                ),
            ));
        }

        // A killed descendant remains observable while it is a zombie. Waiting
        // for ESRCH also requires its reaper to remove that final group member;
        // a slow or non-cooperating external subreaper therefore fails closed.
        backend.sleep(PROCESS_GROUP_EXIT_POLL_INTERVAL.min(timeout - elapsed));
    }
}

fn terminate_and_reap(
    containment: &ProcessContainment,
    child: &mut std::process::Child,
) -> (Option<ExitStatus>, Option<String>) {
    #[cfg(unix)]
    let process_group = process_group_id(child);
    let (status, errors) = reap_terminated_child(
        child,
        |child| containment.terminate(child),
        std::process::Child::kill,
        |child| child.wait_timeout(TERMINATION_REAP_TIMEOUT),
    );
    #[cfg(unix)]
    let verification = process_group
        .and_then(|group| wait_for_process_group_exit(group, TERMINATION_REAP_TIMEOUT));
    #[cfg(unix)]
    let scope = "process-group";
    #[cfg(windows)]
    let verification = containment.wait_for_exit(TERMINATION_REAP_TIMEOUT);
    #[cfg(windows)]
    let scope = "Windows Job";
    #[cfg(not(any(unix, windows)))]
    let (verification, scope) = (Ok(()), "process");
    cleanup_report(status, errors, verification, scope)
}

fn reap_terminated_child<C>(
    child: &mut C,
    terminate: impl FnOnce(&mut C) -> std::io::Result<()>,
    mut kill: impl FnMut(&mut C) -> std::io::Result<()>,
    mut wait: impl FnMut(&mut C) -> std::io::Result<Option<ExitStatus>>,
) -> (Option<ExitStatus>, Vec<String>) {
    let mut errors = Vec::new();
    if let Err(error) = terminate(child) {
        errors.push(format!("containment termination failed: {error}"));
        if let Err(error) = kill(child) {
            errors.push(format!("direct child termination failed: {error}"));
        }
    }

    let status = match wait(child) {
        Ok(Some(status)) => Some(status),
        Ok(None) => {
            if let Err(error) = kill(child) {
                errors.push(format!("direct child termination failed: {error}"));
            }
            match wait(child) {
                Ok(Some(status)) => Some(status),
                Ok(None) => {
                    errors.push("process did not exit within the termination grace period".into());
                    None
                }
                Err(error) => {
                    errors.push(format!("unable to reap terminated process: {error}"));
                    None
                }
            }
        }
        Err(error) => {
            errors.push(format!("unable to reap terminated process: {error}"));
            None
        }
    };
    (status, errors)
}

fn cleanup_report(
    status: Option<ExitStatus>,
    mut errors: Vec<String>,
    verification: std::io::Result<()>,
    scope: &str,
) -> (Option<ExitStatus>, Option<String>) {
    if let Err(error) = verification {
        errors.push(format!("{scope} cleanup could not be verified: {error}"));
    }
    let error = if errors.is_empty() {
        None
    } else {
        Some(errors.join("; "))
    };
    (status, error)
}

fn status_outcome(status: ExitStatus) -> ProcessOutcome {
    status_outcome_with_code(status, status.code())
}

fn status_outcome_with_code(status: ExitStatus, code: Option<i32>) -> ProcessOutcome {
    match code {
        Some(code) => ProcessOutcome::Exited { code },
        None => {
            #[cfg(unix)]
            let detail = termination_detail(status);
            #[cfg(not(unix))]
            let detail = {
                let _ = status;
                "no native exit code was reported".to_owned()
            };
            ProcessOutcome::Terminated { detail }
        }
    }
}

#[cfg(unix)]
fn termination_detail(status: ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt as _;

    match status.signal() {
        Some(signal) if status.core_dumped() => format!("signal {signal} (core dumped)"),
        Some(signal) => format!("signal {signal}"),
        None => "no exit code or signal was reported".to_owned(),
    }
}

fn process_timeout() -> Result<Duration> {
    timeout_from_value(std::env::var_os("FS2_DEV_PROCESS_TIMEOUT_SECONDS").as_deref())
}

fn timeout_from_value(value: Option<&OsStr>) -> Result<Duration> {
    let Some(value) = value else {
        return Ok(DEFAULT_PROCESS_TIMEOUT);
    };
    let value = display_os(value);
    let Ok(seconds) = value.parse::<u64>() else {
        return Err(invalid_data(
            "FS2_DEV_PROCESS_TIMEOUT_SECONDS must be an integer",
        ));
    };
    if !(1..=MAX_PROCESS_TIMEOUT_SECONDS).contains(&seconds) {
        return Err(invalid_data(
            "FS2_DEV_PROCESS_TIMEOUT_SECONDS is outside the supported range",
        ));
    }
    Ok(Duration::from_secs(seconds))
}

pub(crate) fn display_os(value: &OsStr) -> String {
    if let Some(value) = value.to_str() {
        return value.to_owned();
    }
    #[cfg(unix)]
    {
        use std::fmt::Write as _;
        use std::os::unix::ffi::OsStrExt;
        let mut encoded = String::from("unix-bytes:");
        for byte in value.as_bytes() {
            let _ = write!(encoded, "{byte:02x}");
        }
        encoded
    }
    #[cfg(windows)]
    {
        use std::fmt::Write as _;
        use std::os::windows::ffi::OsStrExt;
        let mut encoded = String::from("windows-utf16:");
        for unit in value.encode_wide() {
            let _ = write!(encoded, "{unit:04x}");
        }
        encoded
    }
    #[cfg(not(any(unix, windows)))]
    {
        value.to_string_lossy().into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_capture_stage_keeps_success_and_error_context() {
        assert_eq!(capture_stage(Ok(7_i32), "read fixture").unwrap(), 7);
        let error = capture_stage::<i32>(
            Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "fixture denial").into()),
            "read fixture",
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "unable to read fixture: fixture denial");
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::InvalidData
        );
    }

    struct ScriptedBackend {
        failure: &'static str,
        timed_out: bool,
        status: Option<ExitStatus>,
        cleanup_error: Option<String>,
        events: Vec<&'static str>,
    }

    impl ScriptedBackend {
        fn new(failure: &'static str, cleanup_error: Option<&str>) -> Self {
            Self {
                failure,
                timed_out: false,
                status: Some(fixture_status(0)),
                cleanup_error: cleanup_error.map(str::to_owned),
                events: Vec::new(),
            }
        }

        fn step(&mut self, name: &'static str) -> std::io::Result<()> {
            self.events.push(name);
            if self.failure == name {
                Err(std::io::Error::other(format!("synthetic {name} failure")))
            } else {
                Ok(())
            }
        }
    }

    impl ProcessBackend for ScriptedBackend {
        type Containment = ();
        type Child = ();

        fn configure(&mut self, _: &mut Command) -> std::io::Result<()> {
            self.step("configure")
        }

        fn spawn(&mut self, _: &mut Command) -> std::io::Result<()> {
            self.step("spawn")
        }

        fn attach(&mut self, _: &mut (), _: &()) -> std::io::Result<()> {
            self.step("attach")
        }

        fn observe(&mut self, _: &mut (), _: Duration) -> std::io::Result<ChildObservation> {
            self.step("observe")?;
            if self.timed_out {
                Ok(ChildObservation::TimedOut)
            } else {
                Ok(ChildObservation::Exited(self.status))
            }
        }

        fn complete(
            &mut self,
            _: &(),
            _: &mut (),
            _: Option<ExitStatus>,
        ) -> (Option<ExitStatus>, Option<String>) {
            self.events.push("complete");
            (self.status, self.cleanup_error.clone())
        }

        fn cleanup(&mut self, _: &(), _: &mut ()) -> (Option<ExitStatus>, Option<String>) {
            self.events.push("cleanup");
            (self.status, self.cleanup_error.clone())
        }
    }

    struct FailingCaptureBackend {
        native: NativeCaptureBackend,
        fail_at: usize,
        calls: usize,
        directory: Option<std::path::PathBuf>,
    }

    impl FailingCaptureBackend {
        fn step(&mut self) -> std::io::Result<()> {
            let index = self.calls;
            self.calls += 1;
            if index == self.fail_at {
                Err(std::io::Error::other("synthetic capture failure"))
            } else {
                Ok(())
            }
        }
    }

    impl CaptureBackend for FailingCaptureBackend {
        #[cfg(windows)]
        fn root(&mut self) -> Result<(std::path::PathBuf, Vec<std::fs::File>)> {
            self.step()?;
            self.native.root()
        }

        #[cfg(windows)]
        fn temporary_in(&mut self, root: &std::path::Path) -> Result<tempfile::TempDir> {
            self.step()?;
            let result = self.native.temporary_in(root);
            self.directory = result
                .as_ref()
                .ok()
                .map(|temporary| temporary.path().to_owned());
            result
        }

        #[cfg(windows)]
        fn harden(&mut self, path: &std::path::Path) -> Result<std::fs::File> {
            self.step()?;
            self.native.harden(path)
        }

        #[cfg(not(windows))]
        fn temporary(&mut self) -> Result<tempfile::TempDir> {
            self.step()?;
            let result = self.native.temporary();
            self.directory = result
                .as_ref()
                .ok()
                .map(|temporary| temporary.path().to_owned());
            result
        }

        fn file(&mut self, path: &std::path::Path) -> Result<std::fs::File> {
            self.step()?;
            self.native.file(path)
        }

        fn clone_file(&mut self, file: &std::fs::File) -> std::io::Result<std::fs::File> {
            self.step()?;
            self.native.clone_file(file)
        }

        fn rewind(&mut self, file: &mut std::fs::File) -> std::io::Result<()> {
            self.step()?;
            self.native.rewind(file)
        }

        fn read_to_end(
            &mut self,
            file: &mut std::fs::File,
            bytes: &mut Vec<u8>,
        ) -> std::io::Result<usize> {
            self.step()?;
            self.native.read_to_end(file, bytes)
        }
    }

    #[test]
    fn capture_releases_files_and_authority_after_each_failure_and_success() {
        #[cfg(windows)]
        const CALLS: usize = 11;
        #[cfg(not(windows))]
        const CALLS: usize = 9;

        for fail_at in 0..=CALLS {
            let mut backend = FailingCaptureBackend {
                native: NativeCaptureBackend,
                fail_at,
                calls: 0,
                directory: None,
            };
            let mut command = fixture_command(0);
            let result = capture_with_backend(&mut command, "capture fixture", &mut backend);
            if fail_at == CALLS {
                assert!(result.unwrap().status.success());
                assert_eq!(backend.calls, CALLS);
            } else {
                assert!(
                    result
                        .unwrap_err()
                        .to_string()
                        .contains("synthetic capture failure")
                );
                assert_eq!(backend.calls, fail_at + 1);
            }
            if let Some(path) = backend.directory {
                assert!(
                    !path.exists(),
                    "capture directory was not removed: {path:?}"
                );
            }
        }
    }

    #[test]
    fn execution_failures_preserve_order_cleanup_and_error_classification() {
        let cases: &[(&'static str, &[&str])] = &[
            ("configure", &["configure"]),
            ("spawn", &["configure", "spawn"]),
            ("attach", &["configure", "spawn", "attach", "cleanup"]),
            (
                "observe",
                &["configure", "spawn", "attach", "observe", "cleanup"],
            ),
        ];
        for &(stage, expected) in cases {
            for cleanup_error in [None, Some("synthetic cleanup failure")] {
                let mut backend = ScriptedBackend::new(stage, cleanup_error);
                let execution = execute_with(
                    &mut Command::new("never-executed"),
                    Duration::ZERO,
                    &mut backend,
                );
                assert_eq!(backend.events, expected);
                assert!(!execution.outcome.succeeded());
                assert!(execution.outcome.description().contains(stage));
                match stage {
                    "spawn" => assert!(matches!(
                        execution.outcome,
                        ProcessOutcome::SpawnFailed { .. }
                    )),
                    "observe" => assert!(matches!(
                        execution.outcome,
                        ProcessOutcome::RunnerFailed { .. }
                    )),
                    _ => assert!(matches!(
                        execution.outcome,
                        ProcessOutcome::ContainmentFailed { .. }
                    )),
                }
                if matches!(stage, "attach" | "observe") && cleanup_error.is_some() {
                    assert!(execution.outcome.description().contains("cleanup failed"));
                }
            }
        }
        for status in [None, Some(fixture_status(0)), Some(fixture_status(7))] {
            let mut backend = ScriptedBackend::new("", None);
            backend.status = status;
            let execution = execute_with(
                &mut Command::new("never-executed"),
                Duration::ZERO,
                &mut backend,
            );
            assert_eq!(
                backend.events,
                ["configure", "spawn", "attach", "observe", "complete"]
            );
            assert_eq!(
                execution.outcome.succeeded(),
                status.is_some_and(|status| status.success())
            );
        }
        for status in [None, Some(fixture_status(7))] {
            let mut backend = ScriptedBackend::new("", Some("synthetic cleanup failure"));
            backend.status = status;
            backend.timed_out = true;
            let execution = execute_with(
                &mut Command::new("never-executed"),
                Duration::from_millis(17),
                &mut backend,
            );
            assert!(matches!(execution.outcome, ProcessOutcome::TimedOut {
                timeout_ms: 17, reaped, kill_error: Some(_),
            } if reaped == status.is_some()));
            assert_eq!(backend.events.last(), Some(&"cleanup"));
        }
    }

    #[test]
    fn reaping_handles_fallback_kill_second_wait_and_verification_failures() {
        use std::collections::VecDeque;
        for case in 0..5 {
            let mut waits = match case {
                0 => VecDeque::from([Ok(Some(fixture_status(0)))]),
                1 => VecDeque::from([Err(std::io::Error::other("wait failed"))]),
                2 => VecDeque::from([Ok(None), Ok(None)]),
                3 => VecDeque::from([Ok(None), Err(std::io::Error::other("second wait failed"))]),
                _ => VecDeque::from([Ok(None), Ok(Some(fixture_status(7)))]),
            };
            let mut kills = 0;
            let (status, errors) = reap_terminated_child(
                &mut waits,
                |_| {
                    if case == 1 {
                        Err(std::io::Error::other("terminate failed"))
                    } else {
                        Ok(())
                    }
                },
                |_| {
                    kills += 1;
                    if matches!(case, 1 | 3) {
                        Err(std::io::Error::other("kill failed"))
                    } else {
                        Ok(())
                    }
                },
                |waits| waits.pop_front().unwrap(),
            );
            assert!(waits.is_empty());
            assert_eq!(status.is_some(), matches!(case, 0 | 4));
            assert_eq!(errors.is_empty(), matches!(case, 0 | 4));
            assert_eq!(kills, usize::from(case != 0));
            if case == 1 {
                assert_eq!(errors.len(), 3);
            }
            if case == 2 {
                assert!(errors[0].contains("termination grace period"));
            }
        }
        let status = Some(fixture_status(0));
        assert!(
            cleanup_report(status, Vec::new(), Ok(()), "fixture")
                .1
                .is_none()
        );
        let error = cleanup_report(
            status,
            vec!["first error".to_owned()],
            Err(std::io::Error::other("verification failed")),
            "fixture",
        )
        .1
        .unwrap();
        assert!(error.starts_with("first error; fixture cleanup could not be verified"));
    }

    #[test]
    fn capture_error_context_and_cargo_fallback_are_explicit() {
        assert_eq!(cargo_program(None), OsString::from("cargo"));
        assert_eq!(
            cargo_program(Some(OsString::from("custom cargo"))),
            OsString::from("custom cargo")
        );
        assert_eq!(capture_stage(Ok(7), "fixture").unwrap(), 7);
        for operation in [
            "create capture directory",
            "harden capture directory",
            "create secure stdout capture",
            "create secure stderr capture",
        ] {
            let error = capture_stage::<()>(
                Err(std::io::Error::other("fixture failure").into()),
                operation,
            )
            .unwrap_err()
            .to_string();
            assert_eq!(error, format!("unable to {operation}: fixture failure"));
        }
        assert!(matches!(
            status_outcome_with_code(fixture_status(0), None),
            ProcessOutcome::Terminated { .. }
        ));
        #[cfg(not(unix))]
        assert!(matches!(
            status_outcome_with_code(fixture_status(0), None),
            ProcessOutcome::Terminated { detail } if detail == "no native exit code was reported"
        ));
    }

    #[test]
    fn rustc_identity_fallback_uses_only_child_environment_overrides() {
        if std::env::var_os("FS2_DEV_IDENTITY_FIXTURE").is_some() {
            assert_eq!(cargo().get_program(), OsStr::new("cargo"));
            assert_eq!(toolchain_key().unwrap().len(), 64);
            return;
        }
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "process::tests::rustc_identity_fallback_uses_only_child_environment_overrides",
                "--nocapture",
            ])
            .env("FS2_DEV_IDENTITY_FIXTURE", "1")
            .env_remove("CARGO")
            .env_remove("RUSTUP_TOOLCHAIN");
        capture(&mut child, "rustc identity fixture").unwrap();
    }

    #[test]
    fn native_timeout_reaps_the_contained_process() {
        let mut child = fixture_command(0);
        child
            .env("FS2_DEV_STREAM_FIXTURE_DELAY_MS", "10000")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let execution = execute(&mut child, Duration::from_millis(1));
        let description = execution.outcome.description();
        assert!(
            matches!(
                execution.outcome,
                ProcessOutcome::TimedOut {
                    reaped: true,
                    kill_error: None,
                    ..
                }
            ),
            "{}",
            description
        );
    }

    #[cfg(windows)]
    #[test]
    fn capture_candidates_fail_closed_for_untrusted_and_conflicting_paths() {
        let (root, _guards) = capture_root().unwrap();
        let candidate = tempfile::tempdir_in(root).unwrap();
        let _private = windows_security::harden_new_private_directory(candidate.path()).unwrap();
        let error = capture_root_from([
            PathBuf::from("relative"),
            PathBuf::from(r"\\server\share\not-contacted"),
            candidate.path().join("missing"),
        ])
        .unwrap_err()
        .to_string();
        assert!(error.contains("unable to bind capture temp ancestry"));
        let file = std::fs::File::create(candidate.path().join(".fs2-secure-capture")).unwrap();
        drop(file);
        assert!(capture_root_from([candidate.path().to_path_buf()]).is_err());
    }

    #[cfg(windows)]
    use std::io;

    fn fixture_status(code: i32) -> ExitStatus {
        #[cfg(windows)]
        {
            use std::os::windows::process::ExitStatusExt as _;
            ExitStatus::from_raw(u32::try_from(code).unwrap())
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt as _;
            ExitStatus::from_raw(code << 8)
        }
    }

    fn fixture_command(code: i32) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "process::tests::native_command_fixture",
                "--nocapture",
            ])
            .env("FS2_DEV_STREAM_FIXTURE_EXIT", code.to_string());
        command
    }

    #[test]
    fn native_command_fixture() {
        use std::io::Write as _;

        let Some(code) = std::env::var_os("FS2_DEV_STREAM_FIXTURE_EXIT") else {
            return;
        };
        let delay = std::env::var("FS2_DEV_STREAM_FIXTURE_DELAY_MS")
            .unwrap_or_default()
            .parse()
            .unwrap_or(0);
        std::thread::sleep(Duration::from_millis(delay));
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(b"fs2-stdout\n").unwrap();
        stdout.flush().unwrap();
        let mut stderr = std::io::stderr().lock();
        stderr.write_all(b"fs2-stderr\n").unwrap();
        stderr.flush().unwrap();
        assert_eq!(code.to_str(), Some("0"), "synthetic native failure");
    }

    #[test]
    fn outcomes_keep_success_and_failure_diagnostics_distinct() {
        let cases = [
            (ProcessOutcome::Exited { code: 0 }, true, "native exit 0"),
            (ProcessOutcome::Exited { code: 7 }, false, "native exit 7"),
            (
                ProcessOutcome::Terminated {
                    detail: "signal fixture".to_owned(),
                },
                false,
                "process terminated: signal fixture",
            ),
            (
                ProcessOutcome::TimedOut {
                    timeout_ms: 20,
                    reaped: true,
                    kill_error: None,
                },
                false,
                "process timed out after 20 ms; reaped=true",
            ),
            (
                ProcessOutcome::TimedOut {
                    timeout_ms: 20,
                    reaped: false,
                    kill_error: Some("denied".to_owned()),
                },
                false,
                "process timed out after 20 ms; kill failed: denied",
            ),
            (
                ProcessOutcome::SpawnFailed {
                    error: "missing".to_owned(),
                },
                false,
                "process spawn failed: missing",
            ),
            (
                ProcessOutcome::ContainmentFailed {
                    error: "cleanup".to_owned(),
                },
                false,
                "process containment failed: cleanup",
            ),
            (
                ProcessOutcome::RunnerFailed {
                    error: "wait".to_owned(),
                },
                false,
                "process runner failed: wait",
            ),
        ];
        for (outcome, succeeded, description) in cases {
            assert_eq!(outcome.succeeded(), succeeded);
            assert_eq!(outcome.description(), description);
        }
    }

    #[test]
    fn timeout_values_are_bounded_without_mutating_process_environment() {
        assert_eq!(timeout_from_value(None).unwrap(), DEFAULT_PROCESS_TIMEOUT);
        for seconds in [1, MAX_PROCESS_TIMEOUT_SECONDS] {
            let value = seconds.to_string();
            assert_eq!(
                timeout_from_value(Some(OsStr::new(&value))).unwrap(),
                Duration::from_secs(seconds)
            );
        }
        for value in ["0", "86401", "", "-1", "1.5", "no", "18446744073709551616"] {
            assert!(
                timeout_from_value(Some(OsStr::new(value))).is_err(),
                "{value}"
            );
        }
        assert_eq!(display_os(OsStr::new("plain toolchain")), "plain toolchain");
    }

    #[test]
    fn non_unicode_values_are_losslessly_diagnosed_and_not_valid_timeouts() {
        #[cfg(windows)]
        let (value, expected) = {
            use std::os::windows::ffi::OsStringExt as _;
            (
                OsString::from_wide(&[0xd800, 0x61]),
                "windows-utf16:d8000061",
            )
        };
        #[cfg(unix)]
        let (value, expected) = {
            use std::os::unix::ffi::OsStringExt as _;
            (OsString::from_vec(vec![0xff, b'a']), "unix-bytes:ff61")
        };
        assert_eq!(display_os(&value), expected);
        assert!(timeout_from_value(Some(&value)).is_err());
    }

    #[test]
    fn finished_execution_requires_status_and_successful_cleanup() {
        for code in [0, 7] {
            let execution = finished_execution(Some(fixture_status(code)), None);
            assert_eq!(execution.status.unwrap().code(), Some(code));
            assert!(
                matches!(execution.outcome, ProcessOutcome::Exited { code: actual } if actual == code)
            );
            let execution = finished_execution(
                Some(fixture_status(code)),
                Some("cleanup fixture".to_owned()),
            );
            assert_eq!(execution.status.unwrap().code(), Some(code));
            assert!(matches!(
                execution.outcome,
                ProcessOutcome::ContainmentFailed { .. }
            ));
            assert!(execution.outcome.description().contains("cleanup fixture"));
        }
        for error in [None, Some("missing status fixture".to_owned())] {
            let execution = finished_execution(None, error);
            assert!(execution.status.is_none());
            assert!(matches!(
                execution.outcome,
                ProcessOutcome::ContainmentFailed { .. }
            ));
            assert!(!execution.outcome.succeeded());
        }
    }

    #[test]
    fn captured_output_requires_status_and_preserves_failure_evidence() {
        let output = captured_output(
            finished_execution(Some(fixture_status(0)), None),
            "fixture",
            b"output".to_vec(),
            b"diagnostic".to_vec(),
        )
        .unwrap();
        assert_eq!(output.stdout, b"output");
        assert_eq!(output.stderr, b"diagnostic");
        let missing_status = Execution {
            outcome: ProcessOutcome::Exited { code: 0 },
            status: None,
        };
        assert!(
            captured_output(missing_status, "fixture", Vec::new(), Vec::new())
                .unwrap_err()
                .to_string()
                .contains("successful process has no exit status")
        );
        let error = captured_output(
            finished_execution(Some(fixture_status(7)), None),
            "fixture",
            b" output \n".to_vec(),
            vec![0xff],
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("fixture failed: native exit 7"));
        assert!(error.contains("stdout:\noutput"));
        assert!(error.contains("stderr:\n\u{fffd}"));
    }

    #[test]
    fn native_execution_and_capture_preserve_exit_codes_and_streams() {
        run(&mut fixture_command(0), "successful fixture").unwrap();
        let error = run(&mut fixture_command(7), "failed fixture").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("failed fixture failed: native exit 101")
        );
        let output = capture(&mut fixture_command(0), "captured fixture").unwrap();
        assert!(output.status.success());
        assert!(
            String::from_utf8(output.stdout)
                .unwrap()
                .contains("fs2-stdout")
        );
        assert!(
            String::from_utf8(output.stderr)
                .unwrap()
                .contains("fs2-stderr")
        );
        let error = capture(&mut fixture_command(7), "captured failure")
            .unwrap_err()
            .to_string();
        assert!(error.contains("native exit 101"));
        assert!(error.contains("fs2-stdout"));
        assert!(error.contains("fs2-stderr"));
        assert_eq!(toolchain_key().unwrap().len(), 64);
    }

    #[test]
    fn missing_executable_is_a_spawn_failure_not_success() {
        let directory = tempfile::tempdir().unwrap();
        let mut command = Command::new(directory.path().join("missing-executable"));
        let execution = execute(&mut command, Duration::from_secs(5));
        assert!(matches!(
            execution.outcome,
            ProcessOutcome::SpawnFailed { .. }
        ));
        assert!(execution.status.is_none());
        let error = capture(&mut command, "missing fixture")
            .unwrap_err()
            .to_string();
        assert!(error.contains("missing fixture failed: process spawn failed"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_cleanup_requires_verified_job_exit() {
        for termination_error in [None, Some(io::Error::other("termination fixture"))] {
            let (status, error) =
                completed_windows_job(Some(fixture_status(7)), termination_error, Ok(()));
            assert_eq!(status.unwrap().code(), Some(7));
            assert!(error.is_none());
        }
        for termination_error in [None, Some(io::Error::other("termination fixture"))] {
            let expected_termination = termination_error.is_some();
            let (status, error) = completed_windows_job(
                Some(fixture_status(0)),
                termination_error,
                Err(io::Error::new(io::ErrorKind::TimedOut, "wait fixture")),
            );
            assert_eq!(status.unwrap().code(), Some(0));
            let error = error.unwrap();
            assert!(error.contains("Windows Job cleanup could not be verified"));
            assert_eq!(error.contains("termination fixture"), expected_termination);
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_sleep_fixture() {
        let seconds = std::env::var("FS2_DEV_SLEEP_FIXTURE_SECONDS")
            .unwrap_or_else(|_| "0".to_owned())
            .parse()
            .unwrap();
        std::thread::sleep(Duration::from_secs(seconds));
    }

    #[cfg(windows)]
    #[test]
    fn windows_descendant_fixture() {
        let Some(code) = std::env::var_os("FS2_DEV_DESCENDANT_FIXTURE_EXIT") else {
            return;
        };
        #[expect(
            clippy::zombie_processes,
            reason = "The enclosing Windows Job terminates and verifies this surviving descendant."
        )]
        let _descendant = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "process::tests::windows_sleep_fixture",
                "--nocapture",
            ])
            .env("FS2_DEV_SLEEP_FIXTURE_SECONDS", "30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        // The enclosing Windows Job, not this fixture, owns descendant cleanup.
        assert_eq!(
            code.to_str(),
            Some("0"),
            "synthetic descendant-parent failure"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_job_drains_descendants_after_successful_and_failed_parent_exit() {
        for code in [0, 7] {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "process::tests::windows_descendant_fixture",
                    "--nocapture",
                ])
                .env("FS2_DEV_DESCENDANT_FIXTURE_EXIT", code.to_string())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            let mut containment = ProcessContainment::configure(&mut command).unwrap();
            let mut child = command.spawn().unwrap();
            containment.attach(&child).unwrap();
            let observed = child
                .wait_timeout(Duration::from_secs(10))
                .unwrap()
                .unwrap();
            assert_eq!(observed.code(), Some(if code == 0 { 0 } else { 101 }));
            assert_eq!(
                containment
                    .wait_for_exit(Duration::ZERO)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::TimedOut
            );
            let (status, error) = complete_observed_exit(&containment, &mut child, Some(observed));
            assert_eq!(status.unwrap().code(), observed.code());
            assert!(error.is_none(), "{error:?}");
            containment.wait_for_exit(Duration::ZERO).unwrap();
        }
    }

    #[test]
    fn toolchain_key_is_bounded_and_identity_sensitive() {
        let identity = "rustc 1.98.1 (48a229cea 2026-09-01)\nbinary: rustc\ncommit-hash: 48a229ceaefd4985c50990b14116b6d856af0985\ncommit-date: 2026-09-01\nhost: x86_64-pc-windows-msvc\nrelease: 1.98.1\nLLVM version: 22.1.8\n";
        let key = toolchain_key_for_identity(identity);

        assert_eq!(key.len(), 64);
        assert!(key.chars().all(|character| character.is_ascii_hexdigit()));
        assert_eq!(key, toolchain_key_for_identity(identity));
        assert_ne!(key, toolchain_key_for_identity("rustc 1.88.0"));
    }

    #[cfg(unix)]
    #[test]
    fn unix_cleanup_reports_each_failure_without_losing_reaped_status() {
        for wait_failed in [false, true] {
            for termination_failed in [false, true] {
                for verification_failed in [false, true] {
                    let wait = if wait_failed {
                        Err(std::io::Error::other("reap fixture"))
                    } else {
                        Ok(fixture_status(7))
                    };
                    let termination =
                        termination_failed.then(|| std::io::Error::other("terminate fixture"));
                    let verification = if verification_failed {
                        Err(std::io::Error::other("verify fixture"))
                    } else {
                        Ok(())
                    };
                    let (status, error) = completed_unix_group(wait, termination, verification);
                    let mut expected = Vec::new();
                    if wait_failed {
                        expected.push("unable to reap observed direct child: reap fixture");
                    }
                    if verification_failed {
                        if termination_failed {
                            expected.push("containment termination failed: terminate fixture");
                        }
                        expected
                            .push("process-group cleanup could not be verified: verify fixture");
                    }
                    assert_eq!(
                        status.and_then(|value| value.code()),
                        (!wait_failed).then_some(7)
                    );
                    assert_eq!(error, (!expected.is_empty()).then(|| expected.join("; ")));
                }
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn unix_pid_bounds_reject_overflow_before_any_native_operation() {
        for pid in [0, 1, i32::MAX as u32] {
            assert_eq!(i64::from(checked_process_id(pid).unwrap()), i64::from(pid));
        }
        for pid in [i32::MAX as u32 + 1, u32::MAX] {
            let error = checked_process_id(pid).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
            assert_eq!(error.to_string(), "child process id is too large");
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn unix_termination_details_preserve_signal_and_core_dump_metadata() {
        use std::os::unix::process::ExitStatusExt as _;
        // These platform-specific wait statuses do not create a process or core dump.
        assert_eq!(
            termination_detail(fixture_status(0)),
            "no exit code or signal was reported"
        );
        assert_eq!(
            termination_detail(ExitStatus::from_raw(libc::SIGTERM)),
            format!("signal {}", libc::SIGTERM),
        );
        assert_eq!(
            termination_detail(ExitStatus::from_raw(libc::SIGABRT | 0x80)),
            format!("signal {} (core dumped)", libc::SIGABRT),
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn unix_observation_reports_a_child_already_reaped_by_its_owner() {
        let mut child = Command::new("sh").args(["-c", "exit 0"]).spawn().unwrap();
        let status = child.wait().unwrap();
        assert!(status.success());
        let error = waitid_observe_child_exit(&child, Duration::ZERO)
            .err()
            .unwrap();
        assert_eq!(error.raw_os_error(), Some(libc::ECHILD));
    }

    #[cfg(unix)]
    struct ScriptedGroupWait {
        probes: std::collections::VecDeque<i32>,
        elapsed: Duration,
        sleeps: Vec<Duration>,
        groups: Vec<i32>,
    }

    #[cfg(unix)]
    impl GroupWaitBackend for ScriptedGroupWait {
        fn probe_group(&mut self, process_group: i32) -> std::io::Result<()> {
            self.groups.push(process_group);
            match self
                .probes
                .pop_front()
                .expect("unexpected additional group probe")
            {
                0 => Ok(()),
                errno => Err(std::io::Error::from_raw_os_error(errno)),
            }
        }

        fn elapsed(&self) -> Duration {
            self.elapsed
        }

        fn sleep(&mut self, delay: Duration) {
            self.elapsed += delay;
            self.sleeps.push(delay);
        }
    }

    #[cfg(unix)]
    #[test]
    fn unix_group_wait_preserves_probe_errors_and_bounded_grace() {
        use std::io::ErrorKind;

        let poll = PROCESS_GROUP_EXIT_POLL_INTERVAL;
        let remainder = poll / 2;
        for (probes, timeout, expected_error, expected_sleeps) in [
            (vec![libc::ESRCH], Duration::ZERO, None, vec![]),
            (vec![0, libc::ESRCH], poll, None, vec![poll]),
            (vec![libc::EPERM, libc::ESRCH], poll, None, vec![poll]),
            (vec![0], Duration::ZERO, Some(ErrorKind::TimedOut), vec![]),
            (
                vec![libc::EPERM],
                Duration::ZERO,
                Some(ErrorKind::PermissionDenied),
                vec![],
            ),
            (
                vec![libc::EIO],
                poll,
                Some(std::io::Error::from_raw_os_error(libc::EIO).kind()),
                vec![],
            ),
            (
                vec![libc::EINTR],
                poll,
                Some(ErrorKind::Interrupted),
                vec![],
            ),
            (vec![0, libc::ESRCH], remainder, None, vec![remainder]),
            (
                vec![libc::EPERM, 0],
                poll,
                Some(ErrorKind::TimedOut),
                vec![poll],
            ),
            (
                vec![0, libc::EPERM],
                poll,
                Some(ErrorKind::PermissionDenied),
                vec![poll],
            ),
        ] {
            let count = probes.len();
            let mut backend = ScriptedGroupWait {
                probes: probes.into(),
                elapsed: Duration::ZERO,
                sleeps: Vec::new(),
                groups: Vec::new(),
            };
            let result = wait_for_process_group_exit_with(42, timeout, &mut backend);
            assert_eq!(
                result.as_ref().err().map(std::io::Error::kind),
                expected_error
            );
            assert!(backend.probes.is_empty());
            assert_eq!(backend.groups, vec![42; count]);
            assert_eq!(backend.sleeps, expected_sleeps);
        }
    }

    #[cfg(unix)]
    #[test]
    fn native_group_wait_probes_without_signaling_and_uses_a_monotonic_clock() {
        // SAFETY: getpgrp reads the calling process's group without modifying it.
        let process_group = unsafe { libc::getpgrp() };
        assert!(process_group > 1);
        let mut backend = NativeGroupWait {
            started: Instant::now(),
        };
        backend.probe_group(process_group).unwrap();
        let before = backend.elapsed();
        backend.sleep(Duration::ZERO);
        assert!(backend.elapsed() >= before);
    }

    #[cfg(unix)]
    fn read_process_identities(path: &Path) -> (i32, i32) {
        let identities = fs::read_to_string(path).unwrap();
        let mut identities = identities.split_ascii_whitespace();
        let descendant_pid = identities.next().unwrap().parse::<i32>().unwrap();
        let process_group = identities.next().unwrap().parse::<i32>().unwrap();
        assert!(identities.next().is_none());
        (descendant_pid, process_group)
    }

    #[cfg(unix)]
    fn assert_process_group_absent(descendant_pid: i32, process_group: i32) {
        for signal_target in [descendant_pid, -process_group] {
            // SAFETY: signal zero performs an existence/permission check only.
            assert_eq!(unsafe { libc::kill(signal_target, 0) }, -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ESRCH)
            );
        }
    }

    #[test]
    fn timed_out_processes_are_terminated_and_reaped() {
        #[cfg(windows)]
        let mut command = {
            let mut command = Command::new("cmd");
            command.args(["/C", "ping -n 30 127.0.0.1 >nul"]);
            command
        };
        #[cfg(unix)]
        let mut command = {
            let mut command = Command::new("sh");
            command.args(["-c", "sleep 30"]);
            command
        };

        let execution = execute(&mut command, Duration::from_millis(20));
        assert!(matches!(execution.outcome, ProcessOutcome::TimedOut { .. }));
        assert!(execution.status.is_some());
    }

    #[cfg(unix)]
    #[test]
    fn successful_processes_without_descendants_still_succeed() {
        let mut command = Command::new("sh");
        command.args(["-c", "exit 0"]);

        let execution = execute(&mut command, Duration::from_secs(5));
        assert!(matches!(
            execution.outcome,
            ProcessOutcome::Exited { code: 0 }
        ));
        assert!(execution.status.is_some());
    }

    #[cfg(unix)]
    #[test]
    fn unsuccessful_processes_preserve_exit_status_after_descendant_cleanup() {
        let temporary = tempfile::tempdir().unwrap();
        let identities = temporary.path().join("process-identities");
        let identities_argument = identities.to_str().unwrap();
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "sleep 30 & descendant=$!; printf '%s %s\\n' \"$descendant\" \"$$\" > \"$1\"; exit 7",
            "sh",
            identities_argument,
        ]);

        let execution = execute(&mut command, Duration::from_secs(5));
        assert!(matches!(
            execution.outcome,
            ProcessOutcome::Exited { code: 7 }
        ));
        assert_eq!(execution.status.and_then(|status| status.code()), Some(7));
        let (descendant_pid, process_group) = read_process_identities(&identities);
        assert_process_group_absent(descendant_pid, process_group);
    }

    #[cfg(unix)]
    #[test]
    fn successful_processes_return_after_same_group_descendants_exit() {
        let temporary = tempfile::tempdir().unwrap();
        let identities = temporary.path().join("process-identities");
        let identities_argument = identities.to_str().unwrap();
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "sleep 30 & descendant=$!; printf '%s %s\\n' \"$descendant\" \"$$\" > \"$1\"; exit 0",
            "sh",
            identities_argument,
        ]);

        let execution = execute(&mut command, Duration::from_secs(5));
        assert!(matches!(
            execution.outcome,
            ProcessOutcome::Exited { code: 0 }
        ));
        let (descendant_pid, process_group) = read_process_identities(&identities);
        assert_process_group_absent(descendant_pid, process_group);
    }

    #[cfg(unix)]
    #[test]
    fn timed_out_processes_return_after_same_group_descendants_exit() {
        let temporary = tempfile::tempdir().unwrap();
        let identities = temporary.path().join("process-identities");
        let identities_argument = identities.to_str().unwrap();
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "sleep 30 & descendant=$!; printf '%s %s\\n' \"$descendant\" \"$$\" > \"$1\"; wait",
            "sh",
            identities_argument,
        ]);

        let execution = execute(&mut command, Duration::from_millis(200));
        assert!(matches!(
            execution.outcome,
            ProcessOutcome::TimedOut {
                reaped: true,
                kill_error: None,
                ..
            }
        ));
        assert!(execution.status.is_some());
        let (descendant_pid, process_group) = read_process_identities(&identities);
        assert_process_group_absent(descendant_pid, process_group);
    }

    #[cfg(unix)]
    #[test]
    fn verified_group_exit_overrides_a_termination_race() {
        let mut command = Command::new("sh");
        command.args(["-c", "exit 9"]);
        let mut containment = ProcessContainment::configure(&mut command).unwrap();
        let mut child = command.spawn().unwrap();
        containment.attach(&child).unwrap();
        assert!(matches!(
            observe_child_exit(&mut child, Duration::from_secs(5)).unwrap(),
            ChildObservation::Exited(None)
        ));

        let (status, cleanup_error) =
            complete_observed_exit_with(&containment, &mut child, None, |_containment, _child| {
                Err(std::io::Error::other("injected cleanup failure"))
            });

        assert_eq!(status.and_then(|status| status.code()), Some(9));
        assert!(cleanup_error.is_none());
    }
}
