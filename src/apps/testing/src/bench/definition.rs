use crate::suite::{Commands, Error, Execution, Target};
use serde::Deserialize;
use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path, PathBuf},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    image: String,
    #[serde(default)]
    execution: Execution,
    build: Build,
    cases: Vec<Case>,
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
struct Case {
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
    expect: Expect,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Expect {
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
    build: Build,
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
        self.directory.join(&self.build.source)
    }

    pub fn compiler_name(&self, target: Target) -> &str {
        self.build.compiler.for_target(target)
    }

    pub fn load(directory: &Path, definition: &Path) -> Result<Self, Error> {
        let document: Document = serde_yaml::from_str(&fs::read_to_string(definition)?)?;
        safe_relative(&document.build.source)?;
        let source = directory.join(&document.build.source);
        if document.image.trim().is_empty() || !source.is_file() || document.cases.is_empty() {
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
                safe_relative(&case.expect.stdout_contains)?;
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
            cases,
        })
    }

    pub async fn build(&self, case: &BenchmarkCase, target: Target) -> Result<PathBuf, Error> {
        let output = crate::runtime::workspace()?
            .join("target/testing/bench")
            .join(&self.name)
            .join(target.name())
            .join(&case.id);
        fs::create_dir_all(output.parent().ok_or("benchmark output has no parent")?)?;
        let compiler = self.build.compiler.for_target(target);
        let status = tokio::process::Command::new(compiler)
            .args(&self.build.flags)
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
        Ok(output)
    }
}

fn safe_relative(path: &Path) -> Result<(), Error> {
    if path.is_absolute() || path.components().any(|value| matches!(value, Component::ParentDir)) {
        Err(format!("unsafe source path {}", path.display()).into())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Document;

    #[test]
    fn legacy_repetitions_maps_to_measured_samples() {
        let document: Document = serde_yaml::from_str(
            "image: alpine\nbuild:\n  source: main.c\n  compiler: { arm64: cc, amd64: cc }\n  flags: []\ncases:\n  - id: wall\n    repetitions: 7\n    expect: { stdout_contains: marker.txt }\n",
        )
        .unwrap();
        assert_eq!(document.cases[0].warmups, 1);
        assert_eq!(document.cases[0].samples, 7);
    }
}
