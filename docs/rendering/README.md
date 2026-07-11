# Rendering: Chrome-on-Metal inside dd — state, root causes, and how to run

Goal: run Google Chrome inside the dd Linux container on the accelerated GPU/Metal
path, presented as a native macOS window, visually indistinguishable from Chrome
on macOS (UI + web content sharp, correctly scaled/oriented/clipped, input working).

Status as of 2026-07-10 (late): **UI renders live and correctly. Single-process web
page CONTENT now renders live** (first time on the Metal path since Jul-9; see §3.1 —
content glyphs are currently upside-down, an orientation issue tracked separately).
**Multi-process content is still blank** (§3.2, narrowed further below).

---

## 1. What works (demonstrable, live)

Chrome runs inside the dd container on the Metal path as a native macOS window. The
**browser UI chrome renders correctly**: title bar, window buttons, the info bar —
sharp, upright, correctly scaled, clipped to the window, no white border, stable
across hundreds of byte-identical frames (no flicker/freeze). Input reaches Chrome.

Evidence: `target-chrome-codex/chromium-run-*/frames/surface-6-*.png` from a live run
show the Chrome window with correct UI. A preserved sample is in the session
scratchpad (`render-evidence/live-ui-render.png`).

## 2. The integrated build

Branch **`render-integrated`** (commit `abd8564e`, based on `f26743cd`) in worktree
`.claude/worktrees/merge-protocol`. Contents (27 dd-display lib tests green, mac
release build clean):

- **Wayland protocol completeness** (6 agents merged): wl_surface (buffer transform,
  shm pool resize/destroy, regions, destroy/Drop), xdg-shell (positioner/popup +
  configure handshake, set_max/min_size, maximize/fullscreen, and the request-gated
  window drag via `xdg_toplevel.move` → `NSWindow.performWindowDragWithEvent`),
  wl_seat (touch, grouped axis-scroll with axis_source/axis_discrete, punctuation
  keymap, enter→frame/modifiers), subsurface + viewport (sync/desync compositing,
  source×buffer_scale crop), zwp_linux_dmabuf + wp_presentation (presented feedback)
  + wl_output v4 (scale/name).
- **Content orientation fix** (`metal_backend.rs`): Chrome GPU-rasters page body into
  OFFSCREEN tile textures (bottom-left GL) that Metal stores top-left → content
  upside-down; UI glyphs come from CPU-uploaded atlases (never render targets) →
  upright. Fix: track render-pass target ids; negative-height (Y-flip) viewport for
  passes whose target is an offscreen texture; surface passes untouched. Supersedes
  the old `DD_CONTENT_GLYPH_VFLIP` heuristic. Validated by IR replay (before/after
  in `target-chrome-codex/renderdiff/`).
- **GL→Metal translation** (`gl_shim.c`): mat3 uniform byte-layout fix (was 4B, MSL
  needs 48B → corrupted every uniform after a mat3), comment-stripping in the uniform
  layout pass, matrix column re-striding, mat2/non-square matrix + GLSL builtin fixups.
- **Oracle-driven dd-display fixes**: frame-callback now sends CLOCK_MONOTONIC-ms
  (`frame_time_ms`) not a serial (Weston parity; Chrome paces its frame clock off it);
  `surface_augmenter` (ChromeOS-only global Weston never advertises) gated OFF by
  default (`DD_DISPLAY_AUGMENTER=1` re-enables).
- **Golden-image regression harness** (`dd-display/tests/golden_image_regression.rs`,
  `run_golden.sh`): real MetalBackend IR-replay pixel-diff, 4 bit-exact goldens incl.
  a both-axes-asymmetric orientation pin; dependency-free PNG decoder.

## 3.1 RESOLVED: single-process content renders live (2026-07-10)

With the shared-futex-key engine fix (`bc9aa532`, merged `9c46e49b`; gate
`futex-shared-key` passes 2/2) plus one launch fix, `--single-process` Chrome renders
web page content in the live Metal pipeline: page background `#e9eef7` fills the
content area and the test page's text draws (currently upside-down — see below).

