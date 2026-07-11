# Bug and Gap Audit

Date: 2026-07-10

This directory is a working inventory of suspicious behavior, bad architecture, bugs, and coverage gaps found by parallel review agents plus a local evidence pass. The active hunt is now focused on compatibility bugs, performance cliffs, race conditions, memory leaks, stale state, data corruption, and failures that matter to real workloads. Security-only findings are deprioritized unless they also explain compatibility breakage.

## Priority Index

| Priority | Area | Finding | Confidence | Detail |
|---|---|---:|---:|---|
| P1 | sentry/compat | untrusted split breaks Linux `EFAULT` compatibility | High | [gpu-display-sentry.md](gpu-display-sentry.md#untrusted-split-breaks-linux-efault-compatibility) |
| P1 | daemon/cgroup | fractional `--cpus` loses quota precision | High | [daemon-tests-docs.md](daemon-tests-docs.md#fractional---cpus-loses-quota-precision) |
| P2 | syscall/compat | aarch64 `AT_PAGESZ` exposes host page size | High | [syscall-compat.md](syscall-compat.md#aarch64-at_pagesz-exposes-host-page-size) |
| P3 | syscall/compat | `F_SETLEASE` lease-break signal not delivered (residual; `F_NOTIFY` fixed) | High | [syscall-compat.md](syscall-compat.md#f_setlease-lease-break-signal-not-delivered-residual) |
| P2 | launch/config | path-list config still uses delimiter env strings | Medium | [completeness-and-env.md](completeness-and-env.md#typed-launch-path-lists-still-use-delimiter-env-strings) |
| P1 | syscall/mm | guest `PROT_NONE` mappings remain directly readable | High | [syscall-compat.md](syscall-compat.md#guest-prot_none-mappings-remain-directly-readable) |
| P1 | syscall/mm | writes to `mprotect(PROT_READ)` pages do not fault | High | [syscall-compat.md](syscall-compat.md#writes-to-mprotectprot_read-pages-do-not-fault) |
| P1 | syscall/mm | execute permission is not enforced for guest fetch | High | [syscall-compat.md](syscall-compat.md#execute-permission-is-not-enforced-for-guest-fetch) |
| P2 | syscall/mm | aarch64 4K subpage `munmap` returns `EINVAL` | High | [syscall-compat.md](syscall-compat.md#aarch64-4k-subpage-munmap-returns-einval) |

## Deprioritized

Security-only items from the first pass remain in [syscalls-and-security.md](syscalls-and-security.md), but the manager loop is no longer spending agent time on them unless they also create compatibility failures, hangs, data loss, or performance problems.

## Notes

- Existing uncommitted source changes were present in `dd-jit/src/runtime/container/builder.rs` and `dd-jit/src/runtime/container/mod.rs`. This audit did not modify those files.
- No destructive commands were used. Agents were instructed to avoid modifying the main worktree.
- Runtime behavior was not exhaustively tested. Items marked high confidence are backed by direct source evidence and narrow verification recipes.
