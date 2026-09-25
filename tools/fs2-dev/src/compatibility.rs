use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

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
    let consumer = compatibility.join("v04_consumer.rs");
    frozen_consumer_digest(&consumer).and_then(|digest| {
        println!("v0.4 consumer sha256={digest}");
        #[cfg(test)]
        {
            // The complete native orchestration is exercised through the real CLI by
            // integration tests; unit builds validate its immutable input gates only.
            validate_lockfile(&compatibility.join("Cargo.lock"))
        }
        #[cfg(not(test))]
        {
            validate_lockfile(&compatibility.join("Cargo.lock")).and_then(|()| {
                let manifest = compatibility.join("Cargo.toml");
                process::toolchain_key().and_then(|toolchain| {
                    let target = root.join("target/xtask/compatibility").join(toolchain);
                    let mut format = process::cargo();
                    format
                        .current_dir(root)
                        .args(["fmt", "--manifest-path"])
                        .arg(&manifest)
                        .args(["--all", "--", "--check"]);
                    process::run(&mut format, "format compatibility fixtures")
                        .and_then(|()| compatibility_packages(root, &manifest, &target))
                        .and_then(|packages| {
                            validate_compatibility_packages(&compatibility, &consumer, &packages)
                                .and_then(|()| validate_dependencies(root, &packages))
                                .and_then(|()| {
                                    run_consumers(
                                        root,
                                        &manifest,
                                        &target,
                                        &packages,
                                        &mut process::run,
                                    )
                                })
                        })
                })
            })
        }
    })
}

fn frozen_consumer_digest(consumer: &Path) -> Result<String> {
    let digest = consumer_digest(consumer)?;
    if digest != EXPECTED_CONSUMER_SHA256 {
        return Err(invalid_data(
            "frozen v0.4 consumer changed; update its digest only after an intentional API review",
        ));
    }
    Ok(digest)
}

