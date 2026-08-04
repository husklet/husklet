# Job-control lifecycle oracle audit

This folder owns one process-group and session lifecycle case. `main.c` checks
negative-ID errors, the caller's group/session identity, a child's group change
and reap, successful session creation, and `EPERM` when a process-group leader
calls `setsid`. The pipe only orders the parent and child; `golden/stdout.txt`
is the complete expected output.

## Retained C implementation studied

- `../engine/src/linux_abi/number.c` maps the x86-64 spellings of clone/fork,
  wait4, setpgid, getpgid, getsid, and setsid to the canonical syscall numbers.
- `../engine/src/linux_abi/syscall/dispatch.c` (`service`, `service_local`)
  owns canonical family dispatch.
- `../engine/src/linux_abi/syscall/proc.c` (`svc_proc`) owns setpgid cases
  154-156, process clone case 220, and wait4 case 260. Its
  `bound_fork_prepare`, `bound_fork_complete`, and `fork_child_hooks` preserve
  process state across the host fork and publish the child identity.
- `../engine/src/linux_abi/syscall/rare.c` (`svc_rare`, case 157) forwards
  setsid and preserves its host errno.
- `../engine/src/linux_abi/syscall/io.c` (`svc_io`, cases 59, 63, and 64)
  owns the ordering pipe, while `syscall/fs.c` (`svc_fs`, case 57 and
  `fd_reset_emul`) owns descriptor close and teardown.

The process/task tables own child and group identity until wait4 reaps the
child; getpgid on that reaped identity must then return `ESRCH`. The retained
handlers validate a negative requested group as `EINVAL`, map failed group and
session lookup to `ESRCH`, and preserve the kernel's `EPERM` for a group leader
attempting to create a session. A fork child resets inherited process-private
state before returning to guest execution.

Rust ownership maps process/session identities and fork/wait teardown to
`hl-task` plus the `hl-runtime` process integration. Pipe and open file
description behavior belongs to `hl-descriptor` plus runtime I/O; Linux errno
conversion remains at the personality boundary.

This case does not establish terminal foreground-group behavior, signal
delivery to groups, orphan-group rules, checkpoint restoration, or concurrent
job-control correctness.
