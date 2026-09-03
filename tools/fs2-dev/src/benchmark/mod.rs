mod arguments;
mod common;
mod crates;
mod criterion;
mod evidence;
mod lock;
mod output;
mod paired;
mod refs;
mod statistics;
mod stats;
mod stats_report;
mod stats_source;
#[cfg(unix)]
mod unix_security;
#[cfg(windows)]
pub(crate) mod windows_security;

use std::path::Path;

use clap::{Arg, ArgAction, ArgMatches, Command, value_parser};

use crate::Result;

pub(crate) fn command() -> Command {
    Command::new("bench")
        .about("Run controlled performance comparisons")
        .after_help(
            "SECURITY: refs, crates, lock, and stats build and execute selected code with the current user's ambient authority. They are not sandboxed.",
        )
        .subcommand_required(true)
        .subcommand(
            Command::new("refs")
                .about("Compare two Git revisions with Criterion ABBA blocks")
                .arg(required("baseline"))
                .arg(required("candidate"))
                .arg(trust_selected_code())
                .arg(
                    Arg::new("bench")
                        .long("bench")
                        .action(ArgAction::Append)
                        .value_name("NAME"),
                )
                .arg(
                    Arg::new("filter")
                        .long("filter")
                        .default_value("lock_unlock"),
                )
                .arg(
                    Arg::new("features")
                        .long("features")
                        .default_value("subject-fs2"),
                )
                .arg(number("blocks"))
                .arg(number("sample-size"))
                .arg(float("warm-up-seconds"))
                .arg(float("measurement-seconds"))
                .arg(float("cooldown-seconds"))
                .arg(path("output"))
                .arg(
                    Arg::new("exploratory")
                        .long("exploratory")
                        .help("Report regressions without returning a failing exit status")
                        .action(ArgAction::SetTrue),
                ),
        )
        .subcommand(
            Command::new("crates")
                .about("Compare byte-identical Criterion workloads across crate checkouts")
                .arg(required_path("baseline"))
                .arg(required_path("candidate"))
                .arg(trust_selected_code())
                .arg(
                    Arg::new("baseline-package")
                        .long("baseline-package")
                        .default_value("fs2")
                        .value_parser(["fs2", "fs4"]),
                )
                .arg(
                    Arg::new("candidate-package")
                        .long("candidate-package")
                        .default_value("fs2")
                        .value_parser(["fs2", "fs4"]),
                )
                .arg(
                    Arg::new("bench")
                        .long("bench")
                        .default_value("fs2_legacy")
                        .value_parser(["fs2", "fs2_legacy", "fs_compat"]),
                )
                .arg(Arg::new("filter").long("filter"))
                .arg(path("target-root"))
                .arg(path("report"))
                .arg(number("pairs"))
                .arg(number("sample-size"))
                .arg(float("warm-up-seconds"))
                .arg(float("measurement-seconds"))
                .arg(float("non-inferiority-margin"))
                .arg(
                    Arg::new("allow-different-locks")
                        .long("allow-different-locks")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("retain-targets")
                        .long("retain-targets")
                        .help("Retain isolated Cargo target directories after each replicate")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("exploratory")
                        .long("exploratory")
                        .help("Record results without issuing a strict performance decision")
                        .action(ArgAction::SetTrue),
                ),
        )
        .subcommand(
            Command::new("lock")
                .about("Run same-process paired current/legacy lock measurements")
                .arg(required_path("output"))
                .arg(trust_selected_code())
                .arg(number("replicates"))
                .arg(number("sample-size"))
                .arg(float("warm-up-seconds"))
                .arg(float("measurement-seconds"))
                .arg(float("cooldown-seconds"))
                .arg(
                    Arg::new("skip-aa-control")
                        .long("skip-aa-control")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("exploratory")
                        .long("exploratory")
                        .help("Record results without issuing a strict performance decision")
                        .action(ArgAction::SetTrue),
                ),
        )
        .subcommand(
            Command::new("stats")
                .about("Run same-process paired filesystem-stat measurements")
                .arg(required("baseline"))
                .arg(required("candidate"))
                .arg(trust_selected_code())
                .arg(path("repo"))
                .arg(path("fixture"))
                .arg(path("output-root").help(
                    "Use a new or existing trusted root for secure staging and publication; output must remain beneath it",
                ))
                .arg(required_path("output"))
                .arg(number("replicates"))
                .arg(number("sample-size"))
                .arg(float("warm-up-seconds"))
                .arg(float("measurement-seconds"))
                .arg(float("cooldown-seconds"))
                .arg(
                    Arg::new("skip-aa-control")
                        .long("skip-aa-control")
                        .action(ArgAction::SetTrue),
                )
                .arg(
                    Arg::new("exploratory")
                        .long("exploratory")
                        .help("Record results without issuing a strict performance decision")
                        .action(ArgAction::SetTrue),
                ),
        )
}

