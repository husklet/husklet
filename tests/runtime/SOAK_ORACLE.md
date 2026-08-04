# Soak scheduling oracle audit

The migrated case sources and expected output come from the retained soak
manifest, but the retained execution oracle is also material. The scheduler
audit studied these read-only C owners in `../engine`:

- `tools/matrix_runner.c`: `suite_case_timeout_ms`, `case_timeout_ms`,
  `stall_timeout_ms`, the POSIX and Windows case launch paths, `run_case`,
  `timeout_reason`, and `main`;
- `tools/remote_supervisor.c`: `terminate_group`, `on_signal`, and `main`;
- `src/core/activation.c`: POSIX process-group creation/termination and the
  Windows job-object lifecycle used as the stronger descendant owner.

## Ownership and lifecycle

The matrix runner owns the selected manifest rows, repetition loop, one active
case deadline, bounded output capture, and final verdict. On POSIX, each launch
creates a distinct process group before execution. Timeout or interruption
sends `SIGTERM`, allows a bounded grace period, sends `SIGKILL`, and reaps the
child. The remote supervisor uses the same group identity and teardown order.
On Windows, the activation job object owns descendants that could otherwise
escape a process group; closing or terminating that job retires the complete
launch. The runner never treats an unreaped or output-overflowing launch as a
pass.

The retained repetition parser rejects zero and values above 10,000. Husklet's
typed soak plan deliberately strengthens that admission boundary to 100 and
also bounds per-attempt wall duration, CPU admission units, memory MiB, and
process count. Checked arithmetic rejects an overflowing total schedule before
any guest or container is created.

## Ordering and result semantics

Rows run in manifest order and repetitions run in increasing ordinal order.
Each attempt gets a fresh launch and teardown; output, exit status, timeout,
stall, and resource failure remain distinct verdicts. A partial or timed-out
attempt cannot be hidden by a later success. Signals may interrupt host waits,
but cleanup and reap remain obligatory. The Rust owner is
`src/apps/testing/src/runtime/scheduler.rs` for typed admission and deterministic
attempt enumeration, with runtime execution owning container launch, deadline,
capture, and removal.

## Remaining gap

The typed Rust scheduler represents resource demand but does not by itself
prove host enforcement or process-tree containment. Acceptance requires the
runtime executor to admit these resources before launch and map them to bounded
container/process ownership, including platform-specific descendant teardown.
Until that wiring is demonstrated, the resource fields are fail-closed schema
and scheduling evidence, not a claim of complete parity with the retained
activation job/process-group machinery.
