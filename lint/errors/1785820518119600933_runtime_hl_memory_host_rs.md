# `host.rs`

- [ ] Approved
- Timestamp: `1785820518119600933`
- Domain: `runtime`
- Package: `hl_memory`
- Rule: `file-length`
- Severity: `error`
- Source: `src/runtime/hl-memory/src/mapping/host.rs:1:1`
- Queue: `unclassified`
- lines: `644`
- limit: `500`

## Finding

Rust source contains 644 lines; the maximum is 500

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
use super::plan::{Batch, PlannedOperation};
````

## Related context

No related locations found in the scanned tree.
