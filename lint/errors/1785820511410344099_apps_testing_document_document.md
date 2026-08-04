# `Document_Document`

- [ ] Approved
- Timestamp: `1785820511410344099`
- Domain: `apps`
- Package: `testing`
- Rule: `duplicate-entity-base`
- Severity: `error`
- Source: `src/apps/testing/src/bench/definition.rs:10:1`
- Queue: `unclassified`
- Common fields: `build: Build, cases: Vec<Case>, execution: Execution, image: String`

## Finding

`Document` and `Document` repeat a possible entity basis

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
struct Document {
    image: String,
    #[serde(default)]
    execution: Execution,
    build: Build,
    cases: Vec<Case>,
}
````

## Related context

### second struct; common fields: build: Build, cases: Vec<Case>, execution: Execution, image: String

`src/apps/testing/src/runtime/definition.rs:11:1`

````rust
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    image: String,
    #[serde(default)]
    execution: Execution,
    artifact: Artifact,
    build: Build,
    oracle: Option<Commands>,
    cases: Vec<Case>,
}
````
