# `Readiness_ScenarioReadiness`

- [ ] Approved
- Timestamp: `1785820511412013391`
- Domain: `apps`
- Package: `testing`
- Rule: `duplicate-entity-base`
- Severity: `error`
- Source: `src/apps/testing/src/scenario/definition.rs:129:1`
- Queue: `unclassified`
- Common fields: `attempts: u32, delay_ms: u64, logs: Vec<String>, probe: String, startup: String`

## Finding

`Readiness` and `ScenarioReadiness` repeat a possible entity basis

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
struct Readiness {
    startup: String,
    probe: String,
    attempts: u32,
    delay_ms: u64,
    #[serde(default)]
    logs: Vec<String>,
}
````

## Related context

### second struct; common fields: attempts: u32, delay_ms: u64, logs: Vec<String>, probe: String, startup: String

`src/apps/testing/src/scenario/definition.rs:224:1`

````rust
pub struct ScenarioReadiness {
    pub startup: String,
    pub probe: String,
    pub attempts: u32,
    pub delay_ms: u64,
    pub logs: Vec<String>,
}
````
