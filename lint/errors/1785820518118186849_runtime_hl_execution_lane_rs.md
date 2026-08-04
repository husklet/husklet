# `lane.rs`

- [ ] Approved
- Timestamp: `1785820518118186849`
- Domain: `runtime`
- Package: `hl_execution`
- Rule: `file-length`
- Severity: `error`
- Source: `src/runtime/hl-execution/src/x86/vector/lane.rs:1:1`
- Queue: `unclassified`
- lines: `561`
- limit: `500`

## Finding

Rust source contains 561 lines; the maximum is 500

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
    AccessKind, CpuState, ExecutionExit, GuestOperandMemory, ScalarOperand, ScalarRegister, ScalarWidth, Staged,
    VectorArithmetic, VectorComparison, VectorOperation, VectorShiftKind, VectorShuffleMode, VectorSource,
};
````

## Related context

No related locations found in the scanned tree.
