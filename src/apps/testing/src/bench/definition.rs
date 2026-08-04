use super::Error;
use crate::runtime::definition::Target;
use serde::Deserialize;
use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path, PathBuf},
    process::Command,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    image: String,
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
struct Commands {
    arm64: String,
    amd64: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    #[serde(default)]
    build_flags: Vec<String>,
    #[serde(default)]
    arguments: Vec<String>,
    #[serde(default = "repetitions")]
    repetitions: u32,
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

const fn repetitions() -> u32 {
    3
}
const fn timeout() -> u64 {
    120
}

pub struct Benchmark {
    pub name: String,
    pub directory: PathBuf,
    pub image: String,
    build: Build,
    pub cases: Vec<BenchmarkCase>,
}
pub struct BenchmarkCase {
    pub id: String,
    pub arguments: Vec<String>,
    pub repetitions: u32,
    pub timeout: u64,
    pub exit: i32,
    pub stdout_contains: PathBuf,
    build_flags: Vec<String>,
}

impl Benchmark {
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
                    || !(1..=100).contains(&case.repetitions)
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
                    repetitions: case.repetitions,
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
            build: document.build,
            cases,
        })
    }

    pub fn build(&self, case: &BenchmarkCase, target: Target) -> Result<PathBuf, Error> {
        let output = crate::runtime::workspace()?
            .join("target/testing/bench")
            .join(&self.name)
            .join(target.name())
            .join(&case.id);
        fs::create_dir_all(output.parent().ok_or("benchmark output has no parent")?)?;
        let compiler = match target {
            Target::Arm64 => &self.build.compiler.arm64,
            Target::Amd64 => &self.build.compiler.amd64,
        };
        let status = Command::new(compiler)
            .args(&self.build.flags)
            .args(&case.build_flags)
            .arg(self.directory.join(&self.build.source))
            .arg("-o")
            .arg(&output)
            .status()?;
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
