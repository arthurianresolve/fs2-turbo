use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::process;
use crate::{Result, invalid_data, lower_hex};

const EXPECTED_CONSUMER_SHA256: &str =
    "a04871a9a4c170cc8170f582a67f0fa0ab350c1a4afc014d21812fd9165eb8c7";
const LEGACY_CHECKSUM: &str = "9564fc758e15025b46aa6643b1b77d047d1a56a1aea6e01002ac0c7026876213";
const SUBJECTS: [&str; 2] = ["legacy", "current"];
const REQUIRED_EDITIONS: [&str; 4] = ["2015", "2018", "2021", "2024"];

#[derive(Debug, Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoPackage>,
}

#[derive(Debug, Deserialize)]
struct CargoPackage {
    name: String,
    version: String,
    edition: String,
    default_run: Option<String>,
    features: BTreeMap<String, Vec<String>>,
    source: Option<String>,
    manifest_path: PathBuf,
    dependencies: Vec<CargoDependency>,
    targets: Vec<CargoTarget>,
}

#[derive(Debug, Deserialize)]
struct CargoTarget {
    name: String,
    kind: Vec<String>,
    crate_types: Vec<String>,
    src_path: PathBuf,
}

#[derive(Debug, Deserialize)]
struct CargoDependency {
    name: String,
    req: String,
    source: Option<String>,
    rename: Option<String>,
    path: Option<PathBuf>,
    kind: Option<String>,
    target: Option<String>,
    optional: bool,
    uses_default_features: bool,
    features: Vec<String>,
}

pub(crate) fn run(root: &Path) -> Result<()> {
    let compatibility = root.join("compatibility");
    let manifest = compatibility.join("Cargo.toml");
    let consumer = compatibility.join("v04_consumer.rs");
    let digest = consumer_digest(&consumer)?;
    if digest != EXPECTED_CONSUMER_SHA256 {
        return Err(invalid_data(
            "frozen v0.4 consumer changed; update its digest only after an intentional API review",
        ));
    }
    println!("v0.4 consumer sha256={digest}");
    validate_lockfile(&compatibility.join("Cargo.lock"))?;
    let target = root
        .join("target/xtask/compatibility")
        .join(process::toolchain_key()?);
    let mut format = process::cargo();
    format
        .current_dir(root)
        .args(["fmt", "--manifest-path"])
        .arg(&manifest)
        .args(["--all", "--", "--check"]);
    process::run(&mut format, "format compatibility fixtures")?;

    let packages = compatibility_packages(root, &manifest, &target)?;
    validate_compatibility_packages(&compatibility, &consumer, &packages)?;
    validate_dependencies(root, &packages)?;

    for subject in SUBJECTS {
        let mut check = process::cargo();
        check
            .current_dir(root)
            .env("CARGO_TARGET_DIR", &target)
            .args(["check", "--workspace", "--manifest-path"])
            .arg(&manifest)
            .args(["--no-default-features", "--features", subject, "--locked"]);
        process::run(
            &mut check,
            &format!("check {subject} compatibility surface"),
        )?;
    }

    for package in &packages {
        for subject in SUBJECTS {
            let mut run = process::cargo();
            run.current_dir(root)
                .env("CARGO_TARGET_DIR", &target)
                .args(["run", "--manifest-path"])
                .arg(&manifest)
                .args([
                    "--package",
                    package.name.as_str(),
                    "--bin",
                    package.name.as_str(),
                    "--no-default-features",
                    "--features",
                    subject,
                    "--locked",
                ]);
            process::run(
                &mut run,
                &format!("run {subject} v0.4 consumer in edition {}", package.edition),
            )?;
        }
    }
    Ok(())
}

fn validate_resolved_fs2(root: &Path, packages: &[CargoPackage]) -> Result<()> {
    let resolved = packages
        .iter()
        .filter(|package| package.name == "fs2")
        .collect::<Vec<_>>();
    let legacy = resolved.iter().any(|package| {
        package.version == "0.4.3"
            && package.source.as_deref()
                == Some("registry+https://github.com/rust-lang/crates.io-index")
    });
    let current_manifest = root.join("Cargo.toml").canonicalize()?;
    let current = resolved.iter().any(|package| {
        package.source.is_none()
            && package
                .manifest_path
                .canonicalize()
                .is_ok_and(|path| path == current_manifest)
    });
    if resolved.len() != 2 || !legacy || !current {
        return Err(invalid_data(
            "resolved compatibility graph must contain only approved fs2 0.4.3 and the current checkout",
        ));
    }
    Ok(())
}

