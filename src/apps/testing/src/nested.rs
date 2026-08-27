use crate::suite::SafePath as _;
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs,
    os::unix::{ffi::OsStrExt, fs::PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};

mod adapter;

use adapter::{ProcessOutput, build_artifact, environment, hash_tool, materialize};

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
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Artifact {
    /// Bundle root. Prepared artifacts are materialized below `bin/`, with an
    /// engine's exact native library below `lib/`.
    path: PathBuf,
    #[serde(default)]
    source: ArtifactSource,
    build: Option<Build>,
}

#[derive(Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum Build {
    Engine(EngineBuild),
    C(CBuild),
}

impl Build {
    fn executable(&self) -> &str {
        match self {
            Self::Engine(build) => &build.binary,
            Self::C(build) => &build.output,
        }
    }

    const fn isa(&self) -> GuestIsa {
        match self {
            Self::Engine(build) => build.isa,
            Self::C(build) => build.isa,
        }
    }

    const fn is_engine(&self) -> bool {
        matches!(self, Self::Engine(_))
    }

    fn validate(&self) -> Result<(), Error> {
        match self {
            Self::Engine(build) => build.validate(),
            Self::C(build) => build.validate(),
        }
    }

    fn environment(&self) -> Vec<(String, String)> {
        match self {
            Self::Engine(build) => build.environment(),
            Self::C(build) => vec![(
                build.isa.compiler_variable().to_owned(),
                environment(build.isa.compiler_variable()).unwrap_or_default(),
            )],
        }
    }
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct EngineBuild {
    package: String,
    target: String,
    isa: GuestIsa,
    #[serde(default = "release_profile")]
    profile: String,
    binary: String,
    #[serde(default)]
    rustflags: Vec<String>,
}

impl EngineBuild {
    fn validate(&self) -> Result<(), Error> {
        if self.package.is_empty()
            || self.binary.is_empty()
            || self.target.is_empty()
            || self.profile.is_empty()
            || invalid_segment(&self.binary)
            || self.target != self.isa.rust_target()
        {
            return Err("nested engine build contains an invalid package, target, ISA, profile, or binary".into());
        }
        Ok(())
    }

