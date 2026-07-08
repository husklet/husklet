# dd engine — silent-stub / fake-success hole audit

> **STATUS (2026-07-08): ALL H-class holes FIXED — combined `make test` = 1642 passed / 0 failed / 13 xfail, all three engines OK (linux_aarch64 / linux_x86_64 / darwin_aarch64).** Fixed across 5 disjoint-file agents in one wave: sync (H1 FUTEX_WAKE_OP + H2 PI-futex + robust-list), net (H3 g_br_ip OOB + H4 fd-cap guards + H5 + M), fs (H6 fallocate + H7 mount + H8 memfd-seals + M), mem (H9 mprotect-EXEC SMC / #423 + M), translator (H10–H13 FP miscompiles, byte-exact vs qemu, + M). New regression tests added: `smcmprotect`, `pi_robust`, `fpedge`, `shldflags`.

> **FOLLOW-UP TRANSLATOR HOLES FIXED (2026-07-08, translator agent).** Three more x86-frontend holes closed, x86-exact + byte-exact vs qemu; new `only(LinuxX86_64).oracle()` regression tests `fpdnan`/`repmovsdf`/`x87m80` (all green, and the compat+libc x86 groups stay green):
> 1. **Default/indefinite-NaN sign** (the `DIVSS/DIVPS/DIVSD/DIVPD` follow-up, extended to `ADD*/SUB*/MUL*/SQRT*`). A GENERATED NaN — invalid op with NO NaN input: `0/0`, `inf/inf`, `0*inf`, `inf-inf`, `sqrt(<0)` — now stamps x86's NEGATIVE indefinite (`0xFFC00000` / `0xFFF8000000000000`) where ARM's FDIV/FADD/... emit the positive default NaN (`0x7FC00000` / `0x7FF8..`); a NaN PROPAGATED from an input keeps that input's sign on both ISAs, so the fixup touches ONLY generated NaNs (result-NaN AND no-input-NaN) — `2.0*QNaN(+)` stays `0x7FC00000` (verified). Legacy-SSE inline path: `translate.c emit_dnan_pre`/`emit_dnan_post` (branchless per-lane, scalar+packed share one path; env `NOXFPDNAN` disables for A/B). VEX/AVX C path: `avx.c avx_dnan_f32/f64`.
> 2. **DF (direction flag) → real RUNTIME state** (was the M-item "DF translate-time-only"). `cpu->df` (OFF_DF) is authoritative: `std`/`cld` store it, `popfq` restores it from bit10, `pushfq` + the sigframe EFLAGS read it, and the `rep movs/stos/lods/cmps/scas` lowering honors it at runtime — so a cross-block `std; rep movs` (or popfq-set DF) copies BACKWARD correctly (was a silent forward copy). The memcpy fast-idiom stays ONE direction-aware host call (`dd_rep_movs`/`dd_rep_stos` now take the direction); static `g_df` (DF_FWD/DF_BWD/DF_DYN) still emits a constant +w/-w stride when DF is locally known, and loads `cpu->df` only when DF_DYN.
> 3. **x87 m80 Inf/NaN converters** (`x86_ops.c x87_fstp_m80`/`x87_fld_m80`). FSTP of a double Inf/NaN wrote a rebiased FINITE ext80 exponent `0x43FF` (≈2^1024) instead of Inf/NaN's `0x7FFF`; FLD of an ext80 NaN silently flattened to Inf. Both fixed. This is the tractable, byte-exact slice of H11 — the 80-bit **precision** gap remains architectural (see H11 below).

> **CHROME LIVE-WINDOW VERIFICATION (2026-07-08, post H1–H13 fix wave).** Ran the Debian
> glibc Chromium 150 live window (`target-mac/chromedeb-live.sh`, software render → `wl_shm`
> → dd-display `--window`, `http://example.com`) on the fresh gate-green engine. **The H1
> `FUTEX_WAKE_OP` fix works and cleared the old EnsureConnected/Viz stall**: chromium now
> reaches `wayland_screen.cc Displays updated count:1` + `Display[5] bounds=[0,0 1920x1080]
> external detected`, binds `wl_shm`/`wl_shm_pool`, and blocks cleanly in `ppoll(tmo=-1)`.
> **No window paints yet — but the new wall is NOT a syscall spin.** Direct syscall tracing
> (a file-gated `/tmp/DDPOLLTRACE` trace added to the `ppoll`(73)/`read`(63) handlers and
> rebuilt into the aarch64 engine — env-gated JT/JTS is useless here because chromium
> sanitizes child-process env, so the flag is lost across dd's fresh `ddjit-linux` re-exec
> per guest process; a file check survives it) proved:
>  - **The busy-poll/read spin hypothesis is DISPROVEN.** Every `ppoll` had `tmo_full=-1`
>    (blocking, returns genuinely-readable) — zero `tmo=0` polls, zero `EAGAIN` reads. When
>    settled all engine procs sit at **0% CPU (blocked), not 100%** — the earlier "100% CPU
>    170s" was heavy startup + child-respawn churn, not a hot loop.
>  - The eye-catching **456 uniform `read()→832` calls were RED HERRINGS**: `SO_TYPE` on the
>    fd returns `-1` (ENOTSOCK — a *file*, not the Mojo socket), and each read's first 16
>    bytes are **`7f 45 4c 46 02 01 01…` = ELF magic at offset 0** with 121 distinct
>    checksums → chromium re-reading ELF headers/phdrs of its libs during startup (slow
>    overlay-FS file I/O under DBT), advancing, not a stale re-delivery. The other loud
>    counter, `646× read(fd=0)→1`, is the **harness bash shell** (one pid) consuming
>    `cmds.sh` a byte at a time off the PTY — not chromium at all.
>  - **The real wall: Mojo child↔browser IPC bootstrap never completes.** Every child
>    (gpu / renderer / utility-**NetworkService**) logs
>    `child_thread_impl.cc:908 Terminating current process after 15 seconds with no connection`
>    then dies; the browser blocks forever in `ppoll` waiting for a child that gave up.
>  - Mojo's bootstrap moves the invitation over a **SEQPACKET socketpair via
>    `sendmsg`/`recvmsg` + `SCM_RIGHTS` fd-passing** — which a `read(63)` trace CANNOT see
>    (only one incidental socket read surfaced: `fd=11 sotype=2` = dd's **DGRAM-backed**
>    SEQPACKET, `read→0`/EOF-like). So this is squarely **H4's neighbourhood** (SEQPACKET /
>    passcred / EOF over the DGRAM backing) but on the `sendmsg`/`recvmsg` + fd-inheritance-
>    across-`execve`/`posix_spawn` path, not `read`.
>
> **NEXT DIAGNOSTIC (the precise, bounded next step):** trace `recvmsg`(212)/`sendmsg`(211)
> + `socketpair`(199) + the child-spawn `execve`/`clone` fd table on the bootstrap channel —
> is the browser's `sendmsg(SCM_RIGHTS)` invitation delivered to the child, and does the
> child's inherited `--mojo-platform-channel-handle` fd survive dd's exec with the right
> DGRAM-backed SEQPACKET semantics (message boundaries, non-zero recv, correct peercred/EOF)?
> **Fix hypothesis:** dd's SEQPACKET-over-DGRAM socketpair either drops the SCM_RIGHTS-passed
> invitation datagram or reports a spurious 0/EOF on the child's first `recvmsg`, so
> NodeChannel treats the channel as dead → "no connection". This is the third Chrome blocker,
> adjacent to H4 but on the datagram/ancillary-data + fd-inheritance seam.

