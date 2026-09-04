use std::path::{Path, PathBuf};

use serde::ser::Error as _;
use serde::{Serialize, Serializer};

use super::arguments::EvidenceMode;
use super::evidence::EnvironmentSnapshot;
use super::paired::{Comparison, Control, Measurement, RunRecord};
use crate::process::ProcessRecord;

macro_rules! logical_path_serializer {
    ($name:ident, $value:literal) => {
        fn $name<T: ?Sized, S: Serializer>(
            _value: &T,
            serializer: S,
        ) -> std::result::Result<S::Ok, S::Error> {
            serializer.serialize_str($value)
        }
    };
}

logical_path_serializer!(serialize_fixture_path, "fixture");
logical_path_serializer!(serialize_harness_source, "inputs/stats-harness-source");
logical_path_serializer!(serialize_paired_core_source, "inputs/paired-core-source");
logical_path_serializer!(
    serialize_paired_protocol_source,
    "inputs/paired-protocol-source"
);
logical_path_serializer!(
    serialize_paired_stats_protocol_source,
    "inputs/paired-stats-protocol-source"
);
logical_path_serializer!(serialize_manifest_path, "build/Cargo.toml");
logical_path_serializer!(serialize_policy_path, "inputs/benchmark-policy");
logical_path_serializer!(serialize_logs_path, "logs");
logical_path_serializer!(serialize_baseline_repository, "artifacts/sources/baseline");
logical_path_serializer!(
    serialize_candidate_repository,
    "artifacts/sources/candidate"
);
logical_path_serializer!(serialize_cargo_lock_path, "build/Cargo.lock");

fn serialize_binary_path<S>(value: &Path, serializer: S) -> std::result::Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let file_name = value
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| {
            !name.is_empty()
                && name.len() <= 128
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        })
        .unwrap_or("paired-stats");
    serializer.serialize_str(&format!("artifacts/binary/{file_name}"))
}

fn serialize_sanitized<T, S>(value: &T, serializer: S) -> std::result::Result<S::Ok, S::Error>
where
    T: Serialize + ?Sized,
    S: Serializer,
{
    let mut value = serde_json::to_value(value).map_err(S::Error::custom)?;
    super::output::sanitize_serialized_report(&mut value);
    value.serialize(serializer)
}

#[derive(Serialize)]
struct RawSetupProcesses<'a> {
    source: &'a [ProcessRecord],
    lock: &'a ProcessRecord,
    build: &'a ProcessRecord,
}

pub(super) struct SetupProcesses<'a> {
    pub(super) source: &'a [ProcessRecord],
    pub(super) lock: &'a ProcessRecord,
    pub(super) build: &'a ProcessRecord,
}

impl Serialize for SetupProcesses<'_> {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serialize_sanitized(
            &RawSetupProcesses {
                source: self.source,
                lock: self.lock,
                build: self.build,
            },
            serializer,
        )
    }
}

#[derive(Serialize)]
pub(super) struct SetupFailureReport<'a> {
    pub(super) decision: &'static str,
    pub(super) profile: &'static str,
    pub(super) environment: EnvironmentSnapshot,
    #[serde(serialize_with = "serialize_sanitized")]
    pub(super) baseline_source: &'a str,
    #[serde(serialize_with = "serialize_sanitized")]
    pub(super) candidate_source: &'a str,
    #[serde(serialize_with = "serialize_fixture_path")]
    pub(super) fixture: &'a Path,
    #[serde(serialize_with = "serialize_harness_source")]
    pub(super) harness_source: PathBuf,
    pub(super) harness_source_sha256: String,
    #[serde(serialize_with = "serialize_paired_core_source")]
    pub(super) paired_core_source: PathBuf,
    pub(super) paired_core_source_sha256: String,
    #[serde(serialize_with = "serialize_paired_protocol_source")]
    pub(super) paired_protocol_source: PathBuf,
    pub(super) paired_protocol_source_sha256: String,
    #[serde(serialize_with = "serialize_paired_stats_protocol_source")]
    pub(super) paired_stats_protocol_source: PathBuf,
    pub(super) paired_stats_protocol_source_sha256: String,
    #[serde(serialize_with = "serialize_manifest_path")]
    pub(super) manifest: PathBuf,
    pub(super) manifest_sha256: String,
    #[serde(serialize_with = "serialize_policy_path")]
    pub(super) policy: &'a Path,
    pub(super) policy_sha256: String,
    #[serde(serialize_with = "serialize_logs_path")]
    pub(super) logs: &'a Path,
    pub(super) processes: SetupProcesses<'a>,
}

