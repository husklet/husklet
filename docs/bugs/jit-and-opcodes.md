# JIT, Cache, and Opcode Gaps

This file covers instruction fidelity, opcode coverage, stale translation, and hidden completeness holes.

## Thread-Directed Signals Do Not Interrupt Blocking Reads

Priority: P2
Impact: wrong `EINTR`/restart behavior and delayed signal handling
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-I-jit-runtime-20260710`.

Evidence:

- `tgkill` marks the target thread pending and sets `irq`: `dd-jit-darwin/src/runtime/os/linux/thread.c:1014`.
- It only wakes published futex waits: `dd-jit-darwin/src/runtime/os/linux/thread.c:1021`.
- Blocking `read` stays in the host syscall loop: `dd-jit-darwin/src/runtime/os/linux/syscall/io.c:552`.

Why this is bad:

A thread-directed signal should interrupt a target blocked in read/accept/recv-style syscalls when restart rules allow. dd delays delivery until the host read returns.

Isolated proof:

```sh
timeout 5 qemu-x86_64 target-worker-I/poc/tgkill_read_eintr
timeout 10 mac bash -lc "exec '$PWD/target-worker-I/release/build/dd-jit-darwin-5b0dabfbe6f0af2e/out/ddjit-linux_x86_64' '$PWD/target-worker-I/poc/tgkill_read_eintr'"
```

Observed: qemu `read_ret=-1 errno=4 delayed=0 rc=0`; dd `read_ret=1 errno=0 delayed=1 rc=1`.

