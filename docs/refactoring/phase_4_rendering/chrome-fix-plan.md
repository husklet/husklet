# Chrome multi-process content — root cause and fix plan

Status (2026-07-12): **mechanism reproduced and localized to the engine.** Interim
`--single-process` renders content today; the no-arg multi-process fix is a bounded change to the
engine's epoll→kqueue readiness emulation. This document is the actionable plan.

## 1. Symptom → verdict (settled)

No-arg Chrome cold-boots and renders its **window + browser UI** reliably, but in default
**multi-process** mode the **web-page content area stays white**.

Verdict: **B2 — the renderer process is dormant; it never rasterizes content.** NOT B1 (a dropped
content tile / missing buffer bridge). Proven live (agent `aaf0a61d`, commit `09db779d` instrumentation,
`96875457` docs): a `DD_TILE_TRACE` census in the deployed `gl_shim.c` over 956 frames of default
multi-process GPU compositing shows `sampled_EMPTY=0` and `offscreen_fbo_passes=0` every frame, **zero**
`eglCreateImage`/`eglBindTexImage` calls, and the content region composited as a solid white `ClearRect`
placeholder. There is no external tile to bridge — the renderer simply never produces one, because its
IO thread parks before rasterizing. Historical traces are available in git history; this document retains the
actionable verdict and fix.

The engine's cross-process IPC primitives are individually correct — every prior micro-gate passes
(`scm-recv-epoll` 3939762a, `exec-fd-epoll` 769dd2ec, `xproc-inbound`, `scm-futex`, the pump gates). The
failure is a **load-sensitive timing race**, not a missing primitive, which is why it eluded the earlier
single-leg gates. Release Chromium strips the Mojo node-connect DVLOGs, so the stuck verb cannot be named
from the binary — the reproduction had to be built engine-side.

## 2. Mechanism — reproduced in isolation (no Chrome, no egress)

Micro-gate `dd-tests/guests/ext_ipc/ipc_pump_primary_channel.c` (registered
`port("pump-primary-channel").only(Linux)`; agent `aaf0a61d`, commits **`177102bf`** base + **`da051b8a`**
sharpened) faithfully models `base::MessagePumpEpoll`, combining the one permutation no passing gate
covered:

- an **SCM_RIGHTS-received** `SOCK_STREAM` primary channel carried across `execve`;
- armed **level-triggered** (`EPOLLIN`, **no** `EPOLLET`) on a shared epoll with **≥1024–3000 idle
  watches**;
- **concurrent cross-thread `epoll_ctl` churn** (2 threads);
- an `eventfd` `ScheduleWork` + a cross-thread `WaitableEvent` (IO→main) second pump;
- variable-delay "browser" writes;
- **the key move:** the IO pump periodically **`StopWatch→Watch`** the received socket — i.e.
  `EPOLL_CTL_DEL` then `EPOLL_CTL_ADD` — **while its receive buffer is non-empty** (one-read-per-level-wake
  against 2-message parent bursts). The re-`ADD` must **re-prime level readiness**.

Result on the aarch64 JIT: the base gate is 7/7 PASS (a single leg is fine, like all prior gates), but
the sharpened gate **intermittently STALLS** (e.g. iter 1 PASS, iter 2 FAIL: the in-guest watchdog fires
`_exit(7)` because a pump parked with a message still pending). It passes **3/3 natively**, so the defect
is the **engine**, not the gate.

That parked-with-data-pending pump **is** the dormant renderer.

## 3. Root cause in the engine

`dd-jit-darwin/src/runtime/os/linux/syscall/event.c`, the epoll→kqueue readiness-prime path.

The engine already primes readiness on registration for **edge-triggered** fds: an fd that is already
readable/writable at `EPOLL_CTL_ADD`/`MOD` time when registered `EPOLLET` gets a synthetic readiness event
stashed in `g_ep_prime[]` (`ep_prime_push` / `ep_prime_if_ready`, event.c:18–55), and a cross-thread
`NOTE_TRIGGER` (`ep_flush(ep, wake=1)`, event.c:207–224) makes a peer blocked in `kevent()` return and
re-scan those primes.

**The bug is the standing assumption at event.c:25** — *"Level-triggered fds need no prime (kqueue without
`EV_CLEAR` already reports current readiness), so only `EPOLLET` arms reach here."* That is **false for
re-registration of an already-readable fd under load**:

