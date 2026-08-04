use super::Error;
use serde::Deserialize;
use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path, PathBuf},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    cases: Vec<Case>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    image: String,
    run: Run,
    #[serde(default)]
    fixtures: Vec<Fixture>,
    #[serde(default = "timeout")]
    timeout: u64,
    expect: Expect,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Run {
    program: String,
    #[serde(default)]
    arguments: Vec<String>,
    #[serde(default = "working_directory")]
    working_directory: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    source: PathBuf,
    destination: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Expect {
    #[serde(default)]
    exit: i32,
    stdout_contains: PathBuf,
}

const fn timeout() -> u64 {
    180
}

fn working_directory() -> String {
    "/".to_owned()
}

pub struct Scenario {
    pub name: String,
    pub cases: Vec<ScenarioCase>,
}

pub struct ScenarioCase {
    pub id: String,
    pub image: String,
    pub program: String,
    pub arguments: Vec<String>,
    pub working_directory: String,
    pub fixtures: Vec<ScenarioFixture>,
    pub timeout: u64,
    pub exit: i32,
    pub stdout_contains: PathBuf,
}

pub struct ScenarioFixture {
    pub source: PathBuf,
    pub destination: String,
}

impl Scenario {
    pub fn load(directory: &Path, definition: &Path) -> Result<Self, Error> {
        let document: Document = serde_yaml::from_str(&fs::read_to_string(definition)?)?;
        let name = directory
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or("scenario name is not UTF-8")?
            .to_owned();
        if document.cases.is_empty() {
            return Err(format!("{} defines no cases", definition.display()).into());
        }

        let mut ids = BTreeSet::new();
        let cases = document
            .cases
            .into_iter()
            .map(|case| {
                if !ids.insert(case.id.clone())
                    || !case.id.starts_with(&format!("{name}/"))
                    || case.image.trim().is_empty()
                    || case.run.program.trim().is_empty()
                    || !(1..=3600).contains(&case.timeout)
                {
                    return Err(format!("{} has invalid case {:?}", definition.display(), case.id).into());
                }
                safe_absolute(&case.run.program)?;
                safe_absolute(&case.run.working_directory)?;
                safe_relative(&case.expect.stdout_contains)?;
                let stdout_contains = directory.join(case.expect.stdout_contains);
                if !stdout_contains.is_file() {
                    return Err(format!("missing golden output {}", stdout_contains.display()).into());
                }
                let fixtures = case
                    .fixtures
                    .into_iter()
                    .map(|fixture| {
                        safe_relative(&fixture.source)?;
                        safe_absolute(&fixture.destination)?;
                        let source = directory.join(fixture.source);
                        if !source.is_file() {
                            return Err(format!("missing fixture {}", source.display()).into());
                        }
                        Ok(ScenarioFixture {
                            source,
                            destination: fixture.destination,
                        })
                    })
                    .collect::<Result<Vec<_>, Error>>()?;
                Ok(ScenarioCase {
                    id: case.id,
                    image: case.image,
                    program: case.run.program,
                    arguments: case.run.arguments,
                    working_directory: case.run.working_directory,
                    fixtures,
                    timeout: case.timeout,
                    exit: case.expect.exit,
                    stdout_contains,
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;

        Ok(Self { name, cases })
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