- Proof: run `chromium-run-gpu_retry_205503` — frames `surface-6-00{0,1,2}.png` show
  the rendered page; the teed IR (`spir-000.ir`, preserved with the frame in the
  render/chrome-content worktree under `target-chrome-codex/evidence/`) carries the
  full content signature: `Begin target=512` and `Begin target=514` with clear
  `[0.9137, 0.9333, 0.9686]` (= `#e9eef7`) plus glyph draws inside those passes.
  Independently reproduced minutes later by a second session (run
  `chromium-run-gpu_retry_205931`).
- **Second blocker found and fixed on the way:** the engine implements guest `fork()`
  as a real host `fork()` of a multithreaded objc-using process, and a guest `execve()`
  reloads the image in-place (no host exec) — so if
  `OBJC_DISABLE_INITIALIZE_FORK_SAFETY=YES` is not in the engine's exec environment,
  a guest fork that races another thread's `+initialize` aborts the child on its first
  Foundation use (`objc_initializeAfterForkError`; live signature: Chrome dies with
  `+[NSPlaceholderString initialize] may have been in progress in another thread when
  fork() was called` right after `gl_shim: surface_up`, exit 137). Some harness
  branches dropped the var (the macOS BSD-`script` fallback). `ddcli workspace launch`
  now guarantees it (dd-cli/src/ddjit_launcher.rs).
- **Known residual: content is Y-flipped in the live composite.** The offscreen-tile
  Y-flip in `metal_backend.rs` renders the OLD capture (`chrome-stream-ir-000.ir`)
  upright but the NEW captures flipped — replaying the same new IR through both main's
  and the integrated branch's backend flips identically, so Chrome chose a different
  composite transform in these runs. The static "flip offscreen passes" heuristic is
  insufficient; orientation must be derived per-pass from the emitted
  projection/viewport transform. Tracked by the orientation workstream
  (`render-integrated`).

## 3.2 The remaining blocker: MULTI-process web content is blank

**Content-blank is the cross-process renderer→GPU-service COMMAND-BUFFER IPC transport
— NOT compositing, NOT PRIME/dmabuf.** Ruled out layer by layer with evidence:

- **Not the compositor.** dd-display renders content correctly whenever the IR
  contains it (`chrome-stream-ir-000.ir` replays full content through the integrated
  backend).
- **Content is absent from the IR.** Swept all 2752 `~/.dd/workspaces/chromiumws/upper/tmp/freshir-*.ir`
  captures: 0 have the content signature (offscreen tile `Begin target=512/514` +
  page-bg clear `[0.914,0.933,0.969]`). Every live frame has only `Begin target=1` +
  `ClearRect{texture:1, x16 y82 w480 h270, white}` — a white placeholder where the
  renderer's content texture should be, then UI glyphs on top.
- **Not PRIME/dmabuf.** Added unconditional `[PRIMEDBG]` traces to both PRIME ioctl
  handlers, rebuilt the engine, ran multi-process content: **0 PRIME calls**. Chrome
  does not move content via dmabuf here.
- **It is the command-buffer path.** Chrome's renderer process records paint ops
  (OOP raster) and ships them to the in-process GPU service over a shared-memory
  command-buffer ring + eventfd/futex/Mojo signaling; the GPU service replays them via
  ANGLE → gl_shim → IR. The content raster never arrives = the documented **"Wall 7"**
  (`docs/ENGINE_HOLES.md`).
- **The shared-futex-key fix alone is NOT sufficient for multi-process** (2026-07-10
  live validation). Two independent post-fix multi-process runs (engine with
  `bc9aa532`, `futex-shared-key` gate green, objc var verified in the engine's exec
  env via `ps -wwwE`): `chromium-run-gpu_retry_202521` (6589 teed frames, no crash
  lines) and `chromium-run-gpu_retry_210317` (4221 frames) — **zero content
  signature in every teed frame**; the IR only ever contains `Begin target=1` UI
  passes + the white `ClearRect{texture:1, 16,82 480x270}` placeholder. UI stays
  live throughout, so the GPU service's own channel works; it is specifically the
  renderer's raster stream that never manifests.
