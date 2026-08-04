# `transfer.rs`

- [ ] Approved
- Timestamp: `1785820518113616433`
- Domain: `containers`
- Package: `hl_engine`
- Rule: `file-length`
- Severity: `error`
- Source: `src/containers/hl-engine/src/ffi/linux/execution/path/transfer.rs:1:1`
- Queue: `unclassified`
- lines: `548`
- limit: `500`

## Finding

Rust source contains 548 lines; the maximum is 500

Help: split by cohesive entity, component, screen region, adapter, or service; do not use include! or arbitrary numbered fragments

## Review

- Which independent responsibilities are mixed in this file?
- Does each extracted module have a precise domain name and dependency direction?
- Can the split be tested without relying on source-text assertions?

## Decision


## Dependencies

- None detected

## Source

````rust
use std::collections::BTreeMap;
````

## Related context

No related locations found in the scanned tree.