    fn environment(&self) -> Vec<(String, String)> {
        let linker = format!(
            "CARGO_TARGET_{}_LINKER",
            self.target.to_ascii_uppercase().replace('-', "_")
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
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct CBuild {
    source: PathBuf,
    output: String,
    isa: GuestIsa,
    #[serde(default)]
    flags: Vec<String>,
}

impl CBuild {
    fn validate(&self) -> Result<(), Error> {
        self.source.safe_relative()?;
        if invalid_segment(&self.output) {
            return Err("nested C build contains an invalid output name".into());
        }
        Ok(())
    }
}

fn invalid_segment(value: &str) -> bool {
    value.is_empty()
        || value
            .chars()
            .any(|character| !(character.is_ascii_alphanumeric() || matches!(character, '-' | '_')))
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ArtifactSource {
    #[default]
    Local,
    ForeignBuild,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
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

    const fn rust_target(self) -> &'static str {
        match self {
            Self::Arm64 => "aarch64-unknown-linux-gnu",
            Self::Amd64 => "x86_64-unknown-linux-gnu",
        }
    }

    const fn elf_machine(self) -> u16 {
        match self {
            Self::Arm64 => 183,
            Self::Amd64 => 62,
        }
    }

    const fn compiler(self) -> &'static str {
        match self {
            Self::Arm64 => "aarch64-linux-gnu-gcc",
            Self::Amd64 => "x86_64-linux-gnu-gcc",
        }
    }

    const fn compiler_variable(self) -> &'static str {
        match self {
            Self::Arm64 => "AARCH64_LINUX_STATIC_CC",
            Self::Amd64 => "X86_64_LINUX_STATIC_CC",
        }
    }

    fn compiler_command(self) -> Result<Vec<String>, Error> {
        let command = environment(self.compiler_variable())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| self.compiler().into());
        let parsed = shlex::split(&command).ok_or("nested C compiler command has invalid quoting")?;
        if parsed.is_empty() {
            return Err("nested C compiler command is empty".into());
        }
        Ok(parsed)
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BundleManifest {
    schema: String,
    isa: GuestIsa,
    executable: BundleMember,
    #[serde(skip_serializing_if = "Option::is_none")]
    library: Option<BundleMember>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BundleMember {
    path: PathBuf,
    sha256: String,
    bytes: u64,
    elf_machine: u16,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoaderReceipt {
    schema: String,
    library_sha256: String,
    library_path: PathBuf,
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

struct BuildIdentity {
    key: String,
    cargo: String,
}

fn hash_source_named(digest: &mut crate::record::FramedIdentity, name: &[u8], path: &Path) -> Result<(), Error> {
    digest.field(name)?;
    digest.field(path.as_os_str().as_encoded_bytes())?;
    digest.field(&fs::read(path)?)
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
        let mut digest = crate::record::FramedIdentity::new(b"husklet-nested-build-v3")?;
        match build {
            Build::Engine(engine) => {
                for value in [
                    &engine.package,
                    &engine.target,
                    &engine.profile,
                    &engine.binary,
                    engine.isa.engine_name(),
                ] {
                    digest.field(value.as_bytes())?;
                }
                for value in &engine.rustflags {
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
                hash_tool(&mut digest, "cargo", cargo, &["-V"])?;
                hash_tool(&mut digest, "rustc", &rustc, &["-vV"])?;
                hash_tool(
                    &mut digest,
                    "rustc-target",
                    &rustc,
                    &["--print", "target-libdir", "--target", &engine.target],
                )?;
                self.hash_tree(&mut digest, &self.root.join("src"))?;
            }
            Build::C(c) => {
                digest.field(c.isa.engine_name().as_bytes())?;
                digest.field(c.output.as_bytes())?;
                for value in &c.flags {
                    digest.field(value.as_bytes())?;
                }
                self.hash_source(&mut digest, &self.root.join(&c.source))?;
                let compiler = c.isa.compiler_command()?;
                let (program, prefix) = compiler.split_first().ok_or("nested C compiler command is empty")?;
                let mut arguments = prefix.iter().map(String::as_str).collect::<Vec<_>>();
                arguments.push("--version");
                hash_tool(&mut digest, "c-compiler", program, &arguments)?;
            }
        }
        for (name, value) in values {
            digest.field(name.as_bytes())?;
            digest.field(value.as_bytes())?;
        }
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
                if !matches!(layer.artifact.build.as_ref(), Some(Build::Engine(_))) {
                    return Err(format!(
                        "nested layer {} is not a typed engine bundle",
                        layer.artifact.path.display()
                    )
                    .into());
                }
            }
        }
        Ok(document)
    }

    fn validate_artifact(&self, artifact: &Artifact) -> Result<(), Error> {
        artifact.path.safe_relative()?;
        if let Some(build) = &artifact.build {
            build.validate()?;
        }
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
        build.validate()?;
        let identity = self.build_identity(build)?;
        let key = &identity.key;
        let cache = crate::record::Cache::new(&self.root)?;
        let receipts = cache.receipts();
        let record = receipts.artifact(key, "bundle.tar")?;
        let _lock = receipts.lock(key)?;
        if record.verify()? {
            println!("REUSED {} key={key}", artifact.path.display());
        } else {
            build_artifact(&self.root, build, &identity.cargo, &record)?;
            println!("BUILT {} key={key}", artifact.path.display());
        }
        materialize(record.artifact(), &self.root.join(&artifact.path))?;
        self.verify_bundle(artifact)?;
        Ok(())
    }

    fn build_identity(&self, build: &Build) -> Result<BuildIdentity, Error> {
        let cargo = environment("CARGO").unwrap_or_else(|| "cargo".into());
        let values = build.environment();
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

    fn command(&self, chain: &Chain) -> Result<Vec<String>, Error> {
        let mut arguments = vec![self.executable(&chain.layers[0].artifact)?.display().to_string()];
        for (index, layer) in chain.layers.iter().enumerate() {
            let manifest = self.verify_bundle(&layer.artifact)?;
            let library = manifest
                .library
                .as_ref()
                .ok_or("nested engine bundle omitted its library")?;
            arguments.push("--report-exit".into());
            arguments.push("--loader-receipt".into());
            arguments.extend([
                "--native-library".into(),
                self.root
                    .join(&layer.artifact.path)
                    .join(&library.path)
                    .display()
                    .to_string(),
            ]);
            arguments.extend(["--guest-isa".into(), layer.guest_isa.engine_name().into()]);
            arguments.push("--".into());
            let next = chain.layers.get(index + 1).map_or(&chain.guest, |next| &next.artifact);
            arguments.push(self.executable(next)?.display().to_string());
        }
        arguments.extend(chain.arguments.iter().cloned());
        Ok(arguments)
    }

    fn unavailable(&self, artifact: &Artifact) -> Option<Outcome> {
        let path = match self.executable(artifact) {
            Ok(path) => path,
            Err(error) => return Some(Outcome::Failed(error.to_string())),
        };
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
        let arguments = match self.command(chain) {
            Ok(arguments) => arguments,
            Err(error) => return Outcome::Failed(error.to_string()),
        };
        match ProcessOutput::capture(
            &arguments,
            Duration::from_secs(chain.timeout_seconds),
            chain.capture_limit_bytes,
        ) {
            Ok(captured) if captured.status == Some(chain.expect.exit) && captured.stdout == expected => {
                match self.verify_loader_receipts(chain, &captured.stderr) {
                    Ok(()) => Outcome::Passed,
                    Err(error) => Outcome::Failed(error.to_string()),
                }
            }
            Ok(captured) => Outcome::Failed(format!(
                "exit={status:?} expected={}; stdout={} bytes expected={} bytes; stderr={}",
                chain.expect.exit,
                captured.stdout.len(),
                expected.len(),
                String::from_utf8_lossy(&captured.stderr).trim(),
                status = captured.status,
            )),
            Err(error) => Outcome::Failed(error),
        }
    }

    fn executable(&self, artifact: &Artifact) -> Result<PathBuf, Error> {
        Ok(match &artifact.build {
            Some(build) => self.root.join(&artifact.path).join("bin").join(build.executable()),
            None => self.root.join(&artifact.path),
        })
    }

    fn verify_bundle(&self, artifact: &Artifact) -> Result<BundleManifest, Error> {
        let build = artifact
            .build
            .as_ref()
            .ok_or("local artifact is not a prepared bundle")?;
        let root = self.root.join(&artifact.path);
        let manifest: BundleManifest = serde_yaml::from_str(&fs::read_to_string(root.join("manifest.yaml"))?)?;
        if manifest.schema != "husklet-nested-artifact-v1"
            || manifest.isa != build.isa()
            || manifest.executable.path != Path::new("bin").join(build.executable())
            || manifest.executable.elf_machine != build.isa().elf_machine()
            || manifest.library.is_some() != build.is_engine()
        {
            return Err(format!("nested bundle {} has an invalid manifest contract", root.display()).into());
        }
        self.verify_member(&root, &manifest.executable)?;
        if let Some(library) = &manifest.library {
            if library.path != Path::new("lib").join("libhl_native_engine.so")
                || library.elf_machine != build.isa().elf_machine()
            {
                return Err(format!("nested bundle {} has an invalid library contract", root.display()).into());
            }
            self.verify_member(&root, library)?;
        }
        Ok(manifest)
    }

    fn verify_member(&self, root: &Path, member: &BundleMember) -> Result<(), Error> {
        member.path.safe_relative()?;
        let path = root.join(&member.path);
        let bytes = fs::read(&path)?;
        let machine = elf_machine(&bytes)?;
        if u64::try_from(bytes.len())? != member.bytes
            || crate::record::FramedIdentity::of(&bytes) != member.sha256
            || machine != member.elf_machine
        {
            return Err(format!("nested bundle member {} does not match its manifest", path.display()).into());
        }
        Ok(())
    }

    fn verify_loader_receipts(&self, chain: &Chain, stderr: &[u8]) -> Result<(), Error> {
        let receipts = String::from_utf8_lossy(stderr)
            .lines()
            .filter_map(|line| line.strip_prefix("[hl-loader]\t"))
            .map(serde_json::from_str::<LoaderReceipt>)
            .collect::<Result<Vec<_>, _>>()?;
        if receipts.len() != chain.layers.len() {
            return Err(format!(
                "nested chain emitted {} loader receipts for {} layers",
                receipts.len(),
                chain.layers.len()
            )
            .into());
        }
        for (layer, receipt) in chain.layers.iter().zip(receipts) {
            let manifest = self.verify_bundle(&layer.artifact)?;
            let library = manifest
                .library
                .as_ref()
                .ok_or("nested engine bundle omitted its library")?;
            let root = self.root.join(&layer.artifact.path);
            let expected_library = fs::canonicalize(root.join(&library.path))?;
            if receipt.schema != "husklet-engine-loader-v1"
                || receipt.library_sha256 != library.sha256
                || fs::canonicalize(&receipt.library_path)? != expected_library
            {
                return Err(format!(
                    "nested layer {} did not load its manifest-bound library",
                    layer.artifact.path.display()
                )
                .into());
            }
        }
        Ok(())
    }
}

fn elf_machine(bytes: &[u8]) -> Result<u16, Error> {
    if bytes.len() < 20 || &bytes[..4] != b"\x7fELF" || bytes[4] != 2 || bytes[5] != 1 {
        return Err("nested artifact is not a little-endian ELF64 image".into());
    }
    Ok(u16::from_le_bytes([bytes[18], bytes[19]]))
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

    fn engine_artifact(path: &str) -> Artifact {
        serde_yaml::from_str(&format!(
            "path: {path}\nbuild: {{ kind: engine, package: engine, target: x86_64-unknown-linux-gnu, isa: amd64, binary: hl-engine }}\n"
        ))
        .unwrap()
    }

    fn fake_elf(machine: u16, marker: u8) -> Vec<u8> {
        let mut bytes = vec![marker; 64];
        bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        bytes[16..18].copy_from_slice(&3_u16.to_le_bytes());
        bytes[18..20].copy_from_slice(&machine.to_le_bytes());
        bytes
    }

    fn write_engine_bundle_as(root: &Path, artifact: &Artifact, isa: GuestIsa, machine: u16) -> (PathBuf, PathBuf) {
        let bundle = root.join(&artifact.path);
        let executable_path = PathBuf::from("bin/hl-engine");
        let library_path = PathBuf::from("lib/libhl_native_engine.so");
        let executable = fake_elf(machine, 0x11);
        let library = fake_elf(machine, 0x22);
        fs::create_dir_all(bundle.join("bin")).unwrap();
        fs::create_dir_all(bundle.join("lib")).unwrap();
        fs::write(bundle.join(&executable_path), &executable).unwrap();
        fs::write(bundle.join(&library_path), &library).unwrap();
        let manifest = BundleManifest {
            schema: "husklet-nested-artifact-v1".into(),
            isa,
            executable: BundleMember {
                path: executable_path.clone(),
                sha256: crate::record::FramedIdentity::of(&executable),
                bytes: executable.len() as u64,
                elf_machine: machine,
            },
            library: Some(BundleMember {
                path: library_path.clone(),
                sha256: crate::record::FramedIdentity::of(&library),
                bytes: library.len() as u64,
                elf_machine: machine,
            }),
        };
        fs::write(bundle.join("manifest.yaml"), serde_yaml::to_string(&manifest).unwrap()).unwrap();
        (bundle.join(executable_path), bundle.join(library_path))
    }

    fn write_engine_bundle(root: &Path, artifact: &Artifact) -> (PathBuf, PathBuf) {
        write_engine_bundle_as(root, artifact, GuestIsa::Amd64, 62)
    }

    #[test]
    fn layer_isa_selection_is_attached_to_the_layer_it_configures() {
        let mut chain: Chain = serde_yaml::from_str(
            r"
id: arm-amd
layers:
  - artifact: { path: outer }
    guest_isa: arm64
  - artifact: { path: inner }
    guest_isa: amd64
guest: { path: hello }
expect: { exit: 42, stdout: hello.txt }
",
        )
        .unwrap();
        let root = tempfile::tempdir().unwrap();
        chain.layers[0].artifact = engine_artifact("outer");
        chain.layers[1].artifact = engine_artifact("inner");
        write_engine_bundle(root.path(), &chain.layers[0].artifact);
        write_engine_bundle(root.path(), &chain.layers[1].artifact);
        let arguments = Workspace {
            root: root.path().to_path_buf(),
        }
        .command(&chain)
        .unwrap();
        let outer = root.path().join("outer");
        let inner = root.path().join("inner");
        assert_eq!(
            arguments,
            vec![
                outer.join("bin/hl-engine").display().to_string(),
                "--report-exit".into(),
                "--loader-receipt".into(),
                "--native-library".into(),
                outer.join("lib/libhl_native_engine.so").display().to_string(),
                "--guest-isa".into(),
                "aarch64".into(),
                "--".into(),
                inner.join("bin/hl-engine").display().to_string(),
                "--report-exit".into(),
                "--loader-receipt".into(),
                "--native-library".into(),
                inner.join("lib/libhl_native_engine.so").display().to_string(),
                "--guest-isa".into(),
                "x86_64".into(),
                "--".into(),
                root.path().join("hello").display().to_string(),
            ]
        );
    }

    #[test]
    fn missing_foreign_artifact_is_explicitly_unsupported() {
        let artifact: Artifact = serde_yaml::from_str(
            "path: missing\nsource: foreign-build\nbuild: { kind: engine, package: hl-engine, target: x86_64-unknown-linux-gnu, isa: amd64, binary: hl-engine }\n",
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
    fn engine_bundle_rejects_a_missing_native_library() {
        let root = tempfile::tempdir().unwrap();
        let artifact = engine_artifact("bundle");
        let (_, library) = write_engine_bundle(root.path(), &artifact);
        fs::remove_file(library).unwrap();
        assert!(
            Workspace {
                root: root.path().to_path_buf()
            }
            .verify_bundle(&artifact)
            .is_err()
        );
    }

    #[test]
    fn engine_bundle_rejects_a_swizzled_native_library() {
        let root = tempfile::tempdir().unwrap();
        let artifact = engine_artifact("bundle");
        let (_, library) = write_engine_bundle(root.path(), &artifact);
        fs::write(library, fake_elf(62, 0x33)).unwrap();
        assert!(
            Workspace {
                root: root.path().to_path_buf()
            }
            .verify_bundle(&artifact)
            .is_err()
        );
    }

    #[test]
    fn engine_bundle_rejects_the_wrong_elf_isa() {
        let root = tempfile::tempdir().unwrap();
        let artifact = engine_artifact("bundle");
        write_engine_bundle_as(root.path(), &artifact, GuestIsa::Arm64, 183);
        assert!(
            Workspace {
                root: root.path().to_path_buf()
            }
            .verify_bundle(&artifact)
            .is_err()
        );
    }

    #[test]
    fn loader_receipts_bind_hash_and_path_for_every_layer() {
        let root = tempfile::tempdir().unwrap();
        let outer = engine_artifact("outer");
        let inner = engine_artifact("inner");
        let (_, outer_library) = write_engine_bundle(root.path(), &outer);
        let (_, inner_library) = write_engine_bundle(root.path(), &inner);
        let chain = Chain {
            id: "receipt".into(),
            layers: vec![
                Layer {
                    artifact: outer,
                    guest_isa: GuestIsa::Amd64,
                },
                Layer {
                    artifact: inner,
                    guest_isa: GuestIsa::Amd64,
                },
            ],
            guest: Artifact {
                path: PathBuf::from("hello"),
                source: ArtifactSource::Local,
                build: None,
            },
            arguments: Vec::new(),
            timeout_seconds: 1,
            capture_limit_bytes: 1024,
            expect: Expectation {
                exit: 0,
                stdout: PathBuf::from("hello.txt"),
            },
        };
        let sha256 = crate::record::FramedIdentity::of(&fs::read(&outer_library).unwrap());
        let receipt = |path: &Path, digest: &str| {
            format!(
                "[hl-loader]\t{}\n",
                serde_json::json!({
                    "schema": "husklet-engine-loader-v1",
                    "library_sha256": digest,
                    "library_path": fs::canonicalize(path).unwrap(),
                })
            )
        };
        let workspace = Workspace {
            root: root.path().to_path_buf(),
        };
        let valid = format!(
            "{}{}",
            receipt(&outer_library, &sha256),
            receipt(&inner_library, &sha256)
        );
        workspace.verify_loader_receipts(&chain, valid.as_bytes()).unwrap();

        let missing = receipt(&outer_library, &sha256);
        assert!(workspace.verify_loader_receipts(&chain, missing.as_bytes()).is_err());

        let wrong_path = format!(
            "{}{}",
            receipt(&outer_library, &sha256),
            receipt(&outer_library, &sha256)
        );
        assert!(workspace.verify_loader_receipts(&chain, wrong_path.as_bytes()).is_err());

        let wrong_hash = format!(
            "{}{}",
            receipt(&outer_library, &sha256),
            receipt(&inner_library, &"0".repeat(64))
        );
        assert!(workspace.verify_loader_receipts(&chain, wrong_hash.as_bytes()).is_err());
    }

    #[test]
    fn build_key_binds_source_and_typed_recipe() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("src")).unwrap();
        fs::write(root.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        fs::write(root.path().join("Cargo.lock"), "version = 4\n").unwrap();
        fs::write(root.path().join("src/lib.rs"), "pub fn first() {}\n").unwrap();
        let build: Build = serde_yaml::from_str(
            "kind: engine\npackage: hl-engine\ntarget: aarch64-unknown-linux-gnu\nisa: arm64\nbinary: hl-engine\n",
        )
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
        let changed: Build = serde_yaml::from_str(
            "kind: engine\npackage: hl-engine\ntarget: x86_64-unknown-linux-gnu\nisa: amd64\nbinary: hl-engine\n",
        )
        .unwrap();
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
        let receipts = cache.receipts();
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
        let receipts = cache.receipts();
        let key = "a".repeat(64);
        let first = receipts.lock(&key).unwrap();
        let thread = std::thread::spawn(move || {
            let cache = crate::record::Cache::new(root.path()).unwrap();
            let receipts = cache.receipts();
            receipts.lock(&key).unwrap()
        });
        drop(first);
        drop(thread.join().unwrap());
    }

    #[test]
    fn capture_success() {
        let captured = ProcessOutput::capture(
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
        let error = ProcessOutput::capture(
            &["/bin/sh".into(), "-c".into(), "sleep 2".into()],
            Duration::from_millis(20),
            1024,
        )
        .unwrap_err();
        assert!(error.contains("timed out"));
    }

    #[test]
    fn capture_limit() {
        let error = ProcessOutput::capture(
            &["/bin/sh".into(), "-c".into(), "printf 12345".into()],
            Duration::from_secs(2),
            4,
        )
        .unwrap_err();
        assert_eq!(error, "output exceeded 4 bytes");
    }
}
