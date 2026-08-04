# `Canonical`

- [ ] Approved
- Timestamp: `1785820513833366641`
- Domain: `containers`
- Package: `hl_engine`
- Rule: `struct-noun-naming`
- Severity: `error`
- Source: `src/containers/hl-engine/src/ffi/linux/virtual/file.rs:9:1`
- Queue: `unclassified`
- Kind: `struct`

## Finding

`Canonical` violates the noun/struct rule: type name is an adjective or past participle; name the value it represents

Help: choose a precise noun for the struct, or use #[hl_design::naming(reason = "...")] after human review

## Review

- What domain noun states this value's role?

## Decision


## Dependencies

- None detected

## Source

````rust
#[derive(Debug)]
pub(super) struct Canonical {
    pub(super) file: File,
    pub(super) offset: u64,
}
````

## Related context

No related locations found in the scanned tree.
