# Bug and Gap Audit

Date: 2026-07-10

This directory is a working inventory of suspicious behavior, bad architecture, bugs, and coverage gaps found by parallel review agents plus a local evidence pass. The active hunt is now focused on compatibility bugs, performance cliffs, race conditions, memory leaks, stale state, data corruption, and failures that matter to real workloads. Security-only findings are deprioritized unless they also explain compatibility breakage.

## Priority Index

| Priority | Area | Finding | Confidence | Detail |
|---|---|---:|---:|---|
| P1 | daemon/runtime | workspace VPN egress env is dropped before engine launch | High | [daemon-tests-docs.md](daemon-tests-docs.md#workspace-vpn-egress-is-dropped) |
| P1 | sentry/compat | untrusted split breaks Linux `EFAULT` compatibility | High | [gpu-display-sentry.md](gpu-display-sentry.md#untrusted-split-breaks-linux-efault-compatibility) |
| P1 | sentry/ipc | `DDJIT_UNTRUSTED` SCM_RIGHTS + eventfd loses events | High | [completeness-and-env.md](completeness-and-env.md#ddjit_untrusted-scm_rights-eventfd-loses-events) |
| P1 | syscall/time | periodic `timerfd` ignores earlier first deadline | High | [syscall-compat.md](syscall-compat.md#periodic-timerfd-ignores-earlier-first-deadline) |
| P1 | syscall/signal | multiple `signalfd` descriptors are not independent | High | [syscall-compat.md](syscall-compat.md#multiple-signalfd-descriptors-are-not-independent) |
| P1 | syscall/epoll | epoll loses readiness when watched fd closes but dup remains | High | [syscall-compat.md](syscall-compat.md#epoll-loses-readiness-when-watched-fd-closes-but-dup-remains) |
| P1 | syscall/fd | sentry close-on-exec does not clean virtual fds | High | [syscall-compat.md](syscall-compat.md#sentry-close-on-exec-does-not-clean-virtual-fds) |
| P1 | daemon/cgroup | fractional `--cpus` loses quota precision | High | [daemon-tests-docs.md](daemon-tests-docs.md#fractional---cpus-loses-quota-precision) |
| P1 | env/exec | `execve(..., envp=NULL)` leaks default/stale env | High | [completeness-and-env.md](completeness-and-env.md#execve-envpnull-leaks-a-default-or-stale-environment) |
| P1 | display/clipboard | inert data-device objects silently swallow selection | High | [gpu-display-sentry.md](gpu-display-sentry.md#data-device-objects-are-inert) |
| P2 | syscall/compat | aarch64 `AT_PAGESZ` exposes host page size | High | [syscall-compat.md](syscall-compat.md#aarch64-at_pagesz-exposes-host-page-size) |
| P2 | syscall/compat | `F_SETLEASE` / `F_NOTIFY` fake support | High | [syscall-compat.md](syscall-compat.md#f_setlease-f_notify-return-success-without-arming-anything) |
| P2 | daemon/runtime | live network connect/disconnect mutates daemon state only | High | [daemon-tests-docs.md](daemon-tests-docs.md#live-network-connectdisconnect-mutates-daemon-state-only) |
| P2 | docs/tests | gap inventory and architecture docs are stale/missing | High | [daemon-tests-docs.md](daemon-tests-docs.md#gap-and-architecture-docs-are-not-auditable) |
| P2 | gpu/compat | Metal render target texture id aliases guest texture id `1` | Medium-high | [gpu-display-sentry.md](gpu-display-sentry.md#metal-render-target-texture-id-can-alias-guest-texture-id-1) |
| P2 | display/window | native window close is not propagated | Medium-high | [gpu-display-sentry.md](gpu-display-sentry.md#native-window-close-is-not-propagated) |
| P2 | env/runtime | `DDJIT_SANDBOX` public mode is intentionally avoided by tests | High | [completeness-and-env.md](completeness-and-env.md#ddjit_sandbox-public-mode-is-intentionally-avoided-by-tests) |
| P2 | env/durability | `S3DB_DURABILITY` silently changes fsync semantics | High | [completeness-and-env.md](completeness-and-env.md#s3db_durability-hidden-fsync-semantics) |
| P2 | procfs/env | hidden proc switches change peer procfs | Medium | [completeness-and-env.md](completeness-and-env.md#hidden-proc-switches-change-peer-procfs) |
| P1 | cgroup/accounting | cgroup membership omits forked children | High | [completeness-and-env.md](completeness-and-env.md#cgroup-membership-omits-forked-children) |
| P1 | cgroup/limits | `DD_PIDS_MAX` is not enforced for forked processes | High | [completeness-and-env.md](completeness-and-env.md#dd_pids_max-is-not-enforced-for-forked-processes) |
| P1 | cgroup/accounting | cgroup memory usage is process-local | High | [completeness-and-env.md](completeness-and-env.md#cgroup-memory-usage-is-process-local) |
| P1 | sysfs/network | network-none hides `eth0` in readdir but direct lookup exposes it | High | [completeness-and-env.md](completeness-and-env.md#network-none-hides-eth0-in-readdir-but-direct-lookup-exposes-it) |
| P2 | procfs/ns | peer `/proc/<pid>/ns` is absent | High | [completeness-and-env.md](completeness-and-env.md#peer-procns-is-absent) |
| P1 | procfs/mounts | bind mounts are missing from mount tables | High | [completeness-and-env.md](completeness-and-env.md#bind-mounts-are-missing-from-mount-tables) |
| P1 | devfs/tty | `/dev/tty` nonblocking read reports EOF instead of `EAGAIN` | High | [completeness-and-env.md](completeness-and-env.md#devtty-nonblocking-read-reports-eof-instead-of-eagain) |
| P2 | procfs/state | futex-blocked processes report running in procfs | High | [completeness-and-env.md](completeness-and-env.md#futex-blocked-processes-report-running-in-procfs) |
| P2 | launch/config | path-list config still uses delimiter env strings | Medium | [completeness-and-env.md](completeness-and-env.md#typed-launch-path-lists-still-use-delimiter-env-strings) |
| P1 | syscall/signal | `kill(0, sig)` only signals the caller | High | [syscall-compat.md](syscall-compat.md#kill0-sig-only-signals-the-caller) |
| P1 | syscall/mm | guest `PROT_NONE` mappings remain directly readable | High | [syscall-compat.md](syscall-compat.md#guest-prot_none-mappings-remain-directly-readable) |
| P1 | syscall/mm | writes to `mprotect(PROT_READ)` pages do not fault | High | [syscall-compat.md](syscall-compat.md#writes-to-mprotectprot_read-pages-do-not-fault) |
| P1 | syscall/mm | execute permission is not enforced for guest fetch | High | [syscall-compat.md](syscall-compat.md#execute-permission-is-not-enforced-for-guest-fetch) |
| P2 | syscall/mm | aarch64 4K subpage `munmap` returns `EINVAL` | High | [syscall-compat.md](syscall-compat.md#aarch64-4k-subpage-munmap-returns-einval) |
| P2 | syscall/mm | aligned `mprotect` on unmapped range succeeds | High | [syscall-compat.md](syscall-compat.md#aligned-mprotect-on-unmapped-range-succeeds) |
| P2 | syscall/fs | `renameat2(RENAME_WHITEOUT)` silently becomes plain rename | High | [syscall-compat.md](syscall-compat.md#renameat2rename_whiteout-silently-becomes-plain-rename) |

## Deprioritized

Security-only items from the first pass remain in [syscalls-and-security.md](syscalls-and-security.md), but the manager loop is no longer spending agent time on them unless they also create compatibility failures, hangs, data loss, or performance problems.

## Notes

- Existing uncommitted source changes were present in `dd-jit/src/runtime/container/builder.rs` and `dd-jit/src/runtime/container/mod.rs`. This audit did not modify those files.
- No destructive commands were used. Agents were instructed to avoid modifying the main worktree.
- Runtime behavior was not exhaustively tested. Items marked high confidence are backed by direct source evidence and narrow verification recipes.
