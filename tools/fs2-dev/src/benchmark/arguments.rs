use std::path::{Path, PathBuf};

use clap::ArgMatches;
use serde::Serialize;

use super::criterion::CriterionSettings;
use super::paired;
use crate::policy;
use crate::{Result, invalid_data};

#[derive(Clone, Debug, Serialize)]
pub(crate) struct EvidenceMode {
    pub(crate) strict: bool,
    pub(crate) reasons: Vec<String>,
}

impl EvidenceMode {
    pub(crate) fn strict() -> Self {
        Self {
            strict: true,
            reasons: Vec::new(),
        }
    }

    pub(crate) fn exploratory(reason: impl Into<String>) -> Self {
        Self {
            strict: false,
            reasons: vec![reason.into()],
        }
    }

    pub(crate) fn weaken(&mut self, reason: impl Into<String>) {
        let reason = reason.into();
        self.strict = false;
        if !self.reasons.contains(&reason) {
            self.reasons.push(reason);
        }
    }

    pub(crate) fn strict_configuration(&self) -> bool {
        self.strict
    }
}

#[derive(Clone, Copy)]
pub(crate) enum CriterionProfile {
    RefToRef,
    CrossCrate,
}

pub(crate) fn required_path(arguments: &ArgMatches, name: &str) -> Result<PathBuf> {
    arguments
        .get_one::<PathBuf>(name)
        .cloned()
        .ok_or_else(|| invalid_data(format!("missing --{name}")))
}

pub(crate) fn required_string<'a>(arguments: &'a ArgMatches, name: &str) -> Result<&'a str> {
    arguments
        .get_one::<String>(name)
        .map(String::as_str)
        .ok_or_else(|| invalid_data(format!("missing --{name}")))
}

pub(crate) fn absolute(root: &Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        root.join(path)
    }
}

pub(crate) fn criterion_settings(
    arguments: &ArgMatches,
    policy: &policy::MeasurementPolicy,
) -> Result<CriterionSettings> {
    let settings = CriterionSettings {
        sample_size: arguments
            .get_one::<usize>("sample-size")
            .copied()
            .unwrap_or(usize::try_from(policy.criterion.sample_size)?),
        warm_up_seconds: arguments
            .get_one::<f64>("warm-up-seconds")
            .copied()
            .unwrap_or(policy.criterion.warm_up_seconds),
        measurement_seconds: arguments
            .get_one::<f64>("measurement-seconds")
            .copied()
            .unwrap_or(policy.criterion.measurement_seconds),
    };
    paired::validate_settings(
        1,
        settings.sample_size,
        settings.warm_up_seconds,
        settings.measurement_seconds,
        0.0,
    )
    .map_err(|_| invalid_data("invalid Criterion settings"))?;
    Ok(settings)
}

pub(crate) fn criterion_matches_policy(
    settings: CriterionSettings,
    policy: &policy::MeasurementPolicy,
) -> Result<bool> {
    Ok(
        settings.sample_size == usize::try_from(policy.criterion.sample_size)?
            && settings.warm_up_seconds == policy.criterion.warm_up_seconds
            && settings.measurement_seconds == policy.criterion.measurement_seconds,
    )
}

pub(crate) fn criterion_evidence_mode(
    explicitly_exploratory: bool,
    settings: CriterionSettings,
    policy: &policy::MeasurementPolicy,
    profile: CriterionProfile,
) -> Result<EvidenceMode> {
    let mut mode = if explicitly_exploratory {
        EvidenceMode::exploratory("explicit --exploratory request")
    } else {
        EvidenceMode::strict()
    };
    require_exploratory(
        &mut mode,
        explicitly_exploratory,
        !criterion_matches_policy(settings, policy)?,
        "Criterion settings differ from the measurement policy",
    )?;
    let strict_profile = match profile {
        CriterionProfile::RefToRef => policy.meets_strict_ref_profile(),
        CriterionProfile::CrossCrate => policy.meets_strict_cross_crate_profile(),
    };
    require_exploratory(
        &mut mode,
        explicitly_exploratory,
        !strict_profile,
        "measurement policy is weaker than the immutable strict profile",
    )?;
    Ok(mode)
}

pub(crate) fn require_exploratory(
    mode: &mut EvidenceMode,
    explicitly_exploratory: bool,
    differs: bool,
    reason: &'static str,
) -> Result<()> {
    if !differs {
        return Ok(());
    }
    if explicitly_exploratory {
        mode.weaken(reason);
        Ok(())
    } else {
        Err(invalid_data(format!("{reason}; pass --exploratory")))
    }
}
