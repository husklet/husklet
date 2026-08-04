# POSIX compatibility oracle audit

This folder is the sole owner of the 98 cases formerly registered by
`tests/compat/posix/manifest.tsv`. The move preserves every C source and golden
byte-for-byte. `test.yaml` preserves the case IDs, ISA scopes, compiler flags,
exit codes, and stdout contracts. The checked output is deterministic guest
Linux behavior; QEMU is used to validate it independently of the Rust engine.

## Retained C implementation studied

The read-only oracle was inspected as a complete POSIX call graph, not as 98
isolated fixture failures:

- `../engine/src/linux_abi/syscall/dispatch.c` (`linux_syscall`, syscall-number
  admission and errno return), `guest_copy.c` (`guest_span`,
  `guest_copy_{from,to}`, `guest_iov_import`, `guest_fd_{read,write,vector}`), and
  `io.c` (descriptor allocation/carry, read/write, positional and vectored I/O,
  `dup*`, `fcntl`, `lseek`, pipe and sendfile paths).
- `../engine/src/linux_abi/syscall/fs.c` (`guest_fill_linux_stat`, pathname and
  `*at` operations, truncate/sync, directory entries, tty ioctl dispatch,
  `tty_ctl_block`/`tty_ctl_restore`) plus `stat.c`, `host_dirent.h`,
  `container/vfs.c`, and `container/vfs/resolve.c`.
- `../engine/src/linux_abi/syscall/event.c` (select/pselect, poll/ppoll and
  readiness waits), `time.c` (clock and sleep deadlines), `mem.c` (mmap,
  protection and advice), `proc.c` (identity, limits, groups and wait), and
  `signal.c` together with `../engine/src/linux_abi/signal.c` (masking,
  queued delivery, interruption/restart and signal-frame return).
- `../engine/src/linux_abi/thread.c`, `fork.c`, `pipe.c`, `fdhandle.c`,
  `fdcache.c`, `logical_vma.c`, and `shared.c` for task/OFD/mapping identity,
  fork sharing, locks, wakeups and teardown. The PTY path was followed through
  `host_tty.h` and the controlling-terminal/job-control branches in `fs.c` and
  `container/vfs.c`. POSIX queues and named synchronization were followed
  through the relevant IPC syscall dispatch and host bindings.

## Ownership, ordering, and lifetime

The C engine's process/task state owns signal masks, pending delivery, process
groups, sessions, timers and wait relationships. Its fd table owns descriptor
numbers while shared open-file descriptions own offsets and status flags;
`dup*` shares the OFD but preserves descriptor-local flags. VFS resolution pins
the resolved object across host work, and mapping ledgers own guest address and
file-backing lifetime. Fork explicitly shares or copies these owners according
to Linux semantics; close, exec and exit release registrations and wake blocked
waiters. Locks protect tables only for lookup/publication and are not intended
to span blocking host calls.

The important behavioral contracts are partial read/write and iovec progress,
shared OFD offsets, `EINTR` versus restart, poll/select temporary signal masks,
absolute and relative deadlines, queued-signal values, child wait status,
atomic descriptor replacement, path-resolution errno, shared-mapping
visibility, PTY foreground-group signaling, and teardown wakeups. Guest lengths,
iovec counts, fd bounds and copied strings are validated before host allocation
or access. Linux and Darwin host adapters diverge for tty/session ioctls; guest
AArch64 and x86-64 enter through separate syscall number/layout tables but join
the same domain mechanisms.

## Rust ownership matrix

| Retained C capability | Rust owner | State |
|---|---|---|
| descriptor identity, flags, OFD offsets, dup/close | `hl-descriptor` | implemented domain owner |
| pathname resolution, metadata, links and directory traversal | `hl-vfs`; joins in `hl-runtime` | implemented/divergent pending cohort evidence |
| pipes, POSIX semaphore and queue objects | `hl-ipc` | implemented with known queue-notify gap |
| mappings, advice and shared backing | `hl-memory`; file-backed join in `hl-runtime` | implemented/divergent pending cohort evidence |
| tasks, fork/exec/wait, groups and sessions | `hl-task`; joins in `hl-runtime` | implemented/divergent for full PTY lifecycle |
| readiness and timed waits | `hl-event` and `hl-time`; joins in `hl-runtime` | implemented/divergent pending cohort evidence |
| signal masks, queued delivery and interruption | task/signal integration in `hl-runtime` | async-cancel signal-frame gap remains |
| socket address/interface queries | `hl-network` with host adapter | implemented; host inventory dependent |
| PTY/termios/controlling terminal | descriptor/task/VFS integration in `hl-runtime` | Darwin ioctl and background `tcsetattr` gaps remain |

The non-active contracts are explicit in `test.yaml`: asynchronous pthread
cancellation and mq-notify thread delivery are broken engine mechanisms;
`pty-ctl` and the controlling-terminal/session cases unsupported on the Darwin
backend are retained with evidence rather than silently removed; the known
background `tcsetattr` SIGTTOU deviation is likewise visible. No fixture-specific
production branch is an acceptable repair: each must be resolved in its generic
Rust owner above.

## Migration evidence

All 195 declared case/target rows were cross-compiled concurrently from the moved
sources with the declared flags. QEMU completed every executable within 15
seconds: 187 outputs matched byte-for-byte. The eight expected oracle differences
were both targets of `chmodchown`, `ctty-session`, `mq-notify-thread`, and
`pty-ctl`. The latter six already carry typed engine/platform status. `chmodchown`
is additionally typed broken because an unprivileged QEMU process cannot reproduce
the engine-root chown contract. Thus every active QEMU row matched, while every
divergence remains visibly registered with a reason and this evidence.
