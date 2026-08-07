use crate::suite::SafePath as _;
use super::{scheduler, workspace};
mod environment;
mod host;
pub(super) mod input;
mod manifest;
use crate::suite::{Commands, Error, Execution, Target};
pub(crate) use environment::EnvironmentEntry;
pub(crate) use host::EngineHost;
use host::HostExclusion;
use input::ManifestPath;
use manifest::{CompatClass, Document, Oracle, Status};
use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path, PathBuf},
    process::Command as StdCommand,
    sync::atomic::AtomicBool,
    time::Duration,
};

const ORACLE_CAPTURE_LIMIT: u64 = 1024 * 1024;

pub struct RuntimeCase {
    pub id: String,
    pub arguments: Vec<String>,
    pub environment: Vec<EnvironmentEntry>,
    pub timeout: u64,
    pub exit: i32,
    pub golden: PathBuf,
    pub destination: String,
    pub(in crate::runtime) source: ManifestPath,
    pub(crate) output: String,
    pub(crate) flags: Vec<String>,
    pub(in crate::runtime) inputs: Vec<ManifestPath>,
    status: Status,
    pub(crate) compat: CompatClass,
    pub soak: Option<scheduler::Plan>,
    pub targets: BTreeSet<Target>,
}

pub struct App {
    pub name: String,
    pub directory: PathBuf,
    pub image: String,
    pub execution: Execution,
    pub targets: BTreeSet<Target>,
    compiler: Commands,
    oracle: Option<Oracle>,
    pub cases: Vec<RuntimeCase>,
}

/// A freshly compiled guest; its directory is removed when the build is dropped.
pub struct GuestBuild {
    _directory: tempfile::TempDir,
    path: PathBuf,
}

impl GuestBuild {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl RuntimeCase {
    pub(crate) fn inactive(&self, host: EngineHost) -> Option<(&'static str, &str, &str)> {
        self.status.inactive(host)
    }
}

impl App {
    pub fn supports(&self, target: Target) -> bool {
        self.targets.contains(&target)
    }

    pub(crate) fn compiler_name(&self, target: Target) -> &str {
        self.compiler.for_target(target)
    }

    pub fn cases_for(&self, target: Target) -> impl Iterator<Item = &RuntimeCase> {
        self.cases.iter().filter(move |case| case.targets.contains(&target))
    }

