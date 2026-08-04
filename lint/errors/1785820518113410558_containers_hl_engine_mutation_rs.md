# `mutation.rs`

- [ ] Approved
- Timestamp: `1785820518113410558`
- Domain: `containers`
- Package: `hl_engine`
- Rule: `file-length`
- Severity: `error`
- Source: `src/containers/hl-engine/src/ffi/linux/execution/path/mutation.rs:1:1`
- Queue: `unclassified`
- lines: `584`
- limit: `500`

## Finding

Rust source contains 584 lines; the maximum is 500

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
use std::ffi::CString;
````

## Related context

No related locations found in the scanned tree.
