# `RawCase_Case`

- [ ] Approved
- Timestamp: `1785820511411604308`
- Domain: `apps`
- Package: `testing`
- Rule: `duplicate-entity-base`
- Severity: `error`
- Source: `src/apps/testing/src/runtime/definition.rs:38:1`
- Queue: `unclassified`
- Common fields: `environment: BTreeMap<String, String>, expect: Expect, id: String, timeout: u64`

## Finding

`RawCase` and `Case` repeat a possible entity basis

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
struct RawCase {
    id: String,
    run: Vec<String>,
    #[serde(default)]
    environment: BTreeMap<String, String>,
    #[serde(default = "timeout")]
    timeout: u64,
    expect: Expect,
}
````

## Related context

### second struct; common fields: environment: BTreeMap<String, String>, expect: Expect, id: String, timeout: u64

`src/apps/testing/src/scenario/definition.rs:15:1`

````rust
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    image: String,
    #[serde(default)]
    execution: Execution,
    #[serde(default)]
    class: Class,
    #[serde(default)]
    targets: Vec<Platform>,
    #[serde(default)]
    xfail: Vec<Platform>,
    #[serde(default)]
    resources: Vec<Resource>,
    #[serde(default)]
    environment: BTreeMap<String, String>,
    #[serde(default)]
    fixtures: Vec<Fixture>,
    #[serde(default)]
    actions: Vec<Action>,
    run: Option<Run>,
    #[serde(default)]
    readiness: Option<Readiness>,
    #[serde(default = "timeout")]
    timeout: u64,
    expect: Expect,
}
````
