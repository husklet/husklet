# Phase 1 — cleanup audit

Status: research complete enough to begin reviewed cleanup batches; no cleanup is authorized by this
index. All detailed evidence and manifests are carried in [`research/`](research/README.md).

The current-tree action classification is maintained in [`disposition-ledger.md`](disposition-ledger.md).
Implementation batches and proof gates are in [`execution-plan.md`](execution-plan.md).

## Exit condition

Phase 1 is complete when accepted candidates have been either removed, converted into an implementation
task, or explicitly retained with a compatibility/performance reason; completed rows then disappear from
the active cleanup ledger. Cleanup patches require Rust/C behavioral tests owned by the affected crate.

## Execution batches

| Batch | Scope | Current disposition |
|---|---|---|
| A1 | captured scratch/rootfs/artifact island | delete after the two unique checkpoint behaviors are migrated |
| A2 | compiler-proven private symbols and no-op features | smallest behavior-neutral source cuts |
| A3 | abandoned diagnostics and dense default-off state | remove feature as a unit; verify memory and engine behavior |
| A4 | legacy compositor | retain until Smithay is default and live parity gates pass |
| A5 | duplicated compositor mechanics | centralize exact contracts; keep divergent state machines separate |
| A6 | source-inspection and false-green tests | remove/replace during phase 2, not as isolated deletions |
| A7 | stale docs, comments, host paths, packaging branches | update alongside the owning implementation decision |
| A8 | unsafe/FFI ownership leaks and false-success fallbacks | correctness fixes, not dead-code deletion |

The authoritative findings, measurements, false-positive protections, and acceptance commands are linked
from the [audit index](research/README.md). Phase 2 consumes its test-value findings; phase 3 consumes
its package, path, environment, wire, and persisted-format inventories.

## Things cleanup must not remove

- exported shim/loader ABI merely because Rust has no static caller;
- persisted-format readers before an explicit migration and support window;
- CPU/headless render fallbacks or retryable presentation state;
- architecture-specific JIT paths based on warnings from only one unity translation unit;
- protocol globals whose missing behavior needs implementation rather than concealment;
- checked-in guest binaries until their source/build hash and toolchain availability are reproducible.
