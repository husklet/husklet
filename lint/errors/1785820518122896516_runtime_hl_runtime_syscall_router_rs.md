# `syscall_router.rs`

- [ ] Approved
- Timestamp: `1785820518122896516`
- Domain: `runtime`
- Package: `hl_runtime`
- Rule: `file-length`
- Severity: `error`
- Source: `src/runtime/hl-runtime/src/syscall_router.rs:1:1`
- Queue: `unclassified`
- lines: `551`
- limit: `500`

## Finding

Rust source contains 551 lines; the maximum is 500

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
use std::sync::{Arc, Mutex};
````

## Related context

No related locations found in the scanned tree.
