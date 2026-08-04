# `safe_relative`

- [ ] Approved
- Timestamp: `1785820511279829516`
- Domain: `apps`
- Package: `testing`
- Rule: `unclassified-free-function`
- Severity: `error`
- Source: `src/apps/testing/src/runtime/definition.rs:199:1`
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
        Err(format!("unsafe relative path {}", path.display()).into())
    } else {
        Ok(())
    }
}
````

## Related context

### usage in `load`
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
                    || case
                        .environment
                        .keys()
                        .any(|name| name.is_empty() || name.contains('='))
                    || case
                        .environment
                        .iter()
                        .any(|(name, value)| name.contains('\0') || value.contains('\0'))
                {
                    return Err(format!("{} has invalid case {:?}", definition.display(), case.id).into());
                }
                safe_relative(&case.expect.stdout)?;
                Ok(RuntimeCase {
                    id: case.id,
                    arguments: case.run,
                    environment: case.environment,
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
            execution: document.execution,
            destination: document.artifact.destination,
            build: document.build,
            oracle: document.oracle,
            cases,
        })
    }

`src/apps/testing/src/runtime/definition.rs:89:9`

````rust
safe_relative
````

### usage in `load`
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
                    || case
                        .environment
                        .keys()
                        .any(|name| name.is_empty() || name.contains('='))
                    || case
                        .environment
                        .iter()
                        .any(|(name, value)| name.contains('\0') || value.contains('\0'))
                {
                    return Err(format!("{} has invalid case {:?}", definition.display(), case.id).into());
                }
                safe_relative(&case.expect.stdout)?;
                Ok(RuntimeCase {
                    id: case.id,
                    arguments: case.run,
                    environment: case.environment,
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
            execution: document.execution,
            destination: document.artifact.destination,
            build: document.build,
            oracle: document.oracle,
            cases,
        })
    }

`src/apps/testing/src/runtime/definition.rs:111:17`

````rust
safe_relative
````
