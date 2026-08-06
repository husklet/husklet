use clap::{Args, Subcommand};
use hl_process::{Capture, Command as ProcessCommand, Outcome as ProcessOutcome};
use nix::{
    fcntl::{OFlag, open, openat},
    sys::stat::Mode,
};
use serde::Deserialize;
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io,
    os::unix::{ffi::OsStrExt, fs::PermissionsExt},
    path::{Component, Path, PathBuf},
    sync::atomic::AtomicBool,
    time::Duration,
};

type Error = Box<dyn std::error::Error>;
const DEFAULT_CAPTURE_LIMIT: usize = 1024 * 1024;
const MAXIMUM_LAYERS: usize = 16;

// ORACLE: the retained gate is ../engine/tools/nested_engine_gate.c,
// whose main validates every executable before process_run(), owns the captured
// result until comparison, and releases it on every exit. process_run() in
// tools/process.c forks one child, reports exec failure separately, grows its
// capture without a bound, waits without a timeout, and returns status/stdout. The C gate
// forwards one host argv chain and therefore relies on the inherited host tree.
// Husklet instead gives the outer engine one temporary, flat artifact root: its
// first two executable paths select/enter that root, and later layers use stable
// guest paths in the same root. Bundle owns that root through capture completion;
// dropping it cleans successful, failed, limited, and timed-out runs. Each source
// is opened relative to a no-follow directory descriptor and copied from that
// descriptor, so neither a source leaf nor an intermediate component can be
// replaced with a followed symlink between validation and copy.

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
    expect: Expect,
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
struct Expect {
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
    let root = crate::runtime::workspace()?;
    let (prepare_only, selection) = match options.action {
        Some(Action::Prepare(selection)) => (true, selection),
        Some(Action::Run(selection)) => (false, selection),
        None => (false, Selection::default()),
    };
    let definition = selection
        .manifest
        .map_or_else(|| root.join("tests/runtime/nested/chains.yaml"), |path| root.join(path));
    let document = load(&root, &definition)?;
    prepare(&root, &document)?;
    if prepare_only {
        return Ok(());
    }
    let mut failed = 0;
    let mut unsupported = 0;
    for chain in document.chains {
        match execute(&root, &definition, &chain) {
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

fn load(root: &Path, definition: &Path) -> Result<Document, Error> {
    let document: Document = serde_yaml::from_str(&fs::read_to_string(definition)?)?;
    if document.version != 1 || document.chains.is_empty() {
        return Err(format!("{} has unsupported version or no chains", definition.display()).into());
    }
    let mut ids = BTreeSet::new();
    for chain in &document.chains {
        if chain.id.is_empty()
            || !ids.insert(&chain.id)
            || !(2..=MAXIMUM_LAYERS).contains(&chain.layers.len())
            || !(1..=3600).contains(&chain.timeout_seconds)
            || !(1..=16 * 1024 * 1024).contains(&chain.capture_limit_bytes)
            || !(0..=255).contains(&chain.expect.exit)
        {
            return Err(format!("invalid nested chain {:?}", chain.id).into());
        }
        validate_artifact(root, &chain.guest)?;
        safe_relative(&chain.expect.stdout)?;
        for layer in &chain.layers {
            validate_artifact(root, &layer.artifact)?;
            layer.options.validate()?;
        }
    }
    Ok(document)
}

fn validate_artifact(root: &Path, artifact: &Artifact) -> Result<(), Error> {
    safe_relative(&artifact.path)?;
    if root.join(&artifact.path) == root
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

fn release_profile() -> String {
    "release".into()
}

fn prepare(root: &Path, document: &Document) -> Result<(), Error> {
    let mut artifacts = document
        .chains
        .iter()
        .flat_map(|chain| chain.layers.iter().map(|layer| &layer.artifact).chain([&chain.guest]))
        .filter(|artifact| artifact.build.is_some())
        .collect::<Vec<_>>();
    artifacts.sort_by(|left, right| left.path.cmp(&right.path));
    artifacts.dedup_by(|left, right| left.path == right.path);
    for artifact in artifacts {
        prepare_artifact(root, artifact)?;
    }
    Ok(())
}

fn prepare_artifact(root: &Path, artifact: &Artifact) -> Result<(), Error> {
    let build = artifact.build.as_ref().ok_or("prepared artifact has no build")?;
    validate_build(build)?;
    let identity = build_identity(root, build)?;
    let key = &identity.key;
    let cache = crate::record::Cache::new(root)?;
    let receipts = cache.receipts(crate::record::ReceiptNamespace::Nested);
    let record = receipts.artifact(key, &build.binary)?;
    let _lock = receipts.lock(key)?;
    if !record.verify()? {
        build_artifact(root, build, &identity.cargo, &record)?;
        println!("BUILT {} key={key}", artifact.path.display());
    } else {
        println!("REUSED {} key={key}", artifact.path.display());
    }
    materialize(record.artifact(), &root.join(&artifact.path))?;
    Ok(())
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

fn build_identity(root: &Path, build: &Build) -> Result<BuildIdentity, Error> {
    let cargo = environment("CARGO").unwrap_or_else(|| "cargo".into());
    let values = build_environment(build);
    let key = build_key_with_environment(root, build, &cargo, &values)?;
    Ok(BuildIdentity { key, cargo })
}

fn build_key_with_environment(
    root: &Path,
    build: &Build,
    cargo: &str,
    values: &[(String, String)],
) -> Result<String, Error> {
    let mut digest = crate::record::FramedIdentity::new(b"husklet-nested-build-v2")?;
    for value in [&build.package, &build.target, &build.profile, &build.binary] {
        hash_field(&mut digest, value.as_bytes())?;
    }
    for value in &build.rustflags {
        hash_field(&mut digest, value.as_bytes())?;
    }
    for name in ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml"] {
        let path = root.join(name);
        if path.is_file() {
            hash_source(&mut digest, root, &path)?;
        }
    }
    for path in cargo_configs(root) {
        if path.is_file() {
            hash_source_named(&mut digest, b"cargo-config", &path)?;
        }
    }
    let rustc = environment("RUSTC").unwrap_or_else(|| "rustc".into());
    for (name, value) in values {
        hash_field(&mut digest, name.as_bytes())?;
        hash_field(&mut digest, value.as_bytes())?;
    }
    hash_tool(&mut digest, "cargo", cargo, &["-V"])?;
    hash_tool(&mut digest, "rustc", &rustc, &["-vV"])?;
    hash_tool(
        &mut digest,
        "rustc-target",
        &rustc,
        &["--print", "target-libdir", "--target", &build.target],
    )?;
    hash_tree(&mut digest, root, &root.join("src"))?;
    Ok(digest.finish())
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

fn cargo_configs(root: &Path) -> Vec<PathBuf> {
    let mut paths = root
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

fn hash_source_named(digest: &mut crate::record::FramedIdentity, name: &[u8], path: &Path) -> Result<(), Error> {
    hash_field(digest, name)?;
    hash_field(digest, path.as_os_str().as_encoded_bytes())?;
    hash_field(digest, &fs::read(path)?)
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
    hash_field(digest, name.as_bytes())?;
    hash_field(digest, program.as_bytes())?;
    hash_field(digest, &output.stdout)?;
    hash_field(digest, &output.stderr)
}

fn hash_tree(digest: &mut crate::record::FramedIdentity, root: &Path, directory: &Path) -> Result<(), Error> {
    let mut entries = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            hash_tree(digest, root, &path)?;
        } else if kind.is_file() {
            hash_source(digest, root, &path)?;
        } else if kind.is_symlink() {
            let relative = path.strip_prefix(root)?;
            hash_field(digest, b"symlink")?;
            hash_field(digest, relative.as_os_str().as_bytes())?;
            hash_field(digest, fs::read_link(&path)?.as_os_str().as_bytes())?;
        } else {
            return Err(format!("nested build input is not a regular file: {}", path.display()).into());
        }
    }
    Ok(())
}

fn hash_source(digest: &mut crate::record::FramedIdentity, root: &Path, path: &Path) -> Result<(), Error> {
    let relative = path.strip_prefix(root)?;
    hash_field(digest, relative.as_os_str().as_encoded_bytes())?;
    hash_field(digest, &fs::read(path)?)?;
    Ok(())
}

fn hash_field(digest: &mut crate::record::FramedIdentity, value: &[u8]) -> Result<(), Error> {
    digest.field(value)
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

fn safe_relative(path: &Path) -> Result<(), Error> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|part| matches!(part, Component::ParentDir))
    {
        Err(format!("unsafe relative path {}", path.display()).into())
    } else {
        Ok(())
    }
}

#[derive(Debug)]
struct Bundle {
    _directory: tempfile::TempDir,
    layers: Vec<PathBuf>,
}

impl Bundle {
    fn new(root: &Path, chain: &Chain) -> Result<Self, Error> {
        Self::new_in(root, chain, None)
    }

    fn new_in(root: &Path, chain: &Chain, parent: Option<&Path>) -> Result<Self, Error> {
        let directory = match parent {
            Some(parent) => tempfile::tempdir_in(parent)?,
            None => tempfile::tempdir()?,
        };
        let mut layers = Vec::with_capacity(chain.layers.len());
        for (index, layer) in chain.layers.iter().enumerate() {
            let destination = directory.path().join(format!("layer-{index}"));
            Self::copy(root, &layer.artifact.path, &destination)?;
            layers.push(destination);
        }
        let guest = directory.path().join("guest");
        Self::copy(root, &chain.guest.path, &guest)?;
        Ok(Self {
            _directory: directory,
            layers,
        })
    }

    fn copy(root: &Path, source: &Path, destination: &Path) -> Result<(), Error> {
        safe_relative(source)?;
        let mut directory = open(
            root,
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW,
            Mode::empty(),
        )
        .map_err(|error| format!("cannot open nested bundle root {}: {error}", root.display()))?;
        let components = source.components().collect::<Vec<_>>();
        let (name, parents) = components.split_last().ok_or("nested bundle input has no file name")?;
        for component in parents {
            let Component::Normal(name) = component else {
                return Err(format!("unsafe nested bundle component in {}", source.display()).into());
            };
            directory = openat(
                &directory,
                *name,
                OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW,
                Mode::empty(),
            )
            .map_err(|error| format!("cannot traverse nested bundle input {}: {error}", source.display()))?;
        }
        let Component::Normal(name) = name else {
            return Err(format!("unsafe nested bundle leaf in {}", source.display()).into());
        };
        let input = openat(
            &directory,
            *name,
            OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK,
            Mode::empty(),
        )
        .map_err(|error| format!("cannot open nested bundle input {}: {error}", source.display()))?;
        let mut input = File::from(input);
        if !input.metadata()?.is_file() {
            return Err(format!("nested bundle input is not a regular file: {}", source.display()).into());
        }
        let mut output = OpenOptions::new().write(true).create_new(true).open(destination)?;
        io::copy(&mut input, &mut output)?;
        output.sync_all()?;
        output.set_permissions(fs::Permissions::from_mode(0o555))?;
        Ok(())
    }

    fn host(&self, path: &Path) -> Result<String, Error> {
        path.to_str()
            .map(str::to_owned)
            .ok_or_else(|| "nested bundle path is not UTF-8".into())
    }

    fn guest_layer(index: usize) -> String {
        format!("/layer-{index}")
    }

    fn command(&self, chain: &Chain) -> Result<Vec<String>, Error> {
        let mut arguments = Vec::new();
        for (index, layer) in chain.layers.iter().enumerate() {
            // The outer host must open layer 1 before a guest filesystem exists.
            // Every later path is resolved inside the shared bundle root selected
            // from that host path, so forwarding another host path would escape
            // the parent engine's guest-visible namespace.
            arguments.push(if index < 2 {
                self.host(&self.layers[index])?
            } else {
                Self::guest_layer(index)
            });
            arguments.push("--report-exit".into());
            arguments.extend(["--guest-isa".into(), layer.guest_isa.engine_name().into()]);
            layer.options.append(&mut arguments);
        }
        arguments.push("/guest".into());
        arguments.extend(chain.arguments.iter().cloned());
        Ok(arguments)
    }

    #[cfg(test)]
    fn path(&self) -> &Path {
        self._directory.path()
    }
}

fn command(root: &Path, chain: &Chain) -> Result<(Bundle, Vec<String>), Error> {
    let bundle = Bundle::new(root, chain)?;
    let arguments = bundle.command(chain)?;
    Ok((bundle, arguments))
}

fn unavailable(root: &Path, artifact: &Artifact) -> Option<Outcome> {
    let path = root.join(&artifact.path);
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

fn execute(root: &Path, definition: &Path, chain: &Chain) -> Outcome {
    for artifact in chain.layers.iter().map(|layer| &layer.artifact).chain([&chain.guest]) {
        if let Some(outcome) = unavailable(root, artifact) {
            return outcome;
        }
    }
    let expected = definition.parent().unwrap_or(root).join(&chain.expect.stdout);
    let expected = match fs::read(&expected) {
        Ok(value) => value,
        Err(error) => return Outcome::Failed(format!("cannot read {}: {error}", expected.display())),
    };
    let (_bundle, arguments) = match command(root, chain) {
        Ok(value) => value,
        Err(error) => return Outcome::Failed(format!("nested bundle failed: {error}")),
    };
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        ffi::OsString,
        os::unix::{ffi::OsStringExt, fs::symlink},
    };

    fn tree_key(root: &Path) -> String {
        let mut digest = crate::record::FramedIdentity::new(b"nested-tree-test").unwrap();
        hash_tree(&mut digest, root, &root.join("src")).unwrap();
        digest.finish()
    }

    #[test]
    fn typed_options_are_attached_to_the_layer_they_configure() {
        let root = tempfile::tempdir().unwrap();
        for name in ["outer", "inner", "hello"] {
            fs::write(root.path().join(name), name).unwrap();
        }
        let chain: Chain = serde_yaml::from_str(
            r#"
id: arm-amd
layers:
  - artifact: { path: outer }
    guest_isa: arm64
    options: { native_execution: true, native_diagnostics: true }
  - artifact: { path: inner }
    guest_isa: amd64
guest: { path: hello }
expect: { exit: 42, stdout: hello.txt }
"#,
        )
        .unwrap();
        let (bundle, arguments) = command(root.path(), &chain).unwrap();
        assert_eq!(
            &arguments[..8],
            [
                bundle.path().join("layer-0").to_str().unwrap(),
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
            [
                bundle.path().join("layer-1").to_str().unwrap(),
                "--report-exit",
                "--guest-isa",
                "x86_64",
                "/guest"
            ]
        );
        assert_eq!(fs::read(bundle.path().join("layer-0")).unwrap(), b"outer");
        assert_eq!(fs::read(bundle.path().join("layer-1")).unwrap(), b"inner");
        assert_eq!(fs::read(bundle.path().join("guest")).unwrap(), b"hello");
        for name in ["layer-0", "layer-1", "guest"] {
            assert_eq!(
                fs::metadata(bundle.path().join(name)).unwrap().permissions().mode() & 0o777,
                0o555
            );
        }
        let directory = bundle.path().to_owned();
        drop(bundle);
        assert!(!directory.exists());
    }

    #[test]
    fn bundle_projects_only_descendants_into_the_guest_root() {
        let root = tempfile::tempdir().unwrap();
        for path in ["first/outer", "second/middle", "third/inner", "leaf/hello"] {
            let path = root.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"artifact").unwrap();
        }
        let chain: Chain = serde_yaml::from_str(
            r#"
id: three
layers:
  - artifact: { path: first/outer }
    guest_isa: amd64
  - artifact: { path: second/middle }
    guest_isa: arm64
  - artifact: { path: third/inner }
    guest_isa: amd64
guest: { path: leaf/hello }
expect: { exit: 42, stdout: hello.txt }
"#,
        )
        .unwrap();
        let (bundle, arguments) = command(root.path(), &chain).unwrap();
        assert_eq!(arguments[0], bundle.path().join("layer-0").to_str().unwrap());
        assert_eq!(arguments[4], bundle.path().join("layer-1").to_str().unwrap());
        assert_eq!(arguments[8], "/layer-2");
        assert_eq!(arguments[12], "/guest");
        let names = fs::read_dir(bundle.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<BTreeSet<_>>();
        assert_eq!(names.len(), 4);
    }

    #[test]
    fn bundle_accepts_non_utf8_source_paths_without_exposing_them() {
        let root = tempfile::tempdir().unwrap();
        let name = OsString::from_vec(vec![b'e', b'n', b'g', b'i', b'n', b'e', 0xff]);
        fs::write(root.path().join(&name), b"engine").unwrap();
        fs::write(root.path().join("inner"), b"inner").unwrap();
        fs::write(root.path().join("guest"), b"guest").unwrap();
        let mut chain: Chain = serde_yaml::from_str(
            "id: bytes\nlayers:\n  - artifact: { path: placeholder }\n    guest_isa: arm64\n  - artifact: { path: inner }\n    guest_isa: arm64\nguest: { path: guest }\nexpect: { exit: 0, stdout: out }\n",
        )
        .unwrap();
        chain.layers[0].artifact.path = PathBuf::from(name);
        let (bundle, arguments) = command(root.path(), &chain).unwrap();
        assert_eq!(fs::read(bundle.path().join("layer-0")).unwrap(), b"engine");
        assert!(
            arguments
                .iter()
                .all(|argument| !argument.contains(char::REPLACEMENT_CHARACTER))
        );
    }

    #[test]
    fn bundle_rejects_symlink_inputs_without_following_them() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("target"), b"engine").unwrap();
        symlink("target", root.path().join("outer")).unwrap();
        fs::write(root.path().join("inner"), b"inner").unwrap();
        fs::write(root.path().join("guest"), b"guest").unwrap();
        let chain: Chain = serde_yaml::from_str(
            "id: link\nlayers:\n  - artifact: { path: outer }\n    guest_isa: arm64\n  - artifact: { path: inner }\n    guest_isa: arm64\nguest: { path: guest }\nexpect: { exit: 0, stdout: out }\n",
        )
        .unwrap();
        let error = match command(root.path(), &chain) {
            Ok(_) => panic!("symlink input was accepted"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("cannot open nested bundle input"));
    }

    #[test]
    fn bundle_rejects_intermediate_symlinks_without_following_them() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("real")).unwrap();
        fs::write(root.path().join("real/outer"), b"engine").unwrap();
        symlink("real", root.path().join("linked")).unwrap();
        fs::write(root.path().join("inner"), b"inner").unwrap();
        fs::write(root.path().join("guest"), b"guest").unwrap();
        let chain: Chain = serde_yaml::from_str(
            "id: link\nlayers:\n  - artifact: { path: linked/outer }\n    guest_isa: arm64\n  - artifact: { path: inner }\n    guest_isa: arm64\nguest: { path: guest }\nexpect: { exit: 0, stdout: out }\n",
        )
        .unwrap();
        let error = match command(root.path(), &chain) {
            Ok(_) => panic!("intermediate symlink was accepted"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("cannot traverse nested bundle input"));
    }

    #[test]
    fn duplicate_sources_get_distinct_collision_free_destinations() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("engine"), b"engine").unwrap();
        fs::write(root.path().join("guest"), b"guest").unwrap();
        let chain: Chain = serde_yaml::from_str(
            "id: duplicate\nlayers:\n  - artifact: { path: engine }\n    guest_isa: arm64\n  - artifact: { path: engine }\n    guest_isa: amd64\n  - artifact: { path: engine }\n    guest_isa: arm64\nguest: { path: guest }\nexpect: { exit: 0, stdout: out }\n",
        )
        .unwrap();
        let (bundle, arguments) = command(root.path(), &chain).unwrap();
        assert_eq!(fs::read(bundle.path().join("layer-0")).unwrap(), b"engine");
        assert_eq!(fs::read(bundle.path().join("layer-1")).unwrap(), b"engine");
        assert_eq!(fs::read(bundle.path().join("layer-2")).unwrap(), b"engine");
        assert_eq!(arguments[8], "/layer-2");
        assert_eq!(arguments[12], "/guest");
    }

