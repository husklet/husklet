# `validate_action`

- [ ] Approved
- Timestamp: `1785820511284293641`
- Domain: `apps`
- Package: `testing`
- Rule: `unclassified-free-function`
- Severity: `error`
- Source: `src/apps/testing/src/scenario/definition.rs:397:1`
- Queue: `unclassified`
- Arguments: `2`
- Classification: `unclassified`
- Usage resolution: `unique name in scanned tree`

## Finding

unclassified free function `validate_action` has 2 arguments

Help: refactor it or add a temporary #[hl_design::classify(...)] classification

## Review

- Does one argument already have a meaningful receiver type?
- Do related functions share this value and its invariants?
- Would a wrapper collect cohesive behavior, or only hide one helper?
- Is this a complete low-level algorithm that should remain free?

## Decision


## Dependencies

- `.all`
- `.contains`
- `.first`
- `.into`
- `.is_empty`
- `.is_some`
- `.is_some_and`
- `.iter`
- `.len`
- `.map`
- `.sum`
- `Err`
- `Ok`
- `ScenarioAction::Api`
- `ScenarioAction::Argv`
- `ScenarioAction::Host`
- `ScenarioAction::Shell`
- `bounded_text`
- `format!`
- `usize::from`

## Source

````rust
fn validate_action(id: &str, action: Action) -> Result<ScenarioAction, Error> {
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
        (None, None, None, None, Some(ApiAction { operation })) if bounded_text(&operation) => {
            Ok(ScenarioAction::Api(operation))
        }
        _ => Err(format!("{id} has an empty or invalid action").into()),
    }
}
````

## Related context

### usage in `load_case`
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
    let mut working_directory = "/".to_owned();
    let actions = if let Some(run) = case.run {
        safe_absolute(&run.program)?;
        safe_absolute(&run.working_directory)?;
        working_directory = run.working_directory;
        vec![validate_action(
            &case.id,
            Action {
                argv: Some(ArgvAction {
                    argv: std::iter::once(run.program).chain(run.arguments).collect(),
                }),
                shell: None,
                entrypoint: None,
                host: None,
                api: None,
            },
        )?]
    } else {
        case.actions
            .into_iter()
            .map(|action| validate_action(&case.id, action))
            .collect::<Result<Vec<_>, Error>>()?
    };
    if actions
        .iter()
        .filter(|action| matches!(action, ScenarioAction::Entrypoint))
        .count()
        > 1
    {
        return Err(format!("{} defines entrypoint more than once", case.id).into());
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

`src/apps/testing/src/scenario/definition.rs:320:27`

````rust
validate_action
````
