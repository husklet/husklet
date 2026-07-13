# Test ownership and performance gates

## Ownership after extraction

| Test | Owner | Language |
|---|---|---|
| translator decode/IR/lowering/state differential | `engine/tests/c/translator` | C harness + generated binary fixtures |
| Linux syscall/errno/fd/OFD/process/procfs behavior | `engine/tests/c/linux_abi` | focused Linux guest C programs and C host runner |
| host-service contract/fault injection | `engine/tests/c/host` | C, linked once per backend and fake backend |
| engine lifecycle/config/cache/runner protocol | `engine/tests/c/integration` | C |
| performance and footprint | `engine/perf` | C workloads/scripts producing machine-readable results |
| public Rust ABI, ownership and error mapping | `hl-engine/tests` | Rust, with tiny C ABI fixture where required |
| container/Docker product behavior | higher runtime/daemon tests | Rust orchestration using `hl-engine` |

Existing `dd-tests` cases move only when the engine directory contains every source fixture, compiler invocation,
runner and assertion needed. C guest fixtures remain C because they exercise the Linux/C ABI. Rust tests that merely
search C source or check a symbol string are not migrated as correctness tests.

## Compatibility matrix

Minimum engine gate for each supported guest ISA:

- ELF static/dynamic, PIE/non-PIE, TLS, `execve`, auxv and dynamic loader.
- integer/SIMD/x87/atomic instruction state, faults, self-modifying code, tiering and persistent cache.
- filesystem/overlay/path traversal, metadata, locks, high fds, dup/fork/exec and close-on-exec.
- processes, pthreads, clone flags, pid namespace, wait/rusage, signals/signal frames and seccomp/ptrace ordering.
- futexes, shared mappings, SysV/POSIX IPC and cross-process wakeups.
- epoll level/edge/oneshot, close/dup/fork, timerfd/eventfd/signalfd/inotify and high-fd readiness.
- TCP/UDP/Unix sockets, options, loopback/net namespace, DNS/netlink and published ports.
- `/proc`, `/sys`, cgroup/resource limits, clock/time64, random, tty/pty and checkpoint where supported.
- real workloads: musl/glibc shells, Go, Rust, Python, Node/V8, JVM, databases, Chrome and GUI support processes.

Host-independent expected outputs are shared. Platform-specific fixture code may validate backend internals, but guest
observable differences require an explicit capability/policy rationale.

## Surface tests for `hl-engine`

Keep this suite intentionally small:

1. ABI/version and config-size skew rejection.
2. Artifact selection for host OS/CPU + guest ISA and missing/corrupt artifact errors.
3. One tiny guest launch, stdout/stderr/stdin, exit status and cancellation.
4. FD/handle borrow-versus-transfer ownership, including failure paths and drop.
5. Runner crash/protocol truncation/timeout mapping with no leaked process or descriptor.
6. C/Rust golden encoding parity for the launch config.

Do not copy syscall, opcode or performance coverage into Rust bindings. That would force the future standalone C
repository to depend on a parent Rust test suite.

## Performance baseline

Performance acceptance uses distributions from pinned workloads, machines, compiler flags and cache state. Record:

| Dimension | Metrics |
|---|---|
| build/artifact | text/data/BSS/TLS/file size, exported symbols, linked platform libraries |
| startup | runner spawn, engine create, ELF load, first instruction, peak/RSS before guest work |
| translation | guest instructions/sec, translated blocks, emitted bytes, IR/lowering time, cache flushes |
| execution | steady workload throughput, dispatcher exits, direct-chain/IBTC hit rates, tier promotions |
| syscall | latency/throughput for getpid, clock, read/write, open/stat, futex, epoll and sockets |
| cache | cold save, warm load, hit rate, relocation count, incompatible-cache rejection |
| concurrency | 1/2/4/8/32 threads/processes, wake latency, contention and scaling |

Run at least enough repetitions to report median and tail/dispersion; never accept one wall-clock sample. Separate
cold and warm pcache directories. A change fails if correctness differs, required metrics disappear, or a statistically
credible regression exceeds the wave's predeclared budget. Initially use conservative budgets: no >2% steady-state
regression, >5% cold-start regression or unbounded memory growth without explicit review.

## Split-specific gates

- Object-library split: compare target output and full behavior; binary byte equality is not required, but symbol
  visibility and performance are.
- State deglobalization: two concurrent instances, repeated create/destroy, allocation failure and sanitizer runs.
- Host-service extraction: common contract suite plus guest-visible cross-backend differential.
- IR migration: native/QEMU register/memory/fault differential, emitted-code size and pcache identity.
- Linux/Windows backend: common host contract and the full compatibility matrix, with required prerequisites failing
  preflight rather than returning success after a skip.

## CI tiers

- Tier A on every change: pure C compile, fake-host unit tests, Rust surface tests, formatting/static analysis.
- Tier B per platform: native host backend contract, both available guest engines, sanitizers where compatible.
- Tier C scheduled/release: application matrix, checkpoint/fork stress, high limits, cold/warm performance distributions.
- Tier D release packaging: clean-room build, artifact manifest, signing, installation discovery and license/source bundle.
