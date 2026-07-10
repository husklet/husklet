# Bug and Gap Audit

Date: 2026-07-10

This directory is a working inventory of suspicious behavior, bad architecture, bugs, and coverage gaps found by parallel review agents plus a local evidence pass. The active hunt is now focused on compatibility bugs, performance cliffs, race conditions, memory leaks, stale state, data corruption, and failures that matter to real workloads. Security-only findings are deprioritized unless they also explain compatibility breakage.

## Priority Index

| Priority | Area | Finding | Confidence | Detail |
|---|---|---:|---:|---|
| P1 | JIT/cache | stale translation after `munmap`/`MAP_FIXED` VA reuse | High | [jit-and-opcodes.md](jit-and-opcodes.md#stale-translation-after-unmapremap) |
| P1 | JIT/cache | stale translation after `mremap(MREMAP_FIXED)` VA reuse | High | [jit-and-opcodes.md](jit-and-opcodes.md#mremapmremap_fixed-can-reuse-stale-translations) |
| P1 | daemon/runtime | workspace VPN egress env is dropped before engine launch | High | [daemon-tests-docs.md](daemon-tests-docs.md#workspace-vpn-egress-is-dropped) |
| P1 | daemon/runtime | published port bind failures do not fail container start | Medium-high | [daemon-tests-docs.md](daemon-tests-docs.md#published-port-bind-failures-do-not-fail-start) |
| P1 | daemon/compat | inline named volume sources can escape `volumes_dir` | Medium | [daemon-tests-docs.md](daemon-tests-docs.md#inline-volume-sources-can-escape-volumes_dir) |
| P1 | JIT/memory | 4K guest `munmap` subpage remains readable | High | [jit-and-opcodes.md](jit-and-opcodes.md#4k-guest-munmap-subpage-remains-readable) |
| P1 | JIT/race | `cmpxchg16b` is non-atomic | Medium-high | [jit-and-opcodes.md](jit-and-opcodes.md#cmpxchg16b-is-non-atomic) |
| P1 | daemon/data | `docker commit` drops container writes | High | [daemon-tests-docs.md](daemon-tests-docs.md#docker-commit-drops-container-writes) |
| P1 | daemon/data | `docker export` drops container writes | High | [daemon-tests-docs.md](daemon-tests-docs.md#docker-export-drops-container-writes) |
| P1 | daemon/runtime | failed start leaves a spent `Live` and later fake success | High | [daemon-tests-docs.md](daemon-tests-docs.md#failed-start-leaves-a-spent-live) |
| P1 | sentry/compat | untrusted split breaks Linux `EFAULT` compatibility | High | [gpu-display-sentry.md](gpu-display-sentry.md#untrusted-split-breaks-linux-efault-compatibility) |
| P1 | sentry/ipc | `DDJIT_UNTRUSTED` SCM_RIGHTS + eventfd loses events | High | [completeness-and-env.md](completeness-and-env.md#ddjit_untrusted-scm_rights-eventfd-loses-events) |
| P1 | display/compat | multiple `wl_surface.frame` requests collapse to one | High | [gpu-display-sentry.md](gpu-display-sentry.md#multiple-wl_surfaceframe-requests-collapse-to-one) |
| P1 | gpu/corruption | GPU executor acks success after replay errors/skipped writes | Medium-high | [gpu-display-sentry.md](gpu-display-sentry.md#gpu-executor-acks-success-after-replay-errors-or-skipped-writes) |
| P1 | gpu/corruption | `DrawIndexed.base_vertex` is ignored by Metal replay | Medium-high | [gpu-display-sentry.md](gpu-display-sentry.md#drawindexedbase_vertex-is-ignored-by-metal-replay) |
| P1 | gpu/corruption | Metal replay silently no-ops supported IR commands | Medium-high | [gpu-display-sentry.md](gpu-display-sentry.md#metal-replay-silently-no-ops-supported-ir-commands) |
| P1 | cp/contents | `docker cp` put follows existing destination symlink | High | [archive-fs-compat.md](archive-fs-compat.md#docker-cp-put-follows-existing-destination-symlink) |
| P1 | image/compat | Docker save/load format is not Docker-compatible | High | [archive-fs-compat.md](archive-fs-compat.md#docker-saveload-archive-format-is-not-docker-compatible) |
| P1 | cp/contents | `docker cp` GET drops lower overlay entries | High | [archive-fs-compat.md](archive-fs-compat.md#docker-cp-get-drops-lower-entries-from-overlay-directories) |
| P1 | syscall/time | periodic `timerfd` ignores earlier first deadline | High | [syscall-compat.md](syscall-compat.md#periodic-timerfd-ignores-earlier-first-deadline) |
| P1 | syscall/poll | `pselect6`/`ppoll` ignore temporary signal masks | High | [syscall-compat.md](syscall-compat.md#pselect6-and-ppoll-ignore-temporary-signal-masks) |
| P1 | syscall/signal | multiple `signalfd` descriptors are not independent | High | [syscall-compat.md](syscall-compat.md#multiple-signalfd-descriptors-are-not-independent) |
| P1 | syscall/time | `clock_nanosleep(TIMER_ABSTIME)` swallows interrupts | High | [syscall-compat.md](syscall-compat.md#clock_nanosleeptimer_abstime-swallows-interrupts) |
| P1 | syscall/eventfd | `dup(eventfd)` loses eventfd semantics | High | [syscall-compat.md](syscall-compat.md#dupeventfd-loses-eventfd-semantics) |
| P1 | syscall/signalfd | `signalfd` update keeps stale signals and short reads consume events | High | [syscall-compat.md](syscall-compat.md#signalfd-update-keeps-stale-signals-and-short-reads-consume-events) |
| P1 | syscall/epoll | epoll loses readiness when watched fd closes but dup remains | High | [syscall-compat.md](syscall-compat.md#epoll-loses-readiness-when-watched-fd-closes-but-dup-remains) |
| P1 | syscall/epoll | `dup(epoll_fd)` loses pending interest registration | High | [syscall-compat.md](syscall-compat.md#dupepoll_fd-loses-pending-interest-registration) |
| P1 | syscall/fork | fork children lose inherited epoll/timerfd state | High | [syscall-compat.md](syscall-compat.md#fork-children-lose-inherited-epolltimerfd-state) |
| P1 | syscall/fork | forked child loses inherited inotify watch and can hang | High | [syscall-compat.md](syscall-compat.md#forked-child-loses-inherited-inotify-watch-and-can-hang) |
| P1 | syscall/signalfd | `dup(signalfd)` loses signalfd semantics | High | [syscall-compat.md](syscall-compat.md#dupsignalfd-loses-signalfd-semantics) |
| P1 | syscall/poll | sentry `ppoll` masks stale fds instead of `POLLNVAL` | High | [syscall-compat.md](syscall-compat.md#sentry-ppoll-masks-stale-fds-instead-of-pollnval) |
| P1 | syscall/fd | sentry close-on-exec does not clean virtual fds | High | [syscall-compat.md](syscall-compat.md#sentry-close-on-exec-does-not-clean-virtual-fds) |
| P1 | daemon/cache | build-cache layer replacement is non-atomic | High | [daemon-tests-docs.md](daemon-tests-docs.md#build-cache-layer-replacement-is-non-atomic) |
| P1 | daemon/cgroup | fractional `--cpus` loses quota precision | High | [daemon-tests-docs.md](daemon-tests-docs.md#fractional---cpus-loses-quota-precision) |
| P1 | daemon/exec | exec start is not single-use | High | [daemon-tests-docs.md](daemon-tests-docs.md#exec-start-is-not-single-use) |
| P1 | daemon/restart | daemon restart reloads running containers without live process | High | [daemon-tests-docs.md](daemon-tests-docs.md#daemon-restart-reloads-running-containers-without-live-process) |
| P1 | daemon/prune | container prune leaves network endpoints | High | [daemon-tests-docs.md](daemon-tests-docs.md#container-prune-leaves-network-endpoints) |
| P1 | daemon/persistence | failed spawn terminal state is not persisted | High | [daemon-tests-docs.md](daemon-tests-docs.md#failed-spawn-terminal-state-is-not-persisted) |
| P1 | image/tag | `docker tag` aliases do not survive discovery | High | [daemon-tests-docs.md](daemon-tests-docs.md#docker-tag-aliases-do-not-survive-discovery) |
| P1 | daemon/logs | retained container logs are lost across daemon restart | High | [daemon-tests-docs.md](daemon-tests-docs.md#retained-container-logs-are-lost-across-daemon-restart) |
| P1 | daemon/ports | failed spawn leaks published host-port forwarders | High | [daemon-tests-docs.md](daemon-tests-docs.md#failed-spawn-leaks-published-host-port-forwarders) |
| P1 | daemon/ports | natural container exit leaves published host ports bound | High | [daemon-tests-docs.md](daemon-tests-docs.md#natural-container-exit-leaves-published-host-ports-bound) |
| P1 | image/arch | committed ELF-less x86_64 images rediscover as arm64 | High | [daemon-tests-docs.md](daemon-tests-docs.md#committed-elf-less-x86_64-images-rediscover-as-arm64) |
| P1 | image/arch | Docker save/load corrupts ELF-less Linux x86 images to arm64 | High | [daemon-tests-docs.md](daemon-tests-docs.md#docker-saveload-corrupts-elf-less-linux-x86-images-to-arm64) |
| P1 | image/delete | forced `rmi` deletes rootfs referenced by containers | High | [daemon-tests-docs.md](daemon-tests-docs.md#forced-rmi-deletes-rootfs-referenced-by-containers) |
| P1 | image/delete | non-forced `rmi` can delete rootfs used through alias | High | [daemon-tests-docs.md](daemon-tests-docs.md#non-forced-rmi-can-delete-rootfs-used-through-alias) |
| P1 | image/load | `docker load` of same tag rewrites existing container rootfs | High | [daemon-tests-docs.md](daemon-tests-docs.md#docker-load-of-same-tag-rewrites-existing-container-rootfs) |
| P1 | daemon/wait | container wait returns immediately for created containers | High | [daemon-tests-docs.md](daemon-tests-docs.md#container-wait-returns-immediately-for-created-containers) |
| P1 | daemon/wait | wait condition removed returns before removal | High | [daemon-tests-docs.md](daemon-tests-docs.md#wait-condition-removed-returns-before-removal) |
| P1 | daemon/exec | exec start does not recheck parent container state | High | [daemon-tests-docs.md](daemon-tests-docs.md#exec-start-does-not-recheck-parent-container-state) |
| P1 | daemon/events | exec lifecycle events are missing | High | [daemon-tests-docs.md](daemon-tests-docs.md#exec-lifecycle-events-are-missing) |
| P1 | daemon/durability | lifecycle mutations can succeed without durable state | High | [daemon-tests-docs.md](daemon-tests-docs.md#lifecycle-mutations-can-succeed-without-durable-state) |
| P1 | daemon/create | create with missing network persists partial container state | High | [daemon-tests-docs.md](daemon-tests-docs.md#create-with-missing-network-persists-partial-container-state) |
| P1 | daemon/create | create accepts image records whose rootfs is missing | High | [daemon-tests-docs.md](daemon-tests-docs.md#create-accepts-image-records-whose-rootfs-is-missing) |
| P1 | daemon/create | anonymous volume materialization failures are ignored | High | [daemon-tests-docs.md](daemon-tests-docs.md#anonymous-volume-materialization-failures-are-ignored) |
| P1 | daemon/volumes | volume create/delete/prune report success while storage is wrong | High | [daemon-tests-docs.md](daemon-tests-docs.md#volume-createdeleteprune-report-success-while-storage-is-wrong) |
| P1 | daemon/events | event filters broaden to match-all for supported keys | High | [daemon-tests-docs.md](daemon-tests-docs.md#event-filters-broaden-to-match-all-for-supported-keys) |
| P1 | daemon/attach | attach ignores stream selectors | High | [daemon-tests-docs.md](daemon-tests-docs.md#attach-ignores-stream-selectors) |
| P1 | env/exec | `execve(..., envp=NULL)` leaks default/stale env | High | [completeness-and-env.md](completeness-and-env.md#execve-envpnull-leaks-a-default-or-stale-environment) |
| P1 | env/exec | guest exec truncates argv at 255 args | High | [completeness-and-env.md](completeness-and-env.md#guest-exec-truncates-argv-at-255-args) |
| P1 | procfs/memory | `sysinfo(2)` ignores container memory cap | High | [completeness-and-env.md](completeness-and-env.md#sysinfo2-ignores-container-memory-cap) |
| P1 | display/xdg | xdg configure/ack race allows pre-ack presentation | High | [gpu-display-sentry.md](gpu-display-sentry.md#xdg-configureack-race-allows-pre-ack-presentation) |
| P1 | display/clipboard | inert data-device objects silently swallow selection | High | [gpu-display-sentry.md](gpu-display-sentry.md#data-device-objects-are-inert) |
| P1 | display/lifecycle | Wayland destructors do not remove objects/delete ids | High | [gpu-display-sentry.md](gpu-display-sentry.md#wayland-destructors-do-not-remove-objects) |
| P1 | display/lifecycle | destroyed `wl_buffer` can still be presented | High | [gpu-display-sentry.md](gpu-display-sentry.md#destroyed-wl_buffer-can-still-be-presented) |
| P1 | display/leak | shm pool mappings survive client disconnect | High | [gpu-display-sentry.md](gpu-display-sentry.md#shm-pool-mappings-survive-client-disconnect) |
| P1 | display/input | focus transfer sends enter without leave | High | [gpu-display-sentry.md](gpu-display-sentry.md#focus-transfer-sends-enter-without-leave) |
| P1 | display/input | pointer release/id reuse corrupts input routing | High | [gpu-display-sentry.md](gpu-display-sentry.md#pointer-release-and-id-reuse-corrupt-input-routing) |
| P1 | display/xdg | `xdg_popup` never gets configured or mapped | High | [gpu-display-sentry.md](gpu-display-sentry.md#xdg_popup-never-gets-configured-or-mapped) |
| P1 | gpu/corruption | nonzero texture mip copies alias base level | High | [gpu-display-sentry.md](gpu-display-sentry.md#nonzero-texture-mip-copies-alias-base-level) |
| P1 | gpu/depth | depth attachments ignore guest texture and load/store semantics | High | [gpu-display-sentry.md](gpu-display-sentry.md#depth-attachments-ignore-guest-texture-and-loadstore-semantics) |
| P1 | gpu/bounds | texture copy extents ignore texture dimensions | High | [gpu-display-sentry.md](gpu-display-sentry.md#texture-copy-extents-ignore-texture-dimensions) |
| P1 | gpu/bounds | bind-group buffer ranges are not validated | High | [gpu-display-sentry.md](gpu-display-sentry.md#bind-group-buffer-ranges-are-not-validated) |
| P1 | gpu/usage | GPU resource usage bits are ignored for render attachments | High | [gpu-display-sentry.md](gpu-display-sentry.md#gpu-resource-usage-bits-are-ignored-for-render-attachments) |
| P1 | gpu/usage | copy commands ignore copy usage bits | High | [gpu-display-sentry.md](gpu-display-sentry.md#copy-commands-ignore-copy-usage-bits) |
| P1 | display/shm | invalid shm buffer offset can panic compositor | High | [gpu-display-sentry.md](gpu-display-sentry.md#invalid-shm-buffer-offset-can-panic-compositor) |
| P1 | display/shm | shm buffer stride smaller than row is accepted | High | [gpu-display-sentry.md](gpu-display-sentry.md#shm-buffer-stride-smaller-than-row-is-accepted) |
| P1 | display/viewport | viewport source outside buffer is clamped | High | [gpu-display-sentry.md](gpu-display-sentry.md#viewport-source-outside-buffer-is-clamped) |
| P1 | registry/pull | digest-pinned references are parsed as tags | High | [archive-fs-compat.md](archive-fs-compat.md#digest-pinned-references-are-parsed-as-tags) |
| P1 | registry/pull | `--platform` ignores OS prefix | High | [archive-fs-compat.md](archive-fs-compat.md#--platform-ignores-os-prefix) |
| P1 | registry/pull | downloaded blob digests are not verified | Medium-high | [archive-fs-compat.md](archive-fs-compat.md#pull-does-not-verify-downloaded-blob-digests) |
| P1 | registry/pull | pull ignores config `rootfs.diff_ids` | High | [archive-fs-compat.md](archive-fs-compat.md#pull-ignores-config-rootfsdiff_ids) |
| P1 | registry/pull | failed registry pull leaves partial final rootfs | High | [archive-fs-compat.md](archive-fs-compat.md#failed-registry-pull-leaves-partial-final-rootfs) |
| P1 | registry/pull | pull accepts invalid manifest schema version | High | [archive-fs-compat.md](archive-fs-compat.md#pull-accepts-invalid-manifest-schema-version) |
| P1 | registry/pull | unsupported layer media type is unpacked as gzip | High | [archive-fs-compat.md](archive-fs-compat.md#unsupported-layer-media-type-is-unpacked-as-gzip) |
| P1 | registry/pull | config and layer descriptor sizes are not enforced | High | [archive-fs-compat.md](archive-fs-compat.md#config-and-layer-descriptor-sizes-are-not-enforced) |
| P1 | registry/layer | opaque whiteout pre-pass can remove paths outside rootfs | High | [archive-fs-compat.md](archive-fs-compat.md#opaque-whiteout-pre-pass-can-remove-paths-outside-rootfs) |
| P1 | registry/layer | registry layer extraction follows existing rootfs symlinks | High | [archive-fs-compat.md](archive-fs-compat.md#registry-layer-extraction-follows-existing-rootfs-symlinks) |
| P1 | registry/pull | pull accepts invalid config blobs as empty config | High | [archive-fs-compat.md](archive-fs-compat.md#pull-accepts-invalid-config-blobs-as-empty-config) |
| P1 | image/load | Docker load manifest name can delete outside image store | High | [archive-fs-compat.md](archive-fs-compat.md#docker-load-manifest-name-can-delete-outside-image-store) |
| P1 | image/load | malformed `dd-manifest.json` is treated as rootfs-only | High | [archive-fs-compat.md](archive-fs-compat.md#malformed-dd-manifestjson-is-treated-as-rootfs-only) |
| P1 | registry/push | concurrent registry manifest PUTs share one temp body file | High | [archive-fs-compat.md](archive-fs-compat.md#concurrent-registry-manifest-puts-share-one-temp-body-file) |
| P1 | image/store | image store path encoding collides distinct refs | High | [archive-fs-compat.md](archive-fs-compat.md#image-store-path-encoding-collides-distinct-refs) |
| P1 | archive/race | fixed per-process temp dirs race concurrent operations | Medium-high | [archive-fs-compat.md](archive-fs-compat.md#fixed-per-process-temp-dirs-race-concurrent-operations) |
| P2 | JIT/opcode | F16C `vcvtps2ph` ignores rounding immediate | High | [jit-and-opcodes.md](jit-and-opcodes.md#f16c-vcvtps2ph-ignores-rounding-immediate) |
| P2 | JIT/opcode | SSE4.2 string compare leaves AF stale | High | [jit-and-opcodes.md](jit-and-opcodes.md#sse42-string-compare-leaves-af-stale) |
| P2 | JIT/opcode | VEX `vcvt*ss/sd2si` likely lacks overflow fixups | Medium-high | [jit-and-opcodes.md](jit-and-opcodes.md#vex-vcvtsssd2si-lacks-legacy-overflow-fixups) |
| P2 | JIT/opcode | SSE2 `CVTPD2DQ` / `CVTTPD2DQ` return wrong integer-indefinite values | High | [jit-and-opcodes.md](jit-and-opcodes.md#sse2-cvtpd2dq-cvttpd2dq-return-wrong-integer-indefinite-values) |
| P2 | JIT/opcode | SSE `UCOMISS` / `COMISD` leave AF stale | High | [jit-and-opcodes.md](jit-and-opcodes.md#sse-ucomiss-comisd-leave-af-stale) |
| P2 | JIT/signal | `ICEBP` and invalid `0x62` bytes abort instead of guest traps | High | [jit-and-opcodes.md](jit-and-opcodes.md#icebp-and-invalid-0x62-bytes-abort-instead-of-guest-traps) |
| P2 | JIT/cache | SMC tracking has a capacity cliff | Medium | [jit-and-opcodes.md](jit-and-opcodes.md#smc-tracking-has-a-capacity-cliff) |
| P2 | JIT/signal | thread-directed signals do not interrupt blocking reads | High | [jit-and-opcodes.md](jit-and-opcodes.md#thread-directed-signals-do-not-interrupt-blocking-reads) |
| P2 | JIT/race | `LOCK BTS/BTR/BTC` use non-atomic bit-op path | High | [jit-and-opcodes.md](jit-and-opcodes.md#lock-btsbtrbtc-use-non-atomic-bit-op-path) |
| P2 | JIT/fp | MXCSR sticky exception flags/control bits are not modeled | Medium | [jit-and-opcodes.md](jit-and-opcodes.md#mxcsr-sticky-exception-flags-are-not-modeled) |
| P2 | JIT/x87 | x87 long double precision is truncated | High | [jit-and-opcodes.md](jit-and-opcodes.md#x87-long-double-precision-is-truncated) |
| P2 | syscall/mm | x86_64 `mincore(..., vec=NULL)` succeeds | High | [syscall-compat.md](syscall-compat.md#x86_64-mincore-vecnull-succeeds) |
| P2 | syscall/futex | unknown futex ops/flags can report success | Medium-high | [syscall-compat.md](syscall-compat.md#unknown-futex-opsflags-can-report-success) |
| P2 | syscall/fd | plain `dup()` drops proc-text read-only metadata | Medium | [syscall-compat.md](syscall-compat.md#plain-dup-drops-proc-text-read-only-metadata) |
| P2 | syscall/select | sentry `pselect6` masks invalid virtual fd bits | Medium | [syscall-compat.md](syscall-compat.md#sentry-pselect6-masks-invalid-virtual-fd-bits) |
| P2 | syscall/fd | pipe-size fcntls fake success on invalid fds | High | [syscall-compat.md](syscall-compat.md#pipe-size-fcntls-fake-success-on-invalid-fds) |
| P2 | syscall/signal | signal ucontext stack metadata is zero | High | [syscall-compat.md](syscall-compat.md#signal-ucontext-stack-metadata-is-zero) |
| P2 | syscall/timerfd | `dup(timerfd)` loses timerfd semantics | High | [syscall-compat.md](syscall-compat.md#duptimerfd-loses-timerfd-semantics) |
| P2 | syscall/inotify | `dup(inotify_fd)` loses inotify read semantics | High | [syscall-compat.md](syscall-compat.md#dupinotify_fd-loses-inotify-read-semantics) |
| P2 | syscall/compat | pidfd invalid flags and fixed registry capacity | High | [syscall-compat.md](syscall-compat.md#pidfd-invalid-flags-and-fixed-registry-capacity) |
| P2 | syscall/compat | aarch64 `AT_PAGESZ` exposes host page size | High | [syscall-compat.md](syscall-compat.md#aarch64-at_pagesz-exposes-host-page-size) |
| P2 | syscall/compat | `F_SETLEASE` / `F_NOTIFY` fake support | High | [syscall-compat.md](syscall-compat.md#f_setlease-f_notify-return-success-without-arming-anything) |
| P2 | daemon/runtime | live network connect/disconnect mutates daemon state only | High | [daemon-tests-docs.md](daemon-tests-docs.md#live-network-connectdisconnect-mutates-daemon-state-only) |
| P2 | daemon/build | Dockerfile runtime metadata is accepted but dropped | High | [daemon-tests-docs.md](daemon-tests-docs.md#dockerfile-runtime-metadata-is-accepted-but-dropped) |
| P2 | daemon/inspect | `docker top` returns fake processes for stopped containers | High | [daemon-tests-docs.md](daemon-tests-docs.md#docker-top-returns-fake-processes-for-stopped-containers) |
| P2 | registry/race | concurrent pulls share a layer temp file | High | [daemon-tests-docs.md](daemon-tests-docs.md#concurrent-pulls-share-a-layer-temp-file) |
| P2 | daemon/stats | stats stream captures a stale pid | Medium | [daemon-tests-docs.md](daemon-tests-docs.md#stats-stream-captures-a-stale-pid) |
| P2 | daemon/events | events are live-only and lossy | High | [daemon-tests-docs.md](daemon-tests-docs.md#events-are-live-only-and-lossy) |
| P2 | daemon/logs | `logs -f` can drop output for slow clients | Medium-high | [daemon-tests-docs.md](daemon-tests-docs.md#logs--f-can-drop-output-for-slow-clients) |
| P2 | daemon/health | `Healthcheck: [NONE]` create override makes fake health | High | [daemon-tests-docs.md](daemon-tests-docs.md#healthcheck-none-create-override-makes-fake-health) |
| P2 | daemon/lifecycle | stop timeout marks exited before reaper confirms death | Medium | [daemon-tests-docs.md](daemon-tests-docs.md#stop-timeout-marks-exited-before-reaper-confirms-death) |
| P2 | daemon/config | `DDOCKERD_SOCK` startup unlinks configured path | Medium-high | [daemon-tests-docs.md](daemon-tests-docs.md#ddockerd_sock-startup-unlinks-configured-path) |
| P2 | daemon/events | fast-exit event ordering can emit `die` before `start` | Medium | [daemon-tests-docs.md](daemon-tests-docs.md#fast-exit-event-ordering-can-emit-die-before-start) |
| P2 | daemon/events | container prune deletes without destroy events | High | [daemon-tests-docs.md](daemon-tests-docs.md#container-prune-deletes-without-destroy-events) |
| P2 | daemon/events | network prune deletes without destroy events | High | [daemon-tests-docs.md](daemon-tests-docs.md#network-prune-deletes-without-destroy-events) |
| P2 | daemon/events | volume prune deletes without destroy events | High | [daemon-tests-docs.md](daemon-tests-docs.md#volume-prune-deletes-without-destroy-events) |
| P2 | daemon/events | network connect/disconnect mutate endpoints without events | High | [daemon-tests-docs.md](daemon-tests-docs.md#network-connectdisconnect-mutate-endpoints-without-events) |
| P2 | image/prune | image prune is a hard-coded no-op | High | [daemon-tests-docs.md](daemon-tests-docs.md#image-prune-is-a-hard-coded-no-op) |
| P2 | daemon/events | image event filters drop image events | High | [daemon-tests-docs.md](daemon-tests-docs.md#image-event-filters-drop-image-events) |
| P2 | daemon/routes | `POST /system/prune` is not routed | High | [daemon-tests-docs.md](daemon-tests-docs.md#post-systemprune-is-not-routed) |
| P2 | docs/tests | gap inventory and architecture docs are stale/missing | High | [daemon-tests-docs.md](daemon-tests-docs.md#gap-and-architecture-docs-are-not-auditable) |
| P2 | gpu/robustness | software backend panics on wrapping offsets | High | [gpu-display-sentry.md](gpu-display-sentry.md#gpu-software-backend-panics-on-wrapping-offsets) |
| P2 | gpu/compat | dmabuf advertises LINEAR buffers it cannot use | Medium-high | [gpu-display-sentry.md](gpu-display-sentry.md#dmabuf-advertises-linear-buffers-it-cannot-use) |
| P2 | gpu/compat | Metal backend skips missing bind-group resources | High | [gpu-display-sentry.md](gpu-display-sentry.md#metal-backend-skips-missing-bind-group-resources) |
| P2 | gpu/compat | Metal render target texture id aliases guest texture id `1` | Medium-high | [gpu-display-sentry.md](gpu-display-sentry.md#metal-render-target-texture-id-can-alias-guest-texture-id-1) |
| P2 | display/input | released input objects remain active | Medium | [gpu-display-sentry.md](gpu-display-sentry.md#released-input-objects-remain-active) |
| P2 | display/compat | presenter failures still release buffers and fire callbacks | Medium | [gpu-display-sentry.md](gpu-display-sentry.md#presenter-failures-still-release-buffers-and-fire-frame-callbacks) |
| P2 | display/window | native window close is not propagated | Medium-high | [gpu-display-sentry.md](gpu-display-sentry.md#native-window-close-is-not-propagated) |
| P2 | display/input | keyboard repeat is internally contradictory | Medium | [gpu-display-sentry.md](gpu-display-sentry.md#keyboard-repeat-is-internally-contradictory) |
| P2 | gpu/compat | Metal duplicate IDs and format fallbacks diverge | Medium-high | [gpu-display-sentry.md](gpu-display-sentry.md#metal-duplicate-ids-and-format-fallbacks-diverge-from-checked-backends) |
| P2 | gpu/compat | Metal shader id can retain stale MSL | Medium-high | [gpu-display-sentry.md](gpu-display-sentry.md#metal-shader-id-can-retain-stale-msl) |
| P2 | gpu/corruption | software texture readback ignores `bytes_per_row` | High | [gpu-display-sentry.md](gpu-display-sentry.md#software-texture-readback-ignores-bytes_per_row) |
| P2 | gpu/oracle | software backend accepts draws with missing vertex buffers | High | [gpu-display-sentry.md](gpu-display-sentry.md#software-backend-accepts-draws-with-missing-vertex-buffers) |
| P2 | gpu/compat | multisample texture descriptors are silently downleveled | High | [gpu-display-sentry.md](gpu-display-sentry.md#multisample-texture-descriptors-are-silently-downleveled) |
| P2 | gpu/present | present accepts texture size mismatch | High | [gpu-display-sentry.md](gpu-display-sentry.md#present-accepts-texture-size-mismatch) |
| P2 | display/shm | unsupported `wl_shm` formats are accepted | High | [gpu-display-sentry.md](gpu-display-sentry.md#unsupported-wl_shm-formats-are-accepted) |
| P2 | gpu/compat | 3D/depth texture descriptors are flattened | High | [gpu-display-sentry.md](gpu-display-sentry.md#3ddepth-texture-descriptors-are-flattened) |
| P1 | gpu/robustness | oversized texture descriptors panic software backend | High | [gpu-display-sentry.md](gpu-display-sentry.md#oversized-texture-descriptors-panic-software-backend) |
| P2 | display/scale | `wl_surface.set_buffer_scale(0)` is silently normalized | High | [gpu-display-sentry.md](gpu-display-sentry.md#wl_surfaceset_buffer_scale0-is-silently-normalized) |
| P1 | display/viewport | invalid viewport destination keeps stale state | High | [gpu-display-sentry.md](gpu-display-sentry.md#invalid-viewport-destination-keeps-stale-state) |
| P1 | gpu/viewport | GPU `SetViewport` invalid depth range is accepted | High | [gpu-display-sentry.md](gpu-display-sentry.md#gpu-setviewport-invalid-depth-range-is-accepted) |
| P1 | gpu/corruption | failed GPU submit leaves partial resource mutations | High | [gpu-display-sentry.md](gpu-display-sentry.md#failed-gpu-submit-leaves-partial-resource-mutations) |
| P1 | gpu/lifecycle | bind groups can mutate reused resource IDs | High | [gpu-display-sentry.md](gpu-display-sentry.md#bind-groups-can-mutate-reused-resource-ids) |
| P1 | gpu/validation | invalid texture copy row pitch is accepted | High | [gpu-display-sentry.md](gpu-display-sentry.md#invalid-texture-copy-row-pitch-is-accepted) |
| P1 | gpu/validation | pipeline color-target format can diverge from attachment | High | [gpu-display-sentry.md](gpu-display-sentry.md#pipeline-color-target-format-can-diverge-from-attachment) |
| P1 | gpu/validation | vertex and index draw range validation is missing | High | [gpu-display-sentry.md](gpu-display-sentry.md#vertex-and-index-draw-range-validation-is-missing) |
| P1 | gpu/shader | invalid shader modules are accepted | High | [gpu-display-sentry.md](gpu-display-sentry.md#invalid-shader-modules-are-accepted) |
| P1 | gpu/binding | storage textures can be bound as sampled textures | High | [gpu-display-sentry.md](gpu-display-sentry.md#storage-textures-can-be-bound-as-sampled-textures) |
| P1 | gpu/hazard | same-pass sampled render-target hazard is accepted | High | [gpu-display-sentry.md](gpu-display-sentry.md#same-pass-sampled-render-target-hazard-is-accepted) |
| P1 | gpu/lifecycle | destroyed sampled textures remain usable through bind groups | High | [gpu-display-sentry.md](gpu-display-sentry.md#destroyed-sampled-textures-remain-usable-through-bind-groups) |
| P2 | gpu/compat | zero-mip texture descriptors are accepted | High | [gpu-display-sentry.md](gpu-display-sentry.md#zero-mip-texture-descriptors-are-accepted) |
| P2 | gpu/validation | command encoder pass sequencing is not validated | High | [gpu-display-sentry.md](gpu-display-sentry.md#command-encoder-pass-sequencing-is-not-validated) |
| P2 | gpu/validation | vertex attribute layout validation is missing | High | [gpu-display-sentry.md](gpu-display-sentry.md#vertex-attribute-layout-validation-is-missing) |
| P2 | gpu/sync | software fence waits fabricate completion | High | [gpu-display-sentry.md](gpu-display-sentry.md#software-fence-waits-fabricate-completion) |
| P2 | gpu/sampler | Metal sampler creation drops descriptor fields | High | [gpu-display-sentry.md](gpu-display-sentry.md#metal-sampler-creation-drops-descriptor-fields) |
| P2 | gpu/readback | texture-to-buffer readback accepts unaligned offsets | High | [gpu-display-sentry.md](gpu-display-sentry.md#texture-to-buffer-readback-accepts-unaligned-offsets) |
| P2 | gpu/compat | zero-sized GPU textures are accepted | High | [gpu-display-sentry.md](gpu-display-sentry.md#zero-sized-gpu-textures-are-accepted) |
| P2 | gpu/robustness | wrapping bind-group offsets panic during dispatch | High | [gpu-display-sentry.md](gpu-display-sentry.md#wrapping-bind-group-offsets-panic-during-dispatch) |
| P2 | display/lifecycle | Metal cleanup diverges from checked backends | Medium-high | [gpu-display-sentry.md](gpu-display-sentry.md#metal-cleanup-diverges-from-checked-backends) |
| P2 | env/runtime | `DDJIT_SANDBOX` public mode is intentionally avoided by tests | High | [completeness-and-env.md](completeness-and-env.md#ddjit_sandbox-public-mode-is-intentionally-avoided-by-tests) |
| P2 | env/durability | `S3DB_DURABILITY` silently changes fsync semantics | High | [completeness-and-env.md](completeness-and-env.md#s3db_durability-hidden-fsync-semantics) |
| P2 | env/cache | aarch64 pcache key omits `NOSTEALFAST` | Medium | [completeness-and-env.md](completeness-and-env.md#aarch64-pcache-key-omits-nostealfast) |
| P2 | env/cache | per-container `DDJIT_NOPCACHE` is dropped by typed launch | Medium | [completeness-and-env.md](completeness-and-env.md#per-container-ddjit_nopcache-is-dropped-by-typed-launch) |
| P2 | procfs/limits | `/proc/self/limits` disagrees with `getrlimit(RLIMIT_CORE)` | High | [completeness-and-env.md](completeness-and-env.md#procselflimits-disagrees-with-getrlimitrlimit_core) |
| P2 | procfs/env | `/proc/self/environ` omits guest defaults | High | [completeness-and-env.md](completeness-and-env.md#procselfenviron-omits-guest-defaults) |
| P2 | procfs/identity | `/proc/self/status` reports root uid/gid | High | [completeness-and-env.md](completeness-and-env.md#procselfstatus-reports-root-uidgid) |
| P2 | procfs/env | hidden proc switches change peer procfs | Medium | [completeness-and-env.md](completeness-and-env.md#hidden-proc-switches-change-peer-procfs) |
| P2 | procfs/isa | `/proc/version` is guest-ISA blind | Medium | [completeness-and-env.md](completeness-and-env.md#procversion-is-guest-isa-blind) |
| P1 | cgroup/compat | `/sys/fs/cgroup` root is advertised but not listable | High | [completeness-and-env.md](completeness-and-env.md#sysfscgroup-root-is-advertised-but-not-listable) |
| P1 | cgroup/accounting | cgroup membership omits forked children | High | [completeness-and-env.md](completeness-and-env.md#cgroup-membership-omits-forked-children) |
| P1 | cgroup/limits | `DD_PIDS_MAX` is not enforced for forked processes | High | [completeness-and-env.md](completeness-and-env.md#dd_pids_max-is-not-enforced-for-forked-processes) |
| P1 | cgroup/accounting | cgroup memory usage is process-local | High | [completeness-and-env.md](completeness-and-env.md#cgroup-memory-usage-is-process-local) |
| P1 | procfs/threads | `/proc/self/task` enumeration omits live guest threads | High | [completeness-and-env.md](completeness-and-env.md#procselftask-enumeration-omits-live-guest-threads) |
| P1 | sysfs/network | network-none hides `eth0` in readdir but direct lookup exposes it | High | [completeness-and-env.md](completeness-and-env.md#network-none-hides-eth0-in-readdir-but-direct-lookup-exposes-it) |
| P1 | procfs/fd | closed `/proc/self/fd/N` reports stale existence | High | [completeness-and-env.md](completeness-and-env.md#closed-procselffdn-reports-stale-existence) |
| P2 | procfs/accounting | `/proc/stat processes` is live count instead of cumulative forks | High | [completeness-and-env.md](completeness-and-env.md#procstat-processes-is-live-count-instead-of-cumulative-forks) |
| P2 | procfs/fd | peer `/proc/<pid>/fd` is advertised but not openable | High | [completeness-and-env.md](completeness-and-env.md#peer-procfd-is-advertised-but-not-openable) |
| P1 | sysfs/cpu | CPU topology sysfs is direct-readable but not listable | High | [completeness-and-env.md](completeness-and-env.md#cpu-topology-sysfs-is-direct-readable-but-not-listable) |
| P1 | procfs/ns | `/proc/self/ns` is missing while namespace links work | High | [completeness-and-env.md](completeness-and-env.md#procselfns-is-missing-while-namespace-links-work) |
| P1 | cgroup/compat | cgroup controllers advertised but required files missing | High | [completeness-and-env.md](completeness-and-env.md#cgroup-controllers-advertised-but-required-files-missing) |
| P1 | procfs/threads | `/proc/self/status` Threads is hardcoded to one | High | [completeness-and-env.md](completeness-and-env.md#procselfstatus-threads-is-hardcoded-to-one) |
| P2 | procfs/ns | peer `/proc/<pid>/ns` is absent | High | [completeness-and-env.md](completeness-and-env.md#peer-procns-is-absent) |
| P2 | procfs/net | `/proc/net/unix` ignores live AF_UNIX sockets | High | [completeness-and-env.md](completeness-and-env.md#procnetunix-ignores-live-af_unix-sockets) |
| P1 | procfs/net | `/proc/net` direct leaves exist but directory is not enumerable | High | [completeness-and-env.md](completeness-and-env.md#procnet-direct-leaves-exist-but-directory-is-not-enumerable) |
| P1 | procfs/net | `/proc/net/sockstat` and `sockstat6` are missing | High | [completeness-and-env.md](completeness-and-env.md#procnetsockstat-and-sockstat6-are-missing) |
| P1 | cgroup/compat | cgroup v2 omits additional standard controller files | High | [completeness-and-env.md](completeness-and-env.md#cgroup-v2-omits-additional-standard-controller-files) |
| P1 | procfs/task | `/proc/self/task/<tid>` lists files direct lookup cannot open | High | [completeness-and-env.md](completeness-and-env.md#procselftask-lists-files-direct-lookup-cannot-open) |
| P1 | procfs/self | `/proc/self` readdir omits direct-supported proc files | High | [completeness-and-env.md](completeness-and-env.md#procself-readdir-omits-direct-supported-proc-files) |
| P1 | procfs/mounts | bind mounts are missing from mount tables | High | [completeness-and-env.md](completeness-and-env.md#bind-mounts-are-missing-from-mount-tables) |
| P1 | procfs/smaps | `/proc/self/smaps` can hang on read | High | [completeness-and-env.md](completeness-and-env.md#procselfsmaps-can-hang-on-read) |
| P1 | procfs/statfs | `statfs` is wrong for synthetic proc/sys leaves | High | [completeness-and-env.md](completeness-and-env.md#statfs-is-wrong-for-synthetic-procsys-leaves) |
| P1 | procfs/statfs | `statfs.f_flags` is always zero | High | [completeness-and-env.md](completeness-and-env.md#statfsf_flags-is-always-zero) |
| P1 | procfs/io | `/proc/self/io` is missing | High | [completeness-and-env.md](completeness-and-env.md#procselfio-is-missing) |
| P1 | devfs/random | `/dev/urandom` writes fail with `EPERM` | High | [completeness-and-env.md](completeness-and-env.md#devurandom-writes-fail-with-eperm) |
| P1 | devfs/tty | `/dev/tty` nonblocking read reports EOF instead of `EAGAIN` | High | [completeness-and-env.md](completeness-and-env.md#devtty-nonblocking-read-reports-eof-instead-of-eagain) |
| P2 | procfs/fd | `/proc/self/fdinfo` is missing | High | [completeness-and-env.md](completeness-and-env.md#procselffdinfo-is-missing) |
| P2 | procfs/state | futex-blocked processes report running in procfs | High | [completeness-and-env.md](completeness-and-env.md#futex-blocked-processes-report-running-in-procfs) |
| P2 | sysfs/block | `/sys/class/block` and `/sys/block` are absent | High | [completeness-and-env.md](completeness-and-env.md#sysclassblock-and-sysblock-are-absent) |
| P2 | procfs/fd | `/dev/fd` symlink cannot be enumerated | High | [completeness-and-env.md](completeness-and-env.md#devfd-symlink-cannot-be-enumerated) |
| P2 | procfs/maps | `/proc/self/maps` omits RELRO mapping detail | High | [completeness-and-env.md](completeness-and-env.md#procselfmaps-omits-relro-mapping-detail) |
| P2 | procfs/content | `/proc/meminfo` and `/proc/stat` are sparse | High | [completeness-and-env.md](completeness-and-env.md#procmeminfo-and-procstat-are-sparse) |
| P2 | procfs/tty | `/proc/tty` surface is absent | High | [completeness-and-env.md](completeness-and-env.md#proctty-surface-is-absent) |
| P2 | procfs/devices | `/proc/devices` has empty block device section | High | [completeness-and-env.md](completeness-and-env.md#procdevices-has-empty-block-device-section) |
| P2 | launch/config | path-list config still uses delimiter env strings | Medium | [completeness-and-env.md](completeness-and-env.md#typed-launch-path-lists-still-use-delimiter-env-strings) |
| P2 | rendering/tests | GUI probe sources are outside default matrix | High | [gpu-display-sentry.md](gpu-display-sentry.md#rendering-coverage-gaps-are-silent) |
| P2 | import/cleanup | failed image import leaves partial target | High | [archive-fs-compat.md](archive-fs-compat.md#import-failure-leaves-partial-target) |
| P2 | volume/metadata | anonymous volume copy-up drops seeded metadata | High | [archive-fs-compat.md](archive-fs-compat.md#anonymous-volume-copy-up-drops-seeded-directory-metadata) |
| P1 | volume/contents | image `VOLUME` copy-up can escape the image rootfs | High | [archive-fs-compat.md](archive-fs-compat.md#image-volume-copy-up-can-escape-the-image-rootfs) |
| P1 | archive/metadata | UID/GID metadata is lost on load/import/cp PUT | High | [archive-fs-compat.md](archive-fs-compat.md#uidgid-metadata-is-lost-on-loadimportcp-put) |
| P2 | build/metadata | Dockerfile `COPY`/`ADD` metadata flags are ignored | High | [archive-fs-compat.md](archive-fs-compat.md#dockerfile-copy-add-metadata-flags-are-ignored) |
| P2 | archive/perf | sparse files expand through save/push tar paths | High | [archive-fs-compat.md](archive-fs-compat.md#sparse-files-expand-through-savepush-tar-paths) |
| P2 | archive/metadata | save/cp GET truncate nanosecond mtimes | High | [archive-fs-compat.md](archive-fs-compat.md#savecp-get-truncate-nanosecond-mtimes) |
| P2 | archive/devices | valid device-node tars fail load/import | High | [archive-fs-compat.md](archive-fs-compat.md#valid-device-node-tars-fail-loadimport) |
| P2 | image/metadata | daemon save/load drops lifecycle metadata | High | [archive-fs-compat.md](archive-fs-compat.md#daemon-saveload-drops-lifecycle-metadata) |
| P2 | build/cache | build cache digests break on apostrophes in paths | High | [archive-fs-compat.md](archive-fs-compat.md#build-cache-digests-break-on-apostrophes-in-paths) |
| P1 | build/history | built image history is synthetic | High | [archive-fs-compat.md](archive-fs-compat.md#built-image-history-is-synthetic) |
| P1 | build/cache | build cache seed ignores base image config | High | [archive-fs-compat.md](archive-fs-compat.md#build-cache-seed-ignores-base-image-config) |
| P2 | build/metadata | Dockerfile `LABEL` does not merge base labels | High | [archive-fs-compat.md](archive-fs-compat.md#dockerfile-label-does-not-merge-base-labels) |
| P2 | build/context | `.dockerignore` is not applied | High | [archive-fs-compat.md](archive-fs-compat.md#dockerignore-is-not-applied) |
| P1 | build/env | Dockerfile `ENV` interpolation ignores prior ENV | High | [archive-fs-compat.md](archive-fs-compat.md#dockerfile-env-interpolation-ignores-prior-env) |
| P1 | build/arg | pre-FROM `ARG` leaks into stage scope | High | [archive-fs-compat.md](archive-fs-compat.md#pre-from-arg-leaks-into-stage-scope) |
| P1 | build/shell | Dockerfile `SHELL` is ignored | High | [archive-fs-compat.md](archive-fs-compat.md#dockerfile-shell-is-ignored) |
| P1 | build/onbuild | Dockerfile `ONBUILD` triggers are ignored | High | [archive-fs-compat.md](archive-fs-compat.md#dockerfile-onbuild-triggers-are-ignored) |
| P1 | build/target | unknown build target builds last stage | High | [archive-fs-compat.md](archive-fs-compat.md#unknown-build-target-builds-last-stage) |
| P2 | build/cleanup | failed build leaves partial image directory | High | [archive-fs-compat.md](archive-fs-compat.md#failed-build-leaves-partial-image-directory) |
| P2 | build/parser | Dockerfile `# escape=` directive is ignored | High | [archive-fs-compat.md](archive-fs-compat.md#dockerfile-escape-directive-is-ignored) |
| P2 | build/parser | exec-form JSON drops non-string elements | High | [archive-fs-compat.md](archive-fs-compat.md#exec-form-json-drops-non-string-elements) |
| P2 | build/workdir | relative `WORKDIR ..` persists a different config path | High | [archive-fs-compat.md](archive-fs-compat.md#relative-workdir-dotdot-persists-a-different-config-path) |
| P2 | build/env | `ENV` override moves inherited keys to the end | High | [archive-fs-compat.md](archive-fs-compat.md#env-override-moves-inherited-keys-to-the-end) |
| P2 | image/metadata | save/load drops xattrs | High | [archive-fs-compat.md](archive-fs-compat.md#saveload-drops-xattrs) |
| P2 | build/metadata | Dockerfile `USER` is ignored in build output | High | [archive-fs-compat.md](archive-fs-compat.md#dockerfile-user-is-ignored-in-build-output) |
| P2 | build/base | `FROM` local lookup ignores tag | Medium-high | [archive-fs-compat.md](archive-fs-compat.md#from-local-lookup-ignores-tag) |
| P2 | image/identity | tag/digest reporting is synthetic and inconsistent | Medium-high | [archive-fs-compat.md](archive-fs-compat.md#tagdigest-reporting-is-synthetic-and-inconsistent) |
| P2 | image/metadata | daemon save/load drops image labels | High | [daemon-tests-docs.md](daemon-tests-docs.md#daemon-saveload-drops-image-labels) |
| P2 | image/metadata | daemon discovery drops image labels | High | [daemon-tests-docs.md](daemon-tests-docs.md#daemon-discovery-drops-image-labels) |
| P2 | daemon/restart | restart state load overwrites persisted container arch | High | [daemon-tests-docs.md](daemon-tests-docs.md#restart-state-load-overwrites-persisted-container-arch) |
| P1 | image/commit | Docker commit drops container user | High | [daemon-tests-docs.md](daemon-tests-docs.md#docker-commit-drops-container-user) |
| P1 | daemon/restart | restarting containers can stay stuck after daemon restart | High | [daemon-tests-docs.md](daemon-tests-docs.md#restarting-containers-can-stay-stuck-after-daemon-restart) |
| P2 | daemon/logs | logs time filters reject RFC3339 forms | High | [daemon-tests-docs.md](daemon-tests-docs.md#logs-time-filters-reject-rfc3339-forms) |
| P2 | daemon/logs | logs timestamps are second-precision | High | [daemon-tests-docs.md](daemon-tests-docs.md#logs-timestamps-are-second-precision) |
| P2 | daemon/stats | stats JSON is internally inconsistent | High | [daemon-tests-docs.md](daemon-tests-docs.md#stats-json-is-internally-inconsistent) |
| P2 | daemon/system | system df overcounts containers for sibling tags | High | [daemon-tests-docs.md](daemon-tests-docs.md#system-df-overcounts-containers-for-sibling-tags) |
| P2 | daemon/system | image usage active count counts containers, not images | High | [daemon-tests-docs.md](daemon-tests-docs.md#image-usage-active-count-counts-containers-not-images) |
| P2 | daemon/system | volume usage never reports live references | High | [daemon-tests-docs.md](daemon-tests-docs.md#volume-usage-never-reports-live-references) |
| P2 | daemon/system | build-cache totals can be nonzero with empty item lists | High | [daemon-tests-docs.md](daemon-tests-docs.md#build-cache-totals-can-be-nonzero-with-empty-item-lists) |
| P2 | daemon/version | daemon version endpoints and Server header are stale | High | [daemon-tests-docs.md](daemon-tests-docs.md#daemon-version-endpoints-and-server-header-are-stale) |
| P2 | daemon/events | `event=health_status` filter misses health transitions | High | [daemon-tests-docs.md](daemon-tests-docs.md#eventhealth_status-filter-misses-health-transitions) |
| P2 | daemon/events | non-epoch `until` values turn bounded events into unbounded streams | High | [daemon-tests-docs.md](daemon-tests-docs.md#non-epoch-until-values-turn-bounded-events-into-unbounded-streams) |
| P2 | daemon/events | create events can be emitted before durable state success | High | [daemon-tests-docs.md](daemon-tests-docs.md#create-events-can-be-emitted-before-durable-state-success) |
| P2 | daemon/cleanup | container rm/prune drop state when writable-layer cleanup fails | High | [daemon-tests-docs.md](daemon-tests-docs.md#container-rmprune-drop-state-when-writable-layer-cleanup-fails) |
| P2 | image/cleanup | image rmi reports deletion when backing store removal fails | High | [daemon-tests-docs.md](daemon-tests-docs.md#image-rmi-reports-deletion-when-backing-store-removal-fails) |
| P3 | daemon/inspect | inspect can serialize contradictory dead state | High | [daemon-tests-docs.md](daemon-tests-docs.md#inspect-can-serialize-contradictory-dead-state) |
| P3 | daemon/plugins | plugin inventory endpoint is missing despite info advertising plugins | High | [daemon-tests-docs.md](daemon-tests-docs.md#plugin-inventory-endpoint-is-missing-despite-info-advertising-plugins) |
| P2 | registry/platform | platform selection discards OCI variant | Medium | [archive-fs-compat.md](archive-fs-compat.md#platform-selection-discards-oci-variant) |
| P2 | image/load | Docker load accepts unsupported manifest OS as Linux | High | [archive-fs-compat.md](archive-fs-compat.md#docker-load-accepts-unsupported-manifest-os-as-linux) |
| P2 | registry/platform | registry config with unsupported OS imports as Linux | High | [archive-fs-compat.md](archive-fs-compat.md#registry-config-with-unsupported-os-imports-as-linux) |
| P2 | registry/pull | valid zero-layer manifests are rejected | High | [archive-fs-compat.md](archive-fs-compat.md#valid-zero-layer-manifests-are-rejected) |
| P2 | registry/http | layer downloads treat HTTP error bodies as blobs | High | [archive-fs-compat.md](archive-fs-compat.md#layer-downloads-treat-http-error-bodies-as-blobs) |
| P2 | registry/push | registry push layer packaging breaks on apostrophes in paths | High | [archive-fs-compat.md](archive-fs-compat.md#registry-push-layer-packaging-breaks-on-apostrophes-in-paths) |
| P1 | image/load | Docker load same-name archives delete existing rootfs in place | High | [archive-fs-compat.md](archive-fs-compat.md#docker-load-same-name-archives-delete-existing-rootfs-in-place) |
| P1 | registry/push | Docker push drops runtime metadata from OCI config | High | [archive-fs-compat.md](archive-fs-compat.md#docker-push-drops-runtime-metadata-from-oci-config) |
| P1 | syscall/fd | inotify/timerfd create without flags still sets close-on-exec | High | [syscall-compat.md](syscall-compat.md#inotify_init10-and-timerfd_create-0-set-close-on-exec) |
| P1 | syscall/timerfd | `timerfd` CLOCK_REALTIME absolute deadlines are treated as monotonic | High | [syscall-compat.md](syscall-compat.md#timerfd-clock_realtime-absolute-deadlines-are-treated-as-monotonic) |
| P1 | syscall/epoll | `epoll_pwait` ignores temporary signal mask | High | [syscall-compat.md](syscall-compat.md#epoll_pwait-ignores-temporary-signal-mask) |
| P1 | syscall/futex | `FUTEX_WAIT_BITSET` / `FUTEX_WAKE_BITSET` ignore masks | High | [syscall-compat.md](syscall-compat.md#futex_wait_bitset-futex_wake_bitset-ignore-masks) |
| P1 | syscall/wait | `wait4` misses `WCONTINUED` and corrupts final status | High | [syscall-compat.md](syscall-compat.md#wait4-misses-wcontinued-and-corrupts-final-status) |
| P1 | syscall/signal | `SA_NOCLDWAIT` does not suppress zombies | High | [syscall-compat.md](syscall-compat.md#sa_nocldwait-does-not-suppress-zombies) |
| P1 | syscall/signal | aarch64 signal ucontext omits FPSIMD context record | High | [syscall-compat.md](syscall-compat.md#aarch64-signal-ucontext-omits-fpsimd-context-record) |
| P1 | syscall/signal | `kill(0, sig)` only signals the caller | High | [syscall-compat.md](syscall-compat.md#kill0-sig-only-signals-the-caller) |
| P1 | syscall/tty | `tcgetpgrp` / `tcsetpgrp` fake success on non-TTY fds | High | [syscall-compat.md](syscall-compat.md#tcgetpgrp-tcsetpgrp-fake-success-on-non-tty-fds) |
| P1 | syscall/mm | guest `PROT_NONE` mappings remain directly readable | High | [syscall-compat.md](syscall-compat.md#guest-prot_none-mappings-remain-directly-readable) |
| P1 | syscall/mm | writes to `mprotect(PROT_READ)` pages do not fault | High | [syscall-compat.md](syscall-compat.md#writes-to-mprotectprot_read-pages-do-not-fault) |
| P1 | syscall/mm | execute permission is not enforced for guest fetch | High | [syscall-compat.md](syscall-compat.md#execute-permission-is-not-enforced-for-guest-fetch) |
| P1 | aarch64/atomic | low-address exclusive and pair atomics hang | High | [jit-and-opcodes.md](jit-and-opcodes.md#aarch64-low-address-exclusive-and-pair-atomics-hang) |
| P1 | aarch64/smc | threaded self-modifying code executes stale translations | High | [jit-and-opcodes.md](jit-and-opcodes.md#aarch64-threaded-self-modifying-code-executes-stale-translations) |
| P1 | x86/smc | SMC protection table overflow can hang on code rewrite | High | [jit-and-opcodes.md](jit-and-opcodes.md#x86-smc-protection-table-overflow-can-hang-on-code-rewrite) |
| P1 | syscall/fs | `unlinkat` ignores unknown flags and deletes the file | High | [syscall-compat.md](syscall-compat.md#unlinkat-ignores-unknown-flags-and-deletes-the-file) |
| P1 | syscall/fs | `fallocate` accepts invalid modes and mutates data | High | [syscall-compat.md](syscall-compat.md#fallocate-accepts-invalid-modes-and-mutates-data) |
| P1 | syscall/fs | `fallocate` range overflow reports success | High | [syscall-compat.md](syscall-compat.md#fallocate-range-overflow-reports-success) |
| P1 | syscall/fs | `fchown` / `fchownat` fake success and corrupt ownership | High | [syscall-compat.md](syscall-compat.md#fchown-fchownat-fake-success-and-corrupt-ownership) |
| P1 | syscall/fs | `openat2` ignores ABI validation and resolve restrictions | High | [syscall-compat.md](syscall-compat.md#openat2-ignores-abi-validation-and-resolve-restrictions) |
| P1 | syscall/wait | raw `waitid(..., rusage)` leaves buffer untouched | High | [syscall-compat.md](syscall-compat.md#raw-waitid-rusage-leaves-buffer-untouched) |
| P1 | syscall/wait | default core status contradicts `RLIMIT_CORE=0` | High | [syscall-compat.md](syscall-compat.md#default-core-status-contradicts-rlimit_core0) |
| P2 | syscall/clone | `clone` ignores parent and child TID stores | High | [syscall-compat.md](syscall-compat.md#clone-ignores-parent-and-child-tid-stores) |
| P2 | syscall/signal | `SA_NOCLDSTOP` still delivers stop SIGCHLD | High | [syscall-compat.md](syscall-compat.md#sa_nocldstop-still-delivers-stop-sigchld) |
| P2 | syscall/mm | aarch64 4K subpage `munmap` returns `EINVAL` | High | [syscall-compat.md](syscall-compat.md#aarch64-4k-subpage-munmap-returns-einval) |
| P2 | syscall/mm | aligned `mprotect` on unmapped range succeeds | High | [syscall-compat.md](syscall-compat.md#aligned-mprotect-on-unmapped-range-succeeds) |
| P2 | syscall/mm | aarch64 `mincore` accepts `PROT_NONE` vec | High | [syscall-compat.md](syscall-compat.md#aarch64-mincore-accepts-prot_none-vec) |
| P2 | syscall/wait | `wait4` writes host rusage units into guest layout | High | [syscall-compat.md](syscall-compat.md#wait4-writes-host-rusage-units-into-guest-layout) |
| P2 | procfs/process | `/proc/<pid>/stat` reports wrong process group and session | High | [syscall-compat.md](syscall-compat.md#proc-stat-reports-wrong-process-group-and-session) |
| P2 | JIT/cache | x86 persistent cache key ignores codegen env modes | Medium-high | [jit-and-opcodes.md](jit-and-opcodes.md#x86-persistent-cache-key-ignores-codegen-env-modes) |
| P2 | syscall/fs | `utimensat` ignores unknown flags and updates timestamps | High | [syscall-compat.md](syscall-compat.md#utimensat-ignores-unknown-flags-and-updates-timestamps) |
| P2 | syscall/fs | `renameat2(RENAME_WHITEOUT)` silently becomes plain rename | High | [syscall-compat.md](syscall-compat.md#renameat2rename_whiteout-silently-becomes-plain-rename) |
| P2 | build/compat | `COPY --from=<external-image>` is rejected | High | [archive-fs-compat.md](archive-fs-compat.md#copy---from-is-rejected) |
| P3 | daemon/events | container rename updates state without event | High | [daemon-tests-docs.md](daemon-tests-docs.md#container-rename-updates-state-without-event) |
| P3 | cp/stat | docker cp stat header mis-encodes special mode bits | Medium | [archive-fs-compat.md](archive-fs-compat.md#docker-cp-stat-header-mis-encodes-special-mode-bits) |

## Deprioritized

Security-only items from the first pass remain in [syscalls-and-security.md](syscalls-and-security.md), but the manager loop is no longer spending agent time on them unless they also create compatibility failures, hangs, data loss, or performance problems.

## Notes

- Existing uncommitted source changes were present in `dd-jit/src/runtime/container/builder.rs` and `dd-jit/src/runtime/container/mod.rs`. This audit did not modify those files.
- No destructive commands were used. Agents were instructed to avoid modifying the main worktree.
- Runtime behavior was not exhaustively tested. Items marked high confidence are backed by direct source evidence and narrow verification recipes.
