# Bug and Gap Audit

Date: 2026-07-10

This directory is a working inventory of suspicious behavior, bad architecture, bugs, and coverage gaps found by parallel review agents plus a local evidence pass. The active hunt is now focused on compatibility bugs, performance cliffs, race conditions, memory leaks, stale state, data corruption, and failures that matter to real workloads. Security-only findings are deprioritized unless they also explain compatibility breakage.

## Priority Index

| Priority | Area | Finding | Confidence | Detail |
|---|---|---:|---:|---|
| P1 | daemon/runtime | workspace VPN egress env is dropped before engine launch | High | [daemon-tests-docs.md](daemon-tests-docs.md#workspace-vpn-egress-is-dropped) |
| P1 | daemon/runtime | published port bind failures do not fail container start | Medium-high | [daemon-tests-docs.md](daemon-tests-docs.md#published-port-bind-failures-do-not-fail-start) |
| P1 | sentry/compat | untrusted split breaks Linux `EFAULT` compatibility | High | [gpu-display-sentry.md](gpu-display-sentry.md#untrusted-split-breaks-linux-efault-compatibility) |
| P1 | sentry/ipc | `DDJIT_UNTRUSTED` SCM_RIGHTS + eventfd loses events | High | [completeness-and-env.md](completeness-and-env.md#ddjit_untrusted-scm_rights-eventfd-loses-events) |
| P1 | syscall/time | periodic `timerfd` ignores earlier first deadline | High | [syscall-compat.md](syscall-compat.md#periodic-timerfd-ignores-earlier-first-deadline) |
| P1 | syscall/signal | multiple `signalfd` descriptors are not independent | High | [syscall-compat.md](syscall-compat.md#multiple-signalfd-descriptors-are-not-independent) |
| P1 | syscall/epoll | epoll loses readiness when watched fd closes but dup remains | High | [syscall-compat.md](syscall-compat.md#epoll-loses-readiness-when-watched-fd-closes-but-dup-remains) |
| P1 | syscall/epoll | `dup(epoll_fd)` loses pending interest registration | High | [syscall-compat.md](syscall-compat.md#dupepoll_fd-loses-pending-interest-registration) |
| P1 | syscall/fork | fork children lose inherited epoll/timerfd state | High | [syscall-compat.md](syscall-compat.md#fork-children-lose-inherited-epolltimerfd-state) |
| P1 | syscall/fork | forked child loses inherited inotify watch and can hang | High | [syscall-compat.md](syscall-compat.md#forked-child-loses-inherited-inotify-watch-and-can-hang) |
| P1 | syscall/signalfd | `dup(signalfd)` loses signalfd semantics | High | [syscall-compat.md](syscall-compat.md#dupsignalfd-loses-signalfd-semantics) |
| P1 | syscall/poll | sentry `ppoll` masks stale fds instead of `POLLNVAL` | High | [syscall-compat.md](syscall-compat.md#sentry-ppoll-masks-stale-fds-instead-of-pollnval) |
| P1 | syscall/fd | sentry close-on-exec does not clean virtual fds | High | [syscall-compat.md](syscall-compat.md#sentry-close-on-exec-does-not-clean-virtual-fds) |
| P1 | daemon/cache | build-cache layer replacement is non-atomic | High | [daemon-tests-docs.md](daemon-tests-docs.md#build-cache-layer-replacement-is-non-atomic) |
| P1 | daemon/cgroup | fractional `--cpus` loses quota precision | High | [daemon-tests-docs.md](daemon-tests-docs.md#fractional---cpus-loses-quota-precision) |
| P1 | daemon/logs | retained container logs are lost across daemon restart | High | [daemon-tests-docs.md](daemon-tests-docs.md#retained-container-logs-are-lost-across-daemon-restart) |
| P1 | image/load | `docker load` of same tag rewrites existing container rootfs | High | [daemon-tests-docs.md](daemon-tests-docs.md#docker-load-of-same-tag-rewrites-existing-container-rootfs) |
| P1 | daemon/create | create accepts image records whose rootfs is missing | High | [daemon-tests-docs.md](daemon-tests-docs.md#create-accepts-image-records-whose-rootfs-is-missing) |
| P1 | daemon/create | anonymous volume materialization failures are ignored | High | [daemon-tests-docs.md](daemon-tests-docs.md#anonymous-volume-materialization-failures-are-ignored) |
| P1 | env/exec | `execve(..., envp=NULL)` leaks default/stale env | High | [completeness-and-env.md](completeness-and-env.md#execve-envpnull-leaks-a-default-or-stale-environment) |
| P1 | display/clipboard | inert data-device objects silently swallow selection | High | [gpu-display-sentry.md](gpu-display-sentry.md#data-device-objects-are-inert) |
| P2 | JIT/signal | thread-directed signals do not interrupt blocking reads | High | [jit-and-opcodes.md](jit-and-opcodes.md#thread-directed-signals-do-not-interrupt-blocking-reads) |
| P2 | JIT/fp | MXCSR sticky exception flags/control bits are not modeled | Medium | [jit-and-opcodes.md](jit-and-opcodes.md#mxcsr-sticky-exception-flags-are-not-modeled) |
| P2 | JIT/x87 | x87 long double precision is truncated | High | [jit-and-opcodes.md](jit-and-opcodes.md#x87-long-double-precision-is-truncated) |
| P2 | syscall/select | sentry `pselect6` masks invalid virtual fd bits | Medium | [syscall-compat.md](syscall-compat.md#sentry-pselect6-masks-invalid-virtual-fd-bits) |
| P2 | syscall/signal | signal ucontext stack metadata is zero | High | [syscall-compat.md](syscall-compat.md#signal-ucontext-stack-metadata-is-zero) |
| P2 | syscall/inotify | `dup(inotify_fd)` loses inotify read semantics | High | [syscall-compat.md](syscall-compat.md#dupinotify_fd-loses-inotify-read-semantics) |
| P2 | syscall/compat | aarch64 `AT_PAGESZ` exposes host page size | High | [syscall-compat.md](syscall-compat.md#aarch64-at_pagesz-exposes-host-page-size) |
| P2 | syscall/compat | `F_SETLEASE` / `F_NOTIFY` fake support | High | [syscall-compat.md](syscall-compat.md#f_setlease-f_notify-return-success-without-arming-anything) |
| P2 | daemon/runtime | live network connect/disconnect mutates daemon state only | High | [daemon-tests-docs.md](daemon-tests-docs.md#live-network-connectdisconnect-mutates-daemon-state-only) |
| P2 | daemon/build | Dockerfile runtime metadata is accepted but dropped | High | [daemon-tests-docs.md](daemon-tests-docs.md#dockerfile-runtime-metadata-is-accepted-but-dropped) |
| P2 | registry/race | concurrent pulls share a layer temp file | High | [daemon-tests-docs.md](daemon-tests-docs.md#concurrent-pulls-share-a-layer-temp-file) |
| P2 | daemon/stats | stats stream captures a stale pid | Medium | [daemon-tests-docs.md](daemon-tests-docs.md#stats-stream-captures-a-stale-pid) |
| P2 | daemon/events | events are live-only and lossy | High | [daemon-tests-docs.md](daemon-tests-docs.md#events-are-live-only-and-lossy) |
| P2 | daemon/logs | `logs -f` can drop output for slow clients | Medium-high | [daemon-tests-docs.md](daemon-tests-docs.md#logs--f-can-drop-output-for-slow-clients) |
| P2 | daemon/lifecycle | stop timeout marks exited before reaper confirms death | Medium | [daemon-tests-docs.md](daemon-tests-docs.md#stop-timeout-marks-exited-before-reaper-confirms-death) |
| P2 | docs/tests | gap inventory and architecture docs are stale/missing | High | [daemon-tests-docs.md](daemon-tests-docs.md#gap-and-architecture-docs-are-not-auditable) |
| P2 | gpu/compat | Metal render target texture id aliases guest texture id `1` | Medium-high | [gpu-display-sentry.md](gpu-display-sentry.md#metal-render-target-texture-id-can-alias-guest-texture-id-1) |
| P2 | display/window | native window close is not propagated | Medium-high | [gpu-display-sentry.md](gpu-display-sentry.md#native-window-close-is-not-propagated) |
| P2 | env/runtime | `DDJIT_SANDBOX` public mode is intentionally avoided by tests | High | [completeness-and-env.md](completeness-and-env.md#ddjit_sandbox-public-mode-is-intentionally-avoided-by-tests) |
| P2 | env/durability | `S3DB_DURABILITY` silently changes fsync semantics | High | [completeness-and-env.md](completeness-and-env.md#s3db_durability-hidden-fsync-semantics) |
| P2 | env/cache | aarch64 pcache key omits `NOSTEALFAST` | Medium | [completeness-and-env.md](completeness-and-env.md#aarch64-pcache-key-omits-nostealfast) |
| P2 | env/cache | per-container `DDJIT_NOPCACHE` is dropped by typed launch | Medium | [completeness-and-env.md](completeness-and-env.md#per-container-ddjit_nopcache-is-dropped-by-typed-launch) |
| P2 | procfs/env | hidden proc switches change peer procfs | Medium | [completeness-and-env.md](completeness-and-env.md#hidden-proc-switches-change-peer-procfs) |
| P1 | cgroup/accounting | cgroup membership omits forked children | High | [completeness-and-env.md](completeness-and-env.md#cgroup-membership-omits-forked-children) |
| P1 | cgroup/limits | `DD_PIDS_MAX` is not enforced for forked processes | High | [completeness-and-env.md](completeness-and-env.md#dd_pids_max-is-not-enforced-for-forked-processes) |
| P1 | cgroup/accounting | cgroup memory usage is process-local | High | [completeness-and-env.md](completeness-and-env.md#cgroup-memory-usage-is-process-local) |
| P1 | sysfs/network | network-none hides `eth0` in readdir but direct lookup exposes it | High | [completeness-and-env.md](completeness-and-env.md#network-none-hides-eth0-in-readdir-but-direct-lookup-exposes-it) |
| P2 | procfs/ns | peer `/proc/<pid>/ns` is absent | High | [completeness-and-env.md](completeness-and-env.md#peer-procns-is-absent) |
| P2 | procfs/net | `/proc/net/unix` ignores live AF_UNIX sockets | High | [completeness-and-env.md](completeness-and-env.md#procnetunix-ignores-live-af_unix-sockets) |
| P1 | procfs/mounts | bind mounts are missing from mount tables | High | [completeness-and-env.md](completeness-and-env.md#bind-mounts-are-missing-from-mount-tables) |
| P1 | procfs/smaps | `/proc/self/smaps` can hang on read | High | [completeness-and-env.md](completeness-and-env.md#procselfsmaps-can-hang-on-read) |
| P1 | procfs/statfs | `statfs` is wrong for synthetic proc/sys leaves | High | [completeness-and-env.md](completeness-and-env.md#statfs-is-wrong-for-synthetic-procsys-leaves) |
| P1 | procfs/statfs | `statfs.f_flags` is always zero | High | [completeness-and-env.md](completeness-and-env.md#statfsf_flags-is-always-zero) |
| P1 | devfs/tty | `/dev/tty` nonblocking read reports EOF instead of `EAGAIN` | High | [completeness-and-env.md](completeness-and-env.md#devtty-nonblocking-read-reports-eof-instead-of-eagain) |
| P2 | procfs/state | futex-blocked processes report running in procfs | High | [completeness-and-env.md](completeness-and-env.md#futex-blocked-processes-report-running-in-procfs) |
| P2 | procfs/maps | `/proc/self/maps` omits RELRO mapping detail | High | [completeness-and-env.md](completeness-and-env.md#procselfmaps-omits-relro-mapping-detail) |
| P2 | launch/config | path-list config still uses delimiter env strings | Medium | [completeness-and-env.md](completeness-and-env.md#typed-launch-path-lists-still-use-delimiter-env-strings) |
| P1 | syscall/signal | `SA_NOCLDWAIT` does not suppress zombies | High | [syscall-compat.md](syscall-compat.md#sa_nocldwait-does-not-suppress-zombies) |
| P1 | syscall/signal | aarch64 signal ucontext omits FPSIMD context record | High | [syscall-compat.md](syscall-compat.md#aarch64-signal-ucontext-omits-fpsimd-context-record) |
| P1 | syscall/signal | `kill(0, sig)` only signals the caller | High | [syscall-compat.md](syscall-compat.md#kill0-sig-only-signals-the-caller) |
| P1 | syscall/mm | guest `PROT_NONE` mappings remain directly readable | High | [syscall-compat.md](syscall-compat.md#guest-prot_none-mappings-remain-directly-readable) |
| P1 | syscall/mm | writes to `mprotect(PROT_READ)` pages do not fault | High | [syscall-compat.md](syscall-compat.md#writes-to-mprotectprot_read-pages-do-not-fault) |
| P1 | syscall/mm | execute permission is not enforced for guest fetch | High | [syscall-compat.md](syscall-compat.md#execute-permission-is-not-enforced-for-guest-fetch) |
| P1 | syscall/wait | default core status contradicts `RLIMIT_CORE=0` | High | [syscall-compat.md](syscall-compat.md#default-core-status-contradicts-rlimit_core0) |
| P2 | syscall/signal | `SA_NOCLDSTOP` still delivers stop SIGCHLD | High | [syscall-compat.md](syscall-compat.md#sa_nocldstop-still-delivers-stop-sigchld) |
| P2 | syscall/mm | aarch64 4K subpage `munmap` returns `EINVAL` | High | [syscall-compat.md](syscall-compat.md#aarch64-4k-subpage-munmap-returns-einval) |
| P2 | syscall/mm | aligned `mprotect` on unmapped range succeeds | High | [syscall-compat.md](syscall-compat.md#aligned-mprotect-on-unmapped-range-succeeds) |
| P2 | procfs/process | `/proc/<pid>/stat` reports wrong process group and session | High | [syscall-compat.md](syscall-compat.md#proc-stat-reports-wrong-process-group-and-session) |
| P2 | syscall/fs | `renameat2(RENAME_WHITEOUT)` silently becomes plain rename | High | [syscall-compat.md](syscall-compat.md#renameat2rename_whiteout-silently-becomes-plain-rename) |

## Deprioritized

Security-only items from the first pass remain in [syscalls-and-security.md](syscalls-and-security.md), but the manager loop is no longer spending agent time on them unless they also create compatibility failures, hangs, data loss, or performance problems.

## Notes

- Existing uncommitted source changes were present in `dd-jit/src/runtime/container/builder.rs` and `dd-jit/src/runtime/container/mod.rs`. This audit did not modify those files.
- No destructive commands were used. Agents were instructed to avoid modifying the main worktree.
- Runtime behavior was not exhaustively tested. Items marked high confidence are backed by direct source evidence and narrow verification recipes.
