# `Case_RawCase`

- [ ] Approved
- Timestamp: `1785820511410953266`
- Domain: `apps`
- Package: `testing`
- Rule: `duplicate-entity-base`
- Severity: `error`
- Source: `src/apps/testing/src/bench/definition.rs:29:1`
- Queue: `unclassified`
- Common fields: `expect: Expect, id: String, timeout: u64`

## Finding

`Case` and `RawCase` repeat a possible entity basis

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
    #[serde(default)]
    build_flags: Vec<String>,
    #[serde(default)]
    arguments: Vec<String>,
    #[serde(default = "warmups")]
    warmups: u32,
    #[serde(default = "samples", alias = "repetitions")]
    samples: u32,
    #[serde(default = "timeout")]
    timeout: u64,
    expect: Expect,
}
````

## Related context

### second struct; common fields: expect: Expect, id: String, timeout: u64

`src/apps/testing/src/runtime/definition.rs:38:1`

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
