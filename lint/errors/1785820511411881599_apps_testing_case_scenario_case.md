# `Case_ScenarioCase`

- [ ] Approved
- Timestamp: `1785820511411881599`
- Domain: `apps`
- Package: `testing`
- Rule: `duplicate-entity-base`
- Severity: `error`
- Source: `src/apps/testing/src/scenario/definition.rs:15:1`
- Queue: `unclassified`
- Common fields: `class: Class, environment: BTreeMap<String, String>, execution: Execution, id: String, image: String, resources: Vec<Resource>, timeout: u64`

## Finding

`Case` and `ScenarioCase` repeat a possible entity basis

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

## Related context

### second struct; common fields: class: Class, environment: BTreeMap<String, String>, execution: Execution, id: String, image: String, resources: Vec<Resource>, timeout: u64

`src/apps/testing/src/scenario/definition.rs:192:1`

````rust
pub struct ScenarioCase {
    pub id: String,
    pub image: String,
    pub execution: Execution,
    pub class: Class,
    pub targets: Vec<Target>,
    pub expected_failures: Vec<Target>,
    pub resources: Vec<Resource>,
    pub environment: BTreeMap<String, String>,
    pub working_directory: String,
    pub actions: Vec<ScenarioAction>,
    pub fixtures: Vec<ScenarioFixture>,
    pub readiness: Option<ScenarioReadiness>,
    pub timeout: u64,
    pub exit: i32,
    pub stdout_contains: Vec<PathBuf>,
    pub stdout_exact: Option<PathBuf>,
}
````