pub(crate) fn run(root: &Path, arguments: &ArgMatches) -> Result<()> {
    match arguments.subcommand() {
        Some(("refs", matches)) => {
            require_selected_code_trust(matches)?;
            refs::run(root, matches)
        }
        Some(("crates", matches)) => {
            require_selected_code_trust(matches)?;
            crates::run(root, matches)
        }
        Some(("lock", matches)) => {
            require_selected_code_trust(matches)?;
            lock::run(root, matches)
        }
        Some(("stats", matches)) => {
            require_selected_code_trust(matches)?;
            stats::run(root, matches)
        }
        _ => unreachable!("clap requires a benchmark mode"),
    }
}

fn trust_selected_code() -> Arg {
    Arg::new("trust-selected-code")
        .long("trust-selected-code")
        .help(
            "Acknowledge that selected code is trusted and will execute unsandboxed with ambient user authority",
        )
        .action(ArgAction::SetTrue)
}

fn require_selected_code_trust(arguments: &ArgMatches) -> Result<()> {
    if arguments.get_flag("trust-selected-code") {
        Ok(())
    } else {
        Err(crate::invalid_data(
            "selected benchmark code executes unsandboxed with the current user's ambient authority; inspect it, then pass --trust-selected-code",
        ))
    }
}

fn required(name: &'static str) -> Arg {
    Arg::new(name).long(name).required(true).value_name("VALUE")
}

fn path(name: &'static str) -> Arg {
    Arg::new(name)
        .long(name)
        .value_parser(value_parser!(std::path::PathBuf))
}

fn required_path(name: &'static str) -> Arg {
    path(name).required(true)
}

fn number(name: &'static str) -> Arg {
    Arg::new(name).long(name).value_parser(value_parser!(usize))
}

fn float(name: &'static str) -> Arg {
    Arg::new(name).long(name).value_parser(value_parser!(f64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_code_requires_an_explicit_trust_acknowledgement() {
        let missing = Command::new("test")
            .arg(trust_selected_code())
            .try_get_matches_from(["test"])
            .unwrap();
        assert!(require_selected_code_trust(&missing).is_err());

        let acknowledged = Command::new("test")
            .arg(trust_selected_code())
            .try_get_matches_from(["test", "--trust-selected-code"])
            .unwrap();
        require_selected_code_trust(&acknowledged).unwrap();
    }

    #[test]
    fn stats_accepts_an_explicit_output_root() {
        let matches = command()
            .try_get_matches_from([
                "bench",
                "stats",
                "--baseline",
                "baseline",
                "--candidate",
                "candidate",
                "--output-root",
                "trusted",
                "--output",
                "trusted/result",
            ])
            .unwrap();
        let (_, stats) = matches.subcommand().unwrap();

        assert_eq!(
            stats.get_one::<std::path::PathBuf>("output-root"),
            Some(&std::path::PathBuf::from("trusted"))
        );
    }

    #[test]
    fn lock_mode_enforces_selected_code_trust() {
        let arguments = command()
            .try_get_matches_from(["bench", "lock", "--output", "out"])
            .unwrap();

        assert!(run(Path::new("."), &arguments).is_err());
    }
}
