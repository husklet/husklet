# Multi-process Chrome content: the wl_shm CPU-raster route does NOT dissolve Wall 7

Live-validated 2026-07-12 on current main (`a35d9fea`), all binaries (dd-jit-darwin
engine, ddcli, dd-display) freshly built from this worktree at that rev. This corrects
the working hypothesis in `WALL7-FINDINGS.md` §6 ("switching Chrome content to wl_shm
CPU raster very likely dissolves Wall 7 outright") and the task premise that the renderer
rasters but its output fails to reach viz. **Neither holds: the renderer never rasters —
its primordial IPC channel to the browser never connects, so no content is produced at
all, and no dd-display / buffer-acceptance change can help.**

## What was run

The wl_shm CPU-raster config the codex sibling built toward (`0b8561bc present wl_shm
XRGB content opaque`, already merged; `118a7aca` wl_shm-pool clamp, already merged):

- `CHROME_SW=1` → guest launcher (`ddrun_codex.sh`) swaps GL flags to
  `--disable-gpu --disable-gpu-compositing` (browser + content both software).
- **Default multi-process** (no `--single-process`).
- Offline blue page: `data:text/html,<body style='margin:0;background:#1a73e8;height:100vh'>`.
- `CHROME_WINDOW_SIZE=512,384`, `DD_DISPLAY_DEBUG=1`, bounded timeouts.

5 runs (`target-chrome-codex/shmrun-*`, gitignored). Evidence frame committed:
`docs/rendering/wl_shm-evidence/multiproc-wlshm-white-content.png`.

## Result: UI renders, content is 100% white

Pixel analysis of the content region (below the infobar) across two runs:

```
surface-16-480.png 480x342 content-region: blue=0% white=100% other=0%
surface-16-043.png 480x342 content-region: blue=0% white=100% other=0%
```

The title bar (shows the data: URL), the "unsupported command-line flag" infobar, and the
window buttons render sharp and correct. The tab shows a **perpetual loading spinner**.
Only **one** wl_surface is ever produced — `surface-16`, the browser toplevel; there is no
content surface and no content subsurface (dd-display logs no `wl_subcompositor.get_subsurface`).

## Pinned mechanism (why content is white)

With `--disable-gpu-compositing` the browser's own in-process (software) display compositor
composites the window and presents ONE wl_shm buffer per frame to dd-display — this is why
the **UI** is live. The **content** is produced by the separate **renderer process**, which
must hand its rasterized output to viz as a software `CompositorFrame` (shared-memory bitmap
resources) over the `CompositorFrameSink` Mojo interface. That never happens:

1. **The child processes' primordial IPC channel never connects.** Every run logs
   `INFO:child_thread_impl.cc(957) ChildThreadImpl::EnsureConnected()` from the child PIDs —
   the channel-connect watchdog that fires because `OnChannelConnected(peer_pid)` was never
   reached (same signature as `WALL7-FINDINGS.md` §2, now confirmed to persist in wl_shm mode).
2. **viz receives no frame.** With `--vmodule=*viz*=3,*frame_sink*=4,*compositor*=4,*begin_frame*=4`
   the browser logs **zero** frame-sink / BeginFrame / CompositorFrame activity — viz never
   gets a content frame to aggregate.
3. **`ERROR:network_service_instance_impl.cc(599) Network service crashed, restarting service.`**
   — the same upstream Mojo bring-up failure hits the utility processes too (clean-exit on a
   failed bootstrap the browser reads as a crash).

So the renderer is **dormant pre-raster**, exactly as in the GPU path (`README.md` §3.2). The
wl_shm route only changes *how UI/content would be rasterized* (CPU vs GL→IR→Metal); it does
**not** change the fact that the renderer→viz `CompositorFrameSink` is Mojo-gated by the same
node-connect that never completes. Removing the GPU command-buffer channel did not remove the
dependency on a connected renderer.

The merged `0b8561bc` XRGB-opaque fix is correct and necessary (a CPU-raster XRGB toplevel
whose undefined alpha byte would otherwise wash white), but it only matters *once content
arrives*. Here content never arrives, so the white is empty pixels, not alpha-washed pixels —
confirmed: with the XRGB fix in place the content is still 0% blue.

## The §4 "name the exact broken verb" experiment is infeasible with this Chrome binary

`WALL7-FINDINGS.md` §4 proposed capturing the Mojo node-connect handshake
(`AcceptInvitee → AcceptBrokerClient → MergePorts/SetPeerPid`) via
`--vmodule=node_controller=3,node_channel=3,...`. Ran it two ways (`--v=1` clean, and with
`--log-level=0`): **none of those lines appear.** mojo-core's node_channel/node_controller
handshake logging is `DVLOG`, compiled out of this **release** Chromium. That is *why* no prior
run ever captured the verbs — not a `--log-level` mistake. `EnsureConnected` survives only
because it is `LOG(INFO)`, not `DVLOG`. Naming the exact step would require a Chromium build
with `dcheck_always_on`/debug logging, or `strace`/ltrace-style syscall tracing at the dd
boundary instead of Chrome-side logs.

## Fix plan (deeper than one pass; not a dd-display change)

The blocker is upstream of dd-display and upstream of any buffer sharing — it is the child's
Mojo node-connect completion. Two tracks:

1. **Engine (the real fix).** Per `WALL7-FINDINGS.md` §5.1, the highest-probability gap is an
   SCM_RIGHTS-received socket (the `AcceptBrokerClient` broker channel) whose readiness is
   never re-armed on the child's kqueue after the handle lands — the existing
   xproc-inbound/zygote-inbound/scm-futex micro-gates pass but none arms epoll on an
   *SCM_RIGHTS-received socket* and asserts a post-registration write wakes it. Build that
   micro-gate (`dd-tests/guests/ext_ipc/`); if it reproduces, the fix is in the engine's
   kqueue readiness-prime for received sockets (`dd-jit-darwin/.../syscall/event.c`). Because
   the Chrome-side handshake logs are DVLOG-stripped, drive the diagnosis from that gate and
   from syscall traces, not from Chrome VLOGs. (This is engine work; out of scope for the
   dd-display guardrail, and needs an engine rebuild + serialized live Chrome to validate.)

2. **Interim, supported.** `--single-process` renders content end-to-end (proven, `README.md`
   §3.1). It is the only working *content* path today. `--no-zygote` was tried and does not
   help (still white, renderer still doesn't connect).

## Reproduce

`target-chrome-codex/run_chrome_gpu_bounded.sh` with a short socket dir (AF_UNIX 104-char
limit — the scratchpad path is too long; use `target-chrome-codex/shmrun-*`), `DDCLI/DDJIT_DIR/
DDISP` overridden to a current-main build, and:
`CHROME_SW=1 CHROME_EXTRA_FLAGS="" CHROME_APP_URL=<blue> CHROME_WINDOW_SIZE=512,384
CHROME_KEEP_STATE=0 CHROME_DISPLAY_START_DELAY=12`.
