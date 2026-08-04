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
    artifact: Artifact,
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

#[derive(Clone, Copy, Deserialize)]
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
    source: PathBuf,
    output: String,
    compiler: Commands,
    flags: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCase {
    id: String,
    pub status: Status,
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
pub(crate) enum Status {
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

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum CompatClass {
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
    status: Status,
    pub compat: CompatClass,
    pub soak: Option<scheduler::Plan>,
}

pub struct App {
    pub name: String,
    pub directory: PathBuf,
    pub image: String,
    pub execution: Execution,
    pub targets: BTreeSet<Target>,
    pub destination: String,
    build: Build,
    oracle: Option<Oracle>,
    pub cases: Vec<RuntimeCase>,
}

impl App {
    pub fn load(directory: &Path, definition: &Path) -> Result<Self, Error> {
        let document: Document = serde_yaml::from_str(&fs::read_to_string(definition)?)?;
        if document.image.trim().is_empty() || document.targets.is_empty() || document.cases.is_empty() {
            return Err(format!("{} has an invalid image or case list", definition.display()).into());
        }
        safe_relative(&document.build.source)?;
        safe_absolute(&document.artifact.destination)?;
        let mut ids = BTreeSet::new();
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
                if let Some(plan) = &case.soak {
                    plan.validate()?;
                }
                if matches!(case.compat.class, CompatClass::Soak) != case.soak.is_some() {
                    return Err(format!("{} has inconsistent soak metadata for {:?}", definition.display(), case.id).into());
                }
                safe_relative(&case.expect.stdout)?;
                Ok(RuntimeCase {
                    id: case.id,
                    arguments: case.run,
                    environment: case.environment,
                    timeout: case.timeout,
                    exit: case.expect.exit,
                    golden: directory.join(case.expect.stdout),
                    status: case.status,
                    compat: case.compat.class,
                    soak: case.soak,
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
            destination: document.artifact.destination,
            build: document.build,
            oracle: document.oracle,
            cases,
        })
    }

    pub fn build(&self, target: Target) -> Result<PathBuf, Error> {
        let output = workspace()?
            .join("target/testing/runtime")
            .join(&self.name)
            .join(target.name())
            .join(&self.build.output);
        fs::create_dir_all(output.parent().ok_or("runtime output has no parent")?)?;
        let compiler = self.command(&self.build.compiler, target);
        let status = Command::new(compiler)
            .args(&self.build.flags)
            .arg("-o")
            .arg(&output)
            .arg(self.directory.join(&self.build.source))
            .status()?;
        if !status.success() {
            return Err(format!("{compiler} failed with {status}").into());
        }
        Ok(output)
    }

    pub fn oracle(&self, target: Target, update: bool) -> Result<(), Error> {
        let commands = self
            .oracle
            .as_ref()
            .ok_or_else(|| format!("{} defines no oracle", self.name))?;
        let artifact = self.build(target)?;
        for case in &self.cases {
            if let Some((kind, reason, evidence)) = case.status.inactive() {
                println!("{kind} {} {}: {reason} [{evidence}]", case.id, target.name());
                continue;
            }
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

#[cfg(test)]
mod tests {
    use super::Execution;

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
}
