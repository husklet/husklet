# Rendering: Chrome-on-Metal inside dd — state, root causes, and how to run

Goal: run Google Chrome inside the dd Linux container on the accelerated GPU/Metal
path, presented as a native macOS window, visually indistinguishable from Chrome
on macOS (UI + web content sharp, correctly scaled/oriented/clipped, input working).

Status as of 2026-07-10: **UI renders live and correctly; web page CONTENT is blank**
(engine-level blocker, precisely characterized below).

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

## 3. The remaining blocker: web content is blank (root-caused)

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
  ANGLE → gl_shim → IR. The content raster never arrives → a **lost cross-process
  wakeup** = the documented **"Wall 7"** (`docs/ENGINE_HOLES.md`). The natural sidestep,
  `--single-process` (collapses the renderer/GPU boundary → content renders, as it did
  on Jul-9), is blocked by the *same class* of engine idle-stall: it reaches
  `gl_shim: surface_up` then threads park in `FUTEX_WAIT` and never commit a first
  frame — confirmed identical on both `f26743cd` and `abd8564e` (so NOT a dd-display
  regression; it's the engine).

**The real fix is engine IPC fidelity** (command-buffer transport / cross-process
eventfd/futex wakeup under Chrome's heavy multi-thread startup) — engine-C, the
protected 1636/0 gate, historically unsolved.

### Shelved: the PRIME/IOSurface engine change
A cross-process PRIME/IOSurface bridge was implemented (write IOSurface global id into
the PRIME export fd + add a `PRIME_FD_TO_HANDLE` importer that `IOSurfaceLookup`s it)
in worktree `.claude/worktrees/content-path-analysis`. It builds clean (hash unchanged,
ddcli drop-in) but is **the wrong mechanism** (Chrome makes 0 PRIME calls) — **inert,
harmless, NOT merged, do not pursue** unless a future config actually exercises PRIME.

## 4. Reliability notes / how to run

- **Live pipeline (serialize — ONE at a time; concurrent runs contend and flake):**
  `target-chrome-codex/run_chrome_gpu_bounded.sh`. Override `DDISP=<dd-display binary>`,
  `DDJIT_DIR=<engine out dir>`. Known-good single-process capture config (produced
  content Jul-9): `CHROME_WINDOW_SIZE=512,384` (COMMA, not `x` — Chrome silently
  mis-parses `800x600`), `CHROME_EXTRA_FLAGS=--single-process`, `KEEP_STATE=0`,
  `CHROME_DISPLAY_START_DELAY=12`. Multi-process reliably renders UI at
  `CHROME_TIMEOUT>=180` (the default engine build `dd-jit-darwin-16122afd` emits heavy
  `DDWAKE` trace and is slow → give it time; 70s is too short).
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
