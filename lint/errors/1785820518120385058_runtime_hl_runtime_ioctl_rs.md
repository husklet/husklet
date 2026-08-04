# `ioctl.rs`

- [ ] Approved
- Timestamp: `1785820518120385058`
- Domain: `runtime`
- Package: `hl_runtime`
- Rule: `file-length`
- Severity: `error`
- Source: `src/runtime/hl-runtime/src/filesystem/ioctl.rs:1:1`
- Queue: `unclassified`
- lines: `503`
- limit: `500`

## Finding

Rust source contains 503 lines; the maximum is 500

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
use hl_descriptor::{DescriptorFlags, ObjectError, ObjectKind, StatusFlags};
````

## Related context

No related locations found in the scanned tree.
