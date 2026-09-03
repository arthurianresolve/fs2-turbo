use std::io::Write;
use std::path::Path;

use serde::Serialize;
use tempfile::NamedTempFile;

use crate::Result;

pub(crate) const SCHEMA_VERSION: u64 = 10;

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ReportKind {
    CrossCrate,
    Lock,
    RefToRef,
    Stats,
}

#[derive(Serialize)]
struct ExecutionTrust {
    selected_code_acknowledgement: &'static str,
    authority: &'static str,
    sandbox: &'static str,
    strict_scope: &'static str,
}

impl ExecutionTrust {
    const fn for_kind(_report_kind: ReportKind) -> Self {
        Self {
            selected_code_acknowledgement: "explicitly-acknowledged",
            authority: "ambient-user",
            sandbox: "none",
            strict_scope: "provenance-and-statistical-rigor-for-trusted-subjects",
        }
    }
}

#[derive(Serialize)]
pub(crate) struct ReportEnvelope<T> {
    schema_version: u64,
    report_kind: ReportKind,
    status: &'static str,
    valid: bool,
    execution_trust: ExecutionTrust,
    #[serde(flatten)]
    content: T,
}

impl<T> ReportEnvelope<T> {
    pub(crate) const fn new(
        report_kind: ReportKind,
        status: &'static str,
        valid: bool,
        content: T,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            report_kind,
            status,
            valid,
            execution_trust: ExecutionTrust::for_kind(report_kind),
            content,
        }
    }
}

#[derive(Serialize)]
struct InvalidExecution<'a, T> {
    error: &'a str,
    #[serde(flatten)]
    context: T,
}

#[derive(Serialize)]
struct SetupExecutionFailure<'a> {
    error: &'a str,
    processes: &'a [crate::process::ProcessRecord],
}

pub(crate) fn write_invalid<T: Serialize>(
    path: &Path,
    report_kind: ReportKind,
    error: &str,
    context: T,
) -> Result<()> {
    write_json(
        path,
        &ReportEnvelope::new(
            report_kind,
            "invalid-execution",
            false,
            InvalidExecution { error, context },
        ),
    )
}

pub(crate) fn write_setup_failure(
    path: &Path,
    report_kind: ReportKind,
    error: &str,
    processes: &[crate::process::ProcessRecord],
) -> Result<()> {
    write_json(
        path,
        &ReportEnvelope::new(
            report_kind,
            "setup-failure",
            false,
            SetupExecutionFailure { error, processes },
        ),
    )
}

pub(crate) fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("report path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(temporary.as_file_mut(), value)?;
    temporary.as_file_mut().write_all(b"\n")?;
    temporary.as_file_mut().sync_all()?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| error.error)?;
    #[cfg(unix)]
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_envelopes_publish_the_current_schema() {
        let report = serde_json::to_value(ReportEnvelope::new(
            ReportKind::Lock,
            "completed",
            true,
            serde_json::json!({ "run": 1 }),
        ))
        .unwrap();
        assert_eq!(SCHEMA_VERSION, 10);
        assert_eq!(
            report,
            serde_json::json!({
                "schema_version": 10,
                "report_kind": "lock",
                "status": "completed",
                "valid": true,
                "execution_trust": {
                    "selected_code_acknowledgement": "explicitly-acknowledged",
                    "authority": "ambient-user",
                    "sandbox": "none",
                    "strict_scope": "provenance-and-statistical-rigor-for-trusted-subjects"
                },
                "run": 1
            })
        );

        let invalid = serde_json::to_value(ReportEnvelope::new(
            ReportKind::Stats,
            "invalid-execution",
            false,
            serde_json::json!({ "error": "setup failed" }),
        ))
        .unwrap();
        assert_eq!(invalid["schema_version"], 10);
        assert_eq!(invalid["report_kind"], "stats");
        assert_eq!(invalid["status"], "invalid-execution");
        assert_eq!(invalid["valid"], false);
        assert_eq!(invalid["error"], "setup failed");
        assert_eq!(
            invalid["execution_trust"]["selected_code_acknowledgement"],
            "explicitly-acknowledged"
        );

        let mut command = std::process::Command::new("cargo");
        command.current_dir("repository").arg("build");
        let process = crate::process::ProcessRecord::skipped(
            &command,
            "build",
            "stdout".into(),
            "stderr".into(),
            "setup failed",
        );
        let skipped = serde_json::to_value(ReportEnvelope::new(
            ReportKind::CrossCrate,
            "setup-failure",
            false,
            serde_json::json!({ "process": process }),
        ))
        .unwrap();
        assert_eq!(skipped["schema_version"], SCHEMA_VERSION);
        assert_eq!(skipped["process"]["outcome"]["kind"], "skipped");
        assert_eq!(
            skipped["process"]["command"],
            serde_json::json!(["cargo", "build"])
        );
        assert_eq!(skipped["process"]["current_dir"], "repository");
    }

    #[test]
    fn report_writes_are_atomic_and_never_replace_existing_evidence() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("report.json");
        write_json(&path, &serde_json::json!({ "run": 1 })).unwrap();
        assert!(write_json(&path, &serde_json::json!({ "run": 2 })).is_err());
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&std::fs::read(path).unwrap()).unwrap(),
            serde_json::json!({ "run": 1 })
        );
    }
}