    #[test]
    fn failed_partial_bundle_is_removed_by_raii() {
        let root = tempfile::tempdir().unwrap();
        let parent = tempfile::tempdir().unwrap();
        fs::write(root.path().join("outer"), b"outer").unwrap();
        fs::write(root.path().join("guest"), b"guest").unwrap();
        let chain: Chain = serde_yaml::from_str(
            "id: partial\nlayers:\n  - artifact: { path: outer }\n    guest_isa: arm64\n  - artifact: { path: absent }\n    guest_isa: amd64\nguest: { path: guest }\nexpect: { exit: 0, stdout: out }\n",
        )
        .unwrap();
        assert!(Bundle::new_in(root.path(), &chain, Some(parent.path())).is_err());
        assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 0);
    }

    #[test]
    fn bundle_child_exists_until_owner_is_dropped() {
        let root = tempfile::tempdir().unwrap();
        let parent = tempfile::tempdir().unwrap();
        for name in ["outer", "inner", "guest"] {
            fs::write(root.path().join(name), name).unwrap();
        }
        let chain: Chain = serde_yaml::from_str(
            "id: lifetime\nlayers:\n  - artifact: { path: outer }\n    guest_isa: arm64\n  - artifact: { path: inner }\n    guest_isa: amd64\nguest: { path: guest }\nexpect: { exit: 0, stdout: out }\n",
        )
        .unwrap();
        let bundle = Bundle::new_in(root.path(), &chain, Some(parent.path())).unwrap();
        let child = bundle.path().to_owned();
        assert_eq!(child.parent(), Some(parent.path()));
        assert!(child.join("layer-0").is_file());
        assert!(child.join("layer-1").is_file());
        assert!(child.join("guest").is_file());
        assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 1);
        drop(bundle);
        assert!(!child.exists());
        assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 0);
    }

    #[test]
    fn manifest_accepts_sixteen_layers_and_rejects_seventeen() {
        let root = tempfile::tempdir().unwrap();
        let definition = root.path().join("chains.yaml");
        let document = |count| {
            let layers = (0..count)
                .map(|index| format!("      - artifact: {{ path: layer-{index} }}\n        guest_isa: arm64\n"))
                .collect::<String>();
            format!(
                "version: 1\nchains:\n  - id: boundary\n    layers:\n{layers}    guest: {{ path: guest }}\n    expect: {{ exit: 42, stdout: hi.txt }}\n"
            )
        };
        fs::write(&definition, document(MAXIMUM_LAYERS)).unwrap();
        assert_eq!(load(root.path(), &definition).unwrap().chains[0].layers.len(), 16);
        fs::write(&definition, document(MAXIMUM_LAYERS + 1)).unwrap();
        let error = match load(root.path(), &definition) {
            Ok(_) => panic!("seventeen layers were accepted"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("invalid nested chain"));
    }

    #[test]
    fn missing_foreign_artifact_is_explicitly_unsupported() {
        let artifact: Artifact = serde_yaml::from_str(
            "path: missing\nsource: foreign-build\nbuild: { package: hl-engine, target: x86_64-unknown-linux-musl, binary: hl-engine }\n",
        )
        .unwrap();
        assert!(matches!(
            unavailable(Path::new("/definitely-absent"), &artifact),
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
        let initial = build_key_with_environment(root.path(), &build, "cargo", &environment).unwrap();
        assert_eq!(
            initial,
            build_key_with_environment(root.path(), &build, "cargo", &environment).unwrap()
        );
        fs::write(root.path().join("src/lib.rs"), "pub fn second() {}\n").unwrap();
        assert_ne!(
            initial,
            build_key_with_environment(root.path(), &build, "cargo", &environment).unwrap()
        );
        let changed: Build =
            serde_yaml::from_str("package: hl-engine\ntarget: x86_64-unknown-linux-musl\nbinary: hl-engine\n").unwrap();
        assert_ne!(
            initial,
            build_key_with_environment(root.path(), &changed, "cargo", &environment).unwrap()
        );
        let changed_environment = vec![("RUSTFLAGS".into(), "-Ctarget-cpu=native".into())];
        assert_ne!(
            initial,
            build_key_with_environment(root.path(), &build, "cargo", &changed_environment).unwrap()
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
