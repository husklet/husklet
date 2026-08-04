# `definition.rs`

- [ ] Approved
- Timestamp: `1785820518111168974`
- Domain: `apps`
- Package: `testing`
- Rule: `file-length`
- Severity: `error`
- Source: `src/apps/testing/src/scenario/definition.rs:1:1`
- Queue: `unclassified`
- lines: `528`
- limit: `500`

## Finding

Rust source contains 528 lines; the maximum is 500

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
use crate::suite::{Error, Execution, Target};
````

## Related context

No related locations found in the scanned tree.
