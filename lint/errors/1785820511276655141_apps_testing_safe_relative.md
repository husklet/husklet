# `safe_relative`

- [ ] Approved
- Timestamp: `1785820511276655141`
- Domain: `apps`
- Package: `testing`
- Rule: `unclassified-free-function`
- Severity: `error`
- Source: `src/apps/testing/src/bench/definition.rs:159:1`
- Queue: `unclassified`
- Arguments: `1`
- Classification: `unclassified`
- Usage resolution: `ambiguous name; same-file references only`

## Finding

unclassified free function `safe_relative` has 1 argument

Help: refactor it or add a temporary #[hl_design::classify(...)] classification

## Review

- Does one argument already have a meaningful receiver type?
- Do related functions share this value and its invariants?
- Would a wrapper collect cohesive behavior, or only hide one helper?
- Is this a complete low-level algorithm that should remain free?

## Decision


## Dependencies

- `.any`
- `.components`
- `.into`
- `.is_absolute`
- `Err`
- `Ok`
- `format!`
- `matches!`

## Source

````rust
fn safe_relative(path: &Path) -> Result<(), Error> {
    if path.is_absolute() || path.components().any(|value| matches!(value, Component::ParentDir)) {
        Err(format!("unsafe source path {}", path.display()).into())
    } else {
        Ok(())
    }
}
````

## Related context

### usage in `load`
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

`src/apps/testing/src/bench/definition.rs:86:9`

````rust
safe_relative
````

### usage in `load`
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

`src/apps/testing/src/bench/definition.rs:110:17`

````rust
safe_relative
````
