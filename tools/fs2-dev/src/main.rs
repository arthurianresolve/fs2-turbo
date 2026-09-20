mod compatibility;
mod coverage;
mod matrix;
mod process;

use std::error::Error;
use std::io;
use std::path::{Path, PathBuf};
#[cfg(not(test))]
use std::process::ExitCode;

use clap::{Arg, Command};

type DynError = Box<dyn Error + Send + Sync>;
type Result<T> = std::result::Result<T, DynError>;

// The real CLI is exercised by integration tests; libtest supplies its own entrypoint.
#[cfg(not(test))]
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("fs2-dev: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(test))]
fn run() -> Result<()> {
    dispatch(&command().get_matches())
}

fn command() -> Command {
    Command::new("fs2-dev")
        .about("Repository validation tooling for fs2")
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
        .subcommand(
            Command::new("coverage")
                .about("Validate native LLVM coverage evidence")
                .arg(
                    Arg::new("target")
                        .long("target")
                        .value_name("TRIPLE")
                        .required(true)
                        .num_args(1),
                )
                .arg(
                    Arg::new("json")
                        .long("json")
                        .value_name("PATH")
                        .required(true)
                        .num_args(1),
                )
                .arg(
                    Arg::new("lcov")
                        .long("lcov")
                        .value_name("PATH")
                        .required(true)
                        .num_args(1),
                )
                .arg(
                    Arg::new("unit-json")
                        .long("unit-json")
                        .value_name("PATH")
                        .required(true)
                        .num_args(1),
                )
                .arg(
                    Arg::new("integration-json")
                        .long("integration-json")
                        .value_name("PATH")
                        .required(true)
                        .num_args(1),
                )
                .arg(
                    Arg::new("diagnostics-json")
                        .long("diagnostics-json")
                        .value_name("PATH")
                        .required(true)
                        .num_args(1),
                ),
        )
        .subcommand(Command::new("compatibility").about("Validate the v0.4 API contract"))
}

fn dispatch(matches: &clap::ArgMatches) -> Result<()> {
    match matches.subcommand() {
        Some(("matrix", arguments)) => {
            let output = arguments
                .get_one::<String>("github-output")
                .map(PathBuf::from);
            matrix::run(repository_root(), output.as_deref())
        }
        Some(("coverage", arguments)) => coverage::run(
            arguments
                .get_one::<String>("target")
                .expect("required target is missing"),
            Path::new(
                arguments
                    .get_one::<String>("json")
                    .expect("required JSON path is missing"),
            ),
            Path::new(
                arguments
                    .get_one::<String>("lcov")
                    .expect("required LCOV path is missing"),
            ),
            Path::new(
                arguments
                    .get_one::<String>("unit-json")
                    .expect("required unit JSON path is missing"),
            ),
            Path::new(
                arguments
                    .get_one::<String>("integration-json")
                    .expect("required integration JSON path is missing"),
            ),
            Path::new(
                arguments
                    .get_one::<String>("diagnostics-json")
                    .expect("required diagnostics JSON path is missing"),
            ),
        ),
        Some(("compatibility", _)) => compatibility::run(repository_root()),
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
    fn dispatch_rejects_matches_that_bypass_the_command_contract() {
        let matches = clap::Command::new("empty")
            .try_get_matches_from(["empty"])
            .unwrap();
        let failure =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| super::dispatch(&matches)));
        assert!(failure.is_err());
    }

    #[test]
    fn command_parses_every_mode_and_matrix_output() {
        for mode in ["matrix", "compatibility"] {
            let matches = super::command()
                .try_get_matches_from(["fs2-dev", mode])
                .unwrap();
            assert_eq!(matches.subcommand_name(), Some(mode));
        }
        let matches = super::command()
            .try_get_matches_from(["fs2-dev", "matrix", "--github-output", "output with spaces"])
            .unwrap();
        assert_eq!(
            matches
                .subcommand_matches("matrix")
                .unwrap()
                .get_one::<String>("github-output")
                .unwrap(),
            "output with spaces"
        );
    }

    #[test]
    fn coverage_command_requires_each_evidence_argument() {
        let arguments = [
            "fs2-dev",
            "coverage",
            "--target",
            "x86_64-unknown-linux-gnu",
            "--json",
            "combined.json",
            "--lcov",
            "coverage.lcov",
            "--unit-json",
            "unit.json",
            "--integration-json",
            "integration.json",
            "--diagnostics-json",
            "diagnostics.json",
        ];
        let matches = super::command().try_get_matches_from(arguments).unwrap();
        assert_eq!(matches.subcommand_name(), Some("coverage"));
        for omitted in (2..arguments.len()).step_by(2) {
            let omitted_argument = arguments[omitted];
            let incomplete = arguments
                .iter()
                .enumerate()
                .filter(|(index, _)| *index != omitted && *index != omitted + 1)
                .map(|(_, argument)| *argument);
            let error = super::command()
                .try_get_matches_from(incomplete)
                .unwrap_err();
            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::MissingRequiredArgument,
                "{omitted_argument}"
            );
        }
    }

    #[test]
    fn lower_hex_preserves_leading_zeroes() {
        assert_eq!(super::lower_hex([0x00, 0x0f, 0xa5, 0xff]), "000fa5ff");
    }
}
