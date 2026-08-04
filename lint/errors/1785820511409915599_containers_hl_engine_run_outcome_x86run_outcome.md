# `RunOutcome_X86RunOutcome`

- [ ] Approved
- Timestamp: `1785820511409915599`
- Domain: `containers`
- Package: `hl_engine`
- Rule: `duplicate-entity-base`
- Severity: `error`
- Source: `src/containers/hl-engine/src/native/executor.rs:367:1`
- Queue: `unclassified`
- Common fields: `code: u64, executed: u64, exit: Exit, instruction: u64, remaining: u64`

## Finding

`RunOutcome` and `X86RunOutcome` repeat a possible entity basis

Help: extract a shared base entity and compose specialization, or prove the fields have different semantics

## Review

- Do these fields share identity, invariants, lifecycle, and meaning?

## Decision


## Dependencies

- None detected

## Source

````rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RunOutcome {
    pub(crate) exit: Exit,
    pub(crate) instruction: u64,
    pub(crate) code: u64,
    pub(crate) remaining: u64,
    pub(crate) executed: u64,
}
````

## Related context

### second struct; common fields: code: u64, executed: u64, exit: Exit, instruction: u64, remaining: u64

`src/containers/hl-engine/src/native/executor.rs:376:1`

````rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct X86RunOutcome {
    pub(crate) exit: Exit,
    pub(crate) instruction: u64,
    pub(crate) next: u64,
    pub(crate) address: u64,
    pub(crate) code: u64,
    pub(crate) remaining: u64,
    pub(crate) executed: u64,
}
````
