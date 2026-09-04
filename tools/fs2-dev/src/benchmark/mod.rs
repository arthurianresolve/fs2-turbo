mod arguments;
mod common;
mod crates;
mod criterion;
mod diagnostics;
mod evidence;
mod host;
mod lock;
mod markdown;
mod noise;
mod output;
mod pages;
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
            "SECURITY: code-executing benchmark profiles use the current user's ambient authority. Selected code is not sandboxed.",
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
                .arg(
                    Arg::new("compare-upstream-v0-4-3")
                        .long("compare-upstream-v0-4-3")
                        .help(
                            "Also compare lock_unlock against exact upstream fs2 v0.4.3 under the output directory",
                        )
                        .action(ArgAction::SetTrue),
                )
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
                    Arg::new("common-v0-4")
                        .long("common-v0-4")
                        .help("Measure only filesystem-stat workloads available in fs2 v0.4")
                        .action(ArgAction::SetTrue),
                )
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
        .subcommand(paired_ref_command(
            "common-refs",
            "Run the full common API comparison across exact Git refs",
        ))
        .subcommand(paired_ref_command(
            "lock-refs",
            "Run same-process paired lock measurements across exact Git refs",
        ))
        .subcommand(paired_ref_command(
            "file-create-delete-refs",
            "Measure the original common-API file_create_delete workload in isolation",
        ))
        .subcommand(
            diagnostic_ref_command(
                "duplicate-refs",
                "Measure 64-operation duplicate batches using a dedicated 20-second policy",
            ),
        )
        .subcommand(diagnostic_ref_command(
            "duplicate-single-refs",
            "Measure only the original single-call duplicate workload, without batching",
        ))
        .subcommand(
            Command::new("correlate-samples")
                .about("Join diagnostic sample windows to normalized trace events; never performance evidence")
                .arg(Arg::new("samples").long("samples").required(true).action(ArgAction::Append).value_parser(value_parser!(std::path::PathBuf)))
                .arg(path("events")),
        )
        .subcommand(
            idle_arguments(Command::new("admit-host")
                .about("Observe native Windows host load once; never performance evidence")),
        )
        .subcommand(
            Command::new("noise-report")
                .about("Report paired ratio excursion magnitudes, including rejected blocks")
                .arg(Arg::new("samples").long("samples").required(true)
                    .action(ArgAction::Append).value_parser(value_parser!(std::path::PathBuf))),
        )
        .subcommand(
            Command::new("pages")
                .about("Publish only accepted branch-specific runner benchmark results")
                .arg(required_path("input"))
                .arg(required_path("previous"))
                .arg(required_path("output"))
                .arg(required("branch"))
                .arg(required("candidate"))
                .arg(required("run-id"))
                .arg(required("attempt")),
        )
        .subcommand(
            Command::new("markdown")
                .about("Render retained benchmark reports as PR-ready Markdown")
                .arg(
                    Arg::new("report")
                        .long("report")
                        .required(true)
                        .action(ArgAction::Append)
                        .value_parser(value_parser!(std::path::PathBuf)),
                ),
        )
}

fn paired_ref_command(name: &'static str, about: &'static str) -> Command {
    idle_arguments(Command::new(name)
        .about(about)
        .arg(required("baseline"))
        .arg(required("candidate"))
        .arg(trust_selected_code())
        .arg(path("repo"))
        .arg(path("fixture"))
        .arg(path("output-root").help("Use a new private root, or an existing root with protected permissions; do not pre-create it with inherited ACLs"))
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
        ))
}

fn idle_arguments(command: Command) -> Command {
    command
        .arg(
            float("idle-max-core-busy-percent")
                .help("Explicit maximum summed sibling mean load; requires native host admission")
                .requires("idle-max-sample-busy-percent"),
        )
        .arg(
            float("idle-max-sample-busy-percent")
                .help("Explicit maximum summed sibling load in every pre-run observation")
                .requires("idle-max-core-busy-percent"),
        )
}

fn diagnostic_ref_command(name: &'static str, about: &'static str) -> Command {
    paired_ref_command(name, about)
        .arg(Arg::new("diagnostic-samples").long("diagnostic-samples")
            .help("Capture sample-boundary clocks, thread and CPU IDs on stderr; diagnostic only")
            .requires("exploratory").action(ArgAction::SetTrue))
        .arg(Arg::new("diagnostic-trace").long("diagnostic-trace")
            .help("Also declare external tracing; enables diagnostic samples but does not manage trace sessions")
            .requires("exploratory").action(ArgAction::SetTrue))
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
        Some(("lock-refs", matches)) => {
            require_selected_code_trust(matches)?;
            stats::run_lock_refs(root, matches)
        }
        Some(("common-refs", matches)) => {
            require_selected_code_trust(matches)?;
            stats::run_common_refs(root, matches)
        }
        Some(("file-create-delete-refs", matches)) => {
            require_selected_code_trust(matches)?;
            stats::run_file_create_delete_refs(root, matches)
        }
        Some(("duplicate-refs", matches)) => {
            require_selected_code_trust(matches)?;
            stats::run_duplicate_refs(root, matches)
        }
        Some(("duplicate-single-refs", matches)) => {
            require_selected_code_trust(matches)?;
            stats::run_duplicate_single_refs(root, matches)
        }
        Some(("correlate-samples", matches)) => diagnostics::run(matches),
        Some(("admit-host", matches)) => host::run(matches),
        Some(("noise-report", matches)) => noise::run(matches),
        Some(("markdown", matches)) => markdown::run(matches),
        Some(("pages", matches)) => pages::run(matches),
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
    fn host_admission_limits_must_be_explicit_finite_and_ordered() {
        for (mean, peak, valid) in [
            ("5", "20", true),
            ("NaN", "20", false),
            ("20", "5", false),
            ("0", "20", false),
            ("5", "101", false),
        ] {
            let matches = command()
                .try_get_matches_from([
                    "bench",
                    "admit-host",
                    "--idle-max-core-busy-percent",
                    mean,
                    "--idle-max-sample-busy-percent",
                    peak,
                ])
                .unwrap();
            let (_, arguments) = matches.subcommand().unwrap();
            assert_eq!(host::Policy::from_arguments(arguments).is_ok(), valid);
        }
        assert!(
            command()
                .try_get_matches_from(["bench", "admit-host", "--idle-max-core-busy-percent", "5",])
                .is_err()
        );
    }

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

    #[test]
    fn duplicate_diagnostics_require_exploratory_and_selected_code_trust() {
        for profile in ["duplicate-refs", "duplicate-single-refs"] {
            for flag in ["--diagnostic-samples", "--diagnostic-trace"] {
                let args = [
                    "bench",
                    profile,
                    "--baseline",
                    "base",
                    "--candidate",
                    "head",
                    "--output",
                    "out",
                    flag,
                ];
                assert!(command().try_get_matches_from(args).is_err());
                let args = args.into_iter().chain(["--exploratory"]);
                let matches = command().try_get_matches_from(args).unwrap();
                assert!(run(Path::new("."), &matches).is_err());
            }
        }
    }
}