#[derive(Serialize)]
pub(super) struct StatsMethod {
    pub(super) profile: &'static str,
    pub(super) name: &'static str,
    pub(super) reason: &'static str,
    pub(super) operations_per_timed_interval: u64,
    pub(super) diagnostic_samples: bool,
    pub(super) host_admission_sha256: Option<String>,
    pub(super) noise_report_sha256: String,
    pub(super) non_regression_margin: f64,
    pub(super) aa_equivalence_margin: f64,
    pub(super) confidence: f64,
    pub(super) process_replicates: usize,
    pub(super) sample_size: usize,
    pub(super) warm_up_seconds: f64,
    pub(super) measurement_seconds: f64,
    pub(super) cooldown_seconds: f64,
    pub(super) aa_control: bool,
    pub(super) first_invocation_policy: &'static str,
    pub(super) prime_timings_used: bool,
    pub(super) inference: &'static str,
    pub(super) source_identity: &'static str,
}

#[derive(Serialize)]
pub(super) struct StatsArtifacts<'a> {
    #[serde(serialize_with = "serialize_harness_source")]
    pub(super) harness_source: PathBuf,
    pub(super) harness_source_sha256: String,
    #[serde(serialize_with = "serialize_paired_core_source")]
    pub(super) paired_core_source: PathBuf,
    pub(super) paired_core_source_sha256: String,
    #[serde(serialize_with = "serialize_paired_protocol_source")]
    pub(super) paired_protocol_source: PathBuf,
    pub(super) paired_protocol_source_sha256: String,
    #[serde(serialize_with = "serialize_paired_stats_protocol_source")]
    pub(super) paired_stats_protocol_source: PathBuf,
    pub(super) paired_stats_protocol_source_sha256: String,
    #[serde(serialize_with = "serialize_policy_path")]
    pub(super) policy: &'a Path,
    pub(super) policy_sha256: String,
    #[serde(serialize_with = "serialize_manifest_path")]
    pub(super) manifest: PathBuf,
    pub(super) manifest_sha256: String,
    #[serde(serialize_with = "serialize_baseline_repository")]
    pub(super) baseline_repository: PathBuf,
    pub(super) baseline_repository_sha256: String,
    #[serde(serialize_with = "serialize_candidate_repository")]
    pub(super) candidate_repository: PathBuf,
    pub(super) candidate_repository_sha256: String,
    #[serde(serialize_with = "serialize_cargo_lock_path")]
    pub(super) cargo_lock: PathBuf,
    pub(super) cargo_lock_sha256: String,
    #[serde(serialize_with = "serialize_binary_path")]
    pub(super) binary: PathBuf,
    pub(super) binary_sha256: String,
    #[serde(serialize_with = "serialize_logs_path")]
    pub(super) logs: &'a Path,
}

#[derive(Serialize)]
struct RawStatsProcesses<'a> {
    source: &'a [ProcessRecord],
    lock: &'a ProcessRecord,
    build: &'a ProcessRecord,
    runs: &'a [RunRecord],
}

pub(super) struct StatsProcesses<'a> {
    pub(super) source: &'a [ProcessRecord],
    pub(super) lock: &'a ProcessRecord,
    pub(super) build: &'a ProcessRecord,
    pub(super) runs: &'a [RunRecord],
}

impl Serialize for StatsProcesses<'_> {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serialize_sanitized(
            &RawStatsProcesses {
                source: self.source,
                lock: self.lock,
                build: self.build,
                runs: self.runs,
            },
            serializer,
        )
    }
}

#[derive(Serialize)]
pub(super) struct StatsReport<'a> {
    pub(super) decision: &'static str,
    pub(super) strict_configuration: bool,
    pub(super) evidence_mode: &'a EvidenceMode,
    #[serde(serialize_with = "serialize_sanitized")]
    pub(super) baseline_source: &'a str,
    #[serde(serialize_with = "serialize_sanitized")]
    pub(super) candidate_source: &'a str,
    pub(super) environment: EnvironmentSnapshot,
    pub(super) completed_environment: EnvironmentSnapshot,
    #[serde(serialize_with = "serialize_fixture_path")]
    pub(super) fixture: &'a Path,
    pub(super) method: StatsMethod,
    pub(super) artifacts: StatsArtifacts<'a>,
    pub(super) processes: StatsProcesses<'a>,
    #[serde(serialize_with = "serialize_sanitized")]
    pub(super) anomalies: &'a [String],
    pub(super) ab: Comparison<'a>,
    pub(super) aa_control: Control<'a>,
    pub(super) records: &'a [Measurement],
    pub(super) completed_unix_ms: u128,
}

