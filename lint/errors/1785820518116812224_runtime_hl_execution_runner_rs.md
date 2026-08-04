# `runner.rs`

- [ ] Approved
- Timestamp: `1785820518116812224`
- Domain: `runtime`
- Package: `hl_execution`
- Rule: `file-length`
- Severity: `error`
- Source: `src/runtime/hl-execution/src/execution/runner.rs:1:1`
- Queue: `unclassified`
- lines: `725`
- limit: `500`

## Finding

Rust source contains 725 lines; the maximum is 500

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
    Aarch64Decoder, Aarch64ExecutionExit, Aarch64Interpreter, Aarch64Ir, AccessKind, BlockIdentity, CacheObservation,
    DispatchDecision, ExclusiveMemory, ExecutionCpuSnapshot, ExecutionExit, ExecutionMachine, GuestOperandMemory,
    GuestSystemPort, MemoryFault, PcCoordinatePort, ScalarInterpreter, ScalarIr, X86ScalarDecoder,
    aarch64::register::RegisterExecutor,
};
````

## Related context

No related locations found in the scanned tree.