    pub fn load(directory: &Path, definition: &Path) -> Result<Self, Error> {
        let document: Document = serde_yaml::from_str(&fs::read_to_string(definition)?)?;
        if document.image.trim().is_empty() || document.targets.is_empty() || document.cases.is_empty() {
            return Err(format!("{} has an invalid image or case list", definition.display()).into());
        }
        document.execution.container()?;
        let mut ids = BTreeSet::new();
        let mut outputs = BTreeSet::new();
        let mut destinations = BTreeSet::new();
        let cases = document
            .cases
            .into_iter()
            .map(|case| {
                if !ids.insert(case.id.clone())
                    || !case.id.starts_with("runtime/")
                    || !(1..=3600).contains(&case.timeout)
                {
                    return Err(format!("{} has invalid case {:?}", definition.display(), case.id).into());
                }
                case.status.validate()?;
                let targets = if case.targets.is_empty() {
                    document.targets.clone()
                } else {
                    case.targets
                };
                if targets.is_empty() || !targets.is_subset(&document.targets) {
                    return Err(format!("{} has invalid targets for {:?}", definition.display(), case.id).into());
                }
                if let Some(plan) = &case.soak {
                    plan.validate()?;
                }
                if matches!(case.compat.class, CompatClass::Soak) != case.soak.is_some() {
                    return Err(format!(
                        "{} has inconsistent soak metadata for {:?}",
                        definition.display(),
                        case.id
                    )
                    .into());
                }
                let (source, output, flags, inputs, destination) =
                    document
                        .build
                        .resolve(case.build, case.artifact, document.artifact.as_ref())?;
                let inputs = input::validate(directory, &source, inputs)?;
                safe_output(&output)?;
                std::path::Path::new(&destination).safe_absolute()?;
                if !outputs.insert(output.clone()) || !destinations.insert(destination.clone()) {
                    return Err(format!("{} has duplicate case output or destination", definition.display()).into());
                }
                let golden = validate_golden(directory, &case.expect.stdout)?;
                Ok(RuntimeCase {
                    id: case.id,
                    arguments: case.run,
                    environment: case.environment,
                    timeout: case.timeout,
                    exit: case.expect.exit,
                    golden,
                    destination,
                    source,
                    output,
                    flags,
                    inputs,
                    status: case.status,
                    compat: case.compat.class,
                    soak: case.soak,
                    targets,
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;
        Ok(Self {
            name: directory
                .file_name()
                .and_then(|value| value.to_str())
                .ok_or("runtime app name is not UTF-8")?
                .to_owned(),
            directory: directory.to_path_buf(),
            image: document.image,
            execution: document.execution,
            targets: document.targets,
            compiler: document.build.compiler,
            oracle: document.oracle,
            cases,
        })
    }

    /// Each build owns a private directory, so concurrent runners of one case never share an artifact.
    pub fn build(&self, case: &RuntimeCase, target: Target) -> Result<GuestBuild, Error> {
        let root = workspace()?
            .join("target/testing/runtime")
            .join(&self.name)
            .join(target.name());
        fs::create_dir_all(&root).map_err(|error| format!("create build directory {}: {error}", root.display()))?;
        let directory = tempfile::Builder::new()
            .prefix("build-")
            .tempdir_in(&root)
            .map_err(|error| format!("create build directory in {}: {error}", root.display()))?;
        let output = directory.path().join(&case.output);
        let compiler = self.compiler.for_target(target);
        let source = self.directory.join(case.source.native());
        let status = StdCommand::new(compiler)
            .arg("-o")
            .arg(&output)
            .arg(&source)
            .args(&case.flags)
            .status()
            .map_err(|error| format!("run {compiler} on {}: {error}", source.display()))?;
        if !status.success() {
            return Err(format!("{compiler} failed with {status}").into());
        }
        Ok(GuestBuild {
            _directory: directory,
            path: output,
        })
    }

    pub fn oracle(&self, target: Target, update: bool, selected: Option<&str>) -> Result<(), Error> {
        let commands = self
            .oracle
            .as_ref()
            .ok_or_else(|| format!("{} defines no oracle", self.name))?;
        for case in self
            .cases_for(target)
            .filter(|case| selected.is_none_or(|id| case.id == id))
        {
            if let Some((kind, reason, evidence)) = case.status.oracle_inactive() {
                println!("{kind} {} {}: {reason} [{evidence}]", case.id, target.name());
                continue;
            }
            let artifact = self.build(case, target)?;
            let directory = tempfile::tempdir()?;
            let capture = hl_process::Capture {
                stdout: directory.path().join("stdout"),
                stderr: directory.path().join("stderr"),
                stdout_limit: ORACLE_CAPTURE_LIMIT,
                stderr_limit: ORACLE_CAPTURE_LIMIT,
            };
            let environment = case
                .environment
                .iter()
                .map(|entry| hl_process::EnvironmentEntry::new(entry.name(), entry.value()))
                .collect::<std::io::Result<Vec<_>>>()?;
            let mut command = hl_process::Command::new(self.command(commands, target));
            command.exact_environment(environment)?;
            command.arg(artifact.path()).args(&case.arguments);
            let outcome = hl_process::run(
                &command,
                &capture,
                Duration::from_secs(case.timeout),
                &AtomicBool::new(false),
            )?;
            let stdout = fs::read(&capture.stdout)?;
            let stderr = fs::read(&capture.stderr)?;
            let status = match outcome {
                hl_process::Outcome::Exited(Some(status)) => status,
                hl_process::Outcome::Exited(None) => return Err("oracle exited without a status".into()),
                hl_process::Outcome::Signaled(signal) => {
                    return Err(format!("oracle terminated by signal {signal}; stderr={stderr:?}").into());
                }
                hl_process::Outcome::TimedOut => {
                    return Err(format!("oracle timed out after {} seconds; stderr={stderr:?}", case.timeout).into());
                }
                hl_process::Outcome::Cancelled => return Err("oracle was cancelled".into()),
                hl_process::Outcome::OutputLimit => {
                    return Err(format!("oracle exceeded the 1 MiB capture limit; stderr={stderr:?}").into());
                }
            };
            if status != case.exit {
                return Err(format!(
                    "oracle {} {} exited {status}, expected {}; stderr={stderr:?}",
                    case.id,
                    target.name(),
                    case.exit
                )
                .into());
            }
            if update {
                let temporary = case.golden.with_extension("tmp");
                fs::write(&temporary, &stdout)?;
                fs::rename(temporary, &case.golden)?;
            } else if fs::read(&case.golden)? != stdout {
                return Err(format!("oracle output differs for {} {}", case.id, target.name()).into());
            }
            println!("ORACLE {} {}", case.id, target.name());
        }
        Ok(())
    }

    fn command<'a>(&self, oracle: &'a Oracle, target: Target) -> &'a str {
        let _provider = oracle.provider;
        oracle.commands.for_target(target)
    }
}

fn validate_golden(directory: &Path, relative: &Path) -> Result<PathBuf, Error> {
    relative.safe_relative()?;
    let golden = directory.join(relative);
    let canonical_directory = directory.canonicalize()?;
    let canonical_golden = golden
        .canonicalize()
        .map_err(|error| format!("golden {} is unavailable: {error}", golden.display()))?;
    if !canonical_golden.starts_with(&canonical_directory) || !canonical_golden.is_file() {
        return Err(format!(
            "golden {} must be a file contained by its runtime category",
            golden.display()
        )
        .into());
    }
    Ok(golden)
}

fn safe_output(output: &str) -> Result<(), Error> {
    let path = Path::new(output);
    if output.is_empty()
        || path.components().count() != 1
        || !matches!(path.components().next(), Some(Component::Normal(_)))
    {
        Err(format!("unsafe build output {output:?}").into())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{App, EngineHost, Execution};
    use std::{fs, path::Path};

    #[test]
    fn yaml_native_execution_maps_to_container_configuration() {
        let execution: Execution = serde_yaml::from_str("native: true\ndiagnostics: true\n").unwrap();
        let execution = execution.container().unwrap();
        assert!(execution.is_native());
        assert!(execution.diagnostics());
    }

    #[test]
    fn yaml_diagnostics_require_native_execution() {
        let execution: Execution = serde_yaml::from_str("diagnostics: true\n").unwrap();
        assert_eq!(
            execution.container().unwrap_err().to_string(),
            "native diagnostics require native execution"
        );
    }

    fn category(case_rows: &str) -> Result<App, super::Error> {
        let directory = tempfile::tempdir().unwrap();
        category_at(directory.path(), case_rows)
    }

    fn category_at(directory: &Path, case_rows: &str) -> Result<App, super::Error> {
        fs::write(directory.join("one.c"), "one").unwrap();
        fs::write(directory.join("two.c"), "two").unwrap();
        fs::create_dir_all(directory.join("golden")).unwrap();
        for name in ["one", "two", "unsafe", "default"] {
            fs::write(directory.join(format!("golden/{name}.out")), []).unwrap();
        }
        let definition = directory.join("test.yaml");
        fs::write(
            &definition,
            format!(
                "targets: [arm64, amd64]\nimage: alpine\nexecution: {{}}\nbuild:\n  compiler: {{ arm64: arm-cc, amd64: amd-cc }}\n  flags: []\ncases:\n{case_rows}"
            ),
        )
        .unwrap();
        App::load(directory, &definition)
    }

    #[test]
    fn category_cases_own_safe_unique_builds() {
        let app = category(
            "  - id: runtime/one\n    build: { source: one.c, output: one, flags: [-static, -lm] }\n    artifact: { destination: /opt/one }\n    status: active\n    compat: { class: compatibility }\n    run: []\n    expect: { exit: 0, stdout: golden/one.out }\n  - id: runtime/two\n    build: { source: two.c, output: two, flags: [] }\n    artifact: { destination: /opt/two }\n    status: active\n    compat: { class: compatibility }\n    run: []\n    expect: { exit: 0, stdout: golden/two.out }\n",
        )
        .unwrap();
        assert_eq!(app.cases.len(), 2);
        assert_eq!(app.cases[0].destination, "/opt/one");
        assert_eq!(app.cases[0].output, "one");
    }

    /// Two workers building one case must each get a private, complete artifact.
    #[cfg(unix)]
    #[test]
    fn concurrent_builds_of_one_case_do_not_share_an_artifact() {
        let directory = tempfile::tempdir().unwrap();
        let compiler = directory.path().join("slow-cc");
        fs::write(
            &compiler,
            "#!/bin/sh\nout=\nwhile [ $# -gt 0 ]; do\n  if [ \"$1\" = -o ]; then out=$2; shift; fi\n  shift\ndone\n: > \"$out\"\nsleep 1\nprintf 'GUEST-%s\\n' \"$$\" >> \"$out\"\n",
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt as _;

            fs::set_permissions(&compiler, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let rows = "  - id: runtime/one\n    build: { source: one.c, output: one, flags: [] }\n    artifact: { destination: /opt/one }\n    status: active\n    compat: { class: compatibility }\n    run: []\n    expect: { exit: 0, stdout: golden/one.out }\n";
        fs::write(directory.path().join("one.c"), "one").unwrap();
        fs::write(directory.path().join("two.c"), "two").unwrap();
        fs::create_dir_all(directory.path().join("golden")).unwrap();
        fs::write(directory.path().join("golden/one.out"), []).unwrap();
        let definition = directory.path().join("test.yaml");
        let compiler = compiler.display().to_string();
        fs::write(
            &definition,
            format!(
                "targets: [arm64, amd64]\nimage: alpine\nexecution: {{}}\nbuild:\n  compiler: {{ arm64: {compiler}, amd64: {compiler} }}\n  flags: []\ncases:\n{rows}"
            ),
        )
        .unwrap();
        let app = App::load(directory.path(), &definition).unwrap();

        let build = || app.build(&app.cases[0], crate::suite::Target::Arm64).unwrap();
        let (first, second) = std::thread::scope(|scope| {
            let peer = scope.spawn(build);
            (build(), peer.join().unwrap())
        });

        assert_ne!(first.path(), second.path());
        for artifact in [&first, &second] {
            let text = fs::read_to_string(artifact.path()).unwrap();
            assert_eq!(text.lines().count(), 1, "clobbered artifact: {text:?}");
            assert!(text.starts_with("GUEST-"), "{text:?}");
        }
        drop((first, second));
        let _ = fs::remove_dir_all(super::workspace().unwrap().join("target/testing/runtime").join(&app.name));
    }

    #[test]
    fn category_rejects_unsafe_and_duplicate_case_builds() {
        let unsafe_source = category(
            "  - id: runtime/unsafe\n    build: { source: ../escape.c, output: unsafe, flags: [] }\n    artifact: { destination: /opt/unsafe }\n    status: active\n    compat: { class: compatibility }\n    run: []\n    expect: { exit: 0, stdout: golden/unsafe.out }\n",
        );
        assert!(unsafe_source.is_err());

        let duplicate = category(
            "  - id: runtime/one\n    build: { source: one.c, output: same, flags: [] }\n    artifact: { destination: /opt/one }\n    status: active\n    compat: { class: compatibility }\n    run: []\n    expect: { exit: 0, stdout: golden/one.out }\n  - id: runtime/two\n    build: { source: two.c, output: same, flags: [] }\n    artifact: { destination: /opt/two }\n    status: active\n    compat: { class: compatibility }\n    run: []\n    expect: { exit: 0, stdout: golden/two.out }\n",
        );
        assert!(duplicate.is_err());
    }

    #[test]
    fn category_requires_a_contained_golden_file() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("one.c"), "one").unwrap();
        let row = "  - id: runtime/one\n    build: { source: one.c, output: one, flags: [] }\n    artifact: { destination: /opt/one }\n    status: active\n    compat: { class: compatibility }\n    run: []\n    expect: { exit: 0, stdout: golden/missing.out }\n";
        let Err(error) = category_at(directory.path(), row) else {
            panic!("missing golden unexpectedly loaded");
        };
        assert!(error.to_string().contains("golden/missing.out is unavailable"));

        fs::create_dir(directory.path().join("golden/directory.out")).unwrap();
        let directory_row = row.replace("missing.out", "directory.out");
        let Err(error) = category_at(directory.path(), &directory_row) else {
            panic!("golden directory unexpectedly loaded");
        };
        assert!(error.to_string().contains("must be a file contained"));
    }

    #[test]
    fn build_inputs_are_normalized_sorted_contained_files() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("include")).unwrap();
        fs::write(directory.path().join("include/a.h"), "a").unwrap();
        fs::write(directory.path().join("z.ld"), "z").unwrap();
        let app = category_at(
            directory.path(),
            "  - id: runtime/one\n    build: { source: one.c, output: one, flags: [], inputs: [z.ld, include/a.h] }\n    artifact: { destination: /opt/one }\n    status: active\n    compat: { class: compatibility }\n    run: []\n    expect: { exit: 0, stdout: golden/one.out }\n",
        )
        .unwrap();
        assert_eq!(
            app.cases[0].inputs.iter().map(super::input::ManifestPath::name).collect::<Vec<_>>(),
            vec!["include/a.h", "z.ld"]
        );
    }

    #[test]
    fn document_build_inputs_are_inherited_as_a_complete_default() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("main.c"), "main").unwrap();
        fs::write(directory.path().join("layout.ld"), "layout").unwrap();
        fs::create_dir(directory.path().join("golden")).unwrap();
        fs::write(directory.path().join("golden/default.out"), []).unwrap();
        let definition = directory.path().join("test.yaml");
        fs::write(
            &definition,
            "targets: [arm64]\nimage: alpine\nexecution: {}\nartifact: { destination: /opt/default }\nbuild:\n  source: main.c\n  output: default\n  compiler: { arm64: arm-cc, amd64: amd-cc }\n  flags: [-static]\n  inputs: [layout.ld]\ncases:\n  - id: runtime/default\n    status: active\n    compat: { class: compatibility }\n    run: []\n    expect: { exit: 0, stdout: golden/default.out }\n",
        )
        .unwrap();

