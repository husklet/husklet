use clap::Args;
use hl_process::{Capture, Command as ProcessCommand, Outcome as ProcessOutcome};
use serde::Deserialize;
use std::{
    collections::BTreeSet,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Component, Path, PathBuf},
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
    build: Option<String>,
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
    /// Nested-chain manifest relative to the workspace root.
    manifest: Option<PathBuf>,
}

pub fn run(options: Options) -> Result<(), Error> {
    let root = crate::runtime::workspace()?;
    let definition = options
        .manifest
        .map_or_else(|| root.join("tests/runtime/nested/chains.yaml"), |path| root.join(path));
    let document = load(&root, &definition)?;
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
            || chain.layers.len() < 2
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
        || matches!(artifact.source, ArtifactSource::ForeignBuild)
            && artifact.build.as_deref().is_none_or(str::is_empty)
    {
        return Err(format!(
            "artifact {} has no usable path/build instruction",
            artifact.path.display()
        )
        .into());
    }
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

fn command(root: &Path, chain: &Chain) -> Vec<String> {
    let mut arguments = Vec::new();
    for layer in &chain.layers {
        arguments.push(root.join(&layer.artifact.path).display().to_string());
        arguments.extend(["--guest-isa".into(), layer.guest_isa.engine_name().into()]);
        layer.options.append(&mut arguments);
    }
    arguments.push(root.join(&chain.guest.path).display().to_string());
    arguments.extend(chain.arguments.iter().cloned());
    arguments
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
            "foreign artifact {} is absent or not executable; build with: {}",
            path.display(),
            artifact.build.as_deref().unwrap_or("<missing build instruction>")
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
    let arguments = command(root, chain);
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

    #[test]
    fn typed_options_are_attached_to_the_layer_they_configure() {
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
        let arguments = command(Path::new("/tree"), &chain);
        assert_eq!(
            &arguments[..7],
            [
                "/tree/outer",
                "--guest-isa",
                "aarch64",
                "--engine-option",
                "HL_NATIVE_EXECUTION=1",
                "--engine-option",
                "HL_NATIVE_DIAGNOSTICS=1"
            ]
        );
        assert_eq!(&arguments[7..], ["/tree/inner", "--guest-isa", "x86_64", "/tree/hello"]);
    }

    #[test]
    fn missing_foreign_artifact_is_explicitly_unsupported() {
        let artifact: Artifact =
            serde_yaml::from_str("path: missing\nsource: foreign-build\nbuild: make foreign\n").unwrap();
        assert!(matches!(
            unavailable(Path::new("/definitely-absent"), &artifact),
            Some(Outcome::Unsupported(_))
        ));
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
