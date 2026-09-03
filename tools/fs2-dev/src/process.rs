use std::ffi::OsString;
use std::process::{Command, Output};

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

