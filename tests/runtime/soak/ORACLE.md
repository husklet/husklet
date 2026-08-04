# Soak workload and scheduler oracle audit

This category consolidates the 18 retained soak manifest rows into 17 independent
cases. `threadpool` owns separate AArch64 and x86-64 rows with one shared source
and verdict; `smc2` remains AArch64-only. Every other case targets both guest
ISAs. Sources and byte-exact stdout goldens are unchanged from
`tests/runtime/legacy/oracle/tests/soak` before it was migrated by commit `e8b4e4ac9`.

All cases retain the legacy `-static-pie -O2 -std=gnu11 -pthread -lm` build
contract, zero exit status, and deterministic stdout comparison. The bounded
profile allows 240 seconds and one repetition. The retained extended profile is
an operator-selected repetition increase to ten; it does not alter an individual
attempt's deadline or verdict.

## Retained C execution oracle

The read-only C engine and harness study covered:

- `../engine/tools/matrix_runner.c`: `suite_case_timeout_ms`,
  `case_timeout_ms`, `stall_timeout_ms`, both platform launch paths,
  `run_case`, `timeout_reason`, and `main`;
- `../engine/tools/remote_supervisor.c`: `terminate_group`, `on_signal`, and
  `main`;
- `../engine/src/core/activation.c`: POSIX process-group creation and
  termination plus Windows job-object creation, ownership, and teardown.

The matrix runner owns manifest order, repetition order, one active attempt's
deadline, bounded output capture, and its final verdict. Each repetition is a
fresh launch. On POSIX the launch owns a distinct process group; timeout or
interruption sends `SIGTERM`, waits for a bounded grace interval, sends
`SIGKILL`, and reaps. On Windows the job object owns the descendant tree and is
terminated or closed during teardown. Interrupted waits do not bypass cleanup.
Timeout, stall, output overflow, signal termination, nonzero exit, and output
mismatch remain distinct failures, and a later successful repetition cannot hide
an earlier failure.

The retained parser rejects zero repetitions and values above 10,000. Husklet's
typed plan deliberately narrows checked-in admission to the bounded one-run
profile and explicitly declares CPU, memory, and process demand. Schedule-size
arithmetic must be checked before creating a guest or container.

## Rust ownership and remaining gap

`src/apps/testing/src/runtime/scheduler.rs` owns typed resource admission and
deterministic attempt enumeration. Runtime execution owns container creation,
deadlines, bounded capture, result classification, and removal. The runtime
task/process owners are responsible for cancellation and descendant teardown;
the testing scheduler must not reproduce those engine entities.

The schema records resource demand, but that alone does not prove host-side
enforcement or complete process-tree containment. Acceptance therefore still
requires focused execution evidence showing that each platform maps the declared
bounds to its container/process owner and leaves no descendants. The source and
golden migration makes no broader claim about runtime-domain or performance
parity.

## Case-specific retained semantics

- `reallocchurn` retains the historical macOS process-group-cleanup exclusion;
  Linux execution remains part of the compatibility evidence.
- `threadpool` retains the x86-64 private/shared futex split evidence from
  revision `d6104e31` as well as the independent AArch64 row.
- `smc2` retains the AArch64-only RWX/self-modifying-code contract, including
  precise translated-code invalidation.