fn run_consumers(
    root: &Path,
    manifest: &Path,
    target: &Path,
    packages: &[CargoPackage],
    execute: &mut dyn FnMut(&mut Command, &str) -> Result<()>,
) -> Result<()> {
    for subject in SUBJECTS {
        let mut check = process::cargo();
        check
            .current_dir(root)
            .env("CARGO_TARGET_DIR", target)
            .args(["check", "--workspace", "--manifest-path"])
            .arg(manifest)
            .args(["--no-default-features", "--features", subject, "--locked"]);
        execute(
            &mut check,
            &format!("check {subject} compatibility surface"),
        )?;
    }

    for package in packages {
        for subject in SUBJECTS {
            let mut run = process::cargo();
            run.current_dir(root)
                .env("CARGO_TARGET_DIR", target)
                .args(["run", "--manifest-path"])
                .arg(manifest)
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
            execute(
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
        .filter(|package| package.name == "fs2" || package.name == "fs2-turbo")
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
            "resolved compatibility graph must contain only approved fs2 0.4.3 and the current fs2-turbo checkout",
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
        || contents.matches("name = \"fs2\"").count() != 1
        || contents.matches("name = \"fs2-turbo\"").count() != 1
    {
        return Err(invalid_data(
            "compatibility lockfile does not contain exactly one approved legacy fs2 package and one current fs2-turbo package",
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
    root.canonicalize()
        .map_err(crate::DynError::from)
        .and_then(|current| {
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
                    dependency.name == "fs2-turbo"
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
                        "{} must depend only on exact fs2 0.4.3 and the current fs2-turbo checkout",
                        package.name
                    )));
                }
            }
            Ok(())
        })
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
    serde_json::from_slice(&output.stdout)
        .map_err(crate::DynError::from)
        .and_then(|metadata| validated_packages(root, metadata))
}

fn validated_packages(root: &Path, metadata: CargoMetadata) -> Result<Vec<CargoPackage>> {
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
    fn compatibility_metadata_entrypoint_validates_the_locked_workspace() {
        let directory = tempfile::tempdir().unwrap();
        let root = crate::repository_root();
        let manifest = root.join("compatibility/Cargo.toml");
        let target = directory.path().join("metadata-target");
        let packages = compatibility_packages(root, &manifest, &target).unwrap();
        assert_eq!(
            packages
                .iter()
                .map(|package| package.edition.as_str())
                .collect::<Vec<_>>(),
            REQUIRED_EDITIONS
        );

        let error = compatibility_packages(
            directory.path(),
            &directory.path().join("missing.toml"),
            &target,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("read compatibility metadata"), "{error}");
    }

    #[test]
    fn compatibility_entrypoint_rejects_missing_or_changed_frozen_inputs() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let compatibility = root.join("compatibility");
        let consumer = compatibility.join("v04_consumer.rs");
        let error = frozen_consumer_digest(&consumer).unwrap_err();
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::NotFound
        );

        fs::create_dir(&compatibility).unwrap();
        fs::write(&consumer, "substituted consumer").unwrap();
        assert!(
            frozen_consumer_digest(&consumer)
                .unwrap_err()
                .to_string()
                .contains("frozen v0.4 consumer changed")
        );

        fs::copy(
            crate::repository_root().join("compatibility/v04_consumer.rs"),
            &consumer,
        )
        .unwrap();
        frozen_consumer_digest(&consumer).unwrap();
        let lockfile = compatibility.join("Cargo.lock");
        let error = validate_lockfile(&lockfile).unwrap_err();
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::NotFound
        );
        fs::write(&lockfile, "unapproved lockfile").unwrap();
        assert!(
            validate_lockfile(&lockfile)
                .unwrap_err()
                .to_string()
                .contains("compatibility lockfile")
        );
        fs::copy(
            crate::repository_root().join("compatibility/Cargo.lock"),
            &lockfile,
        )
        .unwrap();
        run(root).unwrap();
    }

    #[test]
    fn compatibility_validation_propagates_missing_canonical_paths() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("missing");
        let compatibility = crate::repository_root().join("compatibility");
        let consumer = compatibility.join("v04_consumer.rs");
        let packages = [fixture_package("2021")];

        assert!(validate_compatibility_packages(&compatibility, &missing, &packages).is_err());
        assert!(validate_compatibility_packages(&missing, &consumer, &packages).is_err());
        assert!(validate_dependencies(directory.path(), &[fixture_with_dependencies()]).is_err());
        assert!(validated_packages(directory.path(), fixture_metadata()).is_err());
    }

    #[test]
    fn consumer_commands_preserve_arguments_and_stop_at_the_first_failure() {
        use std::ffi::{OsStr, OsString};

        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let manifest = root.join("compatibility/Cargo.toml");
        let target = root.join("isolated-target");
        let packages = REQUIRED_EDITIONS.map(fixture_package);
        let command_count = SUBJECTS.len() * (packages.len() + 1);

        for fail_at in 0..=command_count {
            let mut calls = 0;
            let result = run_consumers(
                root,
                &manifest,
                &target,
                &packages,
                &mut |command, label| {
                    let index = calls;
                    calls += 1;
                    let (expected, expected_label): (Vec<OsString>, String) = if index
                        < SUBJECTS.len()
                    {
                        let subject = SUBJECTS[index];
                        let mut arguments = vec![
                            OsString::from("check"),
                            OsString::from("--workspace"),
                            OsString::from("--manifest-path"),
                            manifest.as_os_str().to_owned(),
                        ];
                        arguments.extend(
                            ["--no-default-features", "--features", subject, "--locked"]
                                .map(OsString::from),
                        );
                        (arguments, format!("check {subject} compatibility surface"))
                    } else {
                        let consumer = index - SUBJECTS.len();
                        let package = &packages[consumer / SUBJECTS.len()];
                        let subject = SUBJECTS[consumer % SUBJECTS.len()];
                        let mut arguments = vec![
                            OsString::from("run"),
                            OsString::from("--manifest-path"),
                            manifest.as_os_str().to_owned(),
                        ];
                        arguments.extend(
                            [
                                "--package",
                                package.name.as_str(),
                                "--bin",
                                package.name.as_str(),
                                "--no-default-features",
                                "--features",
                                subject,
                                "--locked",
                            ]
                            .map(OsString::from),
                        );
                        (
                            arguments,
                            format!("run {subject} v0.4 consumer in edition {}", package.edition),
                        )
                    };
                    assert_eq!(command.get_current_dir(), Some(root));
                    assert_eq!(command.get_args().collect::<Vec<_>>(), expected);
                    assert_eq!(label, expected_label);
                    assert_eq!(
                        command
                            .get_envs()
                            .find(|(name, _)| *name == OsStr::new("CARGO_TARGET_DIR")),
                        Some((OsStr::new("CARGO_TARGET_DIR"), Some(target.as_os_str())))
                    );
                    if index == fail_at {
                        Err(invalid_data("injected consumer-command failure"))
                    } else {
                        Ok(())
                    }
                },
            );
            assert_eq!(calls, (fail_at + 1).min(command_count));
            if fail_at < command_count {
                assert_eq!(
                    result.unwrap_err().to_string(),
                    "injected consumer-command failure"
                );
            } else {
                result.unwrap();
            }
        }
    }

    type PackageMutation = fn(&mut CargoPackage);
    type DependencyMutation = fn(&mut CargoDependency);

    fn fixture_metadata() -> CargoMetadata {
        let mut legacy = fixture_package("2021");
        legacy.name = "fs2".to_owned();
        legacy.version = "0.4.3".to_owned();
        legacy.source = Some("registry+https://github.com/rust-lang/crates.io-index".to_owned());
        let mut current = fixture_package("2021");
        current.name = "fs2-turbo".to_owned();
        current.version = "1.0.0".to_owned();
        current.manifest_path = crate::repository_root().join("Cargo.toml");
        let mut packages = vec![legacy, current];
        packages.extend(REQUIRED_EDITIONS.into_iter().rev().map(fixture_package));
        CargoMetadata { packages }
    }

    fn fixture_dependency(current: bool) -> CargoDependency {
        CargoDependency {
            name: if current { "fs2-turbo" } else { "fs2" }.to_owned(),
            req: if current { "*" } else { "=0.4.3" }.to_owned(),
            source: (!current)
                .then(|| "registry+https://github.com/rust-lang/crates.io-index".to_owned()),
            rename: Some(if current { "fs2_current" } else { "fs2_v04" }.to_owned()),
            path: current.then(|| crate::repository_root().to_owned()),
            kind: None,
            target: None,
            optional: true,
            uses_default_features: true,
            features: Vec::new(),
        }
    }

    fn fixture_with_dependencies() -> CargoPackage {
        let mut package = fixture_package("2021");
        package.dependencies = vec![fixture_dependency(false), fixture_dependency(true)];
        package
    }

    #[test]
    fn metadata_requires_exactly_the_reviewed_resolutions() {
        let root = crate::repository_root();
        validate_resolved_fs2(root, &fixture_metadata().packages).unwrap();
        let legacy_changes: &[(&str, PackageMutation)] = &[
            ("version", |package| package.version = "0.4.2".to_owned()),
            ("registry", |package| package.source = None),
            ("source", |package| {
                package.source = Some("git+untrusted".to_owned())
            }),
        ];
        for (name, change) in legacy_changes {
            let mut metadata = fixture_metadata();
            change(&mut metadata.packages[0]);
            assert!(
                validate_resolved_fs2(root, &metadata.packages).is_err(),
                "{name}"
            );
        }
        for path in [
            root.join("compatibility/Cargo.toml"),
            root.join("not-a-compatibility-manifest/Cargo.toml"),
        ] {
            let mut metadata = fixture_metadata();
            metadata.packages[1].manifest_path = path;
            assert!(validate_resolved_fs2(root, &metadata.packages).is_err());
        }
        let mut metadata = fixture_metadata();
        let mut extra = fixture_package("2021");
        extra.name = "fs2".to_owned();
        metadata.packages.push(extra);
        assert!(validate_resolved_fs2(root, &metadata.packages).is_err());
        let missing_root = tempfile::tempdir().unwrap();
        assert!(validate_resolved_fs2(missing_root.path(), &fixture_metadata().packages).is_err());
    }

    #[test]
    fn metadata_sorts_and_requires_the_exact_edition_set() {
        let root = crate::repository_root();
        let packages = validated_packages(root, fixture_metadata()).unwrap();
        assert_eq!(
            packages
                .iter()
                .map(|package| package.edition.as_str())
                .collect::<Vec<_>>(),
            REQUIRED_EDITIONS
        );
        for (name, edit) in [
            ("empty", 0),
            ("missing", 1),
            ("duplicate", 2),
            ("unexpected", 3),
        ] {
            let mut metadata = fixture_metadata();
            match edit {
                0 => metadata.packages.truncate(2),
                1 => {
                    metadata.packages.pop();
                }
                2 => metadata.packages.push(fixture_package("2021")),
                _ => metadata.packages[2].edition = "2099".to_owned(),
            }
            assert!(validated_packages(root, metadata).is_err(), "{name}");
        }
    }

    #[test]
    fn every_frozen_package_and_binary_property_is_enforced() {
        let root = crate::repository_root();
        let compatibility = root.join("compatibility");
        let consumer = compatibility.join("v04_consumer.rs");
        let packages = REQUIRED_EDITIONS.map(fixture_package);
        validate_compatibility_packages(&compatibility, &consumer, &packages).unwrap();
        let mutations: &[(&str, PackageMutation)] = &[
            ("name", |package| package.name.push_str("-substitute")),
            ("version", |package| package.version = "1.0.0".to_owned()),
            ("source", |package| {
                package.source = Some("git+untrusted".to_owned())
            }),
            ("default run", |package| {
                package.default_run = Some(package.name.clone())
            }),
            ("features", |package| package.features.clear()),
            ("manifest", |package| {
                package.manifest_path = crate::repository_root().join("Cargo.toml");
            }),
            ("missing target", |package| package.targets.clear()),
            ("target name", |package| {
                package.targets[0].name.push_str("-substitute")
            }),
            ("target kind", |package| {
                package.targets[0].kind = vec!["lib".to_owned()]
            }),
            ("crate type", |package| {
                package.targets[0].crate_types = vec!["rlib".to_owned()];
            }),
        ];
        for (name, change) in mutations {
            let mut package = fixture_package("2021");
            change(&mut package);
            assert!(
                validate_compatibility_packages(&compatibility, &consumer, &[package]).is_err(),
                "{name}"
            );
        }
    }

    #[test]
    fn dependency_contract_rejects_unreviewed_properties() {
        let root = crate::repository_root();
        validate_dependencies(root, &[fixture_with_dependencies()]).unwrap();
        let mutations: &[(&str, DependencyMutation)] = &[
            ("name", |dependency| {
                dependency.name = "substitute".to_owned()
            }),
            ("alias", |dependency| dependency.rename = None),
            ("kind", |dependency| {
                dependency.kind = Some("build".to_owned())
            }),
            ("target", |dependency| {
                dependency.target = Some("cfg(unix)".to_owned())
            }),
            ("optional", |dependency| dependency.optional = false),
            ("defaults", |dependency| {
                dependency.uses_default_features = false
            }),
            ("features", |dependency| {
                dependency.features.push("unreviewed".to_owned())
            }),
        ];
        for index in 0..2 {
            for (name, change) in mutations {
                let mut package = fixture_with_dependencies();
                change(&mut package.dependencies[index]);
                assert!(
                    validate_dependencies(root, &[package]).is_err(),
                    "{index}: {name}"
                );
            }
        }
        let legacy_changes: &[(&str, DependencyMutation)] = &[
            ("range", |dependency| dependency.req = "^0.4".to_owned()),
            ("path", |dependency| {
                dependency.path = Some(crate::repository_root().to_owned())
            }),
            ("source", |dependency| dependency.source = None),
        ];
        for (name, change) in legacy_changes {
            let mut package = fixture_with_dependencies();
            change(&mut package.dependencies[0]);
            assert!(validate_dependencies(root, &[package]).is_err(), "{name}");
        }
        for path in [
            None,
            Some(root.join("compatibility")),
            Some(root.join("missing-dependency")),
        ] {
            let mut package = fixture_with_dependencies();
            package.dependencies[1].path = path;
            assert!(validate_dependencies(root, &[package]).is_err());
        }
        let mut package = fixture_with_dependencies();
        package.dependencies[1].source = Some("registry+untrusted".to_owned());
        assert!(validate_dependencies(root, &[package]).is_err());
        let mut package = fixture_with_dependencies();
        package.dependencies.push(fixture_dependency(false));
        assert!(validate_dependencies(root, &[package]).is_err());
    }

    #[test]
    fn lockfile_binds_the_legacy_checksum_and_unique_subjects() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("Cargo.lock");
        let legacy = format!(
            "[[package]]\nname = \"fs2\"\nversion = \"0.4.3\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"{LEGACY_CHECKSUM}\"\n"
        );
        let current = "[[package]]\nname = \"fs2-turbo\"\nversion = \"1.0.0\"\n";
        let valid = format!("{legacy}\n{current}");
        assert!(validate_lockfile(&path).is_err());
        for contents in [valid.clone(), valid.replace('\n', "\r\n")] {
            fs::write(&path, contents).unwrap();
            validate_lockfile(&path).unwrap();
        }
        for contents in [
            valid.replace(LEGACY_CHECKSUM, "unreviewed"),
            valid.replace("0.4.3", "0.4.2"),
            legacy.clone(),
            format!("{valid}{legacy}"),
            format!("{valid}{current}"),
        ] {
            fs::write(&path, contents).unwrap();
            assert!(validate_lockfile(&path).is_err());
        }
    }

    #[test]
    fn consumer_digest_normalizes_line_endings_and_rejects_invalid_input() {
        assert_eq!(
            digest_contents("one\ntwo\n"),
            digest_contents("one\r\ntwo\r\n")
        );
        assert_eq!(digest_contents("one\ntwo\n"), digest_contents("one\rtwo\r"));
        let directory = tempfile::tempdir().unwrap();
        let consumer = directory.path().join("consumer.rs");
        assert!(consumer_digest(&consumer).is_err());
        fs::write(&consumer, [0xff]).unwrap();
        assert!(consumer_digest(&consumer).is_err());
        let compatibility = directory.path().join("compatibility");
        fs::create_dir(&compatibility).unwrap();
        fs::write(compatibility.join("v04_consumer.rs"), "altered consumer\n").unwrap();
        let error = frozen_consumer_digest(&compatibility.join("v04_consumer.rs"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("frozen v0.4 consumer changed"));
    }

    #[test]
    fn frozen_consumer_digest_matches() {
        let consumer = crate::repository_root().join("compatibility/v04_consumer.rs");
        assert_eq!(
            frozen_consumer_digest(&consumer).unwrap(),
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
