# `Build_Build`

- [ ] Approved
- Timestamp: `1785820511410567016`
- Domain: `apps`
- Package: `testing`
- Rule: `duplicate-entity-base`
- Severity: `error`
- Source: `src/apps/testing/src/bench/definition.rs:20:1`
- Queue: `unclassified`
- Common fields: `compiler: Commands, flags: Vec<String>, source: PathBuf`

## Finding

`Build` and `Build` repeat a possible entity basis

Help: extract a shared base entity and compose specialization, or prove the fields have different semantics

## Review

- Do these fields share identity, invariants, lifecycle, and meaning?

## Decision


## Dependencies

- None detected

## Source

````rust
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Build {
    source: PathBuf,
    compiler: Commands,
    #[serde(default)]
    flags: Vec<String>,
}
````

## Related context

### second struct; common fields: compiler: Commands, flags: Vec<String>, source: PathBuf

`src/apps/testing/src/runtime/definition.rs:29:1`

````rust
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Build {
    source: PathBuf,
    output: String,
    compiler: Commands,
    flags: Vec<String>,
}
````
