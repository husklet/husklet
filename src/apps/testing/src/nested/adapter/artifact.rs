use super::ProcessOutput;
use crate::nested::{Build, BundleManifest, BundleMember, CBuild, EngineBuild, Error, elf_machine};
use std::{
    fs,
    io::Cursor,
    path::{Path, PathBuf},
    time::Duration,
};

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
    let output = ProcessOutput::capture(&command, Duration::from_secs(30), 128 * 1024)
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
    let built = match build {
        Build::Engine(engine) => build_engine(root, engine, cargo)?,
        Build::C(c) => build_c(root, c)?,
    };
    let archive = archive(build, &built)?;
    record.publish(&archive, false)
}

struct Built {
    executable: Vec<u8>,
    library: Option<Vec<u8>>,
}

fn build_engine(root: &Path, build: &EngineBuild, cargo: &str) -> Result<Built, Error> {
    let mut arguments = vec![
        cargo.into(),
        "rustc".into(),
        "--locked".into(),
        "--offline".into(),
        "--message-format=json-render-diagnostics".into(),
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
    let captured = ProcessOutput::capture(&arguments, Duration::from_secs(3600), 32 * 1024 * 1024)
        .map_err(|error| format!("nested Cargo build failed: {error}"))?;
    if captured.status != Some(0) {
        return Err(format!(
            "nested Cargo build exited {:?}: {}",
            captured.status,
            String::from_utf8_lossy(&captured.stderr)
        )
        .into());
    }
    let (executable, library) = cargo_artifacts(&captured.stdout, &build.binary)?;
    let executable = read_elf(&executable, build.isa.elf_machine(), "nested engine")?;
    let library = read_elf(&library, build.isa.elf_machine(), "nested native library")?;
    Ok(Built {
        executable,
        library: Some(library),
    })
}

fn cargo_artifacts(messages: &[u8], binary: &str) -> Result<(PathBuf, PathBuf), Error> {
    let mut executable = None;
    let mut library = None;
    for line in messages.split(|byte| *byte == b'\n').filter(|line| !line.is_empty()) {
        let Ok(message) = serde_json::from_slice::<serde_json::Value>(line) else {
            continue;
        };
        match message.get("reason").and_then(serde_json::Value::as_str) {
            Some("compiler-artifact")
                if message.pointer("/target/name").and_then(serde_json::Value::as_str) == Some(binary)
                    && message
                        .pointer("/target/kind")
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(|kinds| kinds.iter().any(|kind| kind.as_str() == Some("bin"))) =>
            {
                let path = message
                    .get("executable")
                    .and_then(serde_json::Value::as_str)
                    .ok_or("nested engine compiler artifact omitted its executable")?;
                unique(&mut executable, PathBuf::from(path), "nested engine executable")?;
            }
            Some("build-script-executed") => {
                let path = message
                    .get("env")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(serde_json::Value::as_array)
                    .find(|pair| pair.first().and_then(serde_json::Value::as_str) == Some("HL_NATIVE_LIBRARY_PATH"))
                    .and_then(|pair| pair.get(1))
                    .and_then(serde_json::Value::as_str);
                if let Some(path) = path {
                    unique(&mut library, PathBuf::from(path), "nested native library")?;
                }
            }
            _ => {}
        }
    }
    match (executable, library) {
        (Some(executable), Some(library)) => Ok((executable, library)),
        _ => Err("Cargo did not identify the nested engine and its exact native library".into()),
    }
}

fn unique(slot: &mut Option<PathBuf>, value: PathBuf, name: &str) -> Result<(), Error> {
    if slot.replace(value).is_some() {
        Err(format!("Cargo identified more than one {name}").into())
    } else {
        Ok(())
    }
}

fn build_c(root: &Path, build: &CBuild) -> Result<Built, Error> {
    let directory = tempfile::tempdir_in(root.join("target"))?;
    let output = directory.path().join(&build.output);
    let mut arguments = build.isa.compiler_command()?;
    arguments.extend(["-o".into(), output.display().to_string()]);
    arguments.extend(build.flags.iter().cloned());
    arguments.push(root.join(&build.source).display().to_string());
    let captured = ProcessOutput::capture(&arguments, Duration::from_secs(120), 1024 * 1024)
        .map_err(|error| format!("nested C build failed: {error}"))?;
    if captured.status != Some(0) {
        return Err(format!(
            "nested C build exited {:?}: {}",
            captured.status,
            String::from_utf8_lossy(&captured.stderr)
        )
        .into());
    }
    Ok(Built {
        executable: read_elf(&output, build.isa.elf_machine(), "nested C guest")?,
        library: None,
    })
}

fn read_elf(path: &Path, expected_machine: u16, label: &str) -> Result<Vec<u8>, Error> {
    let bytes = fs::read(path).map_err(|error| format!("cannot read {label} {}: {error}", path.display()))?;
    let machine = elf_machine(&bytes)?;
    if machine != expected_machine {
        return Err(format!(
            "{label} {} has ELF machine {machine}, expected {expected_machine}",
            path.display()
        )
        .into());
    }
    Ok(bytes)
}

fn archive(build: &Build, built: &Built) -> Result<Vec<u8>, Error> {
    let executable = member(
        Path::new("bin").join(build.executable()),
        &built.executable,
        build.isa(),
    )?;
    let library = built
        .library
        .as_ref()
        .map(|bytes| member(PathBuf::from("lib/libhl_native_engine.so"), bytes, build.isa()))
        .transpose()?;
    let manifest = BundleManifest {
        schema: "husklet-nested-artifact-v1".into(),
        isa: build.isa(),
        executable,
        library,
    };
    let manifest_bytes = serde_yaml::to_string(&manifest)?.into_bytes();
    let mut bytes = Vec::new();
    {
        let mut archive = tar::Builder::new(&mut bytes);
        append(&mut archive, &manifest.executable.path, &built.executable, 0o555)?;
        if let (Some(receipt), Some(library)) = (&manifest.library, &built.library) {
            append(&mut archive, &receipt.path, library, 0o555)?;
        }
        append(&mut archive, Path::new("manifest.yaml"), &manifest_bytes, 0o444)?;
        archive.finish()?;
    }
    Ok(bytes)
}

fn member(path: PathBuf, bytes: &[u8], isa: crate::nested::GuestIsa) -> Result<BundleMember, Error> {
    Ok(BundleMember {
        path,
        sha256: crate::record::FramedIdentity::of(bytes),
        bytes: u64::try_from(bytes.len())?,
        elf_machine: isa.elf_machine(),
    })
}

fn append(archive: &mut tar::Builder<&mut Vec<u8>>, path: &Path, bytes: &[u8], mode: u32) -> Result<(), Error> {
    let mut header = tar::Header::new_gnu();
    header.set_size(u64::try_from(bytes.len())?);
    header.set_mode(mode);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_cksum();
    archive.append_data(&mut header, path, Cursor::new(bytes))?;
    Ok(())
}

pub(in crate::nested) fn materialize(source: &Path, destination: &Path) -> Result<(), Error> {
    let parent = destination
        .parent()
        .ok_or("nested artifact destination has no parent")?;
    fs::create_dir_all(parent)?;
    let stage = tempfile::Builder::new().prefix(".nested-prepare-").tempdir_in(parent)?;
    let mut archive = tar::Archive::new(fs::File::open(source)?);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        if !matches!(path.to_str(), Some("manifest.yaml" | "lib/libhl_native_engine.so"))
            && !(path.starts_with("bin") && path.components().count() == 2)
        {
            return Err(format!("nested bundle contains unexpected member {}", path.display()).into());
        }
        if !entry.unpack_in(stage.path())? {
            return Err(format!("nested bundle member escapes its root: {}", path.display()).into());
        }
    }
    if destination.exists() {
        fs::remove_dir_all(destination)?;
    }
    fs::rename(stage.keep(), destination)?;
    Ok(())
}