- **The renderer is fully DORMANT, not crashed** (diagnostic run
  `chromium-run-gpu_retry_211107`, `DD_FATALSIG_LOG=1`): zero `[DDFATAL]` lines
  (no guest process died of a fatal signal all run), and the end-of-run 2-second
  `sample` of the renderer processes (e.g. `sample-61060.txt`: 9 threads) shows
  **100% of samples parked** — 7 threads in `svc_proc → futex_op`
  (guest FUTEX_WAIT, two distinct wait sites) and 2 threads in `svc_event → kevent`
  (the guest epoll pumps, i.e. Chrome's main/IO Mojo message loops) — while the
  browser process paints ~24 fps UI the whole time. The renderer never receives
  the inbound Mojo traffic (GpuChannel establishment reply / BeginFrame) that
  would start content rasterization: its epoll pumps get no events and its
  worker futexes are never woken. A recurring
  `Network service crashed, restarting service.` (Chrome-perceived channel
  death, engine sees no fatal signal) suggests the same inbound-delivery gap
  hits other utility-process Mojo channels too.

**RULED OUT (2026-07-10, branch `render/mojo-delivery`): the fd-transport inbound
delivery is NOT the drop.** The leading hypothesis was that dd loses a cross-process
Mojo wake (a browser `write()`/eventfd-signal on one end failing to wake a child
parked in `epoll_wait`/`FUTEX_WAIT` on the peer end — the same per-process-state
class as the old VA-keyed futex bug). Five deterministic micro-gates now reproduce
every primitive that inbound delivery rides on across a REAL process boundary, and
ALL pass on both Linux engines (`dd-tests -- xproc-inbound zygote-inbound
xproc-prearm scm-futex`, plus the pre-existing `scm-eventfd*`):

- `xproc-inbound` — a fork+**execve** child parked in `epoll_wait`; the parent
  writes a SEQPACKET-socketpair message + signals an eventfd. Both wake the child's
  epoll and carry the correct payload (msg + eventfd counter). This is the exact
  browser→renderer direction and the exact fork+exec boundary Chrome uses; the older
  `scm-eventfd` gate only tested the OPPOSITE direction (parent epolls, child writes).
- `zygote-inbound` — the renderer LAUNCH path: a channel end + eventfd are handed to
  a pre-existing zygote via **SCM_RIGHTS**, which `fork()`s the renderer that inherits
  them; the browser then writes inbound. Delivered correctly to the grandchild.
- `xproc-prearm` — **EPOLLET** (edge-triggered) delivery of a socket message +
  eventfd signal the parent buffered BEFORE the child registered its epoll (Chrome
  routinely sends a child's bootstrap before the child's loop starts). The
  registration-time readiness prime (`g_ep_prime`, event.c) fires cross-process.
- `scm-futex` — an **SCM_RIGHTS-passed memfd** (not fork-inherited) mmap'd at an
  independent VA in a zygote-forked grandchild; a `FUTEX_WAKE` through the browser's
  own mapping releases the grandchild's `FUTEX_WAIT` (the renderer↔GPU command-buffer
  wakeup). The `futex-shared-key` gate only covered a fork-inherited memfd.

So the eventfd counter (MAP_SHARED, keyed by counter slot / SCM_RIGHTS meta trailer),
the SEQPACKET-DGRAM socketpair, epoll/kqueue readiness (incl. the edge-prime), the
SCM_RIGHTS fd/handle passing, and the shared-object futex key all cross real dd
process boundaries correctly. **The multi-process content-blank wall is therefore
NOT the engine's cross-process fd-transport emulation.** It is higher in the Mojo
stack — the browser never producing the renderer's inbound traffic (a failed node/
invitation handshake step upstream, or the sandboxed-renderer bring-up), or the
specific sync EstablishGpuChannel reply the renderer blocks on. The recurring
`Network service crashed, restarting service.` (a clean exit on a failed bootstrap
the browser reads as a crash) points at the SAME upstream handshake, not the wake.

