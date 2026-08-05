# Engine lifecycle performance audit

## 2026-08-04: lazy deadline worker

The retained timer ownership was inspected read-only at C tree
`7b7bddddfe7fc32f98a74579f38ee92b3a76fcdc`.  The lifecycle path remains
`src/core/target/aarch64.c` (`hl_run_linux_guest`, `container_init`,
`engine_global_init`, `load_program`, and `run_loaded`) and
`src/linux_abi/fork.c` (resident-parent initialization, child repair, wait, and
teardown).  Timed waits in `src/linux_abi/syscall/time.c` execute in the calling
guest thread.  Its process-timer owner (`gtimer_init`, `gtimer_loop`, and
`gtimer_atfork_child`) creates the shared host-event set and background thread
only on the first `timer_create`; forked children discard that inherited worker
identity and lazily create their own.  Initialization of an Alpine process that
does not request a timed operation therefore creates no C timer thread.

Rust `routing::composition::create` constructs one deadline queue per process.
Previously `readiness::deadline::Queue::new` immediately created an
`hl-deadline` thread even when the guest never scheduled a deadline.  The queue
owns its eventfd, monotonic origin, registrations, condition variables, and
worker join handle.  The worker owns no independent registration state and is
needed only after `schedule` or `schedule_callback`.  It is now started under
the join-handle mutex by the first insertion.  Concurrent first insertions
serialize there; the worker is published before registration, then observes
the registration under the queue-state mutex and its condition notification.
Drop still marks the state stopped, wakes both conditions, joins the worker when
one exists, and closes the eventfd exactly once.  Schedule failure remains
`ClockError::Failed`; cancellation, wake ordering, capacity, and callback-before-
readiness publication are unchanged.

This is a resource reduction rather than a claimed latency improvement until an
exact committed-tree alternating measurement is recorded.  The focused
deadline cohort includes construction-without-worker, first-schedule activation,
capacity release, rescheduling, interruption, and callback publication.

## 2026-08-04: idle network readiness worker

Exact baseline commit `3e039c86ce62bc800c29b6eb85d9c8e4b17114ae` and
candidate commit `1209c005fcf01aac625b429ec90a0840f51edff5` were built
warning-strict in separate release target directories.  Their `hl-engine`
SHA-256 values were respectively
`431b53ab1d83f970ee1d93eedc6c1cd4f12ca44b991954e6c9b5226d322aecfe`
and `44b190f4c9350b425370af8b1e769752c0654f6ab38b5441579987a7ebac6e42`.
The guest remained the static ARM64 lifecycle image with SHA-256
`c98f0f9cd55c7576079fd5a5b266f8ee66c0cf31c0bbda903a56342c98324016`.

Thirty-one alternating paired fresh-process samples pinned to CPU 17 produced
these host-observed results:

| tree | minimum (us) | median (us) | maximum (us) |
|---|---:|---:|---:|
| baseline | 5,044 | 5,287 | 7,341 |
| candidate | 5,033 | 5,317 | 6,416 |

The 30 us median difference is noise and is not claimed as a latency win.
`strace -f -c`, however, records an exact resource change for the same guest:
the baseline creates seven threads and the candidate creates six.  The omitted
thread is the `hl-inet-ready` reactor; the candidate starts it transactionally
when the first owned socket enters the native table.  Socket workloads retain
the reactor and wake-pipe behaviour after that first insertion.  This removes
one idle thread and its 2 MiB default stack reservation from every process that
does no socket work without weakening network readiness.

The retained C lifecycle ownership was rechecked in
`../engine/src/core/target/aarch64.c` (`hl_run_linux_guest`, `container_init`,
and `engine_global_init`), `../engine/src/linux_abi/fork.c` (resident parent,
worker repair, wait, and teardown), and `../engine/src/linux_abi/syscall/net.c`
(the socket entry points and direct host readiness operations).  The retained
engine does not construct a per-guest network thread: socket state comes to
life at the socket syscall and host `poll`/event operations own blocking and
wakeup ordering.  Rust previously constructed `network::Native` from
`routing::composition::create` for every guest, and `Native::with_authority`
immediately spawned the readiness reactor even when no socket could reach it.
The Rust owner remains process-local: `Native` owns the table and pipe,
`Reactor` borrows it through `Weak`, first insertion publishes the socket
before starting and waking the worker, and final `Native` drop wakes the worker
so its failed upgrade terminates it.  Concurrent first insertions are serialized
by an atomic start transition; all socket identities, error paths, polling,
and teardown remain unchanged.

