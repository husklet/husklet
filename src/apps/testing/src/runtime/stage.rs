//! Content-bound staging for an immutable runtime-corpus runner and its private C engine.

use crate::suite::Error;
use clap::Args;
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use std::{
    fs,
    io::{BufRead, BufReader, Read as _, Write as _},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const RECEIPT_SCHEMA: &str = "husklet-runtime-corpus-artifacts-v1";
const SMOKE_RECEIPT: &str = "hl-native-artifact-smoke-v1";
// The testing application enables hl-native's test hooks, so its exact Cargo-emitted library has
// the test export surface rather than the smaller production-package surface.
const NATIVE_EXPORTS: &str = include_str!("../../../../runtime/hl-native/src/native/bridge/test_exports.txt");

#[derive(Args)]
pub(crate) struct Options {
    /// New immutable prefix relative to the repository root.
    #[arg(long, value_parser = crate::suite::parse::results)]
    output: PathBuf,
    /// Cargo executable from the pinned development shell.
    #[arg(long, default_value = "cargo")]
    cargo: PathBuf,
}

#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
struct Receipt<'a> {
    schema: &'static str,
    commit: &'a str,
    profile: &'static str,
    runner: ArtifactReceipt<'a>,
    library: ArtifactReceipt<'a>,
    smoke: &'static str,
}

#[derive(Serialize)]
struct ArtifactReceipt<'a> {
    path: &'a str,
    sha256: &'a str,
    bytes: u64,
}

struct BuildArtifacts {
    runner: PathBuf,
    library: PathBuf,
}

struct Artifact {
    bytes: Vec<u8>,
    digest: String,
}

pub(crate) fn run(options: Options) -> Result<(), Error> {
    if !cfg!(target_os = "linux") {
        return Err("runtime corpus staging currently supports Linux ELF artifact pairs only".into());
    }
    let workspace = super::workspace()?;
    let output = workspace.join(&options.output);
    if output.exists() {
        return Err(format!("runtime corpus artifact prefix already exists: {}", output.display()).into());
    }
    let commit = source_commit(&workspace)?;
    let built = build(&options.cargo, &workspace)?;
    let runner = Artifact::settled(&built.runner, ElfKind::Executable)?;
    let library = Artifact::settled(&built.library, ElfKind::SharedLibrary)?;
    require_matching_architecture(&runner.bytes, &library.bytes)?;

    let temporary = temporary_prefix(&output)?;
    let result = publish(&workspace, &temporary, &output, &runner, &library, &commit);
    if result.is_err() {
        let _ = fs::remove_dir_all(&temporary);
    }
    result
}

pub(crate) fn artifact_smoke() -> Result<(), Error> {
    #[cfg(unix)]
    {
        let runner = std::env::current_exe()?;
        let prefix = runner
            .parent()
            .and_then(Path::parent)
            .ok_or("native artifact smoke runner has no immutable prefix")?;
        let expected = prefix.join(native_library_receipt_path());
        let expected = fs::canonicalize(expected)?;
        let loaded = hl_native::artifact_paths().ok_or("dladdr could not resolve native engine lifecycle symbols")?;
        for path in loaded {
            if fs::canonicalize(&path)? != expected {
                return Err(format!(
                    "native artifact smoke loaded lifecycle symbol from {}, expected {}",
                    path.display(),
                    expected.display()
                )
                .into());
            }
        }
        if !hl_native::artifact_smoke() {
            return Err("native artifact ABI metadata is invalid".into());
        }
        hl_native::artifact_lifecycle_smoke().map_err(|error| format!("relocated native lifecycle smoke: {error}"))?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        Err("native artifact smoke is unavailable on this host".into())
    }
}

