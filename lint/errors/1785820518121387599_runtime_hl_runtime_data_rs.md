# `data.rs`

- [ ] Approved
- Timestamp: `1785820518121387599`
- Domain: `runtime`
- Package: `hl_runtime`
- Rule: `file-length`
- Severity: `error`
- Source: `src/runtime/hl-runtime/src/network/data.rs:1:1`
- Queue: `unclassified`
- lines: `631`
- limit: `500`

## Finding

Rust source contains 631 lines; the maximum is 500

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
use hl_linux::{Errno, GuestAccess, GuestMarshaller, GuestMemory, LinuxResult, NetworkAbi};
````

## Related context

No related locations found in the scanned tree.