> **CHROME BLOCKER #3 FIXED — Mojo child bootstrap now CONNECTS (2026-07-08, net-agent).** The trace
> pinned it (file-gated `/tmp/DDNETLOG`, env-immune across chromium's child re-exec): the browser's
> invitation `sendmsg` carrying **6 `SCM_RIGHTS` fds** returned **-54 (ECONNRESET)** — the peer end had
> zero references. Root cause was **suspect #3 (spurious EOF), via a fork/bystander seam, NOT the SCM/
> multi-fd primitive** (a standalone test proved macOS passes 6/8/16 fds + 64KB payloads over a DGRAM
> socketpair across fork AND fork+exec fine). Mechanism: `seq_send_eof()` (the synthetic zero-length "EOF"
> datagram that emulates SEQPACKET-close-EOF over the DGRAM backing) fired on the close of **any** inherited
> SEQPACKET fd. When the browser forks a renderer/GPU child, that child inherits **all** the browser's open
> fds — including the browser's channel **SEND** end for a *different* child. The bystander never uses it but
> closes the inherited copy on startup; the old close-time injection then dumped a spurious 0-length datagram
> into the **peer end = the REAL target child's live recv queue**. The real child read the premature 0 as
> end-of-channel and gave up (`child_thread_impl.cc:908 Terminating … 15 seconds … no connection`), dropping
> its ref so the browser's next invitation `sendmsg` ECONNRESET'd. The prior "partner-held" suppression
> couldn't help — a bystander can't see that *another process* (the browser) still holds the peer, and fd-
> number reuse further staled the per-fd peer tracking. **Fix (Linux-exact):** a new per-fd `g_sock_seq_wrote[]`
> gate — only a process that has **actually written** to an endpoint may synthesize the peer's EOF on its
> close. Cleared on fork (`seq_wrote_after_fork` — a child inherits fds but has written to none), set on
> `send`/`sendmsg`/`write`/`writev` to a SEQPACKET/O_DIRECT-pipe fd, carried on dup (`fd_carry_sock`), cleared
> on close (`fd_reset_emul`). A genuine writer's last close still EOFs a blocked reader (rustc/make jobserver,
> O_DIRECT pipe preserved); a bystander's close of an inherited-still-live channel end is now **silent** — as
> on Linux, where a bystander's close signals the peer nothing. **Verified LIVE:** `no connection` = **0**
> (was every child); bystander closes **50 SILENT / 0 INJECTING**; the 6-fd invitation now
> `sendmsg req=908 ret=908`. Zygote + renderer + GPU spawn and stay alive; chromium reaches the Wayland
> compositor handshake (2 clients connected, binds `wl_compositor`/`wl_shm`/`xdg_wm_base`/`wl_seat`/
> `wl_output`/`wp_viewporter`, creates `wl_shm_pool`, detects `Display[5] 1920x1080`). Regression test
> `ext_ipc/seqbystander` (bystander inherits+closes a channel's send end unused → parent's first read must be
> the real record, never a spurious 0) added, oracle-diffed, green on both Linux engines; `seqpacket`/
> `seqcred`/`credpid`/`scm`/`unix`/`sockpair`/`dgram`/`msg`/`peercred` subsets all 0-fail, all 3 engines build.

> **CHROME BLOCKER #4 (NEW WALL, not yet fixed) — `/proc/self/task` stopped-thread fidelity.** With blocker
> #3 fixed, chromium's **GPU process** now FATALs during sandbox bring-up:
> `sandbox/linux/services/thread_helpers.cc:104 Stopped thread does not disappear in /proc (iterations: 30)`
> → SIGABRT, so no `xdg_surface`/`wl_surface.commit`/frame yet (dd-display logs the clients disconnecting
> with `0 frame(s)`). Chromium's `thread_helpers` stops/exits a helper thread and polls `/proc/self/task/`
> expecting the thread to disappear (thread count to drop); dd's `/proc/self/task` listing does not reflect
> the real-time thread teardown, so the count never drops → 30 iterations → FATAL. This is a **procfs
> thread-state gap (fs.c / `/proc/self/task` synth), NOT the SEQPACKET/Mojo seam** — the next Chrome frontier
> for a future task (owner: fs/procfs, not net).

