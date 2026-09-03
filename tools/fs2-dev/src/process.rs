use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{Read as _, Seek as _};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output, Stdio};
use std::time::{Duration, Instant};

use serde::Serialize;
use sha2::{Digest as _, Sha256};
use wait_timeout::ChildExt;

use crate::{Result, invalid_data, lower_hex};

mod containment;

use containment::ProcessContainment;

const DEFAULT_PROCESS_TIMEOUT: Duration = Duration::from_secs(3_600);
const MAX_PROCESS_TIMEOUT_SECONDS: u64 = 86_400;
const TERMINATION_REAP_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(unix)]
const PROCESS_GROUP_EXIT_POLL_INTERVAL: Duration = Duration::from_millis(10);

pub(crate) fn cargo() -> Command {
    Command::new(std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo")))
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

    let mut rejected = Vec::new();
    for root in candidates {
        // Reject UNC and mapped roots before dereferencing the candidate path.
        if !root.is_absolute() || !crate::benchmark::windows_security::is_local_fixed_drive(&root) {
            continue;
        }
        let mut guard = match crate::benchmark::windows_security::guard_directory_ancestry(&root) {
            Ok(guard) => guard,
            Err(error) => {
                rejected.push(format!("{}: {error}", root.display()));
                continue;
            }
        };
        let temporary_root = root.join(".fs2-secure-capture");
        match crate::benchmark::windows_security::create_or_open_private_directory(&temporary_root)
        {
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
    #[cfg(windows)]
    let (_temporary_guard, temporary) = {
        let (temporary_root, mut guard) = capture_root()?;
        // The persistent parent is created atomically with its protected DACL,
        // so randomized children are private from their first observable state.
        let temporary = tempfile::Builder::new()
            .prefix("fs2-capture-")
            .tempdir_in(&temporary_root)
            .map_err(|error| {
                invalid_data(format!("unable to create capture directory: {error}"))
            })?;
        guard.push(
            crate::benchmark::windows_security::harden_new_private_directory(temporary.path())
                .map_err(|error| {
                    invalid_data(format!("unable to harden capture directory: {error}"))
                })?,
        );
        (guard, temporary)
    };
    #[cfg(not(windows))]
    let temporary = tempfile::tempdir()?;
    // Keep the authoritative capture handles: exclusive randomized creation
    // fails closed on a preseed attempt, and no pathname is trusted afterward.
    let mut stdout_file = tempfile::tempfile_in(temporary.path()).map_err(|error| {
        invalid_data(format!("unable to create secure stdout capture: {error}"))
    })?;
    let mut stderr_file = tempfile::tempfile_in(temporary.path()).map_err(|error| {
        invalid_data(format!("unable to create secure stderr capture: {error}"))
    })?;
    command
        .stdout(Stdio::from(stdout_file.try_clone()?))
        .stderr(Stdio::from(stderr_file.try_clone()?));
    let execution = execute(command, process_timeout()?);
    stdout_file.rewind()?;
    stderr_file.rewind()?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    stdout_file.read_to_end(&mut stdout)?;
    stderr_file.read_to_end(&mut stderr)?;
    #[cfg(windows)]
    drop(_temporary_guard);
    if execution.outcome.succeeded() {
        Ok(Output {
            status: execution
                .status
                .ok_or_else(|| invalid_data("successful process has no exit status"))?,
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
    LogSetupFailed {
        error: String,
    },
    ContainmentFailed {
        error: String,
    },
    RunnerFailed {
        error: String,
    },
    Skipped {
        reason: String,
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
            } => kill_error.as_ref().map_or_else(
                || format!("process timed out after {timeout_ms} ms; reaped={reaped}"),
                |error| format!("process timed out after {timeout_ms} ms; kill failed: {error}"),
            ),
            Self::SpawnFailed { error } => format!("process spawn failed: {error}"),
            Self::LogSetupFailed { error } => format!("process log setup failed: {error}"),
            Self::ContainmentFailed { error } => {
                format!("process containment failed: {error}")
            }
            Self::RunnerFailed { error } => format!("process runner failed: {error}"),
            Self::Skipped { reason } => format!("process skipped: {reason}"),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ProcessRecord {
    pub(crate) label: String,
    pub(crate) command: Vec<String>,
    pub(crate) current_dir: Option<String>,
    pub(crate) environment_overrides: BTreeMap<String, Option<String>>,
    pub(crate) outcome: ProcessOutcome,
    pub(crate) duration_ms: u128,
    pub(crate) timeout_ms: Option<u128>,
    pub(crate) containment: &'static str,
    pub(crate) stdout: PathBuf,
    pub(crate) stderr: PathBuf,
}

#[derive(Clone)]
struct ProcessContext {
    command: Vec<String>,
    current_dir: Option<String>,
    environment_overrides: BTreeMap<String, Option<String>>,
}

#[derive(Clone, Copy)]
struct LogPaths<'a> {
    stdout: &'a Path,
    stderr: &'a Path,
}

impl ProcessContext {
    fn capture(command: &Command) -> Self {
        Self {
            command: std::iter::once(command.get_program())
                .chain(command.get_args())
                .map(display_os)
                .collect(),
            current_dir: command
                .get_current_dir()
                .map(|path| display_os(path.as_os_str()))
                .or_else(|| {
                    std::env::current_dir()
                        .ok()
                        .map(|path| display_os(path.as_os_str()))
                }),
            environment_overrides: command
                .get_envs()
                .map(|(name, value)| (display_os(name), value.map(display_os)))
                .collect(),
        }
    }

    fn record(
        &self,
        label: String,
        outcome: ProcessOutcome,
        duration_ms: u128,
        timeout_ms: Option<u128>,
        containment: &'static str,
        logs: LogPaths<'_>,
    ) -> ProcessRecord {
        ProcessRecord {
            label,
            command: self.command.clone(),
            current_dir: self.current_dir.clone(),
            environment_overrides: self.environment_overrides.clone(),
            outcome,
            duration_ms,
            timeout_ms,
            containment,
            stdout: logs.stdout.to_owned(),
            stderr: logs.stderr.to_owned(),
        }
    }
}

impl ProcessRecord {
    pub(crate) fn succeeded(&self) -> bool {
        self.outcome.succeeded()
    }

    pub(crate) fn failure_description(&self) -> String {
        self.outcome.description()
    }

    pub(crate) fn may_still_be_running(&self) -> bool {
        matches!(
            self.outcome,
            ProcessOutcome::TimedOut { reaped: false, .. }
                | ProcessOutcome::TimedOut {
                    kill_error: Some(_),
                    ..
                }
                | ProcessOutcome::ContainmentFailed { .. }
                | ProcessOutcome::RunnerFailed { .. }
        )
    }

    pub(crate) fn skipped(
        command: &Command,
        label: impl Into<String>,
        stdout: PathBuf,
        stderr: PathBuf,
        reason: impl Into<String>,
    ) -> Self {
        ProcessContext::capture(command).record(
            label.into(),
            ProcessOutcome::Skipped {
                reason: reason.into(),
            },
            0,
            process_timeout().ok().map(|timeout| timeout.as_millis()),
            ProcessContainment::METHOD,
            LogPaths {
                stdout: &stdout,
                stderr: &stderr,
            },
        )
    }
}

struct Execution {
    outcome: ProcessOutcome,
    status: Option<ExitStatus>,
    duration_ms: u128,
    timeout_ms: u128,
    containment: &'static str,
}

enum ChildObservation {
    Exited(Option<ExitStatus>),
    TimedOut,
}

fn execute(command: &mut Command, timeout: Duration) -> Execution {
    let started = Instant::now();
    let mut containment = match ProcessContainment::configure(command) {
        Ok(containment) => containment,
        Err(error) => {
            return Execution {
                outcome: ProcessOutcome::ContainmentFailed {
                    error: error.to_string(),
                },
                status: None,
                duration_ms: started.elapsed().as_millis(),
                timeout_ms: timeout.as_millis(),
                containment: ProcessContainment::METHOD,
            };
        }
    };
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return Execution {
                outcome: ProcessOutcome::SpawnFailed {
                    error: error.to_string(),
                },
                status: None,
                duration_ms: started.elapsed().as_millis(),
                timeout_ms: timeout.as_millis(),
                containment: ProcessContainment::METHOD,
            };
        }
    };
    if let Err(error) = containment.attach(&child) {
        let (status, cleanup_error) = terminate_and_reap(&containment, &mut child);
        let error = cleanup_error.map_or_else(
            || error.to_string(),
            |cleanup| format!("{error}; cleanup failed: {cleanup}"),
        );
        return Execution {
            outcome: ProcessOutcome::ContainmentFailed { error },
            status,
            duration_ms: started.elapsed().as_millis(),
            timeout_ms: timeout.as_millis(),
            containment: ProcessContainment::METHOD,
        };
    }
    match observe_child_exit(&mut child, timeout) {
        Ok(ChildObservation::Exited(observed_status)) => {
            let (status, cleanup_error) =
                complete_observed_exit(&containment, &mut child, observed_status);
            let Some(status) = status else {
                return Execution {
                    outcome: ProcessOutcome::ContainmentFailed {
                        error: cleanup_error.unwrap_or_else(|| {
                            "direct child status was unavailable after observed exit".to_owned()
                        }),
                    },
                    status: None,
                    duration_ms: started.elapsed().as_millis(),
                    timeout_ms: timeout.as_millis(),
                    containment: ProcessContainment::METHOD,
                };
            };
            let outcome = status_outcome(status);
            if let Some(error) = cleanup_error {
                return Execution {
                    outcome: ProcessOutcome::ContainmentFailed {
                        error: format!("{}; cleanup failed: {error}", outcome.description()),
                    },
                    status: Some(status),
                    duration_ms: started.elapsed().as_millis(),
                    timeout_ms: timeout.as_millis(),
                    containment: ProcessContainment::METHOD,
                };
            }
            Execution {
                outcome,
                status: Some(status),
                duration_ms: started.elapsed().as_millis(),
                timeout_ms: timeout.as_millis(),
                containment: ProcessContainment::METHOD,
            }
        }
        Ok(ChildObservation::TimedOut) => {
            let (status, kill_error) = terminate_and_reap(&containment, &mut child);
            Execution {
                outcome: ProcessOutcome::TimedOut {
                    timeout_ms: timeout.as_millis(),
                    reaped: status.is_some(),
                    kill_error,
                },
                status,
                duration_ms: started.elapsed().as_millis(),
                timeout_ms: timeout.as_millis(),
                containment: ProcessContainment::METHOD,
            }
        }
        Err(error) => {
            let (status, cleanup_error) = terminate_and_reap(&containment, &mut child);
            let error = cleanup_error.map_or_else(
                || error.to_string(),
                |cleanup| format!("{error}; cleanup failed: {cleanup}"),
            );
            Execution {
                outcome: ProcessOutcome::RunnerFailed { error },
                status,
                duration_ms: started.elapsed().as_millis(),
                timeout_ms: timeout.as_millis(),
                containment: ProcessContainment::METHOD,
            }
        }
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
    let mut errors = Vec::new();
    let process_group = process_group_id(child);
    let termination_error = terminate(containment, child).err();
    let status = match child.wait() {
        Ok(status) => Some(status),
        Err(error) => {
            errors.push(format!("unable to reap observed direct child: {error}"));
            None
        }
    };
    match process_group {
        Ok(process_group) => {
            if let Err(error) = wait_for_process_group_exit(process_group, TERMINATION_REAP_TIMEOUT)
            {
                if let Some(error) = termination_error {
                    errors.push(format!("containment termination failed: {error}"));
                }
                errors.push(format!(
                    "process-group cleanup could not be verified: {error}"
                ));
            }
        }
        Err(error) => {
            if let Some(error) = termination_error {
                errors.push(format!("containment termination failed: {error}"));
            }
            errors.push(format!(
                "process-group cleanup could not be verified: {error}"
            ));
        }
    }
    let error = (!errors.is_empty()).then(|| errors.join("; "));
    (status, error)
}

#[cfg(windows)]
fn complete_observed_exit(
    containment: &ProcessContainment,
    child: &mut std::process::Child,
    observed_status: Option<ExitStatus>,
) -> (Option<ExitStatus>, Option<String>) {
    let termination_error = containment.terminate(child).err();
    match containment.wait_for_exit(TERMINATION_REAP_TIMEOUT) {
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
    i32::try_from(child.id()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "child process id is too large",
        )
    })
}

#[cfg(unix)]
fn wait_for_process_group_exit(process_group: i32, timeout: Duration) -> std::io::Result<()> {
    let started = Instant::now();
    loop {
        // SAFETY: signal zero performs an existence/permission check only. The
        // negative ID targets the dedicated process group configured at spawn.
        let result = unsafe { libc::kill(-process_group, 0) };
        let permission_error = if result != 0 {
            let error = std::io::Error::last_os_error();
            match error.raw_os_error() {
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
            }
        } else {
            None
        };

        let elapsed = started.elapsed();
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
        std::thread::sleep(PROCESS_GROUP_EXIT_POLL_INTERVAL.min(timeout - elapsed));
    }
}

fn terminate_and_reap(
    containment: &ProcessContainment,
    child: &mut std::process::Child,
) -> (Option<ExitStatus>, Option<String>) {
    let mut errors = Vec::new();
    #[cfg(unix)]
    let process_group = process_group_id(child);
    if let Err(error) = containment.terminate(child) {
        errors.push(format!("containment termination failed: {error}"));
        if let Err(error) = child.kill() {
            errors.push(format!("direct child termination failed: {error}"));
        }
    }

    let status = match child.wait_timeout(TERMINATION_REAP_TIMEOUT) {
        Ok(Some(status)) => Some(status),
        Ok(None) => {
            if let Err(error) = child.kill() {
                errors.push(format!("direct child termination failed: {error}"));
            }
            match child.wait_timeout(TERMINATION_REAP_TIMEOUT) {
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
    #[cfg(unix)]
    match process_group {
        Ok(process_group) => {
            if let Err(error) = wait_for_process_group_exit(process_group, TERMINATION_REAP_TIMEOUT)
            {
                errors.push(format!(
                    "process-group cleanup could not be verified: {error}"
                ));
            }
        }
        Err(error) => errors.push(format!(
            "process-group cleanup could not be verified: {error}"
        )),
    }
    #[cfg(windows)]
    if let Err(error) = containment.wait_for_exit(TERMINATION_REAP_TIMEOUT) {
        errors.push(format!(
            "Windows Job cleanup could not be verified: {error}"
        ));
    }
    let error = (!errors.is_empty()).then(|| errors.join("; "));
    (status, error)
}

fn status_outcome(status: ExitStatus) -> ProcessOutcome {
    status.code().map_or_else(
        || ProcessOutcome::Terminated {
            detail: termination_detail(status),
        },
        |code| ProcessOutcome::Exited { code },
    )
}

#[cfg(unix)]
fn termination_detail(status: ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt as _;

    status.signal().map_or_else(
        || "no exit code or signal was reported".to_owned(),
        |signal| {
            if status.core_dumped() {
                format!("signal {signal} (core dumped)")
            } else {
                format!("signal {signal}")
            }
        },
    )
}

#[cfg(not(unix))]
fn termination_detail(_status: ExitStatus) -> String {
    "no native exit code was reported".to_owned()
}

fn process_timeout() -> Result<Duration> {
    let Some(value) = std::env::var_os("FS2_DEV_PROCESS_TIMEOUT_SECONDS") else {
        return Ok(DEFAULT_PROCESS_TIMEOUT);
    };
    let value = display_os(&value);
    let seconds = value
        .parse::<u64>()
        .map_err(|_| invalid_data("FS2_DEV_PROCESS_TIMEOUT_SECONDS must be an integer"))?;
    if !(1..=MAX_PROCESS_TIMEOUT_SECONDS).contains(&seconds) {
        return Err(invalid_data(
            "FS2_DEV_PROCESS_TIMEOUT_SECONDS is outside the supported range",
        ));
    }
    Ok(Duration::from_secs(seconds))
}

pub(crate) fn run_logged(
    command: &mut Command,
    label: impl Into<String>,
    stdout_path: &Path,
    stderr_path: &Path,
) -> ProcessRecord {
    let context = ProcessContext::capture(command);
    let label = label.into();
    let timeout = match process_timeout() {
        Ok(timeout) => timeout,
        Err(error) => {
            return context.record(
                label,
                ProcessOutcome::RunnerFailed {
                    error: error.to_string(),
                },
                0,
                None,
                ProcessContainment::METHOD,
                LogPaths {
                    stdout: stdout_path,
                    stderr: stderr_path,
                },
            );
        }
    };
    let timeout_ms = Some(timeout.as_millis());
    let failed = |outcome| {
        context.record(
            label.clone(),
            outcome,
            0,
            timeout_ms,
            ProcessContainment::METHOD,
            LogPaths {
                stdout: stdout_path,
                stderr: stderr_path,
            },
        )
    };
    for path in [stdout_path, stderr_path] {
        if let Some(parent) = path.parent()
            && let Err(error) = fs::create_dir_all(parent)
        {
            return failed(ProcessOutcome::LogSetupFailed {
                error: error.to_string(),
            });
        }
    }
    let stdout = match File::create(stdout_path) {
        Ok(stdout) => stdout,
        Err(error) => {
            return failed(ProcessOutcome::LogSetupFailed {
                error: error.to_string(),
            });
        }
    };
    let stderr = match File::create(stderr_path) {
        Ok(stderr) => stderr,
        Err(error) => {
            return failed(ProcessOutcome::LogSetupFailed {
                error: error.to_string(),
            });
        }
    };
    println!("+ {label}");
    command
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    let execution = execute(command, timeout);
    if let ProcessOutcome::SpawnFailed { error } = &execution.outcome {
        let _ = fs::write(stderr_path, format!("unable to start process: {error}\n"));
    }
    context.record(
        label,
        execution.outcome,
        execution.duration_ms,
        Some(execution.timeout_ms),
        execution.containment,
        LogPaths {
            stdout: stdout_path,
            stderr: stderr_path,
        },
    )
}

pub(crate) fn run_logged_attempt(
    command: &mut Command,
    label: impl Into<String>,
    stdout_path: &Path,
    stderr_path: &Path,
) -> ProcessRecord {
    run_logged(command, label, stdout_path, stderr_path)
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
    fn toolchain_key_is_bounded_and_identity_sensitive() {
        let identity = "rustc 1.98.1 (48a229cea 2026-09-01)\nbinary: rustc\ncommit-hash: 48a229ceaefd4985c50990b14116b6d856af0985\ncommit-date: 2026-09-01\nhost: x86_64-pc-windows-msvc\nrelease: 1.98.1\nLLVM version: 22.1.8\n";
        let key = toolchain_key_for_identity(identity);

        assert_eq!(key.len(), 64);
        assert!(key.chars().all(|character| character.is_ascii_hexdigit()));
        assert_eq!(key, toolchain_key_for_identity(identity));
        assert_ne!(key, toolchain_key_for_identity("rustc 1.88.0"));
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
    fn skipped_processes_are_not_native_exits() {
        let mut command = Command::new("cargo");
        command
            .current_dir("repository")
            .arg("build")
            .env("RUSTFLAGS", "-Ctarget-cpu=native");
        let process = ProcessRecord::skipped(
            &command,
            "build",
            PathBuf::from("stdout"),
            PathBuf::from("stderr"),
            "setup failed",
        );
        assert!(!process.succeeded());
        assert_eq!(
            process.command,
            vec!["cargo".to_owned(), "build".to_owned()]
        );
        assert_eq!(process.current_dir.as_deref(), Some("repository"));
        assert_eq!(
            process.environment_overrides["RUSTFLAGS"],
            Some("-Ctarget-cpu=native".to_owned())
        );
        let serialized = serde_json::to_value(&process).unwrap();
        assert_eq!(serialized["outcome"]["kind"], "skipped");
        assert!(serialized.get("exit_code").is_none());
        assert!(matches!(&process.outcome, ProcessOutcome::Skipped { .. }));
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