fn build(cargo: &Path, workspace: &Path) -> Result<BuildArtifacts, Error> {
    let packages = package_ids(cargo, workspace)?;
    let mut child = Command::new(cargo)
        .current_dir(workspace)
        .args([
            "build",
            "--release",
            "--locked",
            "--offline",
            "-p",
            "testing",
            "--bin",
            "testing",
            "--message-format=json-render-diagnostics",
        ])
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
    let artifacts = artifacts?;
    artifacts.ok_or_else(|| "Cargo did not identify both the testing runner and hl-native library".into())
}

struct PackageIds {
    testing: String,
    native: String,
}

fn package_ids(cargo: &Path, workspace: &Path) -> Result<PackageIds, Error> {
    let output = Command::new(cargo)
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
    Ok(PackageIds {
        testing: unique_package_id(packages, "testing", &workspace.join("src/apps/testing/Cargo.toml"))?,
        native: unique_package_id(
            packages,
            "hl-native",
            &workspace.join("src/runtime/hl-native/Cargo.toml"),
        )?,
    })
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

fn select_messages(
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

fn publish(
    workspace: &Path,
    temporary: &Path,
    output: &Path,
    runner: &Artifact,
    library: &Artifact,
    commit: &str,
) -> Result<(), Error> {
    let bin = temporary.join("bin");
    let lib = temporary.join("lib");
    fs::create_dir_all(&bin)?;
    fs::create_dir_all(&lib)?;
    let staged_runner = bin.join("testing");
    let staged_library = lib.join(native_library_name());
    runner.write(&staged_runner)?;
    library.write(&staged_library)?;
    inspect_artifacts(&staged_runner, &staged_library)?;
    let launcher = temporary.join("run");
    write_launcher(&launcher)?;

    smoke(&launcher)?;
    runner.verify(&staged_runner)?;
    library.verify(&staged_library)?;
    if source_commit(workspace)? != commit {
        return Err("source commit changed while staging runtime corpus artifacts".into());
    }
    let receipt = Receipt {
        schema: RECEIPT_SCHEMA,
        commit,
        profile: "release",
        runner: ArtifactReceipt {
            path: "bin/testing",
            sha256: &runner.digest,
            bytes: runner.bytes.len() as u64,
        },
        library: ArtifactReceipt {
            path: native_library_receipt_path(),
            sha256: &library.digest,
            bytes: library.bytes.len() as u64,
        },
        smoke: SMOKE_RECEIPT,
    };
    let receipt_path = temporary.join("receipt.yaml");
    fs::write(&receipt_path, serde_yaml::to_string(&receipt)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&receipt_path, fs::Permissions::from_mode(0o444))?;
    }
    sync_directory(&bin)?;
    sync_directory(&lib)?;
    sync_directory(temporary)?;
    publish_prefix(temporary, output)?;
    sync_directory(output.parent().ok_or("runtime corpus output has no parent")?)?;
    println!("READY runtime corpus runner {}", output.join("run").display());
    println!("runner sha256={}", runner.digest);
    println!("library sha256={}", library.digest);
    Ok(())
}

fn source_commit(workspace: &Path) -> Result<String, Error> {
    let commit = command_text(Command::new("git").current_dir(workspace).args(["rev-parse", "HEAD"]))?;
    let commit = commit.trim();
    if commit.len() != 40 || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("git returned an invalid source commit".into());
    }
    Ok(commit.to_owned())
}