**ALSO RULED OUT (2026-07-10, branch `render/mojo-bootstrap`): the child-process
Mojo BOOTSTRAP fd hand-off is NOT the drop.** The next hypothesis after transport
was the LAUNCH handshake: Chrome hands each child its initial platform channel +
shared-memory command buffer at launch by placing each fd at a FIXED number and
naming that number on the child's command line (`--mojo-platform-channel-handle=N`,
field-trial/shared-memory handles). dd implements guest `fork()` as a real host fork
and guest `execve()` as an IN-PLACE image reload (`proc.c` case 221, no host exec),
so a bootstrap fd could in principle be dropped or renumbered across that reload, or
the recent close-on-exec sweep (`exec_close_cloexec`, sweeps FD_CLOEXEC fds by hand
since no real exec runs) could close a fd Chrome needs. Audited + gated clean:
- Engine audit: emulated fds (eventfd/memfd/epoll/timerfd/signalfd) are all
  **real-host-fd-backed**; guest `fcntl(F_SETFD/F_GETFD)` forwards to the same host
  fd the sweep inspects (`io.c` case 1/2 → host `fcntl`), so a guest that clears
  CLOEXEC on a bootstrap fd and the sweep agree — the sweep closes exactly the
  still-CLOEXEC fds and leaves the cleared ones open at their number.
- New gate `bootstrap-handle` (`ext_ipc/ipc_bootstrap_handle.c`) reproduces the full
  bootstrap end-to-end across dd's fork + in-place execve: the browser dup2's a
  SEQPACKET Mojo channel to fd 7 and a **memfd command buffer** to fd 8 (both
  non-CLOEXEC), passes those numbers as argv STRINGS, and execs; the exec'd child
  recovers each fd purely from argv and (1) receives the browser's inbound channel
  message, (2) MAP_SHARED-mmaps the memfd coherently in the NEW image, (3) is
  released by a cross-process `FUTEX_WAKE` on a word inside it, while (4) a sibling
  FD_CLOEXEC alias is correctly swept — proving survival is by honouring the cleared
  flag, not a no-op sweep. Passes on both Linux engines. None of the
  xproc-inbound/zygote-inbound/scm-futex gates carried a memfd across the in-place
  execve or validated the argv fd-number; this closes that gap.

So the bootstrap fd hand-off (fd-number preservation across fork+in-place-execve,
cmdline-number↔fd agreement, the shared-memory invitation, the close-on-exec sweep)
is ALSO correct. Both the transport AND the launch-time fd hand-off are now exonerated
— the wall is purely Chrome-internal Mojo state (a failed node/invitation handshake
step or the sync EstablishGpuChannel reply the renderer never receives).

**Next step**: pin the higher-layer failure from the CHROME side (both the engine
transport AND the bootstrap fd hand-off are exonerated). Because a live multi-process
run is destructive to any
concurrent Chrome session (serialize — check for running `ddjit`/`chromium` first),
coordinate with the live session (e.g. `wall7-live`): drop `--log-level` so Mojo
VLOGs surface, add `--vmodule=node_controller=2,node_channel=2,broker*=2,
gpu_channel*=2` to see WHICH bootstrap step the renderer/network-service stall on,
and confirm whether the browser ever issues the `write()` to the renderer's channel
fd at all (if it never writes, the gap is Chrome-internal state, not delivery).

### Shelved: the PRIME/IOSurface engine change
A cross-process PRIME/IOSurface bridge was implemented (write IOSurface global id into
the PRIME export fd + add a `PRIME_FD_TO_HANDLE` importer that `IOSurfaceLookup`s it)
in worktree `.claude/worktrees/content-path-analysis`. It builds clean (hash unchanged,
ddcli drop-in) but is **the wrong mechanism** (Chrome makes 0 PRIME calls) — **inert,
harmless, NOT merged, do not pursue** unless a future config actually exercises PRIME.

