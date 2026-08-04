# `decode.rs`

- [ ] Approved
- Timestamp: `1785820518116622266`
- Domain: `runtime`
- Package: `hl_execution`
- Rule: `file-length`
- Severity: `error`
- Source: `src/runtime/hl-execution/src/aarch64/simd/decode.rs:1:1`
- Queue: `unclassified`
- lines: `605`
- limit: `500`

## Finding

Rust source contains 605 lines; the maximum is 500

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
use crate::{
    Aarch64DecodeError, Aarch64Instruction, NarrowMode, SimdCopy, SimdLaneOperation, SimdLogic, SimdPermute,
    SimdReduceOperation, SimdShift, SimdUnary, SimdWideOperation,
};
````

## Related context

No related locations found in the scanned tree.