fn sync_directory(path: &Path) -> Result<(), Error> {
    fs::File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn publish_prefix(temporary: &Path, output: &Path) -> Result<(), Error> {
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        temporary,
        rustix::fs::CWD,
        output,
        rustix::fs::RenameFlags::NOREPLACE,
    )?;
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn publish_prefix(_: &Path, _: &Path) -> Result<(), Error> {
    Err("runtime corpus artifact publication is not implemented on this host".into())
}

fn smoke(launcher: &Path) -> Result<(), Error> {
    for injected in [false, true] {
        let mut command = Command::new(launcher);
        command
            .arg("native-artifact-smoke")
            .env_clear()
            .env("PATH", "/usr/bin:/bin");
        if injected {
            command
                .env("LD_PRELOAD", "/husklet-intentionally-missing-preload.so")
                .env("LD_AUDIT", "/husklet-intentionally-missing-audit.so");
        }
        let output = command.output()?;
        if !output.status.success() || String::from_utf8_lossy(&output.stdout).trim() != SMOKE_RECEIPT {
            return Err(format!(
                "staged native artifact smoke failed with {} (injected={injected}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            )
            .into());
        }
    }
    Ok(())
}

fn inspect_artifacts(runner: &Path, library: &Path) -> Result<(), Error> {
    for artifact in [runner, library] {
        let description = command_text(Command::new("file").args(["--brief"]).arg(artifact))?;
        if !description.contains("ELF 64-bit") {
            return Err(format!("staged artifact is not an ELF64 image: {}", artifact.display()).into());
        }
        command_text(Command::new("readelf").args(["--wide", "--file-header"]).arg(artifact))?;
    }
    let runner_dynamic = command_text(Command::new("readelf").args(["--wide", "--dynamic"]).arg(runner))?;
    let library_dynamic = command_text(Command::new("readelf").args(["--wide", "--dynamic"]).arg(library))?;
    let library_symbols = command_text(Command::new("readelf").args(["--wide", "--dyn-syms"]).arg(library))?;
    require_readelf_contract(&runner_dynamic, &library_dynamic, &library_symbols, NATIVE_EXPORTS)
}

fn require_readelf_contract(runner: &str, library: &str, symbols: &str, expected_exports: &str) -> Result<(), Error> {
    if !runner
        .lines()
        .any(|line| line.contains("(NEEDED)") && line.contains("[libhl_native_engine.so]"))
    {
        return Err("staged testing runner does not require libhl_native_engine.so".into());
    }
    if !library
        .lines()
        .any(|line| line.contains("(SONAME)") && line.contains("[libhl_native_engine.so]"))
    {
        return Err("staged native library has the wrong ELF SONAME".into());
    }
    let actual = symbols
        .lines()
        .filter_map(|line| {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            (fields.len() >= 8
                && fields[3] == "FUNC"
                && fields[4] == "GLOBAL"
                && fields[5] == "DEFAULT"
                && fields[6] != "UND")
                .then(|| fields[7].split('@').next().unwrap_or(fields[7]))
        })
        .collect::<std::collections::BTreeSet<_>>();
    let expected = expected_exports
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<std::collections::BTreeSet<_>>();
    if actual != expected {
        return Err(format!("staged native library exports differ: actual={actual:?} expected={expected:?}").into());
    }
    Ok(())
}

fn write_launcher(path: &Path) -> Result<(), Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::write(
            path,
            "#!/bin/sh\nset -eu\nprefix=${0%/*}\nprefix=$(CDPATH= cd -- \"$prefix\" && pwd)\nunset LD_PRELOAD LD_AUDIT\nLD_LIBRARY_PATH=\"$prefix/lib\"\nexport LD_LIBRARY_PATH\nexec \"$prefix/bin/testing\" \"$@\"\n",
        )?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o555))?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err("runtime corpus artifact staging is not implemented on this host".into())
    }
}

impl Artifact {
    fn settled(path: &Path, kind: ElfKind) -> Result<Self, Error> {
        let before =
            fs::symlink_metadata(path).map_err(|error| format!("read artifact {}: {error}", path.display()))?;
        if before.file_type().is_symlink() || !before.is_file() {
            return Err(format!("artifact is not a regular non-symlink file: {}", path.display()).into());
        }
        let mut source = fs::File::open(path)?;
        let opened = source.metadata()?;
        if !opened.is_file() || opened.len() != before.len() || opened.modified()? != before.modified()? {
            return Err(format!("artifact changed before it could be staged: {}", path.display()).into());
        }
        let mut bytes = Vec::new();
        source.read_to_end(&mut bytes)?;
        let after = source.metadata()?;
        if before.len() != after.len() || before.modified()? != after.modified()? {
            return Err(format!("artifact changed while being staged: {}", path.display()).into());
        }
        validate_elf(&bytes, kind)?;
        let digest = sha256(&bytes);
        Ok(Self { bytes, digest })
    }