fn validate_lockfile(path: &Path) -> Result<()> {
    let contents = fs::read_to_string(path)?;
    let legacy = format!(
        "name = \"fs2\"\nversion = \"0.4.3\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"{LEGACY_CHECKSUM}\""
    );
    if !contents.replace("\r\n", "\n").contains(&legacy)
        || contents.matches("name = \"fs2\"").count() != 2
    {
        return Err(invalid_data(
            "compatibility lockfile does not contain exactly the approved legacy and current fs2 packages",
        ));
    }
    Ok(())
}

fn expected_features() -> BTreeMap<String, Vec<String>> {
    BTreeMap::from([
        ("current".to_owned(), vec!["dep:fs2_current".to_owned()]),
        ("default".to_owned(), Vec::new()),
        ("legacy".to_owned(), vec!["dep:fs2_v04".to_owned()]),
    ])
}

fn validate_compatibility_packages(
    compatibility: &Path,
    consumer: &Path,
    packages: &[CargoPackage],
) -> Result<()> {
    let consumer = consumer.canonicalize()?;
    let expected_features = expected_features();
    for package in packages {
        let expected_name = format!("fs2-compat-edition-{}", package.edition);
        let expected_manifest = compatibility
            .join(format!("edition-{}", package.edition))
            .join("Cargo.toml")
            .canonicalize()?;
        let manifest_matches = package
            .manifest_path
            .canonicalize()
            .is_ok_and(|path| path == expected_manifest);
        if package.name != expected_name
            || package.version != "0.0.0"
            || package.source.is_some()
            || package.default_run.is_some()
            || package.features != expected_features
            || !manifest_matches
        {
            return Err(invalid_data(format!(
                "{} does not match the frozen compatibility package contract",
                package.name
            )));
        }
        let [target] = package.targets.as_slice() else {
            return Err(invalid_data(format!(
                "{} must expose exactly one compatibility binary target",
                package.name
            )));
        };
        let source_matches = target
            .src_path
            .canonicalize()
            .is_ok_and(|path| path == consumer);
        if target.name != package.name
            || target.kind.as_slice() != ["bin"]
            || target.crate_types.as_slice() != ["bin"]
            || !source_matches
        {
            return Err(invalid_data(format!(
                "{} binary target must resolve to the frozen v0.4 consumer",
                package.name
            )));
        }
    }
    Ok(())
}

fn validate_dependencies(root: &Path, packages: &[CargoPackage]) -> Result<()> {
    let current = root.canonicalize()?;
    for package in packages {
        let legacy = package.dependencies.iter().any(|dependency| {
            dependency.name == "fs2"
                && dependency.rename.as_deref() == Some("fs2_v04")
                && dependency.req == "=0.4.3"
                && dependency.path.is_none()
                && dependency.source.as_deref()
                    == Some("registry+https://github.com/rust-lang/crates.io-index")
                && dependency.kind.is_none()
                && dependency.target.is_none()
                && dependency.optional
                && dependency.uses_default_features
                && dependency.features.is_empty()
        });
        let current_path = package.dependencies.iter().any(|dependency| {
            dependency.name == "fs2"
                && dependency.rename.as_deref() == Some("fs2_current")
                && dependency.source.is_none()
                && dependency
                    .path
                    .as_deref()
                    .and_then(|path| path.canonicalize().ok())
                    .is_some_and(|path| path == current)
                && dependency.kind.is_none()
                && dependency.target.is_none()
                && dependency.optional
                && dependency.uses_default_features
                && dependency.features.is_empty()
        });
        if package.dependencies.len() != 2 || !legacy || !current_path {
            return Err(invalid_data(format!(
                "{} must depend only on exact fs2 0.4.3 and the current checkout",
                package.name
            )));
        }
    }
    Ok(())
}

