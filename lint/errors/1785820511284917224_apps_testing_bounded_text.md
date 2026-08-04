# `bounded_text`

- [ ] Approved
- Timestamp: `1785820511284917224`
- Domain: `apps`
- Package: `testing`
- Rule: `unclassified-free-function`
- Severity: `error`
- Source: `src/apps/testing/src/scenario/definition.rs:470:1`
- Queue: `unclassified`
- Arguments: `1`
- Classification: `unclassified`
- Usage resolution: `unique name in scanned tree`

## Finding

unclassified free function `bounded_text` has 1 argument

Help: refactor it or add a temporary #[hl_design::classify(...)] classification

## Review

- Does one argument already have a meaningful receiver type?
- Do related functions share this value and its invariants?
- Would a wrapper collect cohesive behavior, or only hide one helper?
- Is this a complete low-level algorithm that should remain free?

## Decision


## Dependencies

- `.contains`
- `.is_empty`
- `.len`
- `.trim`

## Source

````rust
fn bounded_text(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= MAX_TEXT && !value.contains('\0')
}
````

## Related context

### usage in `validate_action`
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

`src/apps/testing/src/scenario/definition.rs:417:68`

````rust
bounded_text
````

### usage in `validate_action`
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

`src/apps/testing/src/scenario/definition.rs:421:68`

````rust
bounded_text
````

### usage in `validate_action`
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

`src/apps/testing/src/scenario/definition.rs:424:68`

````rust
bounded_text
````

### usage in `validate_readiness`
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

`src/apps/testing/src/scenario/definition.rs:446:9`

````rust
bounded_text
````

### usage in `validate_readiness`
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

`src/apps/testing/src/scenario/definition.rs:447:13`

````rust
bounded_text
````
