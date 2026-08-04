# `table.rs`

- [ ] Approved
- Timestamp: `1785820518119284433`
- Domain: `runtime`
- Package: `hl_linux`
- Rule: `file-length`
- Severity: `error`
- Source: `src/runtime/hl-linux/src/syscall/table.rs:1:1`
- Queue: `unclassified`
- lines: `572`
- limit: `500`

## Finding

Rust source contains 572 lines; the maximum is 500

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
use hl_isa::GuestArchitecture;
````

## Related context

No related locations found in the scanned tree.
