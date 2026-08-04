# `Pending`

- [ ] Approved
- Timestamp: `1785820513833168474`
- Domain: `containers`
- Package: `hl_engine`
- Rule: `struct-noun-naming`
- Severity: `error`
- Source: `src/containers/hl-engine/src/ffi/linux/execution/path/fifo.rs:126:1`
- Queue: `unclassified`
- Kind: `struct`

## Finding

`Pending` violates the noun/struct rule: type name is an adjective or past participle; name the value it represents

Help: choose a precise noun for the struct, or use #[hl_design::naming(reason = "...")] after human review

## Review

- What domain noun states this value's role?

## Decision


## Dependencies

- None detected

## Source

````rust
struct Pending {
    endpoint: Arc<dyn OpenFileDescription>,
    published: bool,
}
````

## Related context

No related locations found in the scanned tree.
