# Rendering / Engine IPC Gaps

Engine-level bugs surfaced by the Chrome-on-Metal rendering effort. Full context and
evidence in [../rendering/README.md](../rendering/README.md). These are the deep
blockers to on-screen web content; the compositor and protocol layers are correct.

## Cross-process command-buffer content is lost (Wall 7) — ROOT-CAUSED + ENGINE FIX LANDED

Priority: P0 (blocks all web-page content rendering)
Impact: Chrome renders its UI but the web page content area is permanently blank
Confidence: High (root cause proven by a decisive before/after engine test)
Status (2026-07-10, branch `bugfix/wall7`): the engine lost-wakeup mechanism is FIXED and
regression-tested. The live "content renders on screen" confirmation still needs the macOS
Metal pipeline (see Verification) — it could not be exercised from the Linux dev host.

Root cause (the lost cross-process wakeup):

dd hashed every futex bucket by the futex WORD's **host virtual address**. That is Linux's
PRIVATE futex key (mm + address) and is correct for anon/private words — and for a
**fork-inherited** MAP_SHARED page, which lands at the SAME VA in parent and child (why
`futex-xproc` always passed). But a **file-backed MAP_SHARED** object (memfd/shm) is mapped
INDEPENDENTLY by each peer: Chrome's renderer and the GPU service map the command-buffer
shared memory at DIFFERENT addresses, so the SAME physical futex word has a different VA in
each process. Linux keys such a word by the SHARED object identity (**inode + page offset**),
so a `FUTEX_WAKE` through one mapping reaches a `FUTEX_WAIT` parked through another. dd's
VA-only key put the wake and the wait in **different buckets** → the wake was silently lost →
the renderer's command-buffer flush never woke the GPU service → the content raster never
became GL/IR. This is exactly "Wall 7". (The eventfd/Mojo half rides real host pipes/sockets,
which are shared by the kernel across processes regardless of VA, so *those* wakeups were not
the loss — the trusted `scm-eventfd-dense` cross-fork test already passes; the futex was the
hole.)

Evidence:

- Decisive reproducer `dd-tests/guests/ext_proc/futex_shared_key.c` (case `futex-shared-key`):
  a memfd mapped at TWO different VAs (same process, and cross-process after fork), a
  `FUTEX_WAIT` parked through mapping A, a `FUTEX_WAKE` issued through mapping B. Native
  Linux prints `two_map_woke=1 xproc_woke=1`; the **unfixed** dd engine printed
  `two_map_woke=0 xproc_woke=0` (both waiters hit the 2 s timeout — the wake never crossed the
  VA boundary). This is the Wall 7 mechanism in isolation, deterministic.
- Earlier layer-by-layer ruling still stands: content is absent from all 2752
  `~/.dd/workspaces/chromiumws/upper/tmp/freshir-*.ir` captures (UI-only + white `ClearRect`
  placeholder); **0 PRIME calls** (not dmabuf); dd-display renders content correctly whenever
  the IR contains it (`target-chrome-codex/chrome-stream-ir-000.ir`). So the loss is upstream,
  in the renderer→GPU-service transport — the futex wake proven above.

The fix (engine-C, `dd-jit-darwin/src/runtime/os/linux/`):

Implement Linux "shared" futex-key semantics. `thread.c` gains a small process-private registry
mapping each file-backed MAP_SHARED region's host VA range → `(st_dev, st_ino, file offset)`,
populated at mmap time and trimmed at munmap (`mem.c`, cases 222/215). A new `futex_key(uaddr)`
canonicalises a futex word in such a region to a stable token derived from that identity, and is
used for BOTH the bucket hash AND the per-address parked-waiter slot (`fbk_of` +
`fbk_park/unpark/match/parked`), so a waiter and a cross-mapping/cross-process waker agree on the
bucket and the slot. Words outside any shared region (every private/anon futex — the vast
majority) keep the VA key via a zero-entry lock-free fast path, so non-shared futexes are
byte-identical. A token collision only ever yields a spurious wake (re-checked), never a missed
one. Gate-safe (`1636/0` engine-C).

Verification / repro:

- Engine regression (runs from any host, incl. Linux dev host):
  `dd-tests -e aarch64 -e x86_64 futex-shared-key` — FAIL (`woke=0/0`, ~4.6 s of timeouts)
  before the fix, PASS (`woke=1/1`, <1 s) after. No regressions across `futex`, `futex-xproc`,
  `pi-robust`, `threads` (101), `shm`, `sem`, `sysv-shm/sem`, `sem-named`, `memfd`, `scm-rights`
  — all the process-shared-futex paths.
- Live confirmation (still TODO, needs the macOS Metal pipeline — see ../rendering/README.md §4):
  run multi-process Chrome (`CHROME_TIMEOUT>=180`) and check for the content signature in a fresh
  `freshir-*.ir` (offscreen tile `Begin target=512/514` + page-bg clear `[0.914,0.933,0.969]`) /
  the oracle page (RED-top / glyph / BLUE-bottom) on screen. The engine mechanism that dropped the
  wake is fixed; if content is still blank, the next suspect is a NON-futex wakeup on the same
  channel (e.g. a Mojo data-pipe signal), to be instrumented the same way.

## Single-process first-commit idle-stall

Priority: P1 (blocks the single-process content path)
Impact: `--single-process` (which collapses the renderer/GPU boundary and DOES render
content — proven Jul-9) never commits a first frame now
Confidence: High

Evidence:

- `--single-process` reaches `gl_shim: surface_up` + opens `/dev/dri/renderD128`, then
  browser threads park in `FUTEX_WAIT` and never commit any wl_surface frame (0 frames,
  even at 300s). Same idle-stall on BOTH `f26743cd` and `abd8564e` dd-display builds, so
  it is NOT a dd-display regression — it is the engine.
- Same class as Wall 7: a cross-thread/cross-process wakeup that never arrives during
  Chrome's heavy multi-thread startup.

Why this is bad:

Single-process is the natural content path (no cross-process command-buffer handoff);
un-blocking its first commit would render content without solving the full command-buffer
transport. The stall is a lost wakeup on a posted-task / CV / eventfd during startup.

Likely same class as the Wall 7 futex-key bug (now fixed): even single-process Chrome maps
its GPU command-buffer / transfer-buffer shared memory (memfd) and can reach the SAME word
through TWO different mappings (compositor endpoint vs GPU endpoint) — which the shared-futex-key
fix above (`futex_shared_key.c` "two_map" case, previously `woke=0`) now handles. Re-test
single-process on `bugfix/wall7` before treating this as a separate blocker.

Verification / repro:

`CHROME_EXTRA_FLAGS=--single-process CHROME_WINDOW_SIZE=512,384 KEEP_STATE=0` through
the pipeline; `JTS=1` shows the main thread's idle syscall loop. Sample guest threads at
the stall (per-tid backtrace) to identify the dropped wake. If a thread is parked in
`FUTEX_WAIT` on a memfd-backed shared word, the shared-futex-key fix applies; if on a private
word, look at the posted-task CV / eventfd instead.
