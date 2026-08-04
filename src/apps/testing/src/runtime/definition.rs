use super::{Error, workspace};
use serde::Deserialize;
use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path, PathBuf},
    process::Command,
};

#[derive(Clone, Copy)]
pub enum Target {
    Arm64,
    Amd64,
}

impl Target {
    pub fn parse(value: &str) -> Result<Self, Error> {
        match value {
            "arm64" => Ok(Self::Arm64),
            "amd64" => Ok(Self::Amd64),
            _ => Err(format!("unsupported ISA {value:?}").into()),
        }
    }
    pub const fn name(self) -> &'static str {
        match self {
            Self::Arm64 => "arm64",
            Self::Amd64 => "amd64",
        }
    }
    pub const fn guest(self) -> hl_container::Guest {
        match self {
            Self::Arm64 => hl_container::Guest::Aarch64,
            Self::Amd64 => hl_container::Guest::X86_64,
        }
    }
    pub fn platform(self) -> hl_images::Platform {
        match self {
            Self::Arm64 => hl_images::Platform::linux_arm64(),
            Self::Amd64 => hl_images::Platform::linux_amd64(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    image: String,
    artifact: Artifact,
    build: Build,
    oracle: Option<Commands>,
    cases: Vec<Case>,
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
struct Commands {
    arm64: String,
    amd64: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCase {
    id: String,
    run: Vec<String>,
    #[serde(default = "timeout")]
    timeout: u64,
    expect: Expect,
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
    pub timeout: u64,
    pub exit: i32,
    pub golden: PathBuf,
}

pub struct App {
    pub name: String,
    pub directory: PathBuf,
    pub image: String,
    pub destination: String,
    build: Build,
    oracle: Option<Commands>,
    pub cases: Vec<RuntimeCase>,
}

impl App {
    pub fn load(directory: &Path, definition: &Path) -> Result<Self, Error> {
        let document: Document = serde_yaml::from_str(&fs::read_to_string(definition)?)?;
        if document.image.trim().is_empty() || document.cases.is_empty() {
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
                    || case.run.is_empty()
                    || !(1..=3600).contains(&case.timeout)
                {
                    return Err(format!("{} has invalid case {:?}", definition.display(), case.id).into());
                }
                safe_relative(&case.expect.stdout)?;
                Ok(RuntimeCase {
                    id: case.id,
                    arguments: case.run,
                    timeout: case.timeout,
                    exit: case.expect.exit,
                    golden: directory.join(case.expect.stdout),
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
            let output = Command::new(self.command(commands, target))
                .arg(&artifact)
                .args(&case.arguments)
                .output()?;
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

    fn command<'a>(&self, commands: &'a Commands, target: Target) -> &'a str {
        match target {
            Target::Arm64 => &commands.arm64,
            Target::Amd64 => &commands.amd64,
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
