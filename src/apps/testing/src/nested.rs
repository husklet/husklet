use crate::suite::SafePath as _;
use clap::{Args, Subcommand};
use hl_process::{Capture, Command as ProcessCommand, Outcome as ProcessOutcome};
use serde::Deserialize;
use std::{
    collections::BTreeSet,
    fs,
    os::unix::{ffi::OsStrExt, fs::PermissionsExt},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::Duration,
};

type Error = Box<dyn std::error::Error>;
const DEFAULT_CAPTURE_LIMIT: usize = 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u8,
    chains: Vec<Chain>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Chain {
    id: String,
    layers: Vec<Layer>,
    guest: Artifact,
    #[serde(default)]
    arguments: Vec<String>,
    #[serde(default = "default_timeout")]
    timeout_seconds: u64,
    #[serde(default = "default_capture_limit")]
    capture_limit_bytes: usize,
    expect: Expectation,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Layer {
    artifact: Artifact,
    guest_isa: GuestIsa,
    #[serde(default)]
    options: EngineOptions,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Artifact {
    path: PathBuf,
    #[serde(default)]
    source: ArtifactSource,
    build: Option<Build>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Build {
    package: String,
    target: String,
    #[serde(default = "release_profile")]
    profile: String,
    binary: String,
    #[serde(default)]
    rustflags: Vec<String>,
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ArtifactSource {
    #[default]
    Local,
    ForeignBuild,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum GuestIsa {
    Arm64,
    Amd64,
}

impl GuestIsa {
    const fn engine_name(&self) -> &'static str {
        match self {
            Self::Arm64 => "aarch64",
            Self::Amd64 => "x86_64",
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct EngineOptions {
    #[serde(default)]
    native_execution: bool,
    #[serde(default)]
    native_diagnostics: bool,
}

impl EngineOptions {
    fn validate(&self) -> Result<(), Error> {
        if self.native_diagnostics && !self.native_execution {
            return Err("native diagnostics require native execution".into());
        }
        Ok(())
    }

    fn append(&self, arguments: &mut Vec<String>) {
        if self.native_execution {
            arguments.extend(["--engine-option".into(), "HL_NATIVE_EXECUTION=1".into()]);
        }
        if self.native_diagnostics {
            arguments.extend(["--engine-option".into(), "HL_NATIVE_DIAGNOSTICS=1".into()]);
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Expectation {
    exit: i32,
    stdout: PathBuf,
}

enum Outcome {
    Passed,
    Unsupported(String),
    Failed(String),
}

const fn default_timeout() -> u64 {
    120
}
const fn default_capture_limit() -> usize {
    DEFAULT_CAPTURE_LIMIT
}

#[derive(Args)]
pub(crate) struct Options {
    #[command(subcommand)]
    action: Option<Action>,
}

#[derive(Subcommand)]
enum Action {
    /// Build and cache every declared nested-engine artifact.
    Prepare(Selection),
    /// Execute nested-engine chains, preparing declared artifacts first.
    Run(Selection),
}

#[derive(Args, Default)]
struct Selection {
    /// Nested-chain manifest relative to the workspace root.
    manifest: Option<PathBuf>,
}

pub fn run(options: Options) -> Result<(), Error> {
    let workspace = Workspace {
        root: crate::runtime::workspace()?,
    };
    let root = &workspace.root;
    let (prepare_only, selection) = match options.action {
        Some(Action::Prepare(selection)) => (true, selection),
        Some(Action::Run(selection)) => (false, selection),
        None => (false, Selection::default()),
    };
    let definition = selection
        .manifest
        .map_or_else(|| root.join("tests/runtime/nested/chains.yaml"), |path| root.join(path));
    let document = workspace.load(&definition)?;
    workspace.prepare(&document)?;
    if prepare_only {
        return Ok(());
    }
    let mut failed = 0;
    let mut unsupported = 0;
    for chain in document.chains {
        match workspace.execute(&definition, &chain) {
            Outcome::Passed => println!("PASS {}", chain.id),
            Outcome::Unsupported(reason) => {
                unsupported += 1;
                println!("UNSUPPORTED {}: {reason}", chain.id);
            }
            Outcome::Failed(reason) => {
                failed += 1;
                println!("FAIL {}: {reason}", chain.id);
            }
        }
    }
    println!("nested: {failed} failed; {unsupported} unsupported");
    if failed == 0 && unsupported == 0 {
        Ok(())
    } else {
        Err("nested gate is not green".into())
    }
}

fn release_profile() -> String {
    "release".into()
}

fn validate_build(build: &Build) -> Result<(), Error> {
    if build.package.is_empty()
        || build.binary.is_empty()
        || build.target.is_empty()
        || build.profile.is_empty()
        || build
            .binary
            .chars()
            .any(|character| !(character.is_ascii_alphanumeric() || matches!(character, '-' | '_')))
    {
        return Err("nested Cargo build contains an invalid package, target, profile, or binary".into());
    }
    Ok(())
}

struct BuildIdentity {
    key: String,
    cargo: String,
}

fn environment(name: &str) -> Option<String> {
    std::env::var_os(name).map(|value| value.to_string_lossy().into_owned())
}

fn build_environment(build: &Build) -> Vec<(String, String)> {
    let linker = format!(
        "CARGO_TARGET_{}_LINKER",
        build.target.to_ascii_uppercase().replace('-', "_")
    );
    [
        "CARGO",
        "RUSTC",
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "RUSTUP_TOOLCHAIN",
        &linker,
    ]
    .into_iter()
    .map(|name| (name.to_owned(), environment(name).unwrap_or_default()))
    .collect()
}

fn hash_source_named(digest: &mut crate::record::FramedIdentity, name: &[u8], path: &Path) -> Result<(), Error> {
    digest.field(name)?;
    digest.field(path.as_os_str().as_encoded_bytes())?;
    digest.field(&fs::read(path)?)
}

fn hash_tool(
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

fn build_artifact(
    root: &Path,
    build: &Build,
    cargo: &str,
    record: &crate::record::ArtifactRecord,
) -> Result<(), Error> {
    let arguments = vec![
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
    let mut arguments = arguments;
    if !build.rustflags.is_empty() {
        arguments.push("--".into());
        arguments.extend(build.rustflags.iter().cloned());
    }
    let output = capture(&arguments, Duration::from_secs(3600), 16 * 1024 * 1024)
        .map_err(|error| format!("nested Cargo build failed: {error}"))?;
    if output.status != Some(0) {
        return Err(format!(
            "nested Cargo build exited {:?}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
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

fn materialize(source: &Path, destination: &Path) -> Result<(), Error> {
    fs::create_dir_all(
        destination
            .parent()
            .ok_or("nested artifact destination has no parent")?,
    )?;
    let temporary = destination.with_extension(format!("prepare-{}", std::process::id()));
    if temporary.exists() {
        fs::remove_file(&temporary)?;
    }
    // The runnable destination needs owner-write mode for atomic replacement,
    // while cache objects remain immutable and can back several receipts.
    fs::copy(source, &temporary)?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o755))?;
    fs::rename(temporary, destination)?;
    Ok(())
}

#[derive(Debug)]
struct ProcessOutput {
    status: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn capture(arguments: &[String], timeout: Duration, limit: usize) -> Result<ProcessOutput, String> {
    let (program, guest) = arguments.split_first().ok_or("empty nested command")?;
    let output = tempfile::tempdir().map_err(|error| format!("capture directory failed: {error}"))?;
    let capture = Capture {
        stdout: output.path().join("stdout"),
        stderr: output.path().join("stderr"),
        stdout_limit: u64::try_from(limit).map_err(|_| "capture limit exceeds u64")?,
        stderr_limit: u64::try_from(limit).map_err(|_| "capture limit exceeds u64")?,
    };
    let mut command = ProcessCommand::new(program);
    command.args(guest);
    let outcome = hl_process::run(&command, &capture, timeout, &AtomicBool::new(false))
        .map_err(|error| format!("nested process failed: {error}"))?;
    let status = match outcome {
        ProcessOutcome::Exited(status) => status,
        ProcessOutcome::Signaled(_) => None,
        ProcessOutcome::TimedOut => return Err(format!("timed out after {} seconds", timeout.as_secs())),
        ProcessOutcome::Cancelled => return Err("nested process was cancelled".into()),
        ProcessOutcome::OutputLimit => return Err(format!("output exceeded {limit} bytes")),
    };
    let stdout = fs::read(&capture.stdout).map_err(|error| format!("stdout capture failed: {error}"))?;
    let stderr = fs::read(&capture.stderr).map_err(|error| format!("stderr capture failed: {error}"))?;
    Ok(ProcessOutput { status, stdout, stderr })
}

/// The repository tree a nested chain run reads its artifacts and builds from.
struct Workspace {
    root: PathBuf,
}

impl Workspace {
    fn build_key_with_environment(
        &self,
        build: &Build,
        cargo: &str,
        values: &[(String, String)],
    ) -> Result<String, Error> {
        let mut digest = crate::record::FramedIdentity::new(b"husklet-nested-build-v2")?;
        for value in [&build.package, &build.target, &build.profile, &build.binary] {
            digest.field(value.as_bytes())?;
        }
        for value in &build.rustflags {
            digest.field(value.as_bytes())?;
        }
        for name in ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml"] {
            let path = self.root.join(name);
            if path.is_file() {
                self.hash_source(&mut digest, &path)?;
            }
        }
        for path in self.cargo_configs() {
            if path.is_file() {
                hash_source_named(&mut digest, b"cargo-config", &path)?;
            }
        }
        let rustc = environment("RUSTC").unwrap_or_else(|| "rustc".into());
        for (name, value) in values {
            digest.field(name.as_bytes())?;
            digest.field(value.as_bytes())?;
        }
        hash_tool(&mut digest, "cargo", cargo, &["-V"])?;
        hash_tool(&mut digest, "rustc", &rustc, &["-vV"])?;
        hash_tool(
            &mut digest,
            "rustc-target",
            &rustc,
            &["--print", "target-libdir", "--target", &build.target],
        )?;
        self.hash_tree(&mut digest, &self.root.join("src"))?;
        Ok(digest.finish())
    }

    fn load(&self, definition: &Path) -> Result<Document, Error> {
        let document: Document = serde_yaml::from_str(&fs::read_to_string(definition)?)?;
        if document.version != 1 || document.chains.is_empty() {
            return Err(format!("{} has unsupported version or no chains", definition.display()).into());
        }
        let mut ids = BTreeSet::new();
        for chain in &document.chains {
            if chain.id.is_empty()
                || !ids.insert(&chain.id)
                || chain.layers.len() < 2
                || !(1..=3600).contains(&chain.timeout_seconds)
                || !(1..=16 * 1024 * 1024).contains(&chain.capture_limit_bytes)
                || !(0..=255).contains(&chain.expect.exit)
            {
                return Err(format!("invalid nested chain {:?}", chain.id).into());
            }
            self.validate_artifact(&chain.guest)?;
            chain.expect.stdout.safe_relative()?;
            for layer in &chain.layers {
                self.validate_artifact(&layer.artifact)?;
                layer.options.validate()?;
            }
        }
        Ok(document)
    }

    fn validate_artifact(&self, artifact: &Artifact) -> Result<(), Error> {
        artifact.path.safe_relative()?;
        if self.root.join(&artifact.path) == self.root
            || matches!(artifact.source, ArtifactSource::ForeignBuild) && artifact.build.is_none()
        {
            return Err(format!(
                "artifact {} has no usable path/build instruction",
                artifact.path.display()
            )
            .into());
        }
        Ok(())
    }

    fn prepare(&self, document: &Document) -> Result<(), Error> {
        let mut artifacts = document
            .chains
            .iter()
            .flat_map(|chain| chain.layers.iter().map(|layer| &layer.artifact).chain([&chain.guest]))
            .filter(|artifact| artifact.build.is_some())
            .collect::<Vec<_>>();
        artifacts.sort_by(|left, right| left.path.cmp(&right.path));
        artifacts.dedup_by(|left, right| left.path == right.path);
        for artifact in artifacts {
            self.prepare_artifact(artifact)?;
        }
        Ok(())
    }

    fn prepare_artifact(&self, artifact: &Artifact) -> Result<(), Error> {
        let build = artifact.build.as_ref().ok_or("prepared artifact has no build")?;
        validate_build(build)?;
        let identity = self.build_identity(build)?;
        let key = &identity.key;
        let cache = crate::record::Cache::new(&self.root)?;
        let receipts = cache.receipts(crate::record::ReceiptNamespace::Nested);
        let record = receipts.artifact(key, &build.binary)?;
        let _lock = receipts.lock(key)?;
        if record.verify()? {
            println!("REUSED {} key={key}", artifact.path.display());
        } else {
            build_artifact(&self.root, build, &identity.cargo, &record)?;
            println!("BUILT {} key={key}", artifact.path.display());
        }
        materialize(record.artifact(), &self.root.join(&artifact.path))?;
        Ok(())
    }

    fn build_identity(&self, build: &Build) -> Result<BuildIdentity, Error> {
        let cargo = environment("CARGO").unwrap_or_else(|| "cargo".into());
        let values = build_environment(build);
        let key = self.build_key_with_environment(build, &cargo, &values)?;
        Ok(BuildIdentity { key, cargo })
    }

    fn cargo_configs(&self) -> Vec<PathBuf> {
        let mut paths = self
            .root
            .ancestors()
            .flat_map(|directory| [directory.join(".cargo/config"), directory.join(".cargo/config.toml")])
            .collect::<Vec<_>>();
        let cargo_home = environment("CARGO_HOME")
            .map(PathBuf::from)
            .or_else(|| environment("HOME").map(|home| PathBuf::from(home).join(".cargo")));
        if let Some(home) = cargo_home {
            paths.extend([home.join("config"), home.join("config.toml")]);
        }
        paths.sort();
        paths.dedup();
        paths
    }

    fn hash_tree(&self, digest: &mut crate::record::FramedIdentity, directory: &Path) -> Result<(), Error> {
        let mut entries = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            let kind = entry.file_type()?;
            if kind.is_dir() {
                self.hash_tree(digest, &path)?;
            } else if kind.is_file() {
                self.hash_source(digest, &path)?;
            } else if kind.is_symlink() {
                let relative = path.strip_prefix(&self.root)?;
                digest.field(b"symlink")?;
                digest.field(relative.as_os_str().as_bytes())?;
                digest.field(fs::read_link(&path)?.as_os_str().as_bytes())?;
            } else {
                return Err(format!("nested build input is not a regular file: {}", path.display()).into());
            }
        }
        Ok(())
    }

    fn hash_source(&self, digest: &mut crate::record::FramedIdentity, path: &Path) -> Result<(), Error> {
        let relative = path.strip_prefix(&self.root)?;
        digest.field(relative.as_os_str().as_encoded_bytes())?;
        digest.field(&fs::read(path)?)?;
        Ok(())
    }

    fn command(&self, chain: &Chain) -> Vec<String> {
        let mut arguments = Vec::new();
        for layer in &chain.layers {
            arguments.push(self.root.join(&layer.artifact.path).display().to_string());
            arguments.push("--report-exit".into());
            arguments.extend(["--guest-isa".into(), layer.guest_isa.engine_name().into()]);
            layer.options.append(&mut arguments);
        }
        arguments.push(self.root.join(&chain.guest.path).display().to_string());
        arguments.extend(chain.arguments.iter().cloned());
        arguments
    }

    fn unavailable(&self, artifact: &Artifact) -> Option<Outcome> {
        let path = self.root.join(&artifact.path);
        if path
            .metadata()
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        {
            return None;
        }
        Some(match artifact.source {
            ArtifactSource::ForeignBuild => Outcome::Unsupported(format!(
                "foreign artifact {} is absent or not executable; run `testing nested prepare`",
                path.display(),
            )),
            ArtifactSource::Local => Outcome::Failed(format!(
                "required local artifact {} is absent or not executable",
                path.display()
            )),
        })
    }

    fn execute(&self, definition: &Path, chain: &Chain) -> Outcome {
        for artifact in chain.layers.iter().map(|layer| &layer.artifact).chain([&chain.guest]) {
            if let Some(outcome) = self.unavailable(artifact) {
                return outcome;
            }
        }
        let expected = definition.parent().unwrap_or(&self.root).join(&chain.expect.stdout);
        let expected = match fs::read(&expected) {
            Ok(value) => value,
            Err(error) => return Outcome::Failed(format!("cannot read {}: {error}", expected.display())),
        };
        let arguments = self.command(chain);
        match capture(
            &arguments,
            Duration::from_secs(chain.timeout_seconds),
            chain.capture_limit_bytes,
        ) {
            Ok(captured)
                if captured.status == Some(chain.expect.exit)
                    && captured.stdout == expected
                    && (!chain.layers.iter().any(|layer| layer.options.native_execution)
                        || String::from_utf8_lossy(&captured.stderr).contains("hl-native-detail:")) =>
            {
                Outcome::Passed
            }
            Ok(captured) => Outcome::Failed(format!(
                "exit={status:?} expected={}; stdout={} bytes expected={} bytes; native diagnostics required={}; stderr={}",
                chain.expect.exit,
                captured.stdout.len(),
                expected.len(),
                chain.layers.iter().any(|layer| layer.options.native_execution),
                String::from_utf8_lossy(&captured.stderr).trim(),
                status = captured.status,
            )),
            Err(error) => Outcome::Failed(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        ffi::OsString,
        os::unix::{ffi::OsStringExt, fs::symlink},
    };

    fn tree_key(root: &Path) -> String {
        let mut digest = crate::record::FramedIdentity::new(b"nested-tree-test").unwrap();
        Workspace {
            root: root.to_path_buf(),
        }
        .hash_tree(&mut digest, &root.join("src"))
        .unwrap();
        digest.finish()
    }

    #[test]
    fn typed_options_are_attached_to_the_layer_they_configure() {
        let chain: Chain = serde_yaml::from_str(
            r"
id: arm-amd
layers:
  - artifact: { path: outer }
    guest_isa: arm64
    options: { native_execution: true, native_diagnostics: true }
  - artifact: { path: inner }
    guest_isa: amd64
guest: { path: hello }
expect: { exit: 42, stdout: hello.txt }
",
        )
        .unwrap();
        let arguments = Workspace {
            root: PathBuf::from("/tree"),
        }
        .command(&chain);
        assert_eq!(
            &arguments[..8],
            [
                "/tree/outer",
                "--report-exit",
                "--guest-isa",
                "aarch64",
                "--engine-option",
                "HL_NATIVE_EXECUTION=1",
                "--engine-option",
                "HL_NATIVE_DIAGNOSTICS=1"
            ]
        );
        assert_eq!(
            &arguments[8..],
            ["/tree/inner", "--report-exit", "--guest-isa", "x86_64", "/tree/hello"]
        );
    }

    #[test]
    fn missing_foreign_artifact_is_explicitly_unsupported() {
        let artifact: Artifact = serde_yaml::from_str(
            "path: missing\nsource: foreign-build\nbuild: { package: hl-engine, target: x86_64-unknown-linux-musl, binary: hl-engine }\n",
        )
        .unwrap();
        assert!(matches!(
            Workspace {
                root: PathBuf::from("/definitely-absent"),
            }
            .unavailable(&artifact),
            Some(Outcome::Unsupported(_))
        ));
    }

    #[test]
    fn build_key_binds_source_and_typed_recipe() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("src")).unwrap();
        fs::write(root.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        fs::write(root.path().join("Cargo.lock"), "version = 4\n").unwrap();
        fs::write(root.path().join("src/lib.rs"), "pub fn first() {}\n").unwrap();
        let build: Build =
            serde_yaml::from_str("package: hl-engine\ntarget: aarch64-unknown-linux-musl\nbinary: hl-engine\n")
                .unwrap();
        let environment = vec![("RUSTFLAGS".into(), "-Ctarget-cpu=generic".into())];
        let initial = Workspace {
            root: root.path().to_path_buf(),
        }
        .build_key_with_environment(&build, "cargo", &environment)
        .unwrap();
        assert_eq!(
            initial,
            Workspace {
                root: root.path().to_path_buf()
            }
            .build_key_with_environment(&build, "cargo", &environment)
            .unwrap()
        );
        fs::write(root.path().join("src/lib.rs"), "pub fn second() {}\n").unwrap();
        assert_ne!(
            initial,
            Workspace {
                root: root.path().to_path_buf()
            }
            .build_key_with_environment(&build, "cargo", &environment)
            .unwrap()
        );
        let changed: Build =
            serde_yaml::from_str("package: hl-engine\ntarget: x86_64-unknown-linux-musl\nbinary: hl-engine\n").unwrap();
        assert_ne!(
            initial,
            Workspace {
                root: root.path().to_path_buf()
            }
            .build_key_with_environment(&changed, "cargo", &environment)
            .unwrap()
        );
        let changed_environment = vec![("RUSTFLAGS".into(), "-Ctarget-cpu=native".into())];
        assert_ne!(
            initial,
            Workspace {
                root: root.path().to_path_buf()
            }
            .build_key_with_environment(&build, "cargo", &changed_environment)
            .unwrap()
        );
    }

    #[test]
    fn build_key_hashes_dangling_symlink_without_following_it() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("src")).unwrap();
        symlink("absent-target", root.path().join("src/dangling")).unwrap();
        assert_eq!(tree_key(root.path()), tree_key(root.path()));
    }

    #[test]
    fn build_key_hashes_non_utf8_symlink_target_bytes() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("src")).unwrap();
        let target = OsString::from_vec(vec![b't', b'a', b'r', b'g', b'e', b't', 0xff]);
        symlink(&target, root.path().join("src/non-utf8")).unwrap();
        let first = tree_key(root.path());
        assert_eq!(first, tree_key(root.path()));
        assert!(!first.is_empty());
    }

    #[test]
    fn build_key_hashes_symlink_loop_as_objects() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("src")).unwrap();
        symlink("second", root.path().join("src/first")).unwrap();
        symlink("first", root.path().join("src/second")).unwrap();
        assert_eq!(tree_key(root.path()), tree_key(root.path()));
    }

    #[test]
    fn build_key_changes_when_raw_symlink_target_changes() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("src")).unwrap();
        let link = root.path().join("src/link");
        symlink("first", &link).unwrap();
        let first = tree_key(root.path());
        fs::remove_file(&link).unwrap();
        symlink("second", &link).unwrap();
        let second = tree_key(root.path());
        assert_ne!(first, second);
        assert_eq!(second, tree_key(root.path()));
    }

    #[test]
    fn cache_receipt_rejects_changed_artifact() {
        let directory = tempfile::tempdir().unwrap();
        let cache = crate::record::Cache::new(directory.path()).unwrap();
        let receipts = cache.receipts(crate::record::ReceiptNamespace::Nested);
        let key = "a".repeat(64);
        let record = receipts.artifact(&key, "hl-engine").unwrap();
        let _lock = receipts.lock(&key).unwrap();
        record.publish(b"first", true).unwrap();
        assert!(record.verify().unwrap());
        fs::set_permissions(record.artifact(), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(record.artifact(), b"second").unwrap();
        assert!(!record.verify().unwrap());
    }

    #[test]
    fn concurrent_preparation_is_serialized_by_key() {
        let root = tempfile::tempdir().unwrap();
        let cache = crate::record::Cache::new(root.path()).unwrap();
        let receipts = cache.receipts(crate::record::ReceiptNamespace::Nested);
        let key = "a".repeat(64);
        let first = receipts.lock(&key).unwrap();
        let thread = std::thread::spawn(move || {
            let cache = crate::record::Cache::new(root.path()).unwrap();
            let receipts = cache.receipts(crate::record::ReceiptNamespace::Nested);
            receipts.lock(&key).unwrap()
        });
        drop(first);
        drop(thread.join().unwrap());
    }

    #[test]
    fn capture_success() {
        let captured = capture(
            &[
                "/bin/sh".into(),
                "-c".into(),
                "printf output; printf detail >&2; exit 7".into(),
            ],
            Duration::from_secs(2),
            1024,
        )
        .unwrap();
        assert_eq!(captured.status, Some(7));
        assert_eq!(captured.stdout, b"output");
        assert_eq!(captured.stderr, b"detail");
    }

    #[test]
    fn capture_timeout() {
        let error = capture(
            &["/bin/sh".into(), "-c".into(), "sleep 2".into()],
            Duration::from_millis(20),
            1024,
        )
        .unwrap_err();
        assert!(error.contains("timed out"));
    }

    #[test]
    fn capture_limit() {
        let error = capture(
            &["/bin/sh".into(), "-c".into(), "printf 12345".into()],
            Duration::from_secs(2),
            4,
        )
        .unwrap_err();
        assert_eq!(error, "output exceeded 4 bytes");
    }
}
