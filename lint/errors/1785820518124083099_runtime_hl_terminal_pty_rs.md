# `pty.rs`

- [ ] Approved
- Timestamp: `1785820518124083099`
- Domain: `runtime`
- Package: `hl_terminal`
- Rule: `file-length`
- Severity: `error`
- Source: `src/runtime/hl-terminal/src/pty.rs:1:1`
- Queue: `unclassified`
- lines: `603`
- limit: `500`

## Finding

Rust source contains 603 lines; the maximum is 500

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
use std::collections::{BTreeMap, VecDeque};
````

## Related context

No related locations found in the scanned tree.