fn consumer_digest(path: &Path) -> Result<String> {
    let contents = fs::read_to_string(path)?;
    Ok(digest_contents(&contents))
}

fn digest_contents(contents: &str) -> String {
    let normalized = contents.replace("\r\n", "\n").replace('\r', "\n");
    lower_hex(Sha256::digest(normalized.as_bytes()))
}

fn compatibility_packages(
    root: &Path,
    manifest: &Path,
    target: &Path,
) -> Result<Vec<CargoPackage>> {
    let mut command = process::cargo();
    command
        .current_dir(root)
        .env("CARGO_TARGET_DIR", target)
        .args(["metadata", "--manifest-path"])
        .arg(manifest)
        .args(["--format-version", "1", "--locked", "--all-features"]);
    let output = process::capture(&mut command, "read compatibility metadata")?;
    let metadata: CargoMetadata = serde_json::from_slice(&output.stdout)?;
    validate_resolved_fs2(root, &metadata.packages)?;
    let mut packages = metadata
        .packages
        .into_iter()
        .filter(|package| package.name.starts_with("fs2-compat-edition-"))
        .collect::<Vec<_>>();
    packages.sort_by(|left, right| left.edition.cmp(&right.edition));
    if packages.is_empty() {
        return Err(invalid_data(
            "compatibility workspace has no edition packages",
        ));
    }
    let editions = packages
        .iter()
        .map(|package| package.edition.as_str())
        .collect::<HashSet<_>>();
    if editions.len() != packages.len() {
        return Err(invalid_data(
            "compatibility workspace has duplicate Rust editions",
        ));
    }
    let required = REQUIRED_EDITIONS.into_iter().collect::<HashSet<_>>();
    if editions != required {
        return Err(invalid_data(
            "compatibility workspace must cover editions 2015, 2018, 2021, and 2024 exactly",
        ));
    }
    Ok(packages)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frozen_consumer_digest_matches() {
        let consumer = crate::repository_root().join("compatibility/v04_consumer.rs");
        assert_eq!(
            consumer_digest(&consumer).unwrap(),
            EXPECTED_CONSUMER_SHA256
        );
    }

    fn fixture_package(edition: &str) -> CargoPackage {
        let compatibility = crate::repository_root().join("compatibility");
        let name = format!("fs2-compat-edition-{edition}");
        CargoPackage {
            name: name.clone(),
            version: "0.0.0".to_owned(),
            edition: edition.to_owned(),
            default_run: None,
            features: expected_features(),
            source: None,
            manifest_path: compatibility
                .join(format!("edition-{edition}"))
                .join("Cargo.toml"),
            dependencies: Vec::new(),
            targets: vec![CargoTarget {
                name,
                kind: vec!["bin".to_owned()],
                crate_types: vec!["bin".to_owned()],
                src_path: compatibility.join("v04_consumer.rs"),
            }],
        }
    }

    #[test]
    fn compatibility_target_must_resolve_to_frozen_consumer() {
        let compatibility = crate::repository_root().join("compatibility");
        let consumer = compatibility.join("v04_consumer.rs");
        let mut package = fixture_package("2021");
        package.targets[0].src_path = compatibility.join("Cargo.toml");

        assert!(validate_compatibility_packages(&compatibility, &consumer, &[package]).is_err());
    }

    #[test]
    fn compatibility_package_rejects_extra_executable_targets() {
        let compatibility = crate::repository_root().join("compatibility");
        let consumer = compatibility.join("v04_consumer.rs");
        let mut package = fixture_package("2021");
        package.targets.push(CargoTarget {
            name: "substitute".to_owned(),
            kind: vec!["bin".to_owned()],
            crate_types: vec!["bin".to_owned()],
            src_path: compatibility.join("v04_consumer.rs"),
        });

        assert!(validate_compatibility_packages(&compatibility, &consumer, &[package]).is_err());
    }

    #[test]
    fn changed_consumer_has_a_different_digest() {
        assert_ne!(
            digest_contents("original\n"),
            digest_contents("original\n\n")
        );
    }
}
