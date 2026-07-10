# Verification Ledger

Date: 2026-07-10

This file tracks ongoing verification work. It should stay short and operational: what has hard proof, what still needs an isolated repro, and which lane owns it.

## Manager-Verified

| Finding | Evidence | Status |
|---|---|---|
| `make coverage` false-green | `bash dd-tests/tools/coverage.sh static; echo $?` prints missing `dd-jit/src/runtime/...` files, reports `handled 0 / 321 canonical syscalls`, and exits `0` | Verified read-only |
| docs placement | `docs/bugs/` exists in the repo and avoids internal build-guidance references | Verified |
| Dockerfile `WORKDIR` ignored for `RUN` | isolated test `host_workdir_is_not_guest_cwd` in `/Users/x/dd/verify3-dd-worktree`; passed as proof of bug | Proven |
| workspace VPN egress dropped | isolated test `launch_config_drops_egress_socks_even_when_builder_sets_it`; passed as proof of bug | Proven |
| published port bind failure not propagated | isolated test `start_records_forwarder_even_when_host_bind_fails`; passed as proof of bug | Proven |
| inline volume `..` escapes volume root | isolated test `resolve_mount_src_dotdot_source_resolves_outside_volumes_dir`; passed as proof of bug | Proven |
| live network connect/disconnect state-only | isolated test `live_network_connect_disconnect_only_mutates_network_state`; passed as proof of bug | Proven |
| coverage/test-ci/perf/bench false greens | isolated guard suite `/Users/x/dd/dd-agent4/dd-tests/tests/gate_invariants.rs`; 13 failing tests | Proven |
| untrusted `EFAULT` compatibility | isolated `efault-untrusted` test in `/Users/x/dd/dd-verifier6`; aarch64 fails, x86 silently wrong | Proven |
| Wayland nonblocking flush loss | isolated `flush_preserves_pending_message_after_would_block`; fails | Proven |
| GPU padded texture upload corruption | isolated `software_backend_copy_buffer_to_texture_honors_bytes_per_row`; fails | Proven |
| GPU offset wrapping panic | isolated `software_backend_rejects_wrapping`; panics | Proven |
| `wl_shm_pool.destroy` stale pool | isolated `shm_pool_destroy_removes_pool_object`; fails | Proven |
| GUI matrix missing probes | static `comm` command lists seven unregistered probes | Proven |
| build context copies `.context.tar` | isolated archive PoC script in `/tmp/dd-agent5-sparse.seZJ2E`; copied into image context | Proven |
| Dockerfile symlink followed outside context | isolated archive PoC script; build reads external Dockerfile target | Proven |
| docker cp put follows destination symlink | isolated archive PoC script; writes outside requested path through symlink | Proven |
| build COPY follows symlinked destination | isolated archive PoC script; `cp -a` writes into symlink target | Proven |
| import failure leaves partial target | isolated archive PoC script; failed tar leaves rootfs/linkout | Proven |
| JIT stale unmap/remap translation | isolated `smc_unmap_reuse` probe in `/Users/x/dd/dd-verifier2-wt`; dd returns old code result after `MAP_FIXED` reuse | Proven |
| VEX register-source scalar merge | isolated `x86_vmov_scalar_merge.c`; qemu preserves lanes, dd zeroes upper lanes | Proven |
| F16C rounding immediate | isolated `x86_f16c_roundimm.c`; dd returns RNE for all immediate modes | Proven |
| SSE4.2 AF clearing | isolated `x86_sse42_pcmp_flags.c`; dd leaves AF set while qemu clears it | Proven |
| syscall edge compatibility | isolated dd-tests probes in `/Users/x/dd/dd-verify-agent1-src2-20260710-112023`; close_range, sockcred, pidfd, ns, getresid, scheduler mismatches | Proven |
| `docker commit` drops container writes | isolated `deeper_3b` PoC in `/Users/x/dd/dd-3b-worktree`; committed image reads lower content | Proven |
| `docker export` drops container writes | isolated `deeper_3b` PoC; exported tar reads lower content | Proven |
| Dockerfile runtime metadata dropped | isolated `deeper_3b` PoC; `USER 1001` produces empty user | Proven |
| failed start spent `Live` | isolated `deeper_3b` PoC; second start returns `204` again | Proven |
| `docker top` on stopped container | isolated `deeper_3b` PoC; expected `409`, got `200` | Proven |
| Wayland queued fd drop leak | isolated `drop_closes_queued_received_fds` in `/Users/x/dd/dd-verifier6b`; fd remains open after `Conn` drop | Proven |
| untrusted SCM_RIGHTS + eventfd | isolated run in `/Users/x/dd/dd-verify-4b`; observed `woke=46 read=46 sum=1081 child=4` instead of `48/48/1176/0` | Proven |
| seccomp no-op compatibility | isolated `seccomp_filter_getpid.c`; dd allows `getpid` after successful deny-filter install | Proven |
| syscall event/mm edge cases | isolated `/Users/x/dd/dd-workerA-syscall-audit-20260710`; epoll maxevents, inotify flags, timerfd first deadline, mprotect alignment all mismatch native on both arches | Proven |
| daemon storage/API consistency | isolated `/Users/x/dd/dd-workerC-daemon-storage-20260710`; image alias IDs, non-atomic cache replacement, unnamed volume reuse all fail PoCs | Proven |
| archive cache/overlay behavior | isolated `/Users/x/dd/dd-verify-5b`; symlink target digest, hardlink topology digest, ADD tar extraction, cp GET merged overlay all fail PoCs | Proven |
| JIT FPU/cache follow-up | isolated `/Users/x/dd/dd-worker-b-jit-audit`; `fxrstor-mxcsr` and `smc-mremap-fixed` mismatch qemu/native | Proven |
| display protocol follow-up | isolated `/Users/x/dd/dd-workerD-render-audit-20260710`; shm resize drops frame, multiple frame callbacks collapse | Proven |
| exec env and CPU quota | isolated `/Users/x/dd/dd-slot-e`; null env and newline env mismatch native, fractional CPU quota guard fails | Proven |
| archive/storage follow-up | isolated `/Users/x/dd/dd-workerF-archive-audit-20260710`; rootfs digest mode, Docker save/load, and volume copy-up metadata PoCs fail | Proven |
| daemon API follow-up | isolated `/Users/x/dd/dd-workerH-daemon-api-20260710`; pause/unpause exited state and rename network alias PoCs fail | Proven |
| syscall slot G follow-up | isolated `/Users/x/dd/dd-slot-G`; invalid ppoll/madvise and sentry fd probes fail oracle checks | Proven |
| display/GPU slot J follow-up | isolated `/Users/x/dd/dd-workerJ-display-gpu-20260710`; xdg pre-ack presentation and inert data-device tests fail | Proven |
| JIT/runtime slot I follow-up | isolated `/Users/x/dd/dd-worker-I-jit-runtime-20260710`; AVX signal return and tgkill-read EINTR probes mismatch qemu | Proven |
| archive/storage slot L follow-up | isolated `/Users/x/dd/dd-worker-L-archive-storage-20260710`; sparse save and import cleanup PoCs fail | Proven |
| env/procfs slot K follow-up | isolated `/Users/x/dd/dd-worker-K-sparse`; proc limits and sysinfo memory cap probes fail | Proven |
| daemon API slot M follow-up | isolated `/Users/x/dd/dd-worker-M-daemon-api-20260710`; ignored kill signal and IPAM collision PoCs fail | Proven |
| display/GPU slot O follow-up | isolated `/Users/x/dd/dd-worker-O`; surface destroy/delete-id and destroyed-buffer presentation tests fail | Proven |
| syscall slot N follow-up | isolated `/Users/x/dd/dd-audit-slotN`; pselect timeout, prlimit invalid, and x86 mincore null-vector probes fail | Proven |
| JIT/runtime slot P follow-up | isolated `/Users/x/dd/dd-worker-P-jit-runtime-20260710`; unmaskable signal mask, sigaltstack validation, and aarch64 signalfd size probes fail | Proven |
| env/procfs slot R follow-up | isolated `/Users/x/dd/dd-worker-R-envproc-20260710`; proc environ defaults, many-argv exec, and status uid/gid probes fail | Proven |
| archive/storage slot Q follow-up | isolated `/Users/x/dd/worker-Q/dd-Q3`; apostrophe path digest and xattr save/load PoCs fail | Proven |
| daemon API slot S follow-up | isolated `/Users/x/dd/dd-slot-s`; network disconnect missing container and attach-exited hijack PoCs fail | Proven |
| display/GPU slot T follow-up | isolated `/Users/x/dd/dd-worker-T-display-gpu-20260710`; shm disconnect leak and focus leave/enter PoCs fail | Proven |
| syscall slot U follow-up | isolated `/Users/x/dd/dd-worker-U`; signal validation, pselect mask, and pipe-size fcntl PoCs mismatch native | Proven |
| JIT/runtime slot V follow-up | isolated `/Users/x/dd/dd-worker-V-jit-runtime-20260710`; signalfd independence, signal `uc_stack`, and absolute sleep EINTR PoCs mismatch native | Proven |
| env/cgroup slot W follow-up | isolated `/Users/x/dd/dd-worker-W-envproc-20260710`; cgroup root, cgroup membership, and pids-limit fork probes fail | Proven |
| archive/registry slot X follow-up | isolated `/Users/x/dd/dd-worker-X-archive-followup-20260710`; digest ref parse and platform OS validation PoCs fail | Proven |
| daemon API slot Y follow-up | isolated `/Users/x/dd/dd-worker-Y-daemon-api-20260710`; AutoRemove inspect, missing volume delete, and restarting prune PoCs fail | Proven |
| display/GPU slot Z follow-up | isolated `/Users/x/dd/dd-slot-z`; pointer id reuse, xdg popup configure, and padded texture upload PoCs fail | Proven |
| daemon/image slot AA follow-up | isolated `/Users/x/dd/dd-worker-AA-daemon-image-20260710`; restart reload, prune network endpoint, and basename rmi PoC scripts fail | Proven |
| archive/registry slot AB follow-up | isolated `/Users/x/dd/dd-worker-AB-archive-registry-build-20260710`; blob digest mismatch and opaque whiteout escape PoCs fail | Proven |
| signal/runtime slot AC follow-up | isolated `/Users/x/dd/dd-worker-AC-signal-runtime-20260710`; eventfd overflow/dup, signalfd update/short-read, and timerfd dup probes mismatch Linux | Proven |
| daemon API slot AD follow-up | isolated `/Users/x/dd/dd-worker-AD-daemon-api-20260710`; reload active status and failed-spawn persistence PoCs fail | Proven |
| display/GPU slot AE follow-up | isolated `/Users/x/dd/dd-worker-AE-display-gpu-20260710`; nonzero mip upload and readback row-stride PoCs fail | Proven |
| daemon/image slot AG follow-up | isolated `/Users/x/dd/dd-worker-AG-daemon-image-20260710`; tag replacement and tag discovery persistence PoCs fail | Proven |
| env/procfs slot AF follow-up | isolated `/Users/x/dd/dd-worker-AF-envproc-src-20260710`; cgroup memory, task enumeration, and procstat process-count probes fail | Proven |
| daemon state slot AI follow-up | isolated `/Users/x/dd/dd-audit-daemon-state-ai`; log retention, failed-spawn port cleanup, and prune destroy-event PoCs fail | Proven |
| display/GPU slot AK follow-up | isolated `/Users/x/dd/dd-worker-AK-display-gpu-20260710`; missing depth attachment and missing vertex-buffer validation PoCs fail | Proven |
| archive/registry slot AJ follow-up | isolated `/Users/x/dd/dd-worker-AJ-archive-registry-20260710`; layer symlink escape and invalid config blob PoCs fail | Proven |
| daemon/image slot AL follow-up | isolated `/Users/x/dd/dd-worker-AL-daemon-image-20260710`; explicit tag fallback, commit config collision, and ELF-less arch rediscovery PoCs fail | Proven |
| procfs/sysfs slot AM follow-up | isolated `/Users/x/dd/dd-worker-AM-envproc-20260710`; network-none sysfs, closed proc-fd, and peer proc-fd PoCs fail | Proven |
| daemon lifecycle slot AN follow-up | isolated `/Users/x/dd/dd-worker-AN-daemon-state-20260710`; natural-exit port cleanup, network prune event, and volume prune event PoCs fail | Proven |
| display/GPU slot AO follow-up | isolated `/Users/x/dd/dd-worker-AO-display-gpu-20260710`; texture extent, bind range, and multisample descriptor PoCs fail | Proven |
| runtime fd/event slot AH follow-up | isolated `/Users/x/dd/dd-worker-AH-jit-runtime-20260710`; epoll dup lifetime and fork-inherited epoll/timerfd probes mismatch Linux | Proven |
| archive/load slot AP follow-up | isolated `/Users/x/dd/dd-worker-AP-archive-registry-layer-20260710`; load path escape, malformed manifest, and push quoting PoCs fail | Proven |
| display/GPU slot AS follow-up | isolated `/Users/x/dd/dd-worker-AS-display-gpu-20260710`; render usage, copy usage, and present size PoCs fail | Proven |
| daemon lifecycle slot AU follow-up | isolated `/Users/x/dd/dd-worker-AU-daemon-lifecycle-20260710`; network event, image prune, and rename event PoCs fail | Proven |
| procfs/sysfs slot AR follow-up | isolated `/Users/x/dd/dd-worker-AR-procfs-sysfs-20260710`; CPU topology, self namespace, and peer namespace probes mismatch Linux | Proven |
| runtime fd/event slot AT follow-up | isolated `/Users/x/dd/dd-worker-AT-jit-fd-event-20260710`; inotify rm_watch, signalfd dup, and inotify dup probes mismatch Linux | Proven |
| archive/registry slot AV follow-up | isolated `/Users/x/dd/dd-worker-AV-archive-load-save-push-registry-20260710`; concurrent manifest PUT and label save/load PoCs fail | Proven |
| daemon image slot AQ follow-up | isolated `/Users/x/dd/dd-worker-AQ-daemon-ref-config-20260710`; save/load arch, save ref collision, and persisted arch PoCs fail | Proven |
| display/Wayland slot AW follow-up | isolated `/Users/x/dd/dd-aw-gpu-20260710`; buffer transform and unsupported shm format PoCs fail | Proven |
| daemon lifecycle slot AX follow-up | isolated `/Users/x/dd/dd-worker-AX-daemon-lifecycle-events-prune-image-20260710`; forced rmi, image event filter, and system prune route PoCs fail | Proven |
| display/GPU slot BD2 follow-up | isolated `/Users/x/dd/dd-bd2-worktree`; shm offset panic, shm stride, and 3D texture descriptor PoCs fail | Proven |
| daemon image slot BB2 follow-up | isolated `/Users/x/dd/dd-worker-BB2-daemon-image-ref-state-20260710`; alias rmi, label inheritance, and label discovery PoCs fail | Proven |
| archive slot BC2 follow-up | isolated `/Users/x/dd/dd-worker-BC2-archive-registry-load-save-push-20260710`; store path collision and unsupported OS manifest PoCs fail | Proven |
| runtime fd/event slot BA2 follow-up | isolated `/Users/x/dd/dd-worker-BA2-fd-event-20260710`; epoll dup instance and inotify fork watch probes mismatch Linux | Proven |
| procfs/cgroup slot BE2 follow-up | isolated `/Users/x/dd/dd-worker-BE2-clean-20260710`; cgroup controller files, status threads, and proc net unix probes mismatch Linux | Proven |
| JIT/opcode slot BF2 follow-up | isolated `/Users/x/dd/dd-bf2-audit-20260710`; MMX width, x87 control word, and x87 precision probes mismatch qemu | Proven |
| display/GPU slot BG2 follow-up | isolated `/Users/x/dd/dd-bg2-display-gpu-20260710`; viewport bounds, zero texture, and wrapping bind offset PoCs fail | Proven |
| daemon image/config slot BH2 follow-up | isolated `/Users/x/dd/dd-worker-BH2-daemon-image-ref-config-state-20260710`; same-tag load rootfs and env override PoCs fail | Proven |
| display/GPU slot BL2 follow-up | isolated `/Users/x/dd/dd-audit-bl2`; oversized texture descriptor and buffer scale zero PoCs fail | Proven |
| archive/registry slot BI2 follow-up | isolated `/Users/x/dd/dd-audit-BI2-copy`; same-name load rootfs and push runtime metadata PoCs fail | Proven |
| procfs/cgroup slot BK2 follow-up | isolated `/Users/x/dd/dd-bk2-audit-20260710`; proc net directory, sockstat, and cgroup file probes fail | Proven |
| runtime fd/event slot BJ2 follow-up | isolated `/Users/x/dd/dd-worker-BJ2-fd-event-20260710`; CLOEXEC inheritance and short-read consumption probes mismatch Linux | Proven |
| daemon config slot BN2 follow-up | isolated `/Users/x/dd/dd-audit-BN2-copy`; inspect split-config and commit-user PoCs fail | Proven |
| display/GPU slot BO2 follow-up | isolated `/Users/x/dd/dd-audit-BO2-20260710`; viewport destination, viewport depth, and zero-mip PoCs fail | Proven |
| JIT/opcode slot BM2 follow-up | isolated `/Users/x/dd/dd-audit-BM2-copy`; int3 SIGTRAP, SSE2 conversion, and COMI AF probes mismatch qemu | Proven |
| registry metadata slot BP2 follow-up | isolated `/Users/x/dd/dd-audit-BP2-registry-metadata-20260710`; diff_ids, push metadata, and unsupported OS config PoCs fail | Proven |
| runtime fd/event slot BQ2 follow-up | isolated `/Users/x/dd/dd-worker-BQ2-fd-event-20260710`; realtime timerfd, epoll_pwait mask, and eventfd null-read probes mismatch Linux | Proven |
| daemon config slot Mill follow-up | isolated `/Users/x/dd/dd-audit-container-state-20260710`; ExposedPorts, stdio, and LogConfig inspect PoCs fail | Proven |
| display/GPU slot Aquinas follow-up | isolated `/Users/x/dd/dd-agent-gpu-lifecycle-20260710`; partial submit, stale bind-group, and pass-sequencing PoCs fail | Proven |
| registry metadata slot Descartes follow-up | isolated `/Users/x/dd/dd-oci-audit`; schema version, media type, descriptor size, and zero-layer manifest PoCs fail | Proven |
| procfs/sysfs slot Aristotle follow-up | isolated `/Users/x/dd/dd-procfs-audit-BN`; task status lookup, self readdir, and fdinfo probes mismatch Linux | Proven |
| wait/futex slot Rawls follow-up | isolated `/Users/x/dd/dd-audit-wait-futex-20260710`; futex bitset and futex proc-state probes mismatch qemu | Proven |
| GPU validation slot Einstein follow-up | isolated `/Users/x/dd/dd-audit-gpu-bq2`; row pitch, pipeline format, and fence wait PoCs fail | Proven |
| JIT fault slot Franklin follow-up | isolated `/Users/x/dd/dd-jitfault-audit-20260710`; munmap subpage, UD2, and ICEBP/bad62 probes mismatch qemu | Proven |
| daemon API slot Huygens follow-up | isolated `/Users/x/dd/dd-audit-daemon-api-state-lifecycle-src`; update body, DNS/hosts, device requests, network mode, and domainname PoCs fail | Proven |
| GPU validation slot Euler follow-up | isolated `/Users/x/dd/dd-audit-gpu-validation`; draw range, shader module, and vertex attribute PoCs fail | Proven |
| build/history slot Leibniz follow-up | isolated `/Users/x/dd/dd-audit-docker-history-20260710`; history, base cache seed, label inheritance, and dockerignore PoCs fail | Proven |
| daemon net/mount slot Zeno follow-up | isolated `/Users/x/dd/dd-audit-netmount-20260710-131246`; static IP/alias, readonly archive PUT, and bind propagation PoCs fail | Proven |
| GPU texture slot Mencius follow-up | isolated `/Users/x/dd/dd-gpu-tex-audit`; storage texture binding and render-target hazard PoCs fail; sampler descriptor drop is source-proven | Proven |
| procfs/sysfs slot Lagrange follow-up | isolated `/Users/x/dd/dd-audit-procfs-20260710-130826`; mount table, smaps, and sysfs block probes mismatch Linux | Proven |
| wait/signal slot Sartre follow-up | isolated `/Users/x/dd/dd-wait-audit-20260710`; WCONTINUED, SA_NOCLDWAIT, clone TID, and SA_NOCLDSTOP probes mismatch Linux/qemu | Proven |
| Dockerfile parser slot Nash follow-up | isolated `/Users/x/dd/dd-audit-dockerfile-cf`; ENV/ARG/SHELL/ONBUILD/target/cleanup/escape/exec-form PoCs fail | Proven |
| GPU lifetime slot Raman follow-up | isolated `/Users/x/dd/dd-audit-gpu-readback-lifetime`; destroyed texture binding and unaligned readback PoCs fail | Proven |
| daemon events/logs/stats slot Kant follow-up | isolated `/Users/x/dd/dd-audit-daemon-events-logs-stats-restart-20260710`; restart, logs, stats, and dead-state PoCs fail | Proven |
| aarch64 runtime slot Singer follow-up | isolated `/Users/x/dd/dd-audit-aarch64-runtime-20260710`; FPSIMD signal context, 4K munmap, and mprotect unmapped probes mismatch Linux | Proven |
| process lifecycle slot McClintock follow-up | isolated `/Users/x/dd/dd-audit-proc-lifecycle-20260710`; waitid rusage, core status, and wait4 rusage probes mismatch Linux/qemu | Proven |
| procfs/statfs slot Hume follow-up | isolated `/Users/x/dd/dd-procfs-statfs-audit-20260710`; statfs, procfs content, and devfd probes mismatch Linux/qemu | Proven |
| GPU cleanup slot Hooke follow-up | isolated `/Users/x/dd/dd-audit-gpu-cleanup-20260710`; CUDA transient cleanup and ResourceTable generation leak tests fail; Metal cleanup is source-proven | Proven |
| daemon exec/wait slot Noether follow-up | isolated `/Users/x/dd/dd-audit-daemon-exec-health-wait`; wait, resize, and exec inspect tests fail; exec start, attach selectors, and health timing are source-proven | Proven |
| GPU serde slot Nietzsche follow-up | isolated `/Users/x/dd/dd-audit-gpu-serde-20260710`; partial frame, trailing-frame, and noncanonical-bool tests fail | Proven |
| pgrp/signal slot Kuhn follow-up | isolated `/Users/x/dd/dd-audit-pgrp-signal-20260710`; kill-zero, tty pgrp ioctl, and proc stat pgrp/session probes mismatch Linux/qemu | Proven |
| devfs/procfs slot Avicenna follow-up | isolated `/Users/x/dd/dd-audit-devfs-procfs-20260710`; urandom, tty nonblocking read, proc tty, and proc devices probes mismatch Linux/qemu | Proven |
| daemon wait/events slot Kepler follow-up | isolated `/Users/x/dd/dd-audit-wait-events-health-20260710`; wait condition, exec events, and health event filter PoCs fail | Proven |
| build output slot Socrates follow-up | isolated `/Users/x/dd/dd-audit-build-output-20260710`; image volume copy-up, WORKDIR dotdot, and ENV order PoCs fail | Proven |
| aarch64 perms/atomics slot Linnaeus follow-up | isolated `/Users/x/dd/dd-audit-aarch64-atomics-perms-20260710`; PROT_NONE and low-address atomic probes mismatch Linux/native | Proven |
| daemon cleanup slot Carver follow-up | isolated `/Users/x/dd/dd-audit-fs-lifecycle-20260710`; durable state, volume, container cleanup, and image rmi failure PoCs fail | Proven |
| daemon events slot Kierkegaard follow-up | isolated `/Users/x/dd/dd-audit-events-api-apiworker-20260710`; supported filter keys, malformed filters, and non-epoch until PoCs fail | Proven |
| archive metadata slot Banach follow-up | isolated `/Users/x/dd/dd-audit-archive-meta-20260710`; UID/GID, nanosecond mtime, and device-node archive probes fail | Proven |
| runtime fs syscall slot Pascal follow-up | isolated `/Users/x/dd/dd-audit-runtime-fs-syscalls-20260710`; unlinkat, fallocate, and utimensat probes mismatch Linux/qemu | Proven |
| daemon API durability slot Gauss follow-up | isolated `/Users/x/dd/dd-audit-daemon-api-durability-20260710`; missing-network create, system df tag count, and plugin endpoint PoCs fail | Proven |
| system endpoints slot Confucius follow-up | isolated `/Users/x/dd/dd-audit-system-endpoints-20260710`; system df active image, volume reference, and build-cache item/count PoCs fail | Proven |
| JIT SMC/cache slot Copernicus follow-up | isolated `/Users/x/dd/dd-audit-jit-memorder-cache-20260710`; threaded SMC and SMC capacity probes fail; x86 pcache env key is source-proven | Proven |
| JIT permission/fault slot Locke follow-up | isolated `/Users/x/dd/dd-audit-jit-perm-fault-20260710`; read-only write, no-exec fetch, and aarch64 mincore PROT_NONE probes mismatch Linux/qemu | Proven |
| registry pull failure slot Arendt follow-up | isolated `/Users/x/dd/dd-audit-registry-compression-20260710`; later-layer HTTP failure leaves partial rootfs and emits wrong download events | Proven |
| create atomicity slot Darwin follow-up | isolated `/Users/x/dd/dd-audit-create-start-atomicity-20260710`; missing rootfs, anonymous volume failure, and pre-durable create event PoCs fail | Proven |
| runtime fs syscall slot Godel follow-up | isolated `/Users/x/dd/dd-audit-runtime-fs-syscalls-BR-20260710`; chown, openat2, and renameat2 whiteout probes mismatch Linux/qemu | Proven |
| info/version slot Heisenberg follow-up | isolated `/Users/x/dd/dd-audit-info-version-20260710`; `/info` capacity/runtime and stale version/header PoCs fail | Proven |

