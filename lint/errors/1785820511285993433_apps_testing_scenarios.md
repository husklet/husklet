# `scenarios`

- [ ] Approved
- Timestamp: `1785820511285993433`
- Domain: `apps`
- Package: `testing`
- Rule: `unclassified-free-function`
- Severity: `error`
- Source: `src/apps/testing/src/scenario.rs:51:1`
- Queue: `unclassified`
- Arguments: `1`
- Classification: `unclassified`
- Usage resolution: `unique name in scanned tree`

## Finding

unclassified free function `scenarios` has 1 argument

Help: refactor it or add a temporary #[hl_design::classify(...)] classification

## Review

- Does one argument already have a meaningful receiver type?
- Do related functions share this value and its invariants?
- Would a wrapper collect cohesive behavior, or only hide one helper?
- Is this a complete low-level algorithm that should remain free?

## Decision


## Dependencies

- `.and_then`
- `.as_deref`
- `.collect`
- `.file_name`
- `.filter`
- `.into`
- `.into_iter`
- `.is_dir`
- `.is_empty`
- `.is_file`
- `.is_some_and`
- `.join`
- `.map`
- `.path`
- `.push`
- `.sort`
- `.to_str`
- `.unwrap_or_default`
- `Err`
- `Ok`
- `Scenario::load`
- `Vec::new`
- `format!`
- `std::fs::read_dir`
- `workspace`

## Source

````rust
fn scenarios(options: &Options) -> Result<Vec<Scenario>, Error> {
    let root = workspace()?.join("tests/scenarios");
    let mut directories = std::fs::read_dir(&root)?
        .map(|entry| entry.map(|value| value.path()))
        .collect::<Result<Vec<_>, _>>()?;
    directories.sort();

    let mut result = Vec::new();
    for directory in directories.into_iter().filter(|value| value.is_dir()) {
        let name = directory
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        if options.scenario.as_deref().is_some_and(|selected| selected != name) {
            continue;
        }
        let definition = directory.join("test.yaml");
        if definition.is_file() {
            result.push(Scenario::load(&directory, &definition)?);
        }
    }
    if result.is_empty() {
        return Err(format!("no scenarios matched under {}", root.display()).into());
    }
    Ok(result)
}
````

## Related context

### usage in `run`
pub async fn run(arguments: &[String]) -> Result<(), Error> {
    let options = Options::parse(arguments)?;
    let mut passed = 0_usize;
    let mut expected_failures = 0_usize;
    let mut failed = Vec::new();

    for scenario in scenarios(&options)? {
        for target in options.targets() {
            for result in execution::run(&scenario, target).await? {
                match result {
                    execution::CaseResult::Passed(id) => {
                        println!("PASS {id} {}", target.name());
                        passed += 1;
                    }
                    execution::CaseResult::Failed(id, error) => {
                        println!("FAIL {id} {}: {error}", target.name());
                        failed.push(format!("{id} {}: {error}", target.name()));
                    }
                    execution::CaseResult::ExpectedFailure(id, error) => {
                        println!("XFAIL {id} {}: {error}", target.name());
                        expected_failures += 1;
                    }
                    execution::CaseResult::UnexpectedPass(id) => {
                        println!("XPASS {id} {}", target.name());
                        failed.push(format!("{id} {}: unexpected pass", target.name()));
                    }
                }
            }
        }
    }

    println!(
        "scenarios: {passed} passed; {expected_failures} expected failures; {} failed",
        failed.len()
    );
    if failed.is_empty() {
        Ok(())
    } else {
        Err(failed.join("\n").into())
    }
}

`src/apps/testing/src/scenario.rs:15:21`

````rust
scenarios
````
