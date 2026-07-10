# Rendering / Engine IPC Gaps

Engine-level bugs surfaced by the Chrome-on-Metal rendering effort. Full context and
evidence in [../rendering/README.md](../rendering/README.md). These are the deep
blockers to on-screen web content; the compositor and protocol layers are correct.

## Cross-process command-buffer content is lost (Wall 7)

Priority: P0 (blocks all web-page content rendering)
Impact: Chrome renders its UI but the web page content area is permanently blank
Confidence: High (instrumented, decisive)

Evidence:

- Web content is GPU-rastered in Chrome's separate **renderer** process (OOP raster),
  shipped to the in-process GPU service over a shared-memory command-buffer ring +
  eventfd/futex/Mojo signaling, and replayed by the GPU service via ANGLE → gl_shim → IR.
- The content raster never becomes IR: swept all 2752 `~/.dd/workspaces/chromiumws/upper/tmp/freshir-*.ir`
  captures — 0 contain the content signature (offscreen tile `Begin target=512/514` +
  page-bg clear `[0.914,0.933,0.969]`). Every live frame is UI-only + a white
  `ClearRect` placeholder in the content rect.
- NOT dmabuf/PRIME: added unconditional `[PRIMEDBG]` traces to both PRIME ioctl
  handlers (`dd-jit-darwin/.../container/vfs.c` 0xc00c642d/0xc00c642e), rebuilt, ran
  multi-process content → **0 PRIME calls**. Chrome does not move content via dmabuf.
- NOT the compositor: dd-display renders content correctly whenever the IR contains it
  (`target-chrome-codex/chrome-stream-ir-000.ir` replays full content).

Why this is bad:

The renderer→GPU-service command-buffer transport (shared-mem ring + cross-process
eventfd/futex wakeup) does not deliver the renderer's paint ops to the GPU service, so
the content is never rasterized into GL/IR. This is the documented "Wall 7" lost
cross-process wakeup (`docs/ENGINE_HOLES.md`).

Verification / repro:

Run multi-process Chrome through the pipeline (see ../rendering/README.md §4); frames
show UI only, content blank. Instrument the command-buffer flush + the eventfd/futex
wakeup on the renderer↔GPU-service channel to find the dropped wake.

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

Verification / repro:

`CHROME_EXTRA_FLAGS=--single-process CHROME_WINDOW_SIZE=512,384 KEEP_STATE=0` through
the pipeline; `JTS=1` shows the main thread's idle syscall loop. Sample guest threads at
the stall (per-tid backtrace) to identify the dropped wake.

## Shelved (not a bug): cross-process PRIME/IOSurface bridge

A PRIME id-passing bridge was implemented (`.claude/worktrees/content-path-analysis`) but
Chrome makes 0 PRIME calls for content, so it is inert and NOT the fix. Do not merge or
pursue unless a config actually exercises PRIME dmabuf sharing.
