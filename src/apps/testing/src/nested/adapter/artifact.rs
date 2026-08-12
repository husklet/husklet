use super::{ProcessOutput, capture};
use crate::nested::{Build, Error};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, time::Duration};

pub(in crate::nested) fn environment(name: &str) -> Option<String> {
    std::env::var_os(name).map(|value| value.to_string_lossy().into_owned())
}

pub(in crate::nested) fn hash_tool(
    digest: &mut crate::record::FramedIdentity,
    name: &str,
    program: &str,
    arguments: &[&str],
) -> Result<(), Error> {
    let mut command = vec![program.to_owned()];
    command.extend(arguments.iter().map(|value| (*value).to_owned()));
    let output = capture(&command, Duration::from_secs(30), 128 * 1024)
        .map_err(|error| format!("cannot identify {name}: {error}"))?;
    if output.status != Some(0) {
        return Err(format!("{name} identity command exited {:?}", output.status).into());
    }
    digest.field(name.as_bytes())?;
    digest.field(program.as_bytes())?;
    digest.field(&output.stdout)?;
    digest.field(&output.stderr)
}

pub(in crate::nested) fn build_artifact(
    root: &Path,
    build: &Build,
    cargo: &str,
    record: &crate::record::ArtifactRecord,
) -> Result<(), Error> {
    let mut arguments = vec![
        cargo.into(),
        "rustc".into(),
        "--locked".into(),
        "--offline".into(),
        "--manifest-path".into(),
        root.join("Cargo.toml").display().to_string(),
        "--package".into(),
        build.package.clone(),
        "--target".into(),
        build.target.clone(),
        "--profile".into(),
        build.profile.clone(),
        "--bin".into(),
        build.binary.clone(),
    ];
    if !build.rustflags.is_empty() {
        arguments.push("--".into());
        arguments.extend(build.rustflags.iter().cloned());
    }
    let ProcessOutput { status, stderr, .. } = capture(&arguments, Duration::from_secs(3600), 16 * 1024 * 1024)
        .map_err(|error| format!("nested Cargo build failed: {error}"))?;
    if status != Some(0) {
        return Err(format!(
            "nested Cargo build exited {status:?}: {}",
            String::from_utf8_lossy(&stderr)
        )
        .into());
    }
    let produced = root
        .join("target")
        .join(&build.target)
        .join(&build.profile)
        .join(&build.binary);
    let bytes = fs::read(&produced)
        .map_err(|error| format!("cannot read built nested artifact {}: {error}", produced.display()))?;
    record.publish(&bytes, true)
}

pub(in crate::nested) fn materialize(source: &Path, destination: &Path) -> Result<(), Error> {
    fs::create_dir_all(
        destination
            .parent()
            .ok_or("nested artifact destination has no parent")?,
    )?;
    let temporary = destination.with_extension(format!("prepare-{}", std::process::id()));
    if temporary.exists() {
        fs::remove_file(&temporary)?;
    }
    fs::copy(source, &temporary)?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o755))?;
    fs::rename(temporary, destination)?;
    Ok(())
}
