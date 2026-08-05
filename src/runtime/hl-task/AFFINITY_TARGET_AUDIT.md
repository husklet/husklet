# Affinity target lookup audit

## Retained C oracle

The retained engine was inspected read-only at revision
`7b7bddddfe7fc32f98a74579f38ee92b3a76fcdc` in:

- `src/linux_abi/syscall/proc.c::sched_pid_live` and the
  `sched_setaffinity`/`sched_getaffinity` cases;
- `src/linux_abi/thread.c::thread_tid_alive`, `thread_register`,
  `thread_unregister`, and `thread_after_fork`;
- `src/linux_abi/affinity.c::{hl_linux_affinity_get,
  hl_linux_affinity_set}`.

The C engine owns a fixed 4,096-entry live-thread registry under
`g_threg_m`. Affinity target validation recognizes the caller and init first,
then `thread_tid_alive` scans that entire registry while holding the mutex.
Registration and unregister own entry lifetime; fork repairs the inherited
registry so only the surviving thread remains. A missing in-container identity
returns `ESRCH`. Affinity mask state is process-global in the retained engine,
so the scan proves liveness rather than selecting per-thread mask storage.

The retained implementation therefore establishes the required Linux ordering
and lifecycle behavior, but its lookup is linear in the fixed registry size.
It is a behavioral oracle, not a performance pattern to preserve.

## Rust ownership and change

Rust `hl-task::TaskRegistry` owns generation-qualified process and thread slot
tables. Guest numeric PID/TID `n` is defined by slot `n - 1`; allocation,
fork, clone, exec, exit, reap, and slot reuse all preserve that identity rule.
The old `affinity_target` nevertheless enumerated both tables to rediscover the
slot number. The replacement indexes the one corresponding slot directly,
prefers a live thread exactly as before, and then checks the process leader.

Target zero still validates and returns the generation-qualified caller.
Negative and out-of-range values fail. Slot reuse returns the replacement's
current generation, while stale internal handles remain rejected by ordinary
registry access. Forked leaders, leader PID/TID identity, nonleader exec, and
thread/process collision exclusion are covered by focused lifecycle tests.
The registry mutex and all state ownership, teardown, and errno conversion are
unchanged.

## Exact evidence

The accepted comparison used exact base `d9ef29fd1`, production candidate
`7e53d0506`, and test candidate `4d38a825b`. The ignored diagnostic constructed
all 4,096 thread slots outside the timed interval and performed 200,000 lookups
of the last live slot. Three warmups preceded eleven alternating CPU-17 pairs.

Baseline ns/lookup, sorted:

```text
2066 2066 2067 2077 2081 2082 2088 2090 2094 2099 2145
```

Candidate ns/lookup, sorted:

```text
110 111 111 111 111 112 112 114 115 115 117
```

The median improved from 2,082 ns to 112 ns, or 18.59 times. Candidate p90 was
115 ns versus 2,099 ns, and its maximum 117 ns remained below the baseline
minimum 2,066 ns. Seven focused lifecycle tests passed. The complete candidate
suite reported 128 passes, the same three pre-existing failures as the exact
baseline, and one ignored diagnostic; it introduced no failure. Raw evidence is
preserved at `/tmp/task-lookup-results-d9ef` on the measurement host.