## Active Verification Lanes

| Lane | Scope | Expected output |
|---|---|---|
| syscall compatibility | fd lifecycle, epoll/eventfd/timerfd races, signal restart, fork/exec stale emulation, errno/probe behavior that breaks workloads | isolated dd-tests-style C probes or Linux-vs-dd command output |
| JIT/opcodes | adjacent partial-register/vector merge, x87/MMX/SSE flags, AVX state transitions, atomic/LOCK, mprotect/mremap/fork invalidation | isolated opcode probes with qemu/native oracle and dd result |
| daemon runtime | `WORKDIR` during `RUN`, port bind failures, volume traversal, live network changes, VPN egress | isolated Rust tests or API scripts |
| tests/build | coverage path, dark-lane CI, XPASS policy, completeness counts | isolated failing tests or shell checks |
| archive/fs boundaries | build/cp/load/import tar compatibility, symlink behavior, overlay stale state, cleanup leaks | fixtures and containment/data-integrity checks |
| GPU/sentry/untrusted | GUI/GPU/rendering and sentry split gaps | focused findings with repros where feasible |
| completeness/env | unhandled subcases, silent corruption, env-var-only features, stale xfails | ranked candidates plus failing probes |

## Needs Runtime Proof

- `pidfd_open` host-pid authority leak.
- concurrent registry pull temp-file collision.
- GPU Metal ack-after-error and base-vertex behavior.
- GPU IR decoder allocation caps.
- VEX float-to-int NaN/overflow behavior.
- `cmpxchg16b` atomicity under guest-thread stress.
- SMC protected-page capacity overflow behavior.
- archive extraction traversal/symlink containment.
- duplicate exec start behavior.
- event replay and slow log-follow loss.
- sentry close-on-exec fd cleanup.
- futex unknown op/flag behavior.
- dmabuf LINEAR advertised path.
- procfs ISA/version metadata.
- Metal render-target texture id aliasing.
- released input object event delivery.