    fn write(&self, path: &Path) -> Result<(), Error> {
        let mut file = fs::OpenOptions::new().create_new(true).write(true).open(path)?;
        file.write_all(&self.bytes)?;
        file.sync_all()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(path, fs::Permissions::from_mode(0o555))?;
        }
        self.verify(path)
    }

    fn verify(&self, path: &Path) -> Result<(), Error> {
        let bytes = fs::read(path)?;
        if bytes != self.bytes || sha256(&bytes) != self.digest {
            Err(format!("staged artifact differs from its settled source: {}", path.display()).into())
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy)]
enum ElfKind {
    Executable,
    SharedLibrary,
}

fn validate_elf(bytes: &[u8], kind: ElfKind) -> Result<(), Error> {
    if bytes.len() < 20 || &bytes[..4] != b"\x7fELF" || bytes[4] != 2 || bytes[5] != 1 {
        return Err("artifact is not a little-endian ELF64 image".into());
    }
    let image_type = u16::from_le_bytes([bytes[16], bytes[17]]);
    let valid = match kind {
        ElfKind::Executable => matches!(image_type, 2 | 3),
        ElfKind::SharedLibrary => image_type == 3,
    };
    if valid {
        Ok(())
    } else {
        Err("artifact has the wrong ELF image type".into())
    }
}

fn require_matching_architecture(runner: &[u8], library: &[u8]) -> Result<(), Error> {
    let architecture = runner.get(18..20).ok_or("testing runner omits ELF architecture")?;
    let machine = u16::from_le_bytes(architecture.try_into()?);
    if architecture == library.get(18..20).ok_or("native library omits ELF architecture")?
        && matches!(machine, 62 | 183)
    {
        Ok(())
    } else {
        Err("testing runner and native library have different ELF architectures".into())
    }
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
}

