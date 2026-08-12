use crate::suite::SafePath as _;
use crate::suite::{Commands, Error, Execution, Target};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BenchmarkDefinition {
    image: String,
    #[serde(default)]
    execution: Execution,
    build: Option<Build>,
    workload: Option<Workload>,
    #[serde(default = "lifecycle_phases")]
    phases: Vec<LifecyclePhase>,
    cases: Vec<BenchmarkSpecification>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Workload {
    rootfs: RootfsWorkload,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RootfsWorkload {
    executable: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "kebab-case")]
pub enum LifecyclePhase {
    Create,
    Attach,
    Start,
    WaitAndDrain,
    OutputRead,
}

impl LifecyclePhase {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Attach => "attach",
            Self::Start => "start",
            Self::WaitAndDrain => "wait_and_drain",
            Self::OutputRead => "output_read",
        }
    }
}

fn lifecycle_phases() -> Vec<LifecyclePhase> {
    vec![
        LifecyclePhase::Create,
        LifecyclePhase::Attach,
        LifecyclePhase::Start,
        LifecyclePhase::WaitAndDrain,
        LifecyclePhase::OutputRead,
    ]
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Build {
    source: PathBuf,
    compiler: Commands,
    #[serde(default)]
    flags: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BenchmarkSpecification {
    id: String,
    #[serde(default)]
    build_flags: Vec<String>,
    #[serde(default)]
    arguments: Vec<String>,
    #[serde(default = "warmups")]
    warmups: u32,
    #[serde(default = "samples", alias = "repetitions")]
    samples: u32,
    #[serde(default = "timeout")]
    timeout: u64,
    expect: Expectation,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Expectation {
    #[serde(default)]
    exit: i32,
    stdout_contains: PathBuf,
}

const fn warmups() -> u32 {
    1
}
const fn samples() -> u32 {
    5
}
const fn timeout() -> u64 {
    120
}

pub struct Benchmark {
    pub name: String,
    pub directory: PathBuf,
    pub image: String,
    pub execution: Execution,
    build: Option<Build>,
    pub rootfs_executable: Option<String>,
    pub phases: Vec<LifecyclePhase>,
    pub cases: Vec<BenchmarkCase>,
}
pub struct BenchmarkCase {
    pub id: String,
    pub arguments: Vec<String>,
    pub warmups: u32,
    pub samples: u32,
    pub timeout: u64,
    pub exit: i32,
    pub stdout_contains: PathBuf,
    build_flags: Vec<String>,
}

impl Benchmark {
    pub fn source_path(&self) -> PathBuf {
        self.directory
            .join(&self.build.as_ref().expect("compiled benchmark has a build").source)
    }

    pub fn compiler_name(&self, target: Target) -> &str {
        self.build
            .as_ref()
            .expect("compiled benchmark has a compiler")
            .compiler
            .for_target(target)
    }

    pub fn load(directory: &Path, definition: &Path) -> Result<Self, Error> {
        let document: BenchmarkDefinition = serde_yaml::from_str(&fs::read_to_string(definition)?)?;
        let rootfs_executable = document.workload.map(|workload| workload.rootfs.executable);
        if document.build.is_some() == rootfs_executable.is_some() {
            return Err(format!(
                "{} must define exactly one of build or workload.rootfs",
                definition.display()
            )
            .into());
        }
        if let Some(build) = &document.build {
            build.source.safe_relative()?;
            if !directory.join(&build.source).is_file() {
                return Err(format!("{} has a missing benchmark source", definition.display()).into());
            }
        }
        if rootfs_executable
            .as_deref()
            .is_some_and(|value| !value.starts_with('/') || value == "/")
        {
            return Err(format!("{} has an invalid rootfs executable", definition.display()).into());
        }
        let phase_count = document.phases.len();
        let phases = document.phases.into_iter().collect::<BTreeSet<_>>();
        if phases.is_empty() || phases.len() != phase_count {
            return Err(format!(
                "{} has an empty or duplicate lifecycle phase list",
                definition.display()
            )
            .into());
        }
        if document.image.trim().is_empty() || document.cases.is_empty() {
            return Err(format!("{} has an invalid image, source, or case list", definition.display()).into());
        }
        let name = directory
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or("benchmark name is not UTF-8")?
            .to_owned();
        let mut ids = BTreeSet::new();
        let cases = document
            .cases
            .into_iter()
            .map(|case| {
                if !ids.insert(case.id.clone())
                    || case.id.is_empty()
                    || case.id.contains('/')
                    || case.warmups > 100
                    || !(1..=100).contains(&case.samples)
                    || !(1..=3600).contains(&case.timeout)
                {
                    return Err(format!("{} has invalid case {:?}", definition.display(), case.id).into());
                }
                case.expect.stdout_contains.safe_relative()?;
                let stdout_contains = directory.join(case.expect.stdout_contains);
                if !stdout_contains.is_file() {
                    return Err(format!("missing benchmark golden {}", stdout_contains.display()).into());
                }
                Ok(BenchmarkCase {
                    id: case.id,
                    arguments: case.arguments,
                    warmups: case.warmups,
                    samples: case.samples,
                    timeout: case.timeout,
                    exit: case.expect.exit,
                    stdout_contains,
                    build_flags: case.build_flags,
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;
        Ok(Self {
            name,
            directory: directory.to_path_buf(),
            image: document.image,
            execution: document.execution,
            build: document.build,
            rootfs_executable,
            phases: phases.into_iter().collect(),
            cases,
        })
    }

    pub async fn build(&self, case: &BenchmarkCase, target: Target) -> Result<PathBuf, Error> {
        let build = self.build.as_ref().ok_or("rootfs workload has no build artifact")?;
        // Compile into a private directory so concurrent builders of one case cannot clobber each other.
        let root = crate::runtime::workspace()?
            .join("target/testing/bench")
            .join(&self.name)
            .join(target.name());
        tokio::fs::create_dir_all(&root).await?;
        let staging = tempfile::Builder::new().prefix("build-").tempdir_in(&root)?;
        let output = staging.path().join(case.id.replace('/', "-"));
        let compiler = build.compiler.for_target(target);
        let status = crate::platform::HostProcess::asynchronous(compiler)
            .args(&build.flags)
            .args(&case.build_flags)
            .arg(self.source_path())
            .arg("-o")
            .arg(&output)
            .kill_on_drop(true)
            .status()
            .await?;
        if !status.success() {
            return Err(format!("{compiler} failed building {}/{} with {status}", self.name, case.id).into());
        }
        let identity = Sha256::digest(tokio::fs::read(&output).await?);
        let identity = identity.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        let cache = crate::runtime::workspace()?
            .join("target/testing/bench/cache/artifacts/sha256")
            .join(identity);
        if !cache.is_file() {
            tokio::fs::create_dir_all(cache.parent().ok_or("artifact cache has no parent")?).await?;
            let temporary = tempfile::NamedTempFile::new_in(cache.parent().ok_or("artifact cache has no parent")?)?;
            tokio::fs::copy(&output, temporary.path()).await?;
            match temporary.persist_noclobber(&cache) {
                Ok(_) => {}
                Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.error.into()),
            }
        }
        Ok(cache)
    }
}

#[cfg(test)]
mod tests {
    use super::{Benchmark, BenchmarkDefinition, LifecyclePhase};

    #[test]
    fn legacy_repetitions_maps_to_measured_samples() {
        let document: BenchmarkDefinition = serde_yaml::from_str(
            "image: alpine\nbuild:\n  source: main.c\n  compiler: { arm64: cc, amd64: cc }\n  flags: []\ncases:\n  - id: wall\n    repetitions: 7\n    expect: { stdout_contains: marker.txt }\n",
        )
        .unwrap();
        assert_eq!(document.cases[0].warmups, 1);
        assert_eq!(document.cases[0].samples, 7);
    }

    #[test]
    fn rootfs_workload_and_lifecycle_phases_are_typed() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("empty"), []).unwrap();
        std::fs::write(
            directory.path().join("test.yaml"),
            "image: alpine:3.20\nworkload: { rootfs: { executable: /bin/true } }\nphases: [create, start, wait-and-drain]\ncases:\n  - id: launch\n    warmups: 0\n    samples: 3\n    expect: { stdout_contains: empty }\n",
        )
        .unwrap();
        let benchmark = Benchmark::load(directory.path(), &directory.path().join("test.yaml")).unwrap();
        assert_eq!(benchmark.rootfs_executable.as_deref(), Some("/bin/true"));
        assert_eq!(
            benchmark.phases,
            vec![
                LifecyclePhase::Create,
                LifecyclePhase::Start,
                LifecyclePhase::WaitAndDrain
            ]
        );
    }

    #[test]
    fn rootfs_workload_rejects_an_ambiguous_build() {
        let yaml = "image: alpine\nworkload: { rootfs: { executable: /bin/true } }\nbuild: { source: main.c, compiler: { arm64: cc, amd64: cc } }\ncases: []\n";
        let document: BenchmarkDefinition = serde_yaml::from_str(yaml).unwrap();
        assert!(document.build.is_some() && document.workload.is_some());
    }
}