        let app = App::load(directory.path(), &definition).unwrap();
        assert_eq!(app.cases[0].source.name(), "main.c");
        assert_eq!(app.cases[0].inputs[0].name(), "layout.ld");
        assert_eq!(app.cases[0].flags, ["-static"]);
    }

    #[test]
    fn build_inputs_reject_escape_missing_and_duplicate_identity() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("one.c"), "one").unwrap();
        fs::write(directory.path().join("header.h"), "header").unwrap();
        let row = |inputs: &str| {
            format!(
                "  - id: runtime/one\n    build: {{ source: one.c, output: one, flags: [], inputs: {inputs} }}\n    artifact: {{ destination: /opt/one }}\n    status: active\n    compat: {{ class: compatibility }}\n    run: []\n    expect: {{ exit: 0, stdout: golden/one.out }}\n"
            )
        };

        assert!(category_at(directory.path(), &row("[../escape.h]")).is_err());
        assert!(category_at(directory.path(), &row("[missing.h]")).is_err());
        assert!(category_at(directory.path(), &row("[header.h, header.h]")).is_err());
    }

    #[test]
    fn ordered_environment_preserves_raw_values_and_rejects_nul() {
        let app = category(
            "  - id: runtime/one\n    build: { source: one.c, output: one, flags: [] }\n    artifact: { destination: /opt/one }\n    status: active\n    compat: { class: compatibility }\n    run: []\n    environment:\n      - { name: TZ, value: { bytes: [85, 84, 67, 255] } }\n      - { name: EMPTY, value: \"\" }\n    expect: { exit: 0, stdout: golden/one.out }\n",
        )
        .unwrap();
        assert_eq!(app.cases[0].environment[0].name(), b"TZ");
        assert_eq!(app.cases[0].environment[0].value(), b"UTC\xff");
        assert_eq!(app.cases[0].environment[1].name(), b"EMPTY");
        assert_eq!(app.cases[0].environment[1].value(), b"");

        let raw_name = category(
            "  - id: runtime/one\n    build: { source: one.c, output: one, flags: [] }\n    artifact: { destination: /opt/one }\n    status: active\n    compat: { class: compatibility }\n    run: []\n    environment: [{ name: { bytes: [78, 255] }, value: raw }]\n    expect: { exit: 0, stdout: golden/one.out }\n",
        )
        .unwrap();
        assert_eq!(raw_name.cases[0].environment[0].name(), b"N\xff");
        assert_eq!(raw_name.cases[0].environment[0].value(), b"raw");

        let invalid = category(
            "  - id: runtime/one\n    build: { source: one.c, output: one, flags: [] }\n    artifact: { destination: /opt/one }\n    status: active\n    compat: { class: compatibility }\n    run: []\n    environment: [{ name: BAD, value: { bytes: [0] } }]\n    expect: { exit: 0, stdout: golden/one.out }\n",
        );
        assert!(invalid.is_err());
    }

    #[test]
    fn host_exclusion_is_typed_validated_and_oracle_active() {
        let row = |hosts: &str, reason: &str, evidence: &str| {
            format!(
                "  - id: runtime/one\n    build: {{ source: one.c, output: one, flags: [] }}\n    artifact: {{ destination: /opt/one }}\n    status: !host-excluded\n      hosts: {hosts}\n      reason: {reason:?}\n      evidence: {evidence:?}\n    compat: {{ class: compatibility }}\n    run: []\n    expect: {{ exit: 0, stdout: golden/one.out }}\n"
            )
        };

        let app = category(&row("[macos]", "retained exclusion", "EVIDENCE.md")).unwrap();
        assert!(app.cases[0].inactive(EngineHost::Macos).is_some());
        assert!(app.cases[0].inactive(EngineHost::Linux).is_none());
        assert!(app.cases[0].status.oracle_inactive().is_none());

        for invalid in [
            row("[]", "reason", "evidence"),
            row("[macos, macos]", "reason", "evidence"),
            row("[linux, macos, windows]", "reason", "evidence"),
            row("[solaris]", "reason", "evidence"),
            row("[macos]", "", "evidence"),
            row("[macos]", "reason", ""),
        ] {
            assert!(category(&invalid).is_err());
        }
    }
}