Exact candidate verification used `RUSTFLAGS='-D warnings'`.  The focused
network unit cohort passed 26/26, both public Linux socket integration tests
passed, and the new admission test proves construction is idle and first socket
insertion starts the worker.  The current baseline is already materially newer
than the original 21.8 ms checkpoint: in the same timing window it ran at a
5.287 ms median, versus 1.555 ms host-native and 16.116 ms for the retained C
engine.  Rust therefore already beats the retained C fresh-process lifecycle
by 3.05x on this exact checkpoint, while remaining 3.40x slower than native.

## Exact evidence

The measured Husklet tree is `eb14c27367d4e38af339bd5567de799fe9a73b04`.
The retained read-only C tree is
`7b7bddddfe7fc32f98a74579f38ee92b3a76fcdc`. The arm64 guest is a static PIE
whose SHA-256 is
`c98f0f9cd55c7576079fd5a5b266f8ee66c0cf31c0bbda903a56342c98324016`.

Thirty-one fresh-process samples on Linux arm64 produced these host-observed
times:

| provider | minimum (us) | median (us) | maximum (us) |
|---|---:|---:|---:|
| native | 1,581 | 1,822 | 3,034 |
| retained C | 11,663 | 12,287 | 15,855 |
| Rust | 19,889 | 21,846 | 116,960 |

The repository benchmark adapter independently reported 21-sample median wall
times of 227 us, 13,092 us, and 23,466 us respectively. Its 10 ms `try_wait`
poll interval quantizes short-lived providers, so those wall values identify the
same ordering but are not accepted as sub-10-ms phase evidence. The lifecycle
guest deliberately reports zero guest work; these results therefore measure
process plus engine construction, start, wait, and teardown rather than translated
work.

## Retained implementation studied

The retained C call path was inspected read-only in:

- `tools/lifecycle_e2e_runner.c`, the host-service owner and direct
  `hl_run_linux_guest` caller;
- `src/core/target/aarch64.c`: `hl_run_linux_guest`, `container_init`,
  `engine_global_init`, `load_program`, and `run_loaded`;
- `src/linux_abi/fork.c`: resident server/client/worker ownership and the
  prepare/restore transition around `fork`;
- `cmake/Phase3Gates.cmake`, which proves the measured lifecycle runner uses the
  production target object shape with only the runner translation unit changed.

One invocation owns one Linux ABI box and host-service table. Initialisation
binds the host and guest-memory services, resets per-container accounting and
identity, constructs filesystem/container state, then installs process-global
fault, thread-local, and code-cache state. The standalone path performs all of
that before loading and running the image. The optional resident path deliberately
separates container/global initialisation and image preload from worker execution;
the parent owns stable initialized state and each request uses a forked COW worker.
The child repairs locks, thread registries, code mappings, descriptors, and
fork-critical state before entering the guest. Worker exit is waited and reaped;
global parent state survives until server teardown. Errors remain explicit exit
or typed engine status values. Architecture-specific target files own ELF and CPU
entry details; the fork-server lifecycle is shared.

## Rust ownership and gap

The corresponding Rust path was inspected in:

- `src/containers/hl-engine/src/runtime/api.rs`: `Builder::build`,
  `Engine::construct`, `start`, `wait`, and `destroy`;
- `src/containers/hl-engine/src/runtime/machine.rs`:
  `RustRuntimeFactory::construct` and `RustRuntimeMachine`;
- `src/containers/hl-engine/src/composition.rs`: `MachineLauncher::launch`,
  `wait`, and `Drop`;
- `src/containers/hl-engine/src/engine.rs`: lifecycle state, workspace ownership,
  start/wait/destroy ordering;
- `src/containers/hl-engine/src/ffi/linux/execution/service.rs`:
  `GuestExecutor::start`, `wait`, and `stop`.

Rust constructs every runtime domain and workspace per engine, then creates a
new named host thread for each start. The worker initializes cancellation,
architectural counter, and a 4096-slot thread set before executing; `wait` joins
the worker and teardown destroys the workspace. Locks and condition variables
serialize only the lifecycle transitions and preserve typed start/wait failure.
There is no resident runtime/fork-worker owner corresponding to the retained C
warm path, and an `EngineBackend` cannot be started again after its worker has
been consumed because the machine retains the prior exit.

`strace -f -c` on the minimal Rust launch observed seven `clone3` calls, 88
futex operations, and 453 total syscalls. This is source-consistent with domain
construction and per-launch worker/service threads. It does not by itself assign
all elapsed time to thread creation, so no narrow thread deletion is justified.

The largest coherent optimization is a reusable engine-domain owner which keeps
immutable runtime services and validated image state warm while creating a fresh,
isolated process/task/workspace generation per request. That requires explicit
reset and teardown contracts across every stateful runtime domain. Reusing the
current assembly or merely allowing `MachineLauncher` to start twice would leak
exit, task, descriptor, memory, and cancellation state and is therefore not a
valid optimization. This bounded lane records the gap instead of introducing an
unsafe partial fork-server analogue.
