mod benchmark;
mod compatibility;
mod matrix;
mod policy;
mod process;
mod report;

use std::error::Error;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Arg, Command};

type DynError = Box<dyn Error + Send + Sync>;
type Result<T> = std::result::Result<T, DynError>;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("fs2-dev: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let matches = Command::new("fs2-dev")
        .about("Repository validation and performance tooling for fs2")
        .subcommand_required(true)
        .subcommand(
            Command::new("matrix")
                .about("Validate support metadata and CI workflow")
                .arg(
                    Arg::new("github-output")
                        .long("github-output")
                        .value_name("PATH")
                        .num_args(1),
                ),
        )
        .subcommand(Command::new("compatibility").about("Validate the v0.4 API contract"))
        .subcommand(
            Command::new("policy")
                .about("Validate benchmark measurement policy")
                .arg(Arg::new("path").long("path").value_name("PATH").num_args(1)),
        )
        .subcommand(benchmark::command())
        .get_matches();

    match matches.subcommand() {
        Some(("matrix", arguments)) => {
            let output = arguments
                .get_one::<String>("github-output")
                .map(PathBuf::from);
            matrix::run(repository_root(), output.as_deref())
        }
        Some(("compatibility", _)) => compatibility::run(repository_root()),
        Some(("policy", arguments)) => {
            let path = arguments
                .get_one::<String>("path")
                .map(PathBuf::from)
                .unwrap_or_else(|| repository_root().join("benchmarks/measurement-policy.json"));
            policy::run(&path)
        }
        Some(("bench", arguments)) => benchmark::run(repository_root(), arguments),
        _ => unreachable!("clap requires a known subcommand"),
    }
}

fn repository_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("fs2-dev must remain under tools/fs2-dev")
}

fn lower_hex(bytes: impl AsRef<[u8]>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    let bytes = bytes.as_ref();
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn invalid_data(message: impl Into<String>) -> DynError {
    io::Error::new(io::ErrorKind::InvalidData, message.into()).into()
}

#[cfg(test)]
mod tests {
    #[test]
    fn lower_hex_preserves_leading_zeroes() {
        assert_eq!(super::lower_hex([0x00, 0x0f, 0xa5, 0xff]), "000fa5ff");
    }
}
