# `safe_relative`

- [ ] Approved
- Timestamp: `1785820511278791683`
- Domain: `apps`
- Package: `testing`
- Rule: `unclassified-free-function`
- Severity: `error`
- Source: `src/apps/testing/src/nested.rs:200:1`
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
- `.into`
- `.is_absolute`
- `.is_empty`
- `Err`
- `Ok`
- `format!`
- `matches!`

## Source

````rust
fn safe_relative(path: &Path) -> Result<(), Error> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|part| matches!(part, Component::ParentDir))
    {
        Err(format!("unsafe relative path {}", path.display()).into())
    } else {
        Ok(())
    }
}
````

## Related context

### usage in `load`
fn load(root: &Path, definition: &Path) -> Result<Document, Error> {
    let document: Document = serde_yaml::from_str(&fs::read_to_string(definition)?)?;
    if document.version != 1 || document.chains.is_empty() {
        return Err(format!("{} has unsupported version or no chains", definition.display()).into());
    }
    let mut ids = BTreeSet::new();
    for chain in &document.chains {
        if chain.id.is_empty()
            || !ids.insert(&chain.id)
            || chain.layers.len() < 2
            || !(1..=3600).contains(&chain.timeout_seconds)
            || !(1..=16 * 1024 * 1024).contains(&chain.capture_limit_bytes)
            || !(0..=255).contains(&chain.expect.exit)
        {
            return Err(format!("invalid nested chain {:?}", chain.id).into());
        }
        validate_artifact(root, &chain.guest)?;
        safe_relative(&chain.expect.stdout)?;
        for layer in &chain.layers {
            validate_artifact(root, &layer.artifact)?;
            layer.options.validate()?;
        }
    }
    Ok(document)
}

`src/apps/testing/src/nested.rs:176:9`

````rust
safe_relative
````

### usage in `validate_artifact`
fn validate_artifact(root: &Path, artifact: &Artifact) -> Result<(), Error> {
    safe_relative(&artifact.path)?;
    if root.join(&artifact.path) == root
        || matches!(artifact.source, ArtifactSource::ForeignBuild)
            && artifact.build.as_deref().is_none_or(str::is_empty)
    {
        return Err(format!(
            "artifact {} has no usable path/build instruction",
            artifact.path.display()
        )
        .into());
    }
    Ok(())
}

`src/apps/testing/src/nested.rs:186:5`

````rust
safe_relative
````
