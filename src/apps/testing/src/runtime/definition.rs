use super::{scheduler, workspace};
use crate::suite::{Commands, Error, Execution, Target};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
    process::Command,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    targets: BTreeSet<Target>,
    image: String,
    #[serde(default)]
    execution: Execution,
    artifact: Option<Artifact>,
    build: Build,
    oracle: Option<Oracle>,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Oracle {
    provider: OracleProvider,
    commands: Commands,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum OracleProvider {
    Native,
    Qemu,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Artifact {
    destination: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Build {
    source: Option<PathBuf>,
    output: Option<String>,
    compiler: Commands,
    #[serde(default)]
    flags: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaseBuild {
    pub(crate) source: PathBuf,
    pub(crate) output: String,
    pub(crate) flags: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCase {
    id: String,
    build: Option<CaseBuild>,
    artifact: Option<Artifact>,
    #[serde(default)]
    targets: BTreeSet<Target>,
    status: Status,
    compat: Compat,
    soak: Option<scheduler::Plan>,
    run: Vec<String>,
    #[serde(default)]
    environment: BTreeMap<String, String>,
    #[serde(default = "timeout")]
    timeout: u64,
    expect: Expect,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Status {
    Active,
    Broken(Evidence),
    Unsupported(Evidence),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Evidence {
    reason: String,
    evidence: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Compat {
    class: CompatClass,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CompatClass {
    Smoke,
    Compatibility,
    Soak,
}

type Case = RawCase;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Expect {
    exit: i32,
    stdout: PathBuf,
}

const fn timeout() -> u64 {
    30
}

pub struct RuntimeCase {
    pub id: String,
    pub arguments: Vec<String>,
    pub environment: BTreeMap<String, String>,
    pub timeout: u64,
    pub exit: i32,
    pub golden: PathBuf,
    pub destination: String,
    pub(crate) source: PathBuf,
    pub(crate) output: String,
    pub(crate) flags: Vec<String>,
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

impl RuntimeCase {
    pub(crate) fn inactive(&self) -> Option<(&'static str, &str, &str)> {
        self.status.inactive()
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
                    || case
                        .environment
                        .keys()
                        .any(|name| name.is_empty() || name.contains('='))
                    || case
                        .environment
                        .iter()
                        .any(|(name, value)| name.contains('\0') || value.contains('\0'))
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
                let (source, output, flags, destination) =
                    resolve_build(case.build, case.artifact, &document.build, document.artifact.as_ref())?;
                safe_relative(&source)?;
                safe_output(&output)?;
                safe_absolute(&destination)?;
                if !outputs.insert(output.clone()) || !destinations.insert(destination.clone()) {
                    return Err(format!("{} has duplicate case output or destination", definition.display()).into());
                }
                safe_relative(&case.expect.stdout)?;
                Ok(RuntimeCase {
                    id: case.id,
                    arguments: case.run,
                    environment: case.environment,
                    timeout: case.timeout,
                    exit: case.expect.exit,
                    golden: directory.join(case.expect.stdout),
                    destination,
                    source,
                    output,
                    flags,
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

    pub fn build(&self, case: &RuntimeCase, target: Target) -> Result<PathBuf, Error> {
        let output = workspace()?
            .join("target/testing/runtime")
            .join(&self.name)
            .join(target.name())
            .join(&case.output);
        fs::create_dir_all(output.parent().ok_or("runtime output has no parent")?)?;
        let compiler = self.compiler.for_target(target);
        let status = Command::new(compiler)
            .arg("-o")
            .arg(&output)
            .arg(self.directory.join(&case.source))
            .args(&case.flags)
            .status()?;
        if !status.success() {
            return Err(format!("{compiler} failed with {status}").into());
        }
        Ok(output)
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
            if let Some((kind, reason, evidence)) = case.status.inactive() {
                println!("{kind} {} {}: {reason} [{evidence}]", case.id, target.name());
                continue;
            }
            let artifact = self.build(case, target)?;
            let mut command = Command::new(self.command(commands, target));
            command
                .env_clear()
                .envs(&case.environment)
                .arg(&artifact)
                .args(&case.arguments);
            let output = command.output()?;
            let status = output.status.code().ok_or("oracle terminated without an exit code")?;
            if status != case.exit {
                return Err(format!(
                    "oracle {} {} exited {status}, expected {}",
                    case.id,
                    target.name(),
                    case.exit
                )
                .into());
            }
            if update {
                let temporary = case.golden.with_extension("tmp");
                fs::write(&temporary, &output.stdout)?;
                fs::rename(temporary, &case.golden)?;
            } else if fs::read(&case.golden)? != output.stdout {
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

impl Status {
    fn validate(&self) -> Result<(), Error> {
        if let Self::Broken(evidence) | Self::Unsupported(evidence) = self
            && (evidence.reason.trim().is_empty() || evidence.evidence.trim().is_empty())
        {
            return Err("non-active status requires non-empty reason and evidence".into());
        }
        Ok(())
    }

    pub fn inactive(&self) -> Option<(&'static str, &str, &str)> {
        match self {
            Self::Active => None,
            Self::Broken(evidence) => Some(("BROKEN", &evidence.reason, &evidence.evidence)),
            Self::Unsupported(evidence) => Some(("UNSUPPORTED", &evidence.reason, &evidence.evidence)),
        }
    }
}

fn resolve_build(
    case: Option<CaseBuild>,
    artifact: Option<Artifact>,
    defaults: &Build,
    default_artifact: Option<&Artifact>,
) -> Result<(PathBuf, String, Vec<String>, String), Error> {
    match (case, artifact) {
        (Some(build), Some(artifact)) => Ok((build.source, build.output, build.flags, artifact.destination)),
        (None, None) => Ok((
            defaults.source.clone().ok_or("document build has no default source")?,
            defaults.output.clone().ok_or("document build has no default output")?,
            defaults.flags.clone(),
            default_artifact
                .ok_or("document defines no default artifact")?
                .destination
                .clone(),
        )),
        _ => Err("case build and artifact must be declared together".into()),
    }
}

fn safe_relative(path: &Path) -> Result<(), Error> {
    if path.is_absolute() || path.components().any(|value| matches!(value, Component::ParentDir)) {
        Err(format!("unsafe relative path {}", path.display()).into())
    } else {
        Ok(())
    }
}

fn safe_absolute(path: &str) -> Result<(), Error> {
    let path = Path::new(path);
    if !path.is_absolute() || path.components().any(|value| matches!(value, Component::ParentDir)) {
        Err(format!("unsafe guest path {}", path.display()).into())
    } else {
        Ok(())
    }
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
    use super::{App, Execution};
    use std::fs;

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
        let definition = directory.path().join("test.yaml");
        fs::write(
            &definition,
            format!(
                "targets: [arm64, amd64]\nimage: alpine\nexecution: {{}}\nbuild:\n  compiler: {{ arm64: arm-cc, amd64: amd-cc }}\n  flags: []\ncases:\n{case_rows}"
            ),
        )
        .unwrap();
        App::load(directory.path(), &definition)
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
}
