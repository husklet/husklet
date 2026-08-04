# `reject_duplicate_fixture_destinations`

- [ ] Approved
- Timestamp: `1785820511285226308`
- Domain: `apps`
- Package: `testing`
- Rule: `unclassified-free-function`
- Severity: `error`
- Source: `src/apps/testing/src/scenario/definition.rs:482:1`
- Queue: `unclassified`
- Arguments: `2`
- Classification: `unclassified`
- Usage resolution: `unique name in scanned tree`

## Finding

unclassified free function `reject_duplicate_fixture_destinations` has 2 arguments

Help: refactor it or add a temporary #[hl_design::classify(...)] classification

## Review

- Does one argument already have a meaningful receiver type?
- Do related functions share this value and its invariants?
- Would a wrapper collect cohesive behavior, or only hide one helper?
- Is this a complete low-level algorithm that should remain free?

## Decision


## Dependencies

- `.as_str`
- `.collect`
- `.into`
- `.iter`
- `.len`
- `.map`
- `Err`
- `Ok`
- `format!`

## Source

````rust
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

`src/apps/testing/src/scenario/definition.rs:336:5`

````rust
reject_duplicate_fixture_destinations
````
