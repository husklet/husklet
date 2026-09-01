//! Exact Cargo artifact discovery for immutable corpus staging.

use crate::{platform::HostProcess, suite::Error};
use std::{
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::Stdio,
};

pub(super) struct BuildArtifacts {
    pub(super) runner: PathBuf,
    pub(super) library: PathBuf,
}

pub(super) fn run(cargo: &Path, workspace: &Path) -> Result<BuildArtifacts, Error> {
    let packages = PackageIds::discover(cargo, workspace)?;
    let mut arguments = vec![
        "build",
        "--release",
        "--locked",
        "--offline",
        "-p",
        "testing",
        "--bin",
        "testing",
    ];
    arguments.extend(configured_cargo_feature_arguments());
    arguments.push("--message-format=json-render-diagnostics");
    let mut child = HostProcess::standard(cargo)
        .current_dir(workspace)
        .args(arguments)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|error| format!("start exact runtime corpus build: {error}"))?;
    let stdout = child.stdout.take().ok_or("Cargo build stdout was not captured")?;
    let artifacts = select_messages(BufReader::new(stdout), &packages.testing, &packages.native);
    let status = child.wait()?;
    if !status.success() {
        return Err(format!("exact runtime corpus build failed with {status}").into());
    }
    artifacts?.ok_or_else(|| "Cargo did not identify both the testing runner and hl-native library".into())
}

const fn cargo_feature_arguments(production: bool, hooks: bool) -> &'static [&'static str] {
    if production && !hooks {
        &["--no-default-features", "--features", "production-runtime"]
    } else {
        &[]
    }
}

const fn configured_cargo_feature_arguments() -> &'static [&'static str] {
    cargo_feature_arguments(
        cfg!(feature = "production-runtime"),
        cfg!(feature = "native-test-hooks"),
    )
}

struct PackageIds {
    testing: String,
    native: String,
}

impl PackageIds {
    fn discover(cargo: &Path, workspace: &Path) -> Result<Self, Error> {
        let output = HostProcess::standard(cargo)
            .current_dir(workspace)
            .args(["metadata", "--locked", "--offline", "--no-deps", "--format-version=1"])
            .output()?;
        if !output.status.success() {
            return Err(format!("Cargo metadata failed with {}", output.status).into());
        }
        let metadata: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        let packages = metadata
            .get("packages")
            .and_then(serde_json::Value::as_array)
            .ok_or("Cargo metadata omitted packages")?;
        Ok(Self {
            testing: unique_package_id(packages, "testing", &workspace.join("src/apps/testing/Cargo.toml"))?,
            native: unique_package_id(
                packages,
                "hl-native",
                &workspace.join("src/runtime/hl-native/Cargo.toml"),
            )?,
        })
    }
}

fn unique_package_id(packages: &[serde_json::Value], name: &str, manifest: &Path) -> Result<String, Error> {
    let expected = fs::canonicalize(manifest)?;
    let matches = packages
        .iter()
        .filter(|package| package.get("name").and_then(serde_json::Value::as_str) == Some(name))
        .filter(|package| {
            package
                .get("manifest_path")
                .and_then(serde_json::Value::as_str)
                .and_then(|path| fs::canonicalize(path).ok())
                .as_deref()
                == Some(expected.as_path())
        })
        .filter_map(|package| package.get("id").and_then(serde_json::Value::as_str))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [package] => Ok((*package).to_owned()),
        _ => Err(format!("Cargo metadata identified {} {name} packages", matches.len()).into()),
    }
}

pub(super) fn select_messages(
    reader: impl BufRead,
    testing_package: &str,
    native_package: &str,
) -> Result<Option<BuildArtifacts>, Error> {
    let mut runner = None;
    let mut library = None;
    for line in reader.lines() {
        let line = line?;
        let Ok(message) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        match message.get("reason").and_then(serde_json::Value::as_str) {
            Some("compiler-artifact")
                if message.get("package_id").and_then(serde_json::Value::as_str) == Some(testing_package)
                    && message.pointer("/target/name").and_then(serde_json::Value::as_str) == Some("testing")
                    && message
                        .pointer("/target/kind")
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(|kinds| kinds.iter().any(|kind| kind.as_str() == Some("bin"))) =>
            {
                let executable = message
                    .get("executable")
                    .and_then(serde_json::Value::as_str)
                    .ok_or("testing compiler artifact has no executable")?;
                unique(&mut runner, PathBuf::from(executable), "testing runner")?;
            }
            Some("build-script-executed")
                if message.get("package_id").and_then(serde_json::Value::as_str) == Some(native_package) =>
            {
                let path = message
                    .get("env")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(serde_json::Value::as_array)
                    .find(|pair| pair.first().and_then(serde_json::Value::as_str) == Some("HL_NATIVE_LIBRARY_PATH"))
                    .and_then(|pair| pair.get(1))
                    .and_then(serde_json::Value::as_str)
                    .ok_or("hl-native build result omitted HL_NATIVE_LIBRARY_PATH")?;
                unique(&mut library, PathBuf::from(path), "hl-native library")?;
            }
            _ => {}
        }
    }
    Ok(match (runner, library) {
        (Some(runner), Some(library)) => Some(BuildArtifacts { runner, library }),
        (None, None) => None,
        _ => return Err("Cargo identified only one member of the runtime artifact pair".into()),
    })
}

fn unique(slot: &mut Option<PathBuf>, value: PathBuf, name: &str) -> Result<(), Error> {
    if slot.replace(value).is_some() {
        Err(format!("Cargo identified more than one {name}").into())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::cargo_feature_arguments;

    #[test]
    fn stage_feature_arguments_resolve_default_production_and_all_features() {
        assert!(cargo_feature_arguments(false, true).is_empty(), "default hook runner");
        assert_eq!(
            cargo_feature_arguments(true, false),
            ["--no-default-features", "--features", "production-runtime"]
        );
        assert!(
            cargo_feature_arguments(true, true).is_empty(),
            "hooks take precedence under all-features"
        );
    }
}
