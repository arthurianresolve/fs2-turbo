use std::collections::HashSet;
use std::fs;
use std::path::Path;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::process;
use crate::{Result, invalid_data};

const EXPECTED_CONSUMER_SHA256: &str =
    "3f3b5ea95f12828437a8e851baad8cc58eee3a6206f5957748248195f6ceab29";
const SUBJECTS: [&str; 2] = ["legacy", "current"];

#[derive(Debug, Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoPackage>,
}

#[derive(Debug, Deserialize)]
struct CargoPackage {
    name: String,
    edition: String,
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
    let runtime = packages
        .iter()
        .filter(|package| package.edition == "2015")
        .collect::<Vec<_>>();
    if runtime.len() != 1 {
        return Err(invalid_data(
            "compatibility workspace must define exactly one Rust 2015 package",
        ));
    }

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

    for subject in SUBJECTS {
        let mut run = process::cargo();
        run.current_dir(root)
            .env("CARGO_TARGET_DIR", &target)
            .args(["run", "--manifest-path"])
            .arg(&manifest)
            .args([
                "--package",
                runtime[0].name.as_str(),
                "--no-default-features",
                "--features",
                subject,
                "--locked",
            ]);
        process::run(&mut run, &format!("run {subject} v0.4 consumer"))?;
    }
    Ok(())
}

fn consumer_digest(path: &Path) -> Result<String> {
    let contents = fs::read_to_string(path)?;
    Ok(digest_contents(&contents))
}

fn digest_contents(contents: &str) -> String {
    let normalized = contents.replace("\r\n", "\n").replace('\r', "\n");
    format!("{:x}", Sha256::digest(normalized.as_bytes()))
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
        .args(["--no-deps", "--format-version", "1", "--locked"]);
    let output = process::capture(&mut command, "read compatibility metadata")?;
    let metadata: CargoMetadata = serde_json::from_slice(&output.stdout)?;
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

    #[test]
    fn changed_consumer_has_a_different_digest() {
        assert_ne!(
            digest_contents("original\n"),
            digest_contents("original\n\n")
        );
    }
}
