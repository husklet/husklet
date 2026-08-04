# Compatibility production priorities

Generated from active retained-C passes and explicit `SYS_*`/`__NR_*` source evidence.
It deliberately excludes inferred libc internals and loader/execution failures.

C-pass active rows analyzed: 2180
Rows with an explicit production gap: 128

## Subsystem volume

| Subsystem | Rows |
|---|---:|
| filesystem | 46 |
| scheduling | 28 |
| process | 12 |
| memory | 10 |
| synchronization | 8 |
| asynchronous-io | 6 |
| time | 4 |
| signal | 4 |
| descriptor | 4 |
| security | 2 |
| identity | 2 |
| other | 2 |

## Primary syscall gaps

| Syscall | Status | Rows |
|---|---|---:|
| `faccessat2` | router-domain-only | 8 |
| `clone3` | router-domain-only | 6 |
| `getcpu` | router-domain-only | 6 |
| `openat2` | router-domain-only | 6 |
| `sched_getattr` | missing | 6 |
| `renameat2` | router-domain-only | 6 |
| `copy_file_range` | missing | 4 |
| `fallocate` | router-domain-only | 4 |
| `io_uring_setup` | missing | 4 |
| `membarrier` | missing | 4 |
| `name_to_handle_at` | missing | 4 |
| `process_vm_readv` | missing | 4 |
| `sched_getaffinity` | missing | 4 |
| `adjtimex` | missing | 2 |
| `cachestat` | missing | 2 |
| `flock` | router-domain-only | 2 |
| `futex_waitv` | router-domain-only | 2 |
| `getrusage` | router-domain-only | 2 |
| `io_uring_enter` | missing | 2 |
| `kcmp` | missing | 2 |
| `landlock_create_ruleset` | missing | 2 |
| `memfd_secret` | missing | 2 |
| `get_mempolicy` | missing | 2 |
| `pidfd_getfd` | missing | 2 |
| `getpriority` | missing | 2 |
| `rseq` | missing | 2 |
| `rt_sigtimedwait` | router-domain-only | 2 |
| `sched_get_priority_max` | missing | 2 |
| `sync` | missing | 2 |
| `timer_create` | router-domain-only | 2 |
| `utimensat` | router-domain-only | 2 |
| `fchmodat2` | missing | 2 |
| `dup2` | missing | 2 |
| `sysinfo` | router-domain-only | 2 |
| `mlock2` | missing | 2 |
| `move_pages` | missing | 2 |
| `epoll_pwait2` | router-domain-only | 2 |
| `fanotify_init` | missing | 2 |
| `pwritev2` | missing | 2 |
| `ioprio_get` | missing | 2 |
| `sched_setaffinity` | missing | 2 |
| `sched_setscheduler` | missing | 2 |
| `rt_sigsuspend` | router-domain-only | 2 |
| `waitid` | router-domain-only | 2 |