- On `EPOLL_CTL_DEL`+`ADD` of a socket that **already has buffered data**, a level-triggered arm is NOT
  primed (only `EPOLLET` is), so it depends entirely on kqueue delivering an immediate `EVFILT_READ` for
  the fresh `EV_ADD` of an already-readable socket.
- Under the **W3E deferred-registration fast path** (registrations are batched into `g_ep_chg[]` and
  flushed on the next `kevent()` — event.c:61–66, 207–217) combined with **cross-thread churn** and a
  **high watched-fd count**, that immediate readiness is not reliably delivered to a peer already blocked
  in `kevent()`: the re-`ADD`'s current readiness is stranded on the registering thread. The pump sleeps
  with data waiting.

So the residual is specifically: **re-`ADD`/`MOD` of an already-readable *level-triggered* SCM_RIGHTS-
received socket does not re-arm level readiness cross-thread.** Linux epoll always reports it (its
`epoll_ctl` is internally serialized and a level-ready fd is returned on the very next `epoll_wait`).

## 4. The fix

In `event.c`, on the `EPOLL_CTL_ADD` / `EPOLL_CTL_MOD` registration path:

1. **Prime level-triggered arms too, not only `EPOLLET`.** When registering/modifying interest in an fd,
   call `ep_prime_if_ready(ep, fd, filt, udata)` for the requested directions **regardless** of edge vs
   level — if the fd currently polls ready for a watched direction, stash the synthetic readiness. Delete
   the "only `EPOLLET` arms reach here / level needs no prime" carve-out (event.c:18–26). Linux level
   semantics = "ready now ⇒ report now," which the prime path already models. (Level fds that are *not*
   currently ready still cost nothing — `ep_prime_if_ready` no-ops when `poll()` says not-ready.)
2. **Deliver it cross-thread.** Ensure this registration takes the `wake` path — `ep_flush(ep, wake=1)`
   (event.c:207–224) — so the `NOTE_TRIGGER` returns a peer blocked in `kevent()` to re-scan primes. The
   add/modify path must set `wake=1` (interest was added/modified), which it already does for the edge
   case; extend it to the level case.
3. **Mirror on the raw kqueue arm.** Where interest is armed via `EV_SET(..., EVFILT_READ, EV_ADD, ...)`
   (`ep_rearm_from_interest` event.c:144–160 and the primary register path), a fresh `EV_ADD` of an
   already-readable socket must not be *relied on* for the immediate edge — the prime in step 1 is the
   authority; the `EV_ADD` continues to cover subsequent readiness.
4. **Idempotency / no double-report.** `ep_prime_push` already de-dups by `(ident, filt)` (event.c:32–37),
   and level primes are consulted-then-consumed on the next wait like edge primes, so a genuinely-still-
   readable level fd re-reports naturally on the following `epoll_wait` (correct level semantics), while a
   drained one does not.

This keeps the fast single-threaded path byte-unchanged (uncontended; `poll()` prime is best-effort and
only adds work when an fd is actually ready at registration) and closes the cross-thread re-arm race.

## 5. Validation (required before claiming the fix)

- The bug is **intermittent**, so a couple of green runs prove nothing. Run the sharpened gate
  `pump-primary-channel` **50–100 iterations** and require **zero** stalls.
- **No regression** to the epoll path: all prior `ext_ipc` gates stay green — `scm-recv-epoll`,
  `exec-fd-epoll`, `xproc-inbound`, `scm-futex`, and the pump-`et`/`oneshot`/`worker-dispatch`/
  `epollout-rearm`/`epoll-shared-xthread` gates.
- Full engine harness green (the epoll emulation underpins every guest).
- **Capstone (proof gap):** a live no-arg multi-process Chrome run rendering real content. This is
  currently **egress/image-blocked** (release Chromium images not provisioned; Little Snitch blocks the
  pull while the maintainer is away). Until an image is reachable, the high-iteration micro-gate is the
  achievable proof; run the live capstone the moment a Chrome image is available.

## 6. Ownership / status

- Reproduction gate: committed on branch `worktree-agent-aaf0a61da...` (`177102bf`, `da051b8a`).
- Fix: in progress in `event.c` per §4; to be committed separately, then validated per §5.
- Interim for users today: launch Chrome `--single-process` (renders real content on the accelerated
  Metal path; trades the renderer sandbox). Not the final answer — the no-arg fix is §4.
