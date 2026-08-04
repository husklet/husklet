# Root legacy cohort oracle audit

The retained C engine was studied read-only for the moved root fixtures.
Process-group and session behavior is implemented through
`../engine/src/linux_abi/syscall/proc.c` (`setpgid`, `getpgid`, `getsid`,
`setsid`, `fork`, and `wait4` dispatch paths) with task identity and child
teardown owned by the process/task tables. Positional I/O follows
`../engine/src/linux_abi/syscall/io.c` (`read`, `write`, `pread64`, `pwrite64`,
vectored partial-result handling, `lseek`, duplicate OFD offsets) and
`../engine/src/linux_abi/syscall/files.c` (`openat`, `unlinkat`). Socket-stop
crosses `../engine/src/linux_abi/syscall/network.c` socketpair creation and the
blocking/cancellation read path in `io.c`. Bootstrap exit/write/getpid dispatch
is rooted in `../engine/src/linux_abi/syscall/dispatch.c` and `proc.c`.

Rust ownership maps process/session identities and fork/wait teardown to
`hl-task` plus `hl-runtime` process integration; shared offsets and positional
I/O to `hl-descriptor` plus runtime filesystem operations; socketpair lifetime
to `hl-network` plus runtime adapters; and syscall admission/errno conversion
to `hl-linux` and the runtime dispatcher. The preserved fixtures cover invalid
process-group/session IDs, child group creation/reap, session leadership,
positional scalar/vector widths and partial/EFAULT/EINVAL results, shared OFD
offset invariance, append behavior, blocking socket cancellation setup, and
minimal exit/write/getpid dispatch.

The socket-stop source has no standalone completion path and remains typed
broken until external cancellation orchestration is represented by the folder
runner. The AMD64 positional oracle failure is recorded beside that suite.

## Retired `core` aggregate

The former `tests/runtime/core` folder selected three operations through one
argv-dispatched freestanding binary. Its YAML assigned the same build output
and guest destination to all three cases, so the current runner rejected the
folder before executing a row. The replacement owns each operation as an
independent binary and preserves the guest-visible contracts on both ISAs:

| Former case | Replacement | Preserved contract |
|---|---|---|
| `runtime/core/exit` | `runtime/bootstrap/exit` and the bootstrap-owned `runtime/legacy-exit/status-42` variant | empty stdout and exit 42 |
| `runtime/core/write` | `runtime/bootstrap/write` and the bootstrap-owned `runtime/legacy-write/stdout` variant | exact bounded stdout write and exit zero |
| `runtime/core/getpid` | `runtime/syscall/getpid-write` and `runtime/bootstrap/syscall` | positive guest PID, exact stdout write, and exit zero |

The replacement sources deliberately are not byte-identical to the retired
multi-command source: they remove its argument dispatcher and use distinct
diagnostic strings. The empty-output golden remains byte-identical; the write
and getpid goldens change with those diagnostic strings. Git retains the old
source and golden hashes, while the new folders preserve the tested syscall,
result, and lifecycle semantics without duplicate output ownership.

Both AArch64 and x86-64 QEMU oracle rows pass for all three split replacements
and all three bootstrap counterparts. A global runner preflight then loaded
every runtime YAML definition and fingerprinted every active source/golden,
stopping only at an intentionally invalid results path before guest execution.