fn temporary_prefix(output: &Path) -> Result<PathBuf, Error> {
    let parent = output.parent().ok_or("runtime corpus output has no parent")?;
    fs::create_dir_all(parent)?;
    for attempt in 0..16 {
        let path = parent.join(format!(
            ".runtime-corpus-stage-{}-{}-{attempt}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err("could not reserve a private runtime corpus staging prefix".into())
}

fn command_text(command: &mut Command) -> Result<String, Error> {
    let output = command.output()?;
    if !output.status.success() {
        return Err(format!(
            "command failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

const fn native_library_name() -> &'static str {
    "libhl_native_engine.so"
}

const fn native_library_receipt_path() -> &'static str {
    "lib/libhl_native_engine.so"
}

#[cfg(test)]
mod tests {
    use super::{
        Artifact, ElfKind, publish_prefix, require_matching_architecture, require_readelf_contract, select_messages,
        temporary_prefix,
    };
    use std::{fs, io::Cursor};

    fn elf(kind: u16, machine: u16) -> Vec<u8> {
        let mut bytes = vec![0_u8; 64];
        bytes[..6].copy_from_slice(b"\x7fELF\x02\x01");
        bytes[16..18].copy_from_slice(&kind.to_le_bytes());
        bytes[18..20].copy_from_slice(&machine.to_le_bytes());
        bytes
    }

    #[test]
    fn cargo_messages_bind_one_runner_to_one_native_library() {
        let messages = concat!(
            "{\"reason\":\"compiler-artifact\",\"package_id\":\"path+file:///source/src/apps/testing#0.1.0\",\"target\":{\"name\":\"testing\",\"kind\":[\"bin\"]},\"executable\":\"/build/testing\"}\n",
            "{\"reason\":\"build-script-executed\",\"package_id\":\"path+file:///source/src/runtime/hl-native#0.1.0\",\"env\":[[\"HL_NATIVE_LIBRARY_PATH\",\"/build/libhl_native_engine.so\"]]}\n"
        );
        let testing = "path+file:///source/src/apps/testing#0.1.0";
        let native = "path+file:///source/src/runtime/hl-native#0.1.0";
        let selected = select_messages(Cursor::new(messages), testing, native)
            .unwrap()
            .unwrap();
        assert_eq!(selected.runner, std::path::Path::new("/build/testing"));
        assert_eq!(selected.library, std::path::Path::new("/build/libhl_native_engine.so"));
        assert!(select_messages(Cursor::new(&messages[..messages.find('\n').unwrap()]), testing, native).is_err());
        assert!(select_messages(Cursor::new(format!("{messages}{messages}")), testing, native).is_err());
        assert!(select_messages(Cursor::new(messages), testing, "path+file:///foreign/hl-native#0.1.0").is_err());
        assert!(select_messages(Cursor::new(messages), "path+file:///foreign/testing#0.1.0", native).is_err());
    }

    #[test]
    fn missing_corrupt_symlinked_and_wrong_kind_libraries_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("missing.so");
        assert!(Artifact::settled(&missing, ElfKind::SharedLibrary).is_err());
        let corrupt = directory.path().join("corrupt.so");
        fs::write(&corrupt, b"not an ELF image").unwrap();
        assert!(Artifact::settled(&corrupt, ElfKind::SharedLibrary).is_err());
        let executable = directory.path().join("executable.so");
        fs::write(&executable, elf(2, 183)).unwrap();
        assert!(Artifact::settled(&executable, ElfKind::SharedLibrary).is_err());
        #[cfg(unix)]
        {
            let symlink = directory.path().join("link.so");
            std::os::unix::fs::symlink(&executable, &symlink).unwrap();
            assert!(Artifact::settled(&symlink, ElfKind::SharedLibrary).is_err());
        }
    }

    #[test]
    fn staging_prefix_is_reserved_and_publication_never_replaces_a_name() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("published");
        let temporary = temporary_prefix(&output).unwrap();
        assert!(temporary.is_dir());
        fs::create_dir(&output).unwrap();
        assert!(publish_prefix(&temporary, &output).is_err());
        assert!(temporary.is_dir());

        fs::remove_dir(&output).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("missing", &output).unwrap();
            assert!(publish_prefix(&temporary, &output).is_err());
            assert!(temporary.is_dir());
        }
    }

    #[test]
    fn a_valid_shared_image_for_the_wrong_architecture_is_rejected() {
        let runner = elf(3, 183);
        assert!(require_matching_architecture(&runner, &elf(3, 62)).is_err());
        assert!(require_matching_architecture(&runner, &elf(3, 183)).is_ok());
        assert!(require_matching_architecture(&elf(3, 8), &elf(3, 8)).is_err());
    }

    #[test]
    fn a_same_architecture_shared_object_with_the_wrong_contract_is_rejected() {
        let runner = " 0x1 (NEEDED) Shared library: [libhl_native_engine.so]\n";
        let library = " 0xe (SONAME) Library soname: [libhl_native_engine.so]\n";
        let exports = concat!(
            " 1: 1 1 FUNC GLOBAL DEFAULT 12 hl_engine_abi\n",
            " 2: 1 1 FUNC GLOBAL DEFAULT 12 hl_engine_version\n",
            " 3: 1 1 FUNC GLOBAL DEFAULT 12 hl_c_backend_create\n",
        );
        let expected = "hl_c_backend_create\nhl_engine_abi\nhl_engine_version\n";
        assert!(require_readelf_contract(runner, library, exports, expected).is_ok());
        assert!(require_readelf_contract(runner, library, "", expected).is_err());
        assert!(require_readelf_contract(runner, library, exports, "hl_engine_abi\n").is_err());
        assert!(require_readelf_contract(runner, "SONAME [libm.so.6]", exports, expected).is_err());
        assert!(require_readelf_contract("NEEDED [libm.so.6]", library, exports, expected).is_err());
    }
}
