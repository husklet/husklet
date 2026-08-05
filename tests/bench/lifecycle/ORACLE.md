# Engine lifecycle performance audit

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
