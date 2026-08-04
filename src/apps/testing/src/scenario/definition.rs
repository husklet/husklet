use crate::suite::{Error, Execution, Target};
use clap::ValueEnum;
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
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
    #[serde(default)]
    execution: Execution,
    #[serde(default)]
    class: Class,
    #[serde(default)]
    targets: Vec<Platform>,
    #[serde(default)]
    xfail: Vec<Platform>,
    #[serde(default)]
    resources: Vec<Resource>,
    #[serde(default)]
    environment: BTreeMap<String, String>,
    #[serde(default)]
    fixtures: Vec<Fixture>,
    #[serde(default)]
    actions: Vec<Action>,
    run: Option<Run>,
    #[serde(default)]
    readiness: Option<Readiness>,
    #[serde(default = "timeout")]
    timeout: u64,
    expect: Expect,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Class {
    #[default]
    Quick,
    Long,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "lowercase")]
enum Platform {
    Arm64,
    Amd64,
}

impl Platform {
    const fn target(self) -> Target {
        match self {
            Self::Arm64 => Target::Arm64,
            Self::Amd64 => Target::Amd64,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub enum Resource {
    DiskHeavy,
    Registry,
    Network,
    Pty,
    HostPort,
    ImageMutation,
    ProcessHeavy,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Action {
    argv: Option<ArgvAction>,
    shell: Option<ScriptAction>,
    entrypoint: Option<EmptyAction>,
    host: Option<ScriptAction>,
    api: Option<ApiAction>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ArgvAction {
    argv: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScriptAction {
    script: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApiAction {
    operation: ApiOperation,
    source: PathBuf,
    destination: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum ApiOperation {
    CopyToContainer,
    CopyFromContainer,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyAction {}

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
struct Readiness {
    startup: String,
    probe: String,
    attempts: u32,
    delay_ms: u64,
    #[serde(default)]
    logs: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Expect {
    #[serde(default)]
    exit: i32,
    #[serde(default)]
    stdout_contains: Paths,
    #[serde(default)]
    stdout_exact: Option<PathBuf>,
}

#[derive(Default, Deserialize)]
#[serde(untagged)]
enum Paths {
    #[default]
    Empty,
    One(PathBuf),
    Many(Vec<PathBuf>),
}

impl Paths {
    fn into_vec(self) -> Vec<PathBuf> {
        match self {
            Self::Empty => Vec::new(),
            Self::One(path) => vec![path],
            Self::Many(paths) => paths,
        }
    }
}

const fn timeout() -> u64 {
    180
}

const MAX_CASES: usize = 1024;
const MAX_ACTIONS: usize = 64;
const MAX_FIXTURES: usize = 64;
const MAX_ENVIRONMENT: usize = 64;
const MAX_OUTPUT_CHECKS: usize = 64;
const MAX_ARGUMENTS: usize = 256;
const MAX_TEXT: usize = 64 * 1024;
const MAX_FIELD: usize = 4096;

fn working_directory() -> String {
    "/".to_owned()
}

pub struct Scenario {
    pub name: String,
    pub definition: PathBuf,
    pub cases: Vec<ScenarioCase>,
}

pub struct ScenarioCase {
    pub id: String,
    pub image: String,
    pub execution: Execution,
    pub class: Class,
    pub targets: Vec<Target>,
    pub expected_failures: Vec<Target>,
    pub resources: Vec<Resource>,
    pub environment: BTreeMap<String, String>,
    pub working_directory: String,
    pub actions: Vec<ScenarioAction>,
    pub fixtures: Vec<ScenarioFixture>,
    pub readiness: Option<ScenarioReadiness>,
    pub timeout: u64,
    pub exit: i32,
    pub stdout_contains: Vec<PathBuf>,
    pub stdout_exact: Option<PathBuf>,
}

#[derive(Debug)]
pub enum ScenarioAction {
    Argv(Vec<String>),
    Shell(String),
    Entrypoint,
    Host(String),
    Api(ScenarioApiAction),
}

#[derive(Debug)]
pub enum ScenarioApiAction {
    CopyToContainer { source: PathBuf, destination: String },
    CopyFromContainer { source: String },
}

pub struct ScenarioFixture {
    pub source: PathBuf,
    pub destination: String,
}

pub struct ScenarioReadiness {
    pub startup: String,
    pub probe: String,
    pub attempts: u32,
    pub delay_ms: u64,
    pub logs: Vec<String>,
}

impl ScenarioCase {
    pub fn supports(&self, target: Target) -> bool {
        self.targets.iter().any(|candidate| candidate.name() == target.name())
    }

    pub fn expects_failure(&self, target: Target) -> bool {
        self.expected_failures
            .iter()
            .any(|candidate| candidate.name() == target.name())
    }
}

impl Scenario {
    pub fn load(directory: &Path, definition: &Path) -> Result<Self, Error> {
        let document: Document = serde_yaml::from_str(&fs::read_to_string(definition)?)?;
        let name = directory
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or("scenario name is not UTF-8")?
            .to_owned();
        if document.cases.is_empty() || document.cases.len() > MAX_CASES {
            return Err(format!("{} defines no cases", definition.display()).into());
        }

        let mut ids = BTreeSet::new();
        let cases = document
            .cases
            .into_iter()
            .map(|case| load_case(directory, definition, case, &mut ids))
            .collect::<Result<Vec<_>, Error>>()?;
        Ok(Self {
            name,
            definition: definition.to_path_buf(),
            cases,
        })
    }
}

fn load_case(
    directory: &Path,
    definition: &Path,
    case: Case,
    ids: &mut BTreeSet<String>,
) -> Result<ScenarioCase, Error> {
    if !ids.insert(case.id.clone())
        || !case.id.contains('/')
        || case.image.trim().is_empty()
        || case.image.len() > 512
        || case.image.contains('\0')
        || !(1..=3600).contains(&case.timeout)
    {
        return Err(format!("{} has invalid case {:?}", definition.display(), case.id).into());
    }
    if case.actions.is_empty() == case.run.is_none() {
        return Err(format!("{} must define exactly one of run or actions", case.id).into());
    }
    if case.actions.len() > MAX_ACTIONS
        || case.fixtures.len() > MAX_FIXTURES
        || case.environment.len() > MAX_ENVIRONMENT
    {
        return Err(format!("{} exceeds a scenario collection bound", case.id).into());
    }
    validate_environment(&case.id, &case.environment)?;
    let targets = platforms(case.targets, true, &case.id)?;
    let expected_failures = platforms(case.xfail, false, &case.id)?;
    if expected_failures
        .iter()
        .any(|target| !targets.iter().any(|candidate| candidate.name() == target.name()))
    {
        return Err(format!("{} marks an unsupported target xfail", case.id).into());
    }
    let resources = unique(case.resources, &case.id, "resource")?;
    let (working_directory, actions) = load_actions(directory, &case.id, case.run, case.actions)?;
    let entrypoints = actions
        .iter()
        .filter(|action| matches!(action, ScenarioAction::Entrypoint))
        .count();
    if entrypoints > 1 || (entrypoints == 1 && (actions.len() != 1 || case.readiness.is_some())) {
        return Err(format!(
            "{} entrypoint must be its only action and cannot use readiness",
            case.id
        )
        .into());
    }
    let fixtures = case
        .fixtures
        .into_iter()
        .map(|fixture| load_fixture(directory, fixture))
        .collect::<Result<Vec<_>, Error>>()?;
    reject_duplicate_fixture_destinations(&case.id, &fixtures)?;
    let stdout_contains = case
        .expect
        .stdout_contains
        .into_vec()
        .into_iter()
        .map(|path| local_file(directory, path, "golden output"))
        .collect::<Result<Vec<_>, Error>>()?;
    if stdout_contains.len() > MAX_OUTPUT_CHECKS {
        return Err(format!("{} defines too many output checks", case.id).into());
    }
    if stdout_contains.iter().collect::<BTreeSet<_>>().len() != stdout_contains.len() {
        return Err(format!("{} repeats an output check", case.id).into());
    }
    let stdout_exact = case
        .expect
        .stdout_exact
        .map(|path| local_file(directory, path, "exact golden output"))
        .transpose()?;
    if stdout_contains.is_empty() && stdout_exact.is_none() {
        return Err(format!("{} defines no output oracle", case.id).into());
    }
    let readiness = case.readiness.map(validate_readiness).transpose()?;
    Ok(ScenarioCase {
        id: case.id,
        image: case.image,
        execution: case.execution,
        class: case.class,
        targets,
        expected_failures,
        resources,
        environment: case.environment,
        working_directory,
        actions,
        fixtures,
        readiness,
        timeout: case.timeout,
        exit: case.expect.exit,
        stdout_contains,
        stdout_exact,
    })
}

fn load_actions(
    directory: &Path,
    id: &str,
    run: Option<Run>,
    actions: Vec<Action>,
) -> Result<(String, Vec<ScenarioAction>), Error> {
    let Some(run) = run else {
        let actions = actions
            .into_iter()
            .map(|action| validate_action(directory, id, action))
            .collect::<Result<Vec<_>, Error>>()?;
        return Ok(("/".to_owned(), actions));
    };
    safe_absolute(&run.program)?;
    safe_absolute(&run.working_directory)?;
    let action = Action {
        argv: Some(ArgvAction {
            argv: std::iter::once(run.program).chain(run.arguments).collect(),
        }),
        shell: None,
        entrypoint: None,
        host: None,
        api: None,
    };
    Ok((run.working_directory, vec![validate_action(directory, id, action)?]))
}

fn platforms(values: Vec<Platform>, default_all: bool, id: &str) -> Result<Vec<Target>, Error> {
    let values = if values.is_empty() && default_all {
        vec![Platform::Arm64, Platform::Amd64]
    } else {
        unique(values, id, "target")?
    };
    Ok(values.into_iter().map(Platform::target).collect())
}

fn unique<T: Ord + Copy>(values: Vec<T>, id: &str, noun: &str) -> Result<Vec<T>, Error> {
    let set = values.iter().copied().collect::<BTreeSet<_>>();
    if set.len() != values.len() {
        Err(format!("{id} repeats a {noun}").into())
    } else {
        Ok(values)
    }
}

fn validate_action(directory: &Path, id: &str, action: Action) -> Result<ScenarioAction, Error> {
    let count = usize::from(action.argv.is_some())
        + usize::from(action.shell.is_some())
        + usize::from(action.entrypoint.is_some())
        + usize::from(action.host.is_some())
        + usize::from(action.api.is_some());
    if count != 1 {
        return Err(format!("{id} action must select exactly one operation").into());
    }
    match (action.argv, action.shell, action.entrypoint, action.host, action.api) {
        (Some(ArgvAction { argv }), None, None, None, None)
            if argv.first().is_some_and(|value| !value.is_empty())
                && argv.len() <= MAX_ARGUMENTS
                && argv
                    .iter()
                    .all(|value| value.len() <= MAX_FIELD && !value.contains('\0'))
                && argv.iter().map(String::len).sum::<usize>() <= MAX_TEXT =>
        {
            Ok(ScenarioAction::Argv(argv))
        }
        (None, Some(ScriptAction { script }), None, None, None) if bounded_text(&script) => {
            Ok(ScenarioAction::Shell(script))
        }
        (None, None, Some(EmptyAction {}), None, None) => Ok(ScenarioAction::Entrypoint),
        (None, None, None, Some(ScriptAction { script }), None) if bounded_text(&script) => {
            Ok(ScenarioAction::Host(script))
        }
        (
            None,
            None,
            None,
            None,
            Some(ApiAction {
                operation: ApiOperation::CopyToContainer,
                source,
                destination: Some(destination),
            }),
        ) => {
            safe_absolute(&destination)?;
            Ok(ScenarioAction::Api(ScenarioApiAction::CopyToContainer {
                source: local_path(directory, source, "copy source")?,
                destination,
            }))
        }
        (
            None,
            None,
            None,
            None,
            Some(ApiAction {
                operation: ApiOperation::CopyFromContainer,
                source,
                destination: None,
            }),
        ) => {
            let source = source
                .to_str()
                .ok_or_else(|| format!("{id} copy source is not UTF-8"))?
                .to_owned();
            safe_absolute(&source)?;
            Ok(ScenarioAction::Api(ScenarioApiAction::CopyFromContainer { source }))
        }
        _ => Err(format!("{id} has an empty or invalid action").into()),
    }
}

fn validate_environment(id: &str, environment: &BTreeMap<String, String>) -> Result<(), Error> {
    if environment.iter().any(|(name, value)| {
        name.is_empty()
            || name.len() > 256
            || name.contains(['=', '\0'])
            || value.len() > MAX_TEXT
            || value.contains('\0')
    }) {
        Err(format!("{id} has an invalid environment name").into())
    } else {
        Ok(())
    }
}

fn validate_readiness(value: Readiness) -> Result<ScenarioReadiness, Error> {
    if !bounded_text(&value.startup)
        || !bounded_text(&value.probe)
        || !(1..=1000).contains(&value.attempts)
        || value.delay_ms > 60_000
        || value.logs.len() > 32
    {
        return Err("readiness requires startup, probe, and positive attempts".into());
    }
    if value
        .logs
        .iter()
        .any(|path| path.len() > MAX_FIELD || path.contains('\0') || !Path::new(path).is_absolute())
    {
        return Err("readiness log paths must be absolute guest paths".into());
    }
    Ok(ScenarioReadiness {
        startup: value.startup,
        probe: value.probe,
        attempts: value.attempts,
        delay_ms: value.delay_ms,
        logs: value.logs,
    })
}

fn bounded_text(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= MAX_TEXT && !value.contains('\0')
}

fn load_fixture(directory: &Path, fixture: Fixture) -> Result<ScenarioFixture, Error> {
    safe_absolute(&fixture.destination)?;
    Ok(ScenarioFixture {
        source: local_file(directory, fixture.source, "fixture")?,
        destination: fixture.destination,
    })
}

fn reject_duplicate_fixture_destinations(id: &str, fixtures: &[ScenarioFixture]) -> Result<(), Error> {
    let destinations = fixtures
        .iter()
        .map(|fixture| fixture.destination.as_str())
        .collect::<BTreeSet<_>>();
    if destinations.len() == fixtures.len() {
        Ok(())
    } else {
        Err(format!("{id} repeats a fixture destination").into())
    }
}

fn local_file(directory: &Path, path: PathBuf, noun: &str) -> Result<PathBuf, Error> {
    safe_relative(&path)?;
    let path = directory.join(path);
    if path.is_file() {
        Ok(path)
    } else {
        Err(format!("missing {noun} {}", path.display()).into())
    }
}

fn local_path(directory: &Path, path: PathBuf, noun: &str) -> Result<PathBuf, Error> {
    safe_relative(&path)?;
    let path = directory.join(path);
    if path.exists() {
        Ok(path)
    } else {
        Err(format!("missing {noun} {}", path.display()).into())
    }
}

fn safe_relative(path: &Path) -> Result<(), Error> {
    if path.as_os_str().len() > MAX_FIELD
        || path.as_os_str().to_string_lossy().contains('\0')
        || path.is_absolute()
        || path.components().any(|value| matches!(value, Component::ParentDir))
    {
        Err(format!("unsafe relative path {}", path.display()).into())
    } else {
        Ok(())
    }
}

fn safe_absolute(path: &str) -> Result<(), Error> {
    let path = Path::new(path);
    if path.as_os_str().len() > MAX_FIELD
        || path.as_os_str().to_string_lossy().contains('\0')
        || !path.is_absolute()
        || path.components().any(|value| matches!(value, Component::ParentDir))
    {
        Err(format!("unsafe guest path {}", path.display()).into())
    } else {
        Ok(())
    }
}

#[cfg(test)]
#[path = "definition_test.rs"]
mod tests;
