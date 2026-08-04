# `mod.rs`

- [ ] Approved
- Timestamp: `1785820518114131683`
- Domain: `containers`
- Package: `hl_engine`
- Rule: `file-length`
- Severity: `error`
- Source: `src/containers/hl-engine/src/ffi/linux/execution/routing/mod.rs:2:1`
- Queue: `unclassified`
- lines: `593`
- limit: `500`

## Finding

Rust source contains 593 lines; the maximum is 500

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
use super::process_memory::ProcessMemory;
````

## Related context

No related locations found in the scanned tree.
