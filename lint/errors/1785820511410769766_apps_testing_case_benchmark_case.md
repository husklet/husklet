# `Case_BenchmarkCase`

- [ ] Approved
- Timestamp: `1785820511410769766`
- Domain: `apps`
- Package: `testing`
- Rule: `duplicate-entity-base`
- Severity: `error`
- Source: `src/apps/testing/src/bench/definition.rs:29:1`
- Queue: `unclassified`
- Common fields: `arguments: Vec<String>, build_flags: Vec<String>, id: String, samples: u32, timeout: u64, warmups: u32`

## Finding

`Case` and `BenchmarkCase` repeat a possible entity basis

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

### second struct; common fields: arguments: Vec<String>, build_flags: Vec<String>, id: String, samples: u32, timeout: u64, warmups: u32

`src/apps/testing/src/bench/definition.rs:72:1`

````rust
pub struct BenchmarkCase {
    pub id: String,
    pub arguments: Vec<String>,
    pub warmups: u32,
    pub samples: u32,
    pub timeout: u64,
    pub exit: i32,
    pub stdout_contains: PathBuf,
    build_flags: Vec<String>,
}
````
