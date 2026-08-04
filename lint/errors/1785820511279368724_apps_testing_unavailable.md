# `unavailable`

- [ ] Approved
- Timestamp: `1785820511279368724`
- Domain: `apps`
- Package: `testing`
- Rule: `unclassified-free-function`
- Severity: `error`
- Source: `src/apps/testing/src/nested.rs:223:1`
- Queue: `unclassified`
- Arguments: `2`
- Classification: `unclassified`
- Usage resolution: `unique name in scanned tree`

## Finding

unclassified free function `unavailable` has 2 arguments

Help: refactor it or add a temporary #[hl_design::classify(...)] classification

## Review

- Does one argument already have a meaningful receiver type?
- Do related functions share this value and its invariants?
- Would a wrapper collect cohesive behavior, or only hide one helper?
- Is this a complete low-level algorithm that should remain free?

## Decision


## Dependencies

- `.is_file`
- `.is_ok_and`
- `.join`
- `.metadata`
- `.mode`
- `.permissions`
- `Outcome::Failed`
- `Outcome::Unsupported`
- `Some`
- `format!`

## Source

````rust
fn unavailable(root: &Path, artifact: &Artifact) -> Option<Outcome> {
    let path = root.join(&artifact.path);
    if path
        .metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    {
        return None;
    }
    Some(match artifact.source {
        ArtifactSource::ForeignBuild => Outcome::Unsupported(format!(
            "foreign artifact {} is absent or not executable; build with: {}",
            path.display(),
            artifact.build.as_deref().unwrap_or("<missing build instruction>")
        )),
        ArtifactSource::Local => Outcome::Failed(format!(
            "required local artifact {} is absent or not executable",
            path.display()
        )),
    })
}
````

## Related context

### usage in `execute`
fn execute(root: &Path, definition: &Path, chain: &Chain) -> Outcome {
    for artifact in chain.layers.iter().map(|layer| &layer.artifact).chain([&chain.guest]) {
        if let Some(outcome) = unavailable(root, artifact) {
            return outcome;
        }
    }
    let expected = definition.parent().unwrap_or(root).join(&chain.expect.stdout);
    let expected = match fs::read(&expected) {
        Ok(value) => value,
        Err(error) => return Outcome::Failed(format!("cannot read {}: {error}", expected.display())),
    };
    let arguments = command(root, chain);
    match capture(
        &arguments,
        Duration::from_secs(chain.timeout_seconds),
        chain.capture_limit_bytes,
    ) {
        Ok((status, stdout, stderr))
            if status == Some(chain.expect.exit)
                && stdout == expected
                && (!chain.layers.iter().any(|layer| layer.options.native_execution)
                    || String::from_utf8_lossy(&stderr).contains("hl-native-detail:")) =>
        {
            Outcome::Passed
        }
        Ok((status, stdout, stderr)) => Outcome::Failed(format!(
            "exit={status:?} expected={}; stdout={} bytes expected={} bytes; native diagnostics required={}; stderr={}",
            chain.expect.exit,
            stdout.len(),
            expected.len(),
            chain.layers.iter().any(|layer| layer.options.native_execution),
            String::from_utf8_lossy(&stderr).trim()
        )),
        Err(error) => Outcome::Failed(error),
    }
}

`src/apps/testing/src/nested.rs:246:32`

````rust
unavailable
````