## 4. Reliability notes / how to run

- **Live pipeline (serialize — ONE at a time; concurrent runs contend and flake):**
  `target-chrome-codex/run_chrome_gpu_bounded.sh`. Override `DDISP=<dd-display binary>`,
  `DDJIT_DIR=<engine out dir>`. Known-good single-process content config:
  `CHROME_WINDOW_SIZE=512,384` (COMMA, not `x` — Chrome silently mis-parses `800x600`),
  `CHROME_EXTRA_FLAGS=--single-process`, `KEEP_STATE=0`, `CHROME_DISPLAY_START_DELAY=12`,
  `CHROME_TIMEOUT>=180`. Concurrent runs are actively destructive, not just flaky: each
  launch with `CHROME_KEEP_STATE=0` wipes the shared chromiumws checkpoint/profile and
  swaps the shared `~/.dd/gui/aarch64/lib` contents — starting a second run SIGKILLs the
  first run's Chrome.
- **ddcli must be built from the SAME rev as the engine** — the typed-launch configfd
  wire evolves (e.g. DD_EGRESS_SOCKS); a stale prebuilt ddcli fails instantly with
  `dd: --configfd: short read of N pool bytes` (ddjit_configfd.c). Build ddcli +
  dd-jit-darwin + dd-display together in the worktree and override `DDCLI`/`DDJIT_DIR`/
  `DDISP` (the stock script hardcodes DDCLI — patch it or use a copy that honors the
  env var).
- **The script's end-of-run pid capture greps for the MAIN-tree engine path** —
  a worktree `DDJIT_DIR` doesn't match its awk pattern, so per-pid `sample`/`vmmap`
  are skipped and the run's ddjit processes LEAK (they also hold the shared workspace
  and poison the next run). Broaden the pattern to `dd-jit-darwin-.*ddjit-linux` when
  running from a worktree, and sweep orphans before a new run.
- **Content-signature capture**: the engine's freshir auto-dump was stripped with the
  debug scaffolding; use the gl_shim env `DD_IR_TEE_DUMP=/tmp/<prefix>` (a GUEST path —
  files land in `~/.dd/workspaces/chromiumws/upper/tmp/<prefix>-NNN.ir`) and decode with
  `cargo run -p dd-gpu --example dump_ir`. Signature: `Begin target=Some(512|514)` with
  clear `[0.9137, 0.9333, 0.9686]`.
- `OBJC_DISABLE_INITIALIZE_FORK_SAFETY=YES` is now guaranteed by `ddcli workspace
  launch` itself (see §3.1) — launch environments no longer need to remember it.
- **Deterministic validators (prefer these — no flaky live Chrome):** `run_golden.sh`
  (Metal IR-replay pixel-diff); IR replay of `target-chrome-codex/*.ir`; inspect
  `target-chrome-codex/renderdiff/{before,after}-*.png`.
- **The Weston differential oracle** (`target-chrome-codex/oracle/`, `run_oracle.sh`)
  proved the SAME Chromium renders 6+ frames flawlessly against real Weston in this VM
  — so a blank dd-display window is a protocol/engine gap, never a fundamental engine
  inability. `chrome-weston.png` is the ground-truth image; `GAP_MATRIX.md` maps every
  interface Chrome↔Weston↔dd-display.

## 5. Pointers
- Compositor: `dd-display/src/{server.rs, metal_backend.rs, present_cocoa.rs, present.rs}`
- GL→IR shim: `dd-tests/guests/gl_shim.c`
- Engine (gate): `dd-jit-darwin/src/runtime/os/linux/{container/vfs.c, syscall/*, thread.c}`
- Prior wall analysis: `docs/ENGINE_HOLES.md`, `docs/ideas/RENDERING_PLAN.md`, `docs/ideas/CHROME_GBM_PLAN.md`
