# `safe_relative`

- [ ] Approved
- Timestamp: `1785820511285378141`
- Domain: `apps`
- Package: `testing`
- Rule: `unclassified-free-function`
- Severity: `error`
- Source: `src/apps/testing/src/scenario/definition.rs:504:1`
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
- `.as_os_str`
- `.components`
- `.contains`
- `.into`
- `.is_absolute`
- `.len`
- `.to_string_lossy`
- `Err`
- `Ok`
- `format!`
- `matches!`

## Source

````rust
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
````

## Related context

### usage in `local_file`
fn local_file(directory: &Path, path: PathBuf, noun: &str) -> Result<PathBuf, Error> {
    safe_relative(&path)?;
    let path = directory.join(path);
    if path.is_file() {
        Ok(path)
    } else {
        Err(format!("missing {noun} {}", path.display()).into())
    }
}

`src/apps/testing/src/scenario/definition.rs:495:5`

````rust
safe_relative
````