#[derive(Serialize)]
pub(super) struct StatsInvalidContext<'a> {
    pub(super) decision: &'static str,
    pub(super) profile: &'static str,
    pub(super) environment: Option<EnvironmentSnapshot>,
    #[serde(serialize_with = "serialize_sanitized")]
    pub(super) baseline_ref: &'a str,
    #[serde(serialize_with = "serialize_sanitized")]
    pub(super) candidate_ref: &'a str,
    #[serde(serialize_with = "serialize_sanitized")]
    pub(super) baseline_source: Option<String>,
    #[serde(serialize_with = "serialize_sanitized")]
    pub(super) candidate_source: Option<String>,
    #[serde(serialize_with = "serialize_fixture_path")]
    pub(super) fixture: &'a Path,
    pub(super) harness_source_sha256: Option<String>,
    pub(super) policy_sha256: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::process::ProcessOutcome;

    fn private_path(name: &str) -> PathBuf {
        PathBuf::from(r"C:\Users\sentinel-user\private-benchmark").join(name)
    }

    #[test]
    fn report_paths_serialize_as_logical_ids_without_losing_hash_bindings() {
        let policy = private_path("benchmark-policy.toml");
        let logs = private_path("logs");
        let artifacts = StatsArtifacts {
            harness_source: private_path("harness.rs"),
            harness_source_sha256: "harness-sha256".to_owned(),
            paired_core_source: private_path("paired_core.rs"),
            paired_core_source_sha256: "core-sha256".to_owned(),
            paired_protocol_source: private_path("paired_protocol.rs"),
            paired_protocol_source_sha256: "protocol-sha256".to_owned(),
            paired_stats_protocol_source: private_path("paired_stats_protocol.rs"),
            paired_stats_protocol_source_sha256: "stats-protocol-sha256".to_owned(),
            policy: &policy,
            policy_sha256: "policy-sha256".to_owned(),
            manifest: private_path("Cargo.toml"),
            manifest_sha256: "manifest-sha256".to_owned(),
            baseline_repository: private_path("baseline-repository"),
            baseline_repository_sha256: "baseline-tree-sha256".to_owned(),
            candidate_repository: private_path("candidate-repository"),
            candidate_repository_sha256: "candidate-tree-sha256".to_owned(),
            cargo_lock: private_path("Cargo.lock"),
            cargo_lock_sha256: "lock-sha256".to_owned(),
            binary: private_path("paired-stats.exe"),
            binary_sha256: "binary-sha256".to_owned(),
            logs: &logs,
        };

        let json = serde_json::to_string(&artifacts).unwrap();

        assert!(!json.contains("sentinel-user"));
        assert!(!json.contains("private-benchmark"));
        assert!(json.contains("artifacts/sources/baseline"));
        assert!(json.contains("artifacts/sources/candidate"));
        assert!(json.contains("artifacts/binary/paired-stats.exe"));
        assert!(json.contains("baseline-tree-sha256"));
        assert!(json.contains("candidate-tree-sha256"));
        assert!(json.contains("binary-sha256"));

        let fixture = private_path("private-fixture");
        let context = StatsInvalidContext {
            decision: "invalid-execution",
            profile: "test-profile",
            environment: None,
            baseline_ref: r"C:\Users\sentinel-user\private-ref",
            candidate_ref: "candidate",
            baseline_source: None,
            candidate_source: None,
            fixture: &fixture,
            harness_source_sha256: Some("harness-sha256".to_owned()),
            policy_sha256: Some("policy-sha256".to_owned()),
        };
        let json = serde_json::to_string(&context).unwrap();
        assert!(!json.contains("sentinel-user"));
        assert!(!json.contains("private-fixture"));
        assert!(json.contains("\"fixture\":\"fixture\""));
    }

    #[test]
    fn process_serialization_omits_operational_paths_and_override_values() {
        let mut environment_overrides = BTreeMap::new();
        environment_overrides.insert(
            "CARGO_HOME".to_owned(),
            Some(private_path("cargo-home").display().to_string()),
        );
        let record = ProcessRecord {
            label: "capture Cargo facts".to_owned(),
            command: vec![
                private_path("cargo.exe").display().to_string(),
                "--version".to_owned(),
            ],
            current_dir: Some(private_path("working-directory").display().to_string()),
            environment_overrides,
            outcome: ProcessOutcome::Exited { code: 0 },
            duration_ms: 1,
            timeout_ms: Some(1_000),
            containment: "test",
            stdout: private_path("cargo.stdout"),
            stderr: private_path("cargo.stderr"),
        };
        let source = [record.clone()];
        let processes = SetupProcesses {
            source: &source,
            lock: &record,
            build: &record,
        };

        let json = serde_json::to_string(&processes).unwrap();

        assert!(!json.contains("sentinel-user"));
        assert!(!json.contains("private-benchmark"));
        assert!(json.contains("\"command\":[\"<host-path>\",\"--version\"]"));
        assert!(json.contains("\"current_dir\":\"<working-directory>\""));
        assert!(json.contains("\"CARGO_HOME\":\"<configured>\""));
        assert!(json.contains("\"stdout\":\"logs/cargo.stdout\""));
        assert!(json.contains("\"stderr\":\"logs/cargo.stderr\""));
    }
}
