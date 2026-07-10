# Compatibility and Performance Focus

Date: 2026-07-10

Current manager priority is to prove bugs that break real workloads or make them slow/flaky:

- JIT cache invalidation and stale-code bugs.
- Opcode semantic mismatches that silently corrupt output.
- Syscall behavior that causes common software probes to choose the wrong path, hang, spin, leak fds, or lose wakeups.
- Docker API/build/runtime behavior that diverges from expected workflows.
- Race conditions in fd, pid, epoll/eventfd, networking, rendering, and daemon lifecycle state.
- Memory leaks and unbounded cache/state growth.
- False-green test/build gates that hide compatibility regressions.

## Active High-Value Targets

| Area | Target | Why it matters |
|---|---|---|
| JIT/cache | executable VA unmap/remap with stale translated block | guest JITs, loaders, and allocators can execute old code |
| JIT/opcodes | AVX scalar merge, F16C rounding, SSE4.2 flags | silent data corruption in optimized libc/crypto/math paths |
| syscalls | epoll/eventfd/timerfd/fork/exec lifecycle races | wakeup loss, hangs, fd reuse corruption |
| daemon build | Dockerfile `WORKDIR` during `RUN` | common Dockerfiles build in wrong directory |
| daemon runtime | published port bind failure, live network connect/disconnect | containers report running/reachable but are not |
| tests/build | coverage false-green, dark CI lanes, XPASS green | regressions ship unnoticed |
| archive/fs | tar/cp/load/import compatibility and data integrity | wrong image contents, broken builds, stale overlay state |
| GPU/display | render correctness, frame stalls, resource leaks | GUI workloads hang, leak, or display wrong output |

## Manager Rule

For each theory, prefer a narrow repro that compares:

1. expected Linux/Docker/native behavior,
2. current dd behavior,
3. source line explaining the divergence,
4. test or command that can become a regression gate.
