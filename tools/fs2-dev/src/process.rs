use std::ffi::OsString;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Instant;

use serde::Serialize;

use crate::{Result, invalid_data};

pub(crate) fn cargo() -> Command {
    Command::new(std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo")))
}

pub(crate) fn run(command: &mut Command, label: &str) -> Result<()> {
    println!("+ {label}");
    let status = command.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(invalid_data(format!(
            "{label} exited with {}",
            status
                .code()
                .map_or_else(|| "no exit code".to_owned(), |code| code.to_string())
        )))
    }
}

pub(crate) fn capture(command: &mut Command, label: &str) -> Result<Output> {
    let output = command.output()?;
    if output.status.success() {
        Ok(output)
    } else {
        let detail = String::from_utf8_lossy(if output.stderr.is_empty() {
            &output.stdout
        } else {
            &output.stderr
        });
        Err(invalid_data(format!("{label} failed: {}", detail.trim())))
    }
}

pub(crate) fn toolchain_key() -> Result<String> {
    let identity = if let Some(toolchain) = std::env::var_os("RUSTUP_TOOLCHAIN") {
        toolchain.to_string_lossy().into_owned()
    } else {
        let output = capture(Command::new("rustc").arg("-vV"), "rustc -vV")?;
        String::from_utf8(output.stdout)?
    };
    Ok(identity
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect())
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ProcessRecord {
    pub(crate) label: String,
    pub(crate) command: Vec<String>,
    pub(crate) exit_code: i32,
    pub(crate) duration_ms: u128,
    pub(crate) stdout: PathBuf,
    pub(crate) stderr: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) spawn_error: Option<String>,
}

impl ProcessRecord {
    pub(crate) fn succeeded(&self) -> bool {
        self.exit_code == 0 && self.spawn_error.is_none()
    }
}

pub(crate) fn run_logged(
    command: &mut Command,
    label: impl Into<String>,
    stdout_path: &Path,
    stderr_path: &Path,
) -> Result<ProcessRecord> {
    if let Some(parent) = stdout_path.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Some(parent) = stderr_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let rendered = std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(|value| value.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let label = label.into();
    println!("+ {label}");
    let stdout = File::create(stdout_path)?;
    let stderr = File::create(stderr_path)?;
    command
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    let started = Instant::now();
    match command.status() {
        Ok(status) => Ok(ProcessRecord {
            label,
            command: rendered,
            exit_code: status.code().unwrap_or(-1),
            duration_ms: started.elapsed().as_millis(),
            stdout: stdout_path.to_owned(),
            stderr: stderr_path.to_owned(),
            spawn_error: None,
        }),
        Err(error) => {
            fs::write(stderr_path, format!("unable to start process: {error}\n"))?;
            Ok(ProcessRecord {
                label,
                command: rendered,
                exit_code: 127,
                duration_ms: started.elapsed().as_millis(),
                stdout: stdout_path.to_owned(),
                stderr: stderr_path.to_owned(),
                spawn_error: Some(error.to_string()),
            })
        }
    }
}