> **CHROME BLOCKER #5 (NEW WALL, not yet fixed) — profiled 2026-07-08, ONE bounded run.** With blockers
> #1–#4 fixed (gate 1649/0), chromium's **GPU process survives `thread_helpers.cc:104`** and boots into
> Viz. The GPU process (confirmed pid via `[PID:PID:...gpu-process...]` log tags) reaches **Viz service
> main init** — last GPU line is `components/viz/service/main/viz_main_impl.cc:87 VizNullHypothesis is
> disabled`, immediately after `wayland_buffer_manager_gpu.cc:456 Failed to initialize drm render node`
> (expected, software path) and `sandbox_linux.cc:405 InitializeSandbox() called with multiple threads`.
> On the wayland wire (dd-display.log) it binds **all** globals (wl_compositor/shm/output/seat/xdg_wm_base/
> subcompositor/viewporter/data_device_manager), creates + resizes a `wl_shm_pool`, calls
> `wl_compositor.create_surface`, and issues a `wl_display.sync` roundtrip — **then goes silent. No
> `xdg_surface`/`xdg_toplevel`, no `wl_surface.attach`, no `commit`, dump dir empty (0 frames).**
>
> **CRITICAL RE-FRAMING — the "100% CPU stall" is NOT the GPU process.** Sampling (`sample`, `top -l2`)
> shows every guest chromium process at **0.0% CPU, fully blocked**: the GPU process (viz-main thread in
> `futex_op`→`_pthread_cond_wait`→`__psynch_cvwait` = guest `FUTEX_WAIT`; its IO/wayland thread in
> `svc_event`→`kevent` = guest `epoll_wait`); the browser process (21 threads) likewise all in
> futex/poll/kevent. The **only** process at 100% CPU is the **HOST `ddcli` (workspace launch) main thread
> in `ddcli::workspace::run_inline`** (dd-cli/src/workspace.rs:239) — a PTY relay busy-spinning across
> `poll`(51%)/`read`(30%)/`try_wait`→`__wait4`(18%)/`ioctl`. **Root cause of the host spin:** in this
> harness stdin is a *redirected regular file* (`< cmds.sh`), and `poll()` on a regular file returns
> `POLLIN` immediately every iteration, defeating the intended 10 ms pacing → the loop free-runs
> (read→EOF→re-`pty.write(&[])`→drain→waitpid) as fast as the CPU allows. **This host busy-spin is a real
> bug but a RED HERRING for the missing frame** — it never touches the wayland socket or rendering; the
> prior "GPU process at 100% CPU" premise was a mis-attribution of ddcli's CPU to the guest.
>
> **The actual missing-frame blocker is class (b): the guest Viz pipeline is BLOCKED FOREVER on a wakeup
> dd never delivers** (same family as #1 WAKE_OP / #3 SEQPACKET-EOF). All guest threads sleep at 0% CPU
> (so NOT slow compute, NOT a guest spin) after the GPU process issued `wl_display.sync`. dd-display *does*
> emit the reply (`server.rs:280–284` sends `wl_callback.done` + `wl_display.delete_id`), so the fault is on
> the **delivery/wakeup path**, not the compositor: either (i) dd's engine never raises socket-read
> readiness on the guest's wayland fd so its `epoll_wait`/`kevent` thread never wakes to consume the
> `done`, or (ii) viz-main is waiting on a cross-process mojo/eventfd IPC from the (also-idle) browser
> that dd never delivers. Both are the "lost cross-thread/-process wakeup" seam. **To pin the exact fd:**
> the fixer should re-run with a file-gated readiness/epoll trace (the `DDWAKELOG` env hook is already
> wired into `target-mac/chromedeb-live.sh`) and diff which wayland-socket / eventfd readiness the guest's
> `epoll_wait` never sees. Owner: sync/net readiness (io.c/net.c/epoll), NOT the compositor. Two separable
> fixes: **(A)** host `run_inline` must not busy-spin when stdin is a non-tty/regular file (block on the
> guest-output fd, or drop STDIN from the poll set at EOF); **(B)** the guest Viz wakeup delivery.

**What this is.** A whole-engine sweep for the bug class that cost us the entire Chrome
saga: a syscall/op/instruction that **returns success (or a plausible value) without
actually doing the work**, so the guest silently misbehaves — lost wakeup, wrong result,
corruption — with *no error to trace*. The exemplar: `FUTEX_WAKE_OP` was a `return 0`
no-op (`thread.c:576`); glibc condvars wake through it, so Chromium's main thread waited
forever on a wakeup dd threw away → "Wall-7", which we'd misdiagnosed for days as an
internal Chromium deadlock / firewall limit.

**How found.** Five parallel read-only audits (2026-07-08), one per subsystem:
sync/proc/signals, fs/vfs/proc, net/sockets, mem/ioctl/rare, x86+arm64 translator.

**Verdict.** The engine is *not* riddled — most of it is genuinely implemented with
documented rationale (the unhandled-syscall fallback honestly returns `ENOSYS`;
io_uring/bpf/userfaultfd correctly refuse; signalfd/timerfd/eventfd are real). But there
is a consistent seam of **~13 H-class holes** on the less-trodden paths, several of which
map to bugs we've already been bitten by (WAKE_OP, the fd-cap, #423, #248, #389).

Severity: **H** = silent fake-success/wrong-result → corruption/deadlock, undebuggable
(the WAKE_OP class). **M** = wrong but semi-loud or on less-common paths. **L** =
cosmetic / documented-benign / fails loudly.

---

## H — dangerous, silent, undebuggable (fix these)

| # | Location | Hole | Real semantics | Breaks | Chrome? |
|---|---|---|---|---|---|
| H1 | `thread.c:576` | `FUTEX_WAKE_OP` was `return 0` no-op | atomic op on uaddr2 + wake uaddr (+cond uaddr2) | glibc `pthread_cond_signal/broadcast` → lost wakeup → deadlock | **yes — fix in flight** |
| H2 | `thread.c:654` | **PI-futexes** `LOCK_PI/UNLOCK_PI/TRYLOCK_PI/WAIT_REQUEUE_PI/CMP_REQUEUE_PI` fake-acquire (`return 0`, no block, no TID write, no wake) | block until owner releases; kernel writes owner-TID; unlock hands to next waiter | contended `PTHREAD_PRIO_INHERIT`/robust mutex → **two threads in the critical section → silent data corruption** (systemd, PulseAudio, RT) | maybe |
| H3 | `netns.c:539` | **`g_br_ip[1024]` OOB** — sibling tables are `[DD_NFD=65536]`, this one wasn't migrated; written on every `socket()` at fd<65536 | array sized to max fd | **live heap/BSS corruption** on ordinary high-fd `socket()` (fd-heavy procs: chromium/node) | **yes (likely)** |
| H4 | `net.c` (many: 319/955/1013/1073/1181/1189/1227, `netns.c:548`) | socket-state tables are `[DD_NFD]` but **accessors still guard `< 1024`** → AF_UNIX/SEQPACKET/passcred/peercred/EOF/lo/bridge/DNS all no-op for fd ≥ 1024 | applies regardless of fd number | Mojo SEQPACKET ≥1024 → EMSGSIZE on >2KB msgs, wrong peercred, no EOF → **IPC handshake wedges** | **yes (likely 2nd blocker)** |
| H5 | `net.c:983` | `setsockopt` = `r<0 ? 0 : 0` — **always returns success**, masks every real errno | return the actual errno | feature-probing code takes the wrong path; real-option failures vanish | — |
| H6 | `fs.c:1303` | `fallocate` **ignores `ZERO_RANGE`/`COLLAPSE_RANGE`/`INSERT_RANGE`/`UNSHARE_RANGE`** (falls to no-op extend, `return 0`); also swallows `ftruncate` ENOSPC | zero / remove-and-shift / insert-and-shift the byte range | journald rotation, SQLite WAL, ext4 tools → **success with data unmodified → silent corruption**; space-reservation void | — |
| H7 | `fs.c:1196` | `mount`/`umount2`/`pivot_root` = `G_RET=0` unconditionally | actually (un)mount | entrypoint `mount --bind`/`-t tmpfs`/`remount,ro` → **wrong dir content, unenforced read-only** | — |
| H8 | `fs.c:1274/1303` | memfd **`F_SEAL_SHRINK`/`F_SEAL_GROW` not enforced** on `ftruncate`/`fallocate` (only write/pwrite check seals) | shrink/grow of a sealed memfd → EPERM | sender shrinks a sealed shared buffer under a receiver → **SIGBUS/OOB** (Wayland/graphics/IPC trust boundary) | related |
| H9 | `mem.c:653` | `mprotect(PROT_EXEC)` no-op — updates PROT_NONE registry but **never sets `g_rwx_guest`** → SMC not armed (**#423**) | make a written page executable + invalidate stale translations | `mmap(RW)`→write→`mprotect(RX)` toggle JITs (.NET/Wasm) → **stale translation → silent miscompile** (x86 most exposed) | — |
| H10 | `x86_64/translate.c:2821`, `avx.c:159` | **`MINPS/MAXPS/MINSS/MINSD/MAXSS/MAXSD` → ARM `FMIN/FMAX`** — wrong on NaN & signed-zero (x86 returns the 2nd src on NaN/equal; ARM propagates NaN / picks by sign) | x86 min/max NaN+±0 rules | **any min/max over data with NaN/±0** (clamp loops, `Math.min/max`, ML/media, `-ffast-math`) → silent wrong finite value | — |
| H11 | `x86_64/x87.c:12,187` | **x87 stack stored at 64-bit double, not 80-bit extended** — Inf/NaN m80 converters FIXED (2026-07-08); the **mantissa/exponent precision** gap is ARCHITECTURAL, not fixed | 80-bit mantissa+exponent | C `long double` math, `printf("%Lf")`, x87 intermediates → **silent precision drift** (relates #248/#249) | — |
| H12 | `x86_64/translate.c:2897` | **`CMPPS/PD/SS/SD` `NLT`/`NLE` → ordered `FCMGE/FCMGT`** — no unordered handling (x86 returns all-ones on NaN; ARM returns 0) | NaN → true mask | `_mm_cmpnlt_ps` / vectorized `!(a<b)` → **wrong blend/branch mask → corruption** | — |
| H13 | `x86_64/translate.c:2947` | **float→int `CVTT*`/`CVT*` → saturating `FCVTZS/FCVTNS`** (x86 yields "integer indefinite" `0x80000000…` on overflow/NaN) | x86 indefinite value | out-of-range/NaN float→int (JS ToInt32-ish, saturating vs indefinite) → wrong integer (corner: only overflow/NaN inputs) | — |

## M — wrong but semi-loud / less-common

- `proc.c:505` **`set_robust_list` no-op** → thread dies holding a robust mutex → waiters never get `EOWNERDEAD` → deadlock.
- `proc.c:492` `set_tid_address` returns pid but doesn't store tidptr → re-armed clear-on-exit gives no join wakeup.
- `net.c:980` **`SO_RCVTIMEO`/`SO_SNDTIMEO` silently ignored** → blocking `recv`/`read` **hangs forever** instead of ETIMEDOUT (RPC/health-check clients). Same: `SO_BINDTODEVICE`, `SO_RCVLOWAT`.
- `net.c:980/1056` `IPPROTO_IP` (level 0) optnames passed **untranslated** to macOS (IP_TOS/TTL/HDRINCL/multicast differ) → wrong option set.
- `netns.c:1750` **RTNETLINK non-GET** (`RTM_NEWADDR/NEWROUTE/SETLINK`) → empty `NLMSG_DONE`, no `NLMSG_ERROR` ack → `ip addr/route add` **phantom-succeeds doing nothing**.
- `net.c:1024` `SO_PEERCRED` uid/gid **hardcoded to container identity** → Postgres `peer`/ident, polkit, systemd auth keys on wrong uid.
- `net.c:1036` `getsockopt` unknown optname → `*val=0` + success (should `ENOPROTOOPT`) → feature-probe reads "supported, 0".
- `io.c:964` `fcntl` `F_SETLEASE/F_GETLEASE/F_NOTIFY` no-op — **`F_GETLEASE` returns 0=`F_RDLCK`** (fabricated held lease); dnotify arms nothing.
- `io.c:610` **`sendfile` treats a mid-copy read error as EOF** → 0-byte/short "success", no errno → silent truncation (copy_file_range is correct).
- `fs.c:2016` overlay `getdents64` **fabricates `d_ino = pos+1`** → `ls -i`/`find -inum`/hardlink detection wrong on layered images.
- `rare.c:80` **`seccomp` no-op** → self-sandboxing guest believes syscalls blocked; all still serviced (security fake-success).
- `mem.c:693/701/740` `mlock`/`munlock`/`mlockall` swallow host failure → crypto/RT guest's key pages may never wire (RLIMIT_MEMLOCK unenforced).
- `rare.c:515` `move_pages` returns 0 without writing `status[]` → NUMA introspection reads uninitialized buffer.
- `x86_64/translate.c:3141` `SHLD/SHRD` **CF/PF approximate** (only SF/ZF materialized) → `shld;jc/jp` wrong.
- ~~`x86_64/translate.c:1023,1805` **DF flag translate-time-only**~~ **FIXED 2026-07-08** — DF is now the runtime bit `cpu->df`, set by std/cld/popfq and honored by the rep-string lowering + pushfq + sigframe; cross-block/dynamic-DF `rep movs/stos/scas` copies the correct direction. Test `repmovsdf`.
- `avx.c:1784` `ROUNDPS/PD/SS/SD` with MXCSR-mode bit → "treated as nearest" → wrong rounding if MXCSR.RC set.
- `avx.c:926`, `translate.c` reason 99 — genuinely unhandled VEX/EVEX/legacy-SSE → **exit 70 / abort** (loud, the *safe* failure mode, not silent).

## L — cosmetic / documented-benign / loud

`/proc/{stat,meminfo,vmstat}` + cgroup fields hardcoded 0 (monitoring rates read 0);
`fadvise`/`msync` advisory no-ops; `capset`/securebits/namespace no-ops (all-root
container model, path-jail is the boundary); ELF RELRO best-effort (DBT never executes
guest code natively); `select()` doesn't write back remaining time; `setfsuid/gid`
ownership-only; `sched_yield`/`sched_setscheduler` hints-only; explicit `SCM_CREDENTIALS`
*send* mistranslation (receive is fine); `NETLINK_KOBJECT_UEVENT` empty; `SIOCETHTOOL` zeroed.

---

## Recommended fix order

**Wave A — Chrome-critical (unblock the window):**
1. **H1 `FUTEX_WAKE_OP`** — in flight.
2. **H3 `g_br_ip[1024]`→`[DD_NFD]`** — one-line, kills a live corruption bug chromium triggers constantly.
3. **H4 net.c `< 1024` guards → `< DD_NFD`** — the fd-cap's second half; likely the 2nd Chrome blocker.
4. **H2 PI-futexes** — implement alongside H1 (adjacent code; both are sync-primitive fake-success).

**Wave B — broadly-impactful correctness (each a latent multi-hour investigation):**
5. **H10/H12/H13 FP miscompiles** (MIN/MAX NaN±0, CMPPS NLT/NLE, float→int indefinite) — pervasive, silent-wrong.
6. **H6 `fallocate`** range modes + ENOSPC; **H7 `mount`** family; **H8 memfd shrink/grow seals**.
7. **H9 `mprotect(PROT_EXEC)` SMC arm (#423)**; **H11 x87 80-bit (#248/#249)**.
8. M-wave: `set_robust_list`, `SO_RCVTIMEO`, RTNETLINK apply, `SO_PEERCRED`, `getsockopt`→ENOPROTOOPT, `sendfile` error, overlay `d_ino`.

Each fix is Linux-exact + gate `make test` 1638/0. H1–H4 together are the full Chrome unblock;
H10–H13 protect every numeric/ML workload; the rest close the long tail that would otherwise
each cost a Chrome-style saga to rediscover.

---

## H11 x87 80-bit — assessment & concrete plan (deferred: architectural, multi-day, precision-only) (#248/#249)

**What's fixed vs what remains.** The m80↔double CONVERTERS are now byte-exact for every value class that
does NOT depend on carrier width (±0/±Inf/NaN/exact-in-double) — the `x87m80` test pins this, including the
Inf/NaN exponent bugs. What remains is the CARRIER: `cpu->st[8]` is `double[8]` (52-bit mantissa, 11-bit
exponent), so a value with more than 53 significant bits or |exp| beyond binary64 range loses its tail.
Affected: C `long double`/`__float128`-via-x87 chains, `printf("%Lf"/"%Le")`, x87 arithmetic intermediates,
and 80-bit `fldt`/`fstpt` object round-trips of non-double values (drift in the low ~11 mantissa bits).

**Why it is NOT a cheap fix (honest path taken = document, not fake).** A true carrier means `cpu->st[]`
becomes an 80-bit representation (`struct { uint64_t frac; uint16_t se; }`, i.e. 10/16 bytes/slot). That is
cross-cutting and high-risk:
- **Baked offsets/addressing.** `OFF_ST` slot stride changes from 8 to 16 (`x87.c fp_slot_addr`, `emit.c
  e_st_addr`, every `ldr_d/str_d` of a slot) — all currently `slot*8`.
- **Every inline D8–DF op** (`translate.c` FADD/FSUB/FMUL/FDIV/FSQRT/FCOM via host `FDIV d`/`FADD d` on
  doubles, plus `x87.c` fprem/fscale/fxtract/frndint/fxam/ftst) must either keep computing at double (no
  gain) or call a **software ext80** add/sub/mul/div/sqrt/compare/round library — killing the inline fast
  path (a per-op block exit) and adding a classic soft-float bug surface.
- **The m80 converters** become near-trivial (direct copy) but FILD/FIST/FBLD and all int↔ext80 paths need
  rewriting against the new representation.
- **Host has no hardware ext80.** On arm64 `long double == double` (64-bit), so the host FPU cannot be
  leaned on; the arithmetic MUST be software. And the **transcendentals** (`x87_func`: F2XM1/FYL2X/FSIN/…)
  can only use host `double` libm regardless — they stay double-precision even after a soft-ext80 carrier,
  so a full carrier is a *partial* win (basic arithmetic + round-trips gain the tail; transcendentals do not).

**Concrete plan for the full carrier (when prioritized):** (1) add `typedef struct { uint64_t f; uint16_t se; } x87r;`
and change `cpu->st` to `x87r[8]`; bump the baked slot stride; (2) implement a small libm-free soft-ext80 core
(normalize, add/sub via exponent-align + 64-bit(+guard/round/sticky) mantissa, mul/div via `__int128`,
sqrt via Newton on the 64-bit mantissa, compare, round-to-int with the 4 RC modes); (3) route the inline
D8–DF arithmetic to a C helper (block exit, like the existing transcendental/rcl exits) operating on `x87r`;
(4) rewrite the m80/FILD/FIST/FBLD converters as direct field ops; (5) keep transcendentals on host double and
document that residual; (6) gate the whole thing (`NOX87EXT`) so the double-carrier fast path stays available
for A/B and perf. Estimated multi-day with heavy differential testing (long-double libm goldens vs qemu).

---

## Verification pass (2026-07-08, READ-ONLY audit of the landed wave)

Every item below was re-read at its **current** source location (the live tree is
`dd-jit-darwin/src/runtime/…`, not the stale `dd-jit/…` paths in the H-table headers). CONFIRMED =
the stub is genuinely gone and the real semantics are implemented; the file:line is where it now lives.

### CONFIRMED-FIXED

| # | Current location | What is actually there now |
|---|---|---|
| H1 | `os/linux/thread.c:731` (`futex_op` op==5) | `FUTEX_WAKE_OP` calls `futex_wake_op_apply(uaddr2,val3,&do_wake2)` — real atomic op on uaddr2 + conditional 2nd wake, both legacy-queue and bucketed paths. No longer `return 0`. |
| H2 | `os/linux/thread.c:508–627,632–690` | PI-futexes fully implemented: `futex_lock_pi` (CAS-acquire, FUTEX_WAITERS, EOWNERDEAD, blocks on condvar), `futex_unlock_pi` (owner-only, hand-off), WAIT_REQUEUE_PI/CMP_REQUEUE_PI dispatched (op 6/7/8/11/12/13). Real ownership, blocking, TID writes. |
| H3 | `os/linux/container/netns.c:540` | `static uint32_t g_br_ip[DD_NFD];` — migrated off `[1024]`. All 15 accessors guard `< DD_NFD`. OOB corruption gone. |
| H4 | `os/linux/syscall/net.c` (152/157/269/342/383/394/421/487/528–531/…/1042) | Every socket-state accessor now guards `< DD_NFD` (grep shows **zero** residual `< 1024` guards on the socket tables). SEQPACKET/passcred/peercred/EOF/lo/bridge/DNS all work at fd ≥ 1024. |
| H5 | `os/linux/syscall/net.c:1026` | `setsockopt` returns `r < 0 ? -errno : 0`. Known-benign options short-circuit to success *before* the call (`opt<0`); everything reaching the syscall surfaces its true errno. No longer `?0:0`. |
| H6 | `os/linux/syscall/fs.c:1453–1646` | `fallocate` implements PUNCH_HOLE (F_PUNCHHOLE), ZERO_RANGE (zero-fill), COLLAPSE_RANGE + INSERT_RANGE (read-shift-truncate/grow), plain reserve. Offset-inside-file validation + seal checks. No longer a no-op extend. |
| H7 | `os/linux/syscall/fs.c:1279` (`svc_mount`), `:1283` umount2, `:1298` pivot_root | `mount` → `svc_mount(c,…)` against the vfs (bind/tmpfs/remount,ro); umount2/pivot_root real. No longer unconditional `G_RET=0`. |
| H8 | `os/linux/syscall/fs.c:1407,1419` (ftruncate) + `:1483/1521/1525/1555/1589/1633` (fallocate) | `F_SEAL_SHRINK(0x2)`/`F_SEAL_GROW(0x4)` enforced on **both** ftruncate and every fallocate mode → EPERM. No longer write/pwrite-only. |
| H9 | `os/linux/syscall/mem.c:674` | `mprotect(PROT_EXEC)` sets `g_rwx_guest = 1` (gated `!NORWXFIX`), arming SMC write-fault invalidation exactly like the RWX-mmap case 222. #423 closed. |
| H10 | `translate/x86_64/translate.c:2831` | MIN/MAX lowered to compare+select: `FCMGT` mask + `BSL` → returns 2nd src on NaN/equal/±0, byte-exact with x86. No longer bare `FMIN/FMAX`. |
| H12 | `translate/x86_64/translate.c:2932` | CMPPS/PD/SS/SD: ordered `FCMGE/FCMGT` + `NOT`-invert for the N-forms (NLT/NLE) and NEQ → NaN lane becomes all-ones. UNORD/ORD built from `a==a & b==b`. |
| H13 | `translate/x86_64/translate.c:2810–2830` | float→int substitutes the "integer indefinite" (`0x80000000…`) on overflow/NaN via an FCMP-against-threshold + select, instead of ARM's saturating FCVTZS result. |
| M | `os/linux/syscall/proc.c:507` | `set_robust_list` records `c->robust_list`; `futex_robust_exit(c)` walks it on thread exit → OWNER_DIED + wake. |
| M | `os/linux/syscall/proc.c:493` | `set_tid_address` stores tidptr as clear_child_tid (zeroed + futex-woken on exit). |
| M | `os/linux/syscall/net.c:994` | `SO_RCVTIMEO/SO_SNDTIMEO` (+ _NEW 66/67) arm a real host `setsockopt`; getsockopt reports it back (`:1090`). |
| M | `os/linux/syscall/net.c:1108,1111` | Unknown `getsockopt` optname → `ENOPROTOOPT` (SOL_SOCKET + IPPROTO_TCP), not `*val=0`+success. |
| M | `os/linux/syscall/net.c:1015–1026` | `IPPROTO_IP` optnames translated via `ip_opt_l2m()` before the host call. |
| M | `os/linux/syscall/io.c:974` | `F_GETLEASE` returns `F_UNLCK(2)` (no lease held), not fabricated `0=F_RDLCK`. |
| M | `os/linux/syscall/io.c:629` | `sendfile` mid-copy read error returns the error (or the bytes already sent), no longer swallowed as EOF. |
| M | `os/linux/syscall/fs.c:2339–2349` | overlay `getdents64` `d_ino` = real `lstat(...).st_ino` of the merged entry; `pos+1` only as an unresolvable fallback. |
| M | `os/linux/syscall/rare.c:555` | `move_pages` QUERY mode fills `status[i]` (0 = present node / `-ENOENT` = unmapped); no longer leaves the buffer uninitialised. |
| M | `translate/x86_64/translate.c:3141,3205,3234` | `SHLD/SHRD` materialize CF (last bit shifted out of the original dst) + OF; regression `shldflags`. No longer SF/ZF-only. |

### STILL-OPEN — with architectural-vs-fixable verdict

| Item | Location | Verdict | Rationale |
|---|---|---|---|
| **DIVSS/DIVPS 0/0 NaN sign** (the FP follow-up in STATUS) | `translate/x86_64/translate.c:2884,2895` (plain `FDIV`) | **FIXABLE (→ fix agent)** | x86 `0/0` (and `∞/∞`) yields QNaN `0xFFC00000` (sign set); ARM FDIV yields default-NaN `0x7FC00000`. Only NaN-producing inputs differ. Fix = a sign-fixup select like H10, or detect the indefinite case. Cheap, low-frequency. Wrong NaN *bit pattern* only — not a spin/corruption. |
| **H11 x87 80-bit extended** | `translate/x86_64/translate/x87.c:15` (self-labelled "KNOWN GAP, NOT fixed") | **ARCHITECTURAL (toolchain); fixable only via soft-float** | `cpu->st[]` is 64-bit `double`; the macOS/arm64 build ABI makes `long double == double`, so there is **no native 80-bit carrier** to widen to. A true fix needs a software ext80 type + reworking every x87 op — a large feature, not a stub-fill. Impact bounded to C `long double`/`%Lf`/80-bit `fldt/fstpt`. Leave documented. |
| **seccomp no-op** | `os/linux/syscall/rare.c:85` (self-labelled SECURITY MODEL GAP) | **FIXABLE-in-principle, deliberately deferred** | dd services syscalls itself; there is no in-kernel BPF engine to hand the filter to. Truly enforcing needs a cBPF interpreter run against every syscall (large). Deliberate gap in the all-root/path-jail model; fails **open**, loudly documented — not silent-corruption class. Not a fix-agent target unless sandbox-fidelity becomes a goal. |
| **SO_PEERCRED per-peer uid/gid** | `os/linux/syscall/net.c:1070` (peer **pid** now resolved correctly; only uid/gid is container identity) | **ARCHITECTURAL** | Every container process runs under the **same host uid** (guest uids are emulated, `setuid` is ownership-only), and there is no cross-process channel to the peer's *emulated* guest uid. macOS LOCAL_PEERCRED can only ever return the shared host uid. Genuinely unfixable in dd's identity model. Impact: Postgres `peer`/ident + polkit see container identity, not a dropped-priv client uid. |
| **DF flag translate-time-only** | `translate/x86_64/translate.c:385` (`g_df`, reset per block, not restored by POPFQ) | **FIXABLE-but-benign (low priority)** | A `popfq` that sets DF=1 followed by string ops in a *later* block loses the direction. Real compilers/libc emit `std`/`cld` block-locally around the string op (SysV keeps DF=0), so no real workload hits it. Fix = thread DF through cpu state + restore on POPFQ/SAHF. Defer. |
| **RTNETLINK non-GET apply** | `os/linux/container/netns.c:1751` (`RTM_NEWADDR/NEWROUTE/SETLINK` fall through to an empty `NLMSG_DONE`) | **PARTIALLY FIXABLE** | The *apply* is architectural (dd's synthetic single-eth0/lo model has nowhere to add an address/route), but the **ACK is still wrong**: a request with `NLM_F_ACK` should get `NLMSG_ERROR{error=0}`, not an empty dump. `ip addr/route add` phantom-succeeds silently. Fixable = emit the correct `NLMSG_ERROR` ack (and reject unsupported applies with a real errno). |

**Net:** all **13 H-class** holes + all M-items the wave claimed are genuinely landed. The only
translator follow-up is the already-known **DIVSS-NaN sign** (fixable). Of the five long-standing
documented items, **two are truly architectural** (x87 80-bit — no host carrier; SO_PEERCRED uid —
shared host identity), and **three are fixable** but deliberately deferred / benign (seccomp BPF
interpreter; DF cross-block; RTNETLINK ack).

---

## Round-2: drain / readiness audit (the Chrome 100%-CPU spin class)

Chromium spun because its `MessagePumpLibevent` wakeup fd stayed perpetually `poll`-readable — a
`read` that didn't fully consume, so `poll` re-fired forever. This pass re-audits **every**
fd-wakeup/readiness primitive for that exact class: does `read` FULLY drain, and does the fd report
**not-ready** afterward? **Headline: no sibling of the Chrome spin survives** — every primitive
fully drains and re-syncs its readiness signal. Findings below are the residual observations, ranked.

### The primitives, verified

| Primitive | Read/drain path | Readiness backing | Verdict |
|---|---|---|---|
| **eventfd** | `io.c:314–366` | pipe (`fds[0]/fds[1]`) + `g_eventfd_count[]` | **CORRECT.** Read returns the counter and **resets to 0** (or `-1` for `EFD_SEMAPHORE`), then **drains the pipe** (`fcntl O_NONBLOCK` + `while(read>0){}`, `:353–355`) and **re-signals exactly one byte iff count still > 0** (`:357–360`). Write (`:459–474`) bumps the counter under `g_eventfd_lock` and drains-to-one-byte so each write is a fresh EV_CLEAR edge. The exemplar bug is *closed*. |
| **timerfd** | `io.c:293–311` | kqueue `EVFILT_TIMER` | **CORRECT.** `kevent` consumes+clears the pending expiration count and returns `kv.data`; periodic timers re-arm per interval; a subsequent poll/kevent is not-ready until the next tick. |
| **signalfd** | `io.c:194–219` | self-pipe `g_sigfd_pipe` (byte-per-signal) | **CORRECT (drain-safe).** Read consumes **one** wake byte → one 128-byte siginfo, clears the `g_pending` bit. Empty pipe → not-readable. See L1 (returns one record, not a full buffer — a fidelity nit, not a spin). |
| **pipe / socketpair self-pipe** | generic `io.c:392–405` | real host fd | **CORRECT.** `read()` drains what the host kernel has; host poll reports not-ready when empty. No dd-level faking. |
| **DNS socket** | `netns.c:2392–2416` `dns_send` | real socketpair — reply `write()`-en into the peer | **CORRECT.** Readiness is a real byte in a real socketpair; recv drains it. No synthetic readiness. |
| **AF_NETLINK** | `net.c:213/223`, `io.c:176`, `netns.c:1752` `nl_send` | real socketpair — dump `send()` synchronously on the request write | **CORRECT.** By the time `write()`/`sendmsg()` returns the response bytes are already queued in the socketpair; recv drains with MSG_PEEK/TRUNC size semantics. |
| **epoll/poll/select** | `event.c:22 (case 22)`, `:602 (pselect6)`, `:664 (ppoll)` | kqueue + one-shot `g_ep_prime` | **CORRECT.** Real kqueue readiness. Edge-primes are consumed once and removed (`:505–531`). The `epoll_pwait` re-block loop (`:538–544`) prevents a bare cross-thread `EVFILT_USER` wake or an EV_ERROR echo from returning a spurious `0` (which would spin libuv/node). |

### Ranked residual findings

- **L1 — signalfd read returns a single siginfo regardless of buffer size** (`io.c:208–218`).
  Linux fills `floor(buf/128)` records per read; dd returns exactly one (128 bytes). **Not a spin**
  (level-triggered: the pipe still has bytes, the guest re-reads and drains one more each time —
  forward progress every call, empties correctly). Pure throughput/fidelity nit. Rank **L**.

- **L2 — eventfd non-blocking `EAGAIN` path is invariant-dependent** (`io.c:331–336`). If a byte were
  ever stranded in the pipe with `count==0`, an `O_NONBLOCK` reader would return `EAGAIN` *without*
  draining it → `poll` stays readable → re-poll → EAGAIN → **spin**. **Verified safe:** the
  byte-present ⟺ count>0 invariant holds because *every* pipe write is done under `g_eventfd_lock`
  (write `io.c:459`, read `io.c:330`, AIO completion `aio.c:90–99`) and each re-syncs the pipe to the
  counter. No path writes the eventfd pipe outside the lock. Flagged only as a latent constraint:
  **any future code that signals the eventfd pipe MUST take `g_eventfd_lock` and re-sync**, or it
  reintroduces exactly the Chrome spin. Rank **L (watch-item, currently correct).**

- **L3 — timerfd/epoll readiness rides on macOS kqueue being `poll(2)`-able.** A guest that
  `poll()`s a timerfd/epoll fd directly (not via dd's `epoll_wait`) relies on a kqueue descriptor
  reporting POLLIN when it has pending events — which macOS does, and `read`/`kevent` then drains it.
  Verified consistent; noted because it is load-bearing and untested in isolation. Rank **L**.

**Conclusion:** the drain/readiness class is clean — the eventfd exemplar and all its siblings
(timerfd, signalfd, self-pipe, DNS, netlink, epoll primes) fully consume and report not-ready
afterward. No new H/M spin hole. The single actionable guard-rail is **L2**: the eventfd
pipe↔counter invariant is correctness-critical and must be preserved by any future signaller.
