# dd Accelerated-Rendering Performance Plan + Benchmark Harness

Status: design-only (read-only analysis, 2026-07-07). Companion to `RENDERING_PLAN.md`
(the correctness/coverage roadmap). This document is the **perf** roadmap: where the frame
time goes, the ranked levers to reclaim it, and a rigorous, reproducible harness to measure
each hop. Nothing here is implemented yet — it is written so it can be executed the moment
Chrome/glmark2 paints on real Metal hardware.

Baseline reference point: `RENDERING_PLAN.md` M5 records glmark2 `build` at **~48 FPS
(Score 46)** through the full guest→host-GPU path. As §2 shows, that number is almost
certainly a **fixed-`usleep` artifact** (a 20 ms/frame sleep ≈ 50 FPS ceiling), not a GPU
limit — so the headline is: the current pipeline is nowhere near the hardware floor, and the
top levers are IPC/serialization/caching, not shader math.

---

## 1. Frame-path anatomy — one frame, end to end

The pipeline is **guest (C shim) → dd-gpu IR → `DD_GPU_EXEC` socket → dd-display Metal
executor → IOSurface → dd-display compositor → CAMetalLayer present**, spanning **two host
processes** (the engine-hosted guest, and `dd-display`) and issuing **two independent Metal
command submissions per frame** (executor render pass + compositor blit).

Trace of one `eglSwapBuffers` (file:function references are load-bearing):

| # | Hop | Where | Cost characteristics |
|---|-----|-------|----------------------|
| 1 | App issues GL calls; shim mutates state machine | `gl_shim.c` `glDrawArrays`/`glDrawElements`/`glBufferData`/`glUniform*` | cheap; buffers `malloc`+`memcpy` on `glBufferData` |
| 2 | `eglSwapBuffers` builds the **entire frame's IR** from scratch | `gl_shim.c:926 eglSwapBuffers` | re-emits **CreateBuffer+WriteBuffer for every VBO/index/texture EVERY frame** (`iu8(1)/iu8(3)` at lines ~973-998), re-emits full pipeline desc + **the MSL shader text** (`ir_shader`, line 1014), bind group, submit. For glmark2's horse this is ~1 MB `memcpy` into the static 8 MB `ir[]` buffer (line 223) per frame |
| 3 | `exec_stream` opens a **fresh unix socket**, writes `[id,w,h,len]`+IR, blocks on 1-byte ack | `gl_shim.c:674 exec_stream` | `socket()`+`connect()`+`write(hdr)`+`write(ir, irn)`+`read(ack)`+`close()` — **per frame**. `write(ir,irn)` ships the whole frame's IR (up to MBs) through the socket |
| 4 | Executor accepts, reads header+stream | `metal_backend.rs:503 run_executor` / `handle` | `read_exact` header + body |
| 5 | Executor **constructs a brand-new `MetalBackend`** | `metal_backend.rs:539 MetalBackend::new(ctx)` **inside `handle`** | **recompiles `BUILTIN_MSL`** (`newLibraryWithSource`, line 118), rebuilds depth-stencil state — **every frame**; and every resource cache (buffers/textures/pipelines/shaders/samplers) starts **empty** |
| 6 | `resolve_iosurface(id)` via mach-bridge map | `metal.rs:117` | mutex lookup + `CFRetain` |
| 7 | `replay_stream` decodes IR and drives the backend | `replay.rs:56` → `ir.rs Cmd::decode` | per-cmd `Vec` allocs (`Cmd::WriteBuffer{data: …to_vec()}`, `words()`), decode is linear |
| 8 | `create_shader` **recompiles MSL→AIR** | `metal_backend.rs:757` `newLibraryWithSource_options_error` | **the single most expensive call**; runs every frame because the guest re-sends `CreateShader(20)` every swap |
| 9 | `create_render_pipeline` **recompiles the pipeline** | `metal_backend.rs:778` `newRenderPipelineStateWithDescriptor_error` | second-most-expensive Metal call; rebuilds the `MTLVertexDescriptor` + PSO every frame |
| 10 | `create_buffer`+`write_buffer` allocate + copy | `metal_backend.rs:677/690` | fresh `newBufferWithLength` (Shared/unified) + `copy_nonoverlapping` per buffer, every frame |
| 11 | `submit`: build render encoder, draw, **`waitUntilCompleted`** | `metal_backend.rs:866` submit; **line 1032 `cmd.waitUntilCompleted()`** | CPU **blocks on GPU completion** before returning |
| 12 | Executor writes 1-byte ack | `metal_backend.rs:547` | unblocks the guest |
| 13 | Guest `wl_commit`: wayland create_params/add(fd)/create_immed/attach/damage/commit, then **`usleep(20000)`** | `gl_shim.c:697 wl_commit`; **line 718 `usleep(20000)`** | second socket (wayland), SCM_RIGHTS fd, then a **fixed 20 ms sleep** |
| 14 | Compositor `commit` → `extract` → `present` | `server.rs:433 commit` / `501 extract` | dmabuf/IOSurface path is zero-copy (`extract` returns id, no pixels); shm path does a **per-row CPU copy** (`server.rs:534-542`) |
| 15 | Compositor wraps IOSurface, **blits into CAMetalLayer drawable**, presents | `present_cocoa.rs:261 present`; `texture_from_iosurface` (zero-copy) → `copyFromTexture_toTexture` (line 296) → `presentDrawable` (298) → `commit` (299) | **a second full-frame GPU blit** + drawable present; not vsync-throttled here but `nextDrawable` can block on the drawable pool |
| 16 | Frame callback fires so the client draws next frame | `server.rs:480-483` | already implemented — but the guest **doesn't wait on it** (it sleeps instead, hop 13) |

### Copies / allocations / syscalls / round-trips per frame (the tally)

- **Socket round-trips: 2** (executor req+ack; wayland commit) + a **connect/accept/close on the executor socket every frame**.
- **Metal command submissions: 2** (executor render pass, compositor blit) → **2 GPU-completion latencies** on the critical path (hop 11 is synchronous; hop 15 presents).
- **Shader compiles: 1/frame** (hop 8) and **pipeline compiles: 1/frame** (hop 9) — both should be **0/frame after warmup**.
- **Full buffer re-uploads: N/frame** (every VBO/IBO/texture, hop 2+10) even for static geometry.
- **Large `memcpy`s: ≥3× frame-geometry-size** (shim `ir[]` encode, socket write, Metal `write_buffer`).
- **Fixed sleeps: 20 ms/frame** (hop 13) + 50 ms once at surface bring-up (`gl_shim.c:666`).
- **Backend/library reconstruction: 1/frame** (hop 5, recompiles builtin MSL).

The critical path is therefore **serial and stall-dominated**: guest encodes → blocks on
executor GPU → sleeps 20 ms → compositor GPU. CPU and GPU never overlap; two GPU submits run
back-to-back; and the two most expensive Metal calls (shader + pipeline compile) run every
single frame.

---

## 2. Ranked optimization levers

Ranked by **impact × safety**. Each lists the file:function, the mechanism, and a predicted
win. "Predicted win" is an engineering estimate to be confirmed by the §3 harness — the point
is that each is individually measurable and attributable to one hop.

### L1 — Delete the fixed per-frame `usleep`; gate on the wl frame callback. ★ highest ROI, trivial
- **Where:** `gl_shim.c:718` `usleep(20000)` in `wl_commit`, and `gl_shim.c:666` `usleep(50000)` in `surface_up`.
- **Why:** 20 ms/frame is a hard **~50 FPS ceiling** — and the recorded baseline is 48 FPS.
  The sleep is a correctness crutch (give the compositor time). The compositor **already**
  fires `wl_callback.done` on commit (`server.rs:480`); the shim just doesn't read it.
- **Fix:** replace the sleep with a real frame-callback wait: request `wl_surface.frame` and
  block (bounded) on the callback before the next frame, or run fully pipelined with
  double-buffered surfaces (see L4). Delete the 50 ms bring-up sleep once the handshake is
  ack-driven.
- **Predicted win:** removes the 50-FPS cap outright. On any scene currently near 48 FPS the
  ceiling jumps to whatever the real pipeline sustains (expected 2–4×). **Safety: high**
  (behavioral, well-scoped; guarded by the frame-callback which is already sent).

### L2 — Persist the executor `MetalBackend` across frames (per surface). ★
- **Where:** `metal_backend.rs:539`, inside `run_executor::handle` — `MetalBackend::new(ctx)`
  is constructed **per connection**, and `gl_shim.c:684 exec_stream` opens a **new connection
  per frame**.
- **Why:** every frame this (a) recompiles `BUILTIN_MSL`, (b) rebuilds the depth-stencil
  state, and — worst — (c) **throws away all resource caches**, forcing the guest to re-upload
  everything and Metal to recompile the shader+pipeline (L3/L5 only pay off once the backend
  survives).
- **Fix:** hold one long-lived `MetalBackend` keyed by IOSurface id (or one per live surface)
  in `run_executor`, and make `exec_stream` keep a **persistent socket** open for the surface's
  lifetime (frame = header+stream on the same fd; ack as now). The backend's `HashMap`s become
  the persistent resource cache.
- **Predicted win:** eliminates 1 library compile + 1 depth-state build per frame immediately,
  and unlocks L3/L5/L6. **Safety: high** (the backend is already stateful; just stop dropping it).

### L3 — Content-key the shader library and the render-pipeline state (cache, don't recompile). ★
- **Where:** `metal_backend.rs:757 create_shader` (`newLibraryWithSource_options_error`) and
  `metal_backend.rs:778 create_render_pipeline` (`newRenderPipelineStateWithDescriptor_error`).
- **Why:** these are the two heaviest Metal calls (MSL→AIR compile and PSO link; each can be
  single- to double-digit ms cold). The guest re-emits `CreateShader(20)`+`CreateRenderPipeline(30)`
  every swap (`gl_shim.c:1014-1050`), so without keying they recompile every frame **even with L2**.
- **Fix:** in the backend, hash the MSL source → cache `MTLLibrary`; hash the
  `RenderPipelineDesc` (shader ids + vertex layout + formats + topology + depth) → cache
  `MTLRenderPipelineState`. `create_shader`/`create_render_pipeline` become map hits after
  frame 1. (Optionally also let the shim skip re-emitting an unchanged shader/pipeline — but
  backend-side keying is the safe, guest-agnostic fix and also protects Chrome/ANGLE.)
- **Predicted win:** removes ~5–30 ms/frame after warmup on shader-bearing scenes; converts
  glmark2 `shading`/`build` from compile-bound to draw-bound. **Safety: high** (pure memoization;
  identical output). This is the single biggest steady-state CPU win.

### L4 — Make the executor submit asynchronous + N-buffer the IOSurface (overlap CPU/GPU). ★
> **LANDED 2026-07-08 (tearing-free, in `dd-display`).** Implemented as **async submit (drop
> `waitUntilCompleted`, ack after `commit()`) + a bidirectional cross-queue `MTLEvent` tearing fence
> keyed by IOSurface id** — *not* the vfs.c IOSurface ring. Analysis showed the ring is unnecessary:
> the only cross-frame mutable GPU resource shared executor→compositor is the IOSurface render target
> (per-frame uniform buffers get a fresh `newBufferWithLength` each frame — the old one stays alive,
> retained by the in-flight command buffer; static VBO/IBO/textures are L5-resident = read-only), so
> there is no CPU-writes-while-GPU-reads buffer hazard, and the wl-frame-callback pacing keeps the guest
> ≤1 frame ahead. Two `MTLEvent`s per surface (both queues forced onto ONE shared `MTLDevice`):
> `render_ev` (executor SIGNALs gen g at render-complete; compositor WAITs ≥g before its blit → fence a,
> never sample a half-rendered surface) and `present_ev` (compositor SIGNALs gen g at blit-complete;
> executor WAITs ≥g−1 before render g overwrites the surface → fence b, never overwrite a surface the
> compositor still reads). Deadlock-free (`≥` waits + monotonic gens + 1-frame pacing). Gated by
> `DD_RENDER_NOASYNC` (default ON). **Measured (glmark2 256×256):** build 1672→2988 FPS (+79%), texture
> 1756→3463 FPS (+97%), Score 1713→3224 (+88%); executor `gpu_us` 195µs→0 (serial GPU wait fully hidden
> behind the guest's next-frame build); compiles still 0/frame. Verified tearing-free by inspecting 25+
> consecutive PNG frames spanning build rotation, the build→texture transition, and texture rotation —
> every frame a complete, correct image. Files: `dd-display/src/metal.rs` (shared device + fence
> registry + `blit_fenced`), `metal_backend.rs` (`submit` async + fence), `present_cocoa.rs` (windowed
> path fenced). The N-buffer ring stays DEFERRED — it would only add GPU/GPU parallelism (marginal for
> GPU-bound scenes) at higher risk; the fence already delivers the CPU/GPU-overlap win safely.

- **Where:** `metal_backend.rs:1032 submit` (`cmd.waitUntilCompleted()`); `metal.rs:291 blit`
  and `291`-style waits; single-surface reuse in `vfs.c:3535 dd_gpu_alloc` (reuses ONE same-size
  surface).
- **Why:** the executor CPU-blocks on GPU completion before acking, and the guest blocks on the
  ack — so frame N+1's CPU work cannot overlap frame N's GPU work, and the two Metal submits
  (executor + compositor) run strictly serially. This is a classic "no pipelining" stall.
- **Fix:** replace `waitUntilCompleted` with `addCompletedHandler` that sends the ack (or signals
  a shared fence/timeline — the IR already has `Fence`/`WaitFence` and `CommandBuffer.signal`).
  Rotate over **2–3 IOSurfaces** per surface (extend `dd_gpu_alloc`'s registry to an N-deep ring)
  so the guest can build frame N+1 into surface B while the GPU finishes surface A. The
  compositor already tolerates a per-frame IOSurface id in the dmabuf modifier.
- **Predicted win:** up to ~2× on GPU-bound scenes (full CPU/GPU overlap) and removes one
  GPU-completion latency from the critical path. **Safety: medium** (needs fence/lifetime care —
  don't overwrite a surface the compositor is still sampling; the N-buffer ring is the guard).

### L5 — Persist guest buffers; upload only what changed (delta upload). ★
- **Where:** `gl_shim.c:926 eglSwapBuffers` (re-emits CreateBuffer+WriteBuffer for every VBO/IBO/
  texture each frame, lines ~973-998) → `metal_backend.rs:677/690 create_buffer/write_buffer`.
- **Why:** glmark2's horse re-uploads ~258 KB/attribute (~1 MB total) every frame though the mesh
  is static; textures re-upload+re-blit every frame. That's ~1 MB shim `memcpy` + ~1 MB socket
  write + fresh `newBufferWithLength` + copy, per frame, for data that never changes.
- **Fix:** give each guest buffer a **generation/content hash** in the shim; emit `WriteBuffer`
  only when it changed. Stop emitting `CreateBuffer`/`CreateTexture`/`CreateSampler`/pipeline for
  ids the host already holds (a persistent backend from L2 keeps them). Static VBOs/IBOs/textures
  then upload **once**. Requires stable ids across frames (the shim already uses fixed ids
  200+slot / 50+k / 12 — good) and the backend to not destroy them between frames (L2).
- **Predicted win:** removes ~1–2 MB/frame of encode+socket+alloc+copy on mesh/texture scenes;
  large on `build`/`texture`, negligible on `es2tri`. **Safety: medium** (must invalidate on
  `glBufferData`/`glBufferSubData`/`glTexImage2D`; get the dirty-tracking right or risk stale
  geometry).

### L6 — Collapse the two-hop present into one GPU submit (executor renders into the drawable / present without a blit).
> **LANDED 2026-07-08 (async compositor present — the dominant remaining lever).** The real gap to native was
> NOT the executor (steady state: 15µs decode/encode, 0 compiles/frame, ~1.8KB IR/frame — L2/L3/L5 all working)
> but the **compositor's per-frame `waitUntilCompleted`**: `blit_fenced` (`metal.rs`) blocked the CPU on GPU
> completion every frame, and because the wl frame-callback that paces the guest is sent *synchronously after
> `present()` returns* (`server.rs commit()`), that stall — the executor GPU render **plus** the compositor blit —
> sat on the guest's per-frame critical path. L4 only made the *executor* async; the compositor still waited.
> Fix: (a) `blit_fenced_ex(.., wait_completion)` — the steady present path now commits WITHOUT waiting (the
> cross-queue `present_ev` fence, which the executor waits on before overwriting the surface, still guarantees
> tearing-free ordering), so the frame-callback fires as soon as the blit is *encoded*; sync only on a PNG-sample
> frame (readback follows). (b) reuse a **persistent composite target per surface** (the old per-frame
> `new_bgra_texture` was a benchmark artifact the real windowed present never pays). (c) gate the per-frame
> `"executor: replayed N IR bytes"` eprintln behind `DD_DISPLAY_DEBUG` (at ~9k fps a per-frame format+`write()`
> before the ack is real latency; also cut exec replay 23µs→15µs). **Measured (glmark2 256×256, build+texture):
> Score 3290 → 9491 (2.9×); build 2984→9435 FPS, texture 3598→9549 FPS; frame 335µs→106µs — native M5 class.**
> A/B-gated by `DD_DISPLAY_SYNC_PRESENT=1` (restores the sync blit → ~3767). Horse + crate byte-correct across
> sampled PNGs. Files: `dd-display/src/metal.rs` (`blit_fenced_ex` + dst cache + `sync_present`),
> `metal_backend.rs` (gated eprintln). The IOSurface-as-layer-contents variant (removing the blit entirely) is
> DEFERRED — the async blit already overlaps on the GPU off the critical path, so it buys little now.
> Separately, `DD_GPU_BRIDGE_NAME` (env, both `metal.rs` + engine `vfs.c`, default `com.dd.display.gpu`) lets
> multiple dd-display instances coexist (the multi-agent reality) instead of colliding on the singleton mach
> service. **Remaining gap to >10k:** the `conditionals` scene is a hard outlier (74 FPS, identical sync vs async
> → GPU/shader-translation-bound, NOT present-bound); mainstream scenes are already native-class. Pushing past
> ~9.5k on the fixed-overhead scenes needs the last ~90µs (2 socket round-trips + guest IR encode under JIT +
> 15µs exec) — a physical IPC/JIT floor native lacks.


- **Where:** executor render pass (`metal_backend.rs submit`) **and** the redundant compositor
  blit (`present_cocoa.rs:296 copyFromTexture_toTexture` → `presentDrawable`).
- **Why:** the frame is rendered into an IOSurface by the executor, then **copied again** by the
  compositor into the CAMetalLayer drawable. Two processes, two GPU submits, one extra full-frame
  blit — for content that could be presented directly.
- **Fix (two options):** (a) the compositor uses the IOSurface-backed `MTLTexture` **as** the
  drawable/layer contents (`CALayer.contents = IOSurface`, or a zero-copy present) so no blit is
  needed; or (b) the executor renders **directly into the drawable's texture** for the focused
  fullscreen surface, removing the compositor Metal submit entirely. Keep the IOSurface path for
  multi-window compositing.
- **Predicted win:** removes one full-frame GPU blit + one process hop + one submit's latency.
  **Safety: medium** (touches the compositor present contract + drawable lifetime; do it after L4
  so buffering invariants are already in place).

### L7 — Replace per-frame socket connect/close (and eventually the whole-IR socket write) with the shm command ring.
- **Where:** `gl_shim.c:684 exec_stream` (`socket`/`connect`/`close` per frame; `write(fd, ir, irn)`
  ships the entire IR); `ring.rs` (the SPSC byte ring is **modeled but unused** — the file's own
  doc says the real host uses a POSIX-shm region with atomic head/tail + a futex/eventfd doorbell).
- **Why:** connect/accept/teardown per frame is pure syscall overhead; and pushing MBs of IR
  through a stream socket copies the frame twice more (guest→kernel→executor).
- **Fix:** step 1 (cheap) — keep the executor fd open for the surface's lifetime (pairs with L2).
  step 2 (deep) — realize `ring.rs` in the shared IOSurface-adjacent shm region with an atomic
  head/tail + eventfd doorbell, so IR frames are written in place and drained without a socket
  copy; this is also the substrate for **draw batching** (multiple submits coalesced before the
  doorbell). Note `ring.rs::write_bytes/read_bytes` are byte-at-a-time loops (line 41-59) — the
  real impl must use two `copy_nonoverlapping`s around the wrap.
- **Predicted win:** step 1 removes ~3 syscalls/frame; step 2 removes 1–2 MB/frame of socket copy
  and enables batching. **Safety: high for step 1, low for step 2** (shm ring + doorbell is an
  ABI change across the engine boundary — the biggest architectural item).

### L8 — Pre-size IR encoder buffers; trim per-frame re-encode; tighten decode allocs. (quick, minor)
- **Where:** `wire.rs:16 Encoder::new` (starts `Vec::new()`, grows by `extend_from_slice` per
  field); `ir.rs Cmd::decode` (`WriteBuffer{ data: d.bytes()?.to_vec() }` copies, `words()` allocs).
- **Why:** small constant-factor churn; dwarfed by L2–L5 but free once those land.
- **Fix:** `Encoder::with_capacity` sized from the previous frame; where the executor decodes,
  borrow the ring bytes instead of `to_vec()` for `WriteBuffer` (decode→`write_buffer` can take a
  `&[u8]` slice of the ring). With L5 most descriptors stop being re-encoded at all.
- **Predicted win:** a few %; mainly reduces allocator pressure/jitter (helps p99). **Safety: high.**

### Lever summary (impact × safety)

| Lever | Hop attacked | Predicted steady-state win | Safety | Class |
|-------|-------------|----------------------------|--------|-------|
| L1 delete usleep / frame-callback | 13 | lifts 50-FPS cap (2–4×) | high | quick |
| L2 persist backend + socket | 5 | removes lib compile + unlocks L3/L5 | high | quick |
| L3 cache shader lib + PSO | 8,9 | −5..30 ms/frame after warmup | high | quick |
| L4 async submit + N-buffer | 11 | up to 2× (CPU/GPU overlap) | medium | deep |
| L5 buffer persistence / delta upload | 2,10 | −1..2 MB/frame on mesh/tex | medium | deep |
| L6 collapse two-hop present | 15 | −1 blit, −1 hop | medium | deep |
| L7 persistent fd (→ shm ring) | 3 | −3 syscalls/frame (→ −MB copy, batching) | high / low | quick / deep |
| L8 encoder capacity / decode borrow | 2,7 | few %, p99 jitter | high | quick |

---

## 3. Benchmark harness design

Goal: measure the pipeline **rigorously, reproducibly, and per-hop**, so every lever's win is
attributable to a specific hop and regressions (a recompile creeping back in) are caught.

### 3.1 Metrics

Primary:
- **FPS** (frames presented / wall-second, steady-state window after warmup).
- **Frame time p50 / p95 / p99** (ms) — from a per-frame present timestamp; p99 exposes the
  jitter L8/L4 target.
- **Per-hop frame-time breakdown** (see §3.3): guest-GL+encode → socket → executor-decode →
  Metal-build → GPU-exec → ack → wl-commit → present.

Secondary (per-frame counters, the "did the cache work?" signals):
- **IR bytes/frame** (`irn` at end of `eglSwapBuffers`).
- **Draw calls/frame** and **buffers/bytes uploaded/frame** (should collapse to ~0 after warmup with L5).
- **Socket round-trips/frame** and **connects/frame** (→ 0 connects with L2/L7).
- **Shader compiles/frame** and **pipeline compiles/frame** — **must be 0 after frame 1** post-L3;
  this is the key steady-state regression guard.
- **Present latency** = present-done − first-GL-of-frame (end-to-end, crosses both processes).

### 3.2 Workloads

Chosen to isolate different hops:

| Workload | Guest | Isolates |
|----------|-------|----------|
| `es2tri` | `dd-tests/guests/es2tri.c` | **fixed per-frame overhead floor** (1 triangle, no upload) — pure L1/L2/L3/L7 signal |
| `es2tex` | `dd-tests/guests/es2tex.c` | texture upload + indexed draw (L5 texture path) |
| `es2depth` | `dd-tests/guests/es2depth.c` | depth-attachment path + depth-state |
| `es2uniform` | `dd-tests/guests/es2uniform.c` | uniform-buffer churn |
| glmark2 `-b build` | stock aarch64 glmark2 | large static VBO (horse ~21.5k vtx) → **L5 delta-upload** dominates |
| glmark2 `-b texture` | stock aarch64 glmark2 | textured cube → texture reuse + sampling |
| glmark2 `-b shading` | stock aarch64 glmark2 | heavier fragment shader → **L3 shader-compile** + GPU-fill |
| **stress-draws** (new micro-guest) | many small draws/frame, tiny buffers | **per-draw + per-IR-op overhead** (encode/decode, submit) independent of vertex count |
| **stress-verts** (glmark2 build already serves) | one huge draw | upload + GPU vertex throughput |

The two stress axes matter: `stress-draws` isolates per-op cost (encode/decode/submit/round-trip)
while `build` isolates per-byte upload cost — they move under different levers.

### 3.3 Per-hop instrumentation — proposed `DD_RENDER_PROF` (design, not implemented)

The path crosses two processes, so a single in-process timer can't see it. Design a **per-frame
timestamp ledger keyed by a frame sequence number**, sampled at each hop, joined offline.

- **Clock:** `CLOCK_MONOTONIC` in the guest (C shim) and `mach_absolute_time()` in `dd-display`
  (macOS). Both are cheap (~20 ns). Emit **raw timestamps**, not deltas, so cross-process joins
  are exact; convert to a common timebase in the post-processor (record each process's
  `mach_timebase`/clock epoch once at startup).
- **Frame id:** the shim already has an implicit frame counter; make it explicit and **carry it
  in the executor header** (extend the 16-byte header `[id,w,h,len]` with a `u32 frame_seq`, or
  reuse the surface id + a monotonic counter). The compositor tags its present with the same seq
  (the dmabuf modifier / IOSurface id can carry it under an N-buffer ring, L4).
- **Gate:** env `DD_RENDER_PROF=1` (guest) / `DD_RENDER_PROF=1` (dd-display). Zero cost when
  unset — mirror the existing `DD_SHIM_DEBUG`/`DD_DISPLAY_DEBUG` `getenv`-once pattern
  (`server.rs:16 dbg_on`). When set, append one CSV line per frame per process to
  `$DD_RENDER_PROF_DIR/{shim,exec,comp}-<pid>.csv`.

Hop sample points (tag → where to stamp):

| Tag | Timestamp point | File:line to instrument |
|-----|-----------------|-------------------------|
| `t_gl0` | first GL call of the frame (or `eglSwapBuffers` entry) | `gl_shim.c:926` |
| `t_enc` | IR encode complete (`irn` known) | `gl_shim.c:1102` (before `exec_stream`) |
| `t_sock_w` | after `write(fd, ir, irn)` | `gl_shim.c:691` |
| `t_exec_rx` | executor finished `read_exact(body)` | `metal_backend.rs:530` |
| `t_decode` | `replay_stream` returned (all cmds decoded+applied to encoder) | `metal_backend.rs:541` |
| `t_build` | just before `cmd.commit()` | `metal_backend.rs:1031` |
| `t_gpu` | inside `addCompletedHandler` (GPU done) — after L4; today = after `waitUntilCompleted` | `metal_backend.rs:1032` |
| `t_ack` | guest after `read(&ack)` | `gl_shim.c:693` |
| `t_commit` | after wayland commit sent | `gl_shim.c:713` |
| `t_present` | compositor after `presentDrawable`/`commit` | `present_cocoa.rs:299` |

Derived per-hop times (the breakdown): `encode = t_enc−t_gl0`, `socket_up = t_exec_rx−t_sock_w`,
`decode = t_decode−t_exec_rx`, `metal_build = t_build−t_decode`, `gpu_exec = t_gpu−t_build`,
`ack_rtt = t_ack−t_gpu`, `commit = t_commit−t_ack`, `present = t_present−t_commit`,
**end_to_end = t_present−t_gl0**. Counters (`ir_bytes`, `draw_calls`, `bufs_uploaded`,
`bytes_uploaded`, `shader_compiles`, `pipeline_compiles`, `connects`) ride on the same CSV line
from whichever process owns them (compiles/uploads from the executor; ir_bytes/draw_calls/connects
from the shim).

A tiny post-processor (`dd-tests/src/bin/render_prof.rs`, or a shell+awk) joins the three CSVs on
`frame_seq`, drops warmup frames, and prints the p50/p95/p99 table + a stacked per-hop bar (text).

Cheaper first cut (before the cross-process ledger lands): the existing `DD_SHIM_DEBUG` /
`DD_DISPLAY_DEBUG` traces already stamp key events; a one-off run with those on + `ts`-style
wall-clock prefixes gives a coarse breakdown to validate the hop model. `DD_RENDER_PROF` is the
rigorous, low-overhead, always-available replacement.

### 3.4 Reproducibility / methodology

- **Warmup + steady state:** discard the first K frames (default K=30 — the first frame pays all
  compiles/uploads; the harness must separate cold from steady, exactly as `bench.rs` discards a
  warm-up lane, `dd-tests/src/bin/bench.rs:17`). Report cold-frame time separately (it's the
  compile-cost metric).
- **N repetitions, median:** follow the existing `BENCH_N` median-of-N convention
  (`bench.rs:145`); env `RENDER_BENCH_N` (default 5), report median + p99 across runs.
- **Fixed frame count / duration:** run each workload for a fixed N frames (glmark2 `--benchmark
  <scene>:duration=…`), not wall-time, so scheduling noise doesn't change the sample set.
- **Pin the surface size** (e.g. 800×600 and 1920×1080 as two points) — fill-bound scenes scale
  with pixels; report both so GPU-floor vs overhead is separable across resolution.
- **Isolation:** reap orphaned engines between runs (see memory: mac-bridge orphans pile to 100%
  CPU and skew results); run the executor + compositor fresh per workload.

### 3.5 Four-way comparison + attributing overhead vs. the GPU floor

Run each workload through four configurations and compare the §3.1 metrics:

1. **Software backend** (`software.rs SoftwareBackend`) — CPU executor, no Metal. Establishes a
   correctness reference and a *lower bound on transport overhead* (no GPU latency at all, so its
   frame time ≈ encode+socket+decode+CPU-raster; subtract to isolate the pure IPC cost).
2. **dd Metal — current** (baseline; all levers off).
3. **dd Metal — optimized** (levers under test, toggled individually via env flags so each lever's
   delta is attributable — e.g. `DD_NO_PSO_CACHE=1` to A/B lever L3, mirroring the engine's
   `NOXALUFLAGELIDE`-style A/B gates in the perf memory).
4. **GPU floor / "engine-only" reference** — the same MSL pipeline + geometry driven in a **tight
   in-process loop with no IPC, no compositor, no per-frame compile** (extend the existing
   `selftest-replay`/`selftest-shim-ir` harness, `metal_backend.rs:261/566`, to loop M times and
   time only `submit`). This is the theoretical hardware ceiling for that content on this Mac.
   Optionally add a **native reference**: stock glmark2 on the same Mac via real GL→Metal (or
   Apple's Metal sample) for an external sanity anchor.

**Attribution:** `overhead = measured_frame_time(config 2/3) − GPU_floor(config 4)`. Break the
overhead down with §3.3 so it's assigned to hops: transport (socket+decode), CPU-compile
(shader+PSO — should vanish under L3), upload (should vanish under L5), serialization (the gap
that L4 closes = `gpu_exec` that overlaps nothing), and present (the second blit L6 removes).
The success criterion per lever is: its targeted hop's contribution to `overhead` drops toward
zero without moving the others.

---

## 4. Quick-wins vs. deep-wins split

**Quick wins — land first, measure immediately (small, safe, high ROI):**
- **L1** delete the 20 ms `usleep`, gate on the wl frame callback (lifts the 50-FPS cap).
- **L2** persist the executor `MetalBackend` + keep the executor socket open per surface.
- **L3** content-key the `MTLLibrary` + `MTLRenderPipelineState` caches (compiles → 0/frame).
- **L7 step 1** persistent executor fd (no connect/close per frame).
- **L8** `Encoder::with_capacity` + borrow-don't-copy `WriteBuffer` decode.
- **`DD_RENDER_PROF`** instrumentation (additive; needed to prove all the above).

These are contained to `run_executor`/`create_shader`/`create_render_pipeline` (metal_backend.rs),
`exec_stream`/`wl_commit` (gl_shim.c), and `wire.rs` — no cross-boundary ABI change. Order:
land `DD_RENDER_PROF` → L1 → L2 → L3 → L7.1 → L8, re-measuring after each so the per-hop table
shows each hop collapse.

**Deep wins — architectural, stage after the quick wins (bigger blast radius):**
- **L4** async submit (`addCompletedHandler`/fence) + N-buffered IOSurface ring in `dd_gpu_alloc`
  (`vfs.c:3535`) — real CPU/GPU pipelining; needs lifetime/fence discipline.
- **L5** guest buffer persistence + delta upload (shim dirty-tracking + host cache from L2) — the
  big win on mesh/texture scenes; needs correct invalidation.
- **L6** collapse the two-hop present (executor→drawable, or IOSurface-as-layer-contents) — removes
  a whole GPU submit + process hop; touches the compositor present contract.
- **L7 step 2** realize the `ring.rs` SPSC transport in shm with an eventfd doorbell (replace the
  socket) — the substrate for **draw batching / submit coalescing**; the largest change (engine↔
  dd-display ABI).

Sequencing rationale: the quick wins remove the *artificial* ceilings (sleep, per-frame compiles,
per-frame backend churn) and give the harness a clean signal; only then do the deep wins (overlap,
delta-upload, single-present, shm-ring) push toward the GPU floor measured in §3.5 config 4.

---

## 5. One-line takeaways

- The current ~48 FPS is a **20 ms/frame `usleep` artifact**, not a GPU limit (**L1**).
- The executor **recompiles the shader + pipeline and rebuilds the whole backend every frame**
  (**L2/L3**) — the dominant steady-state CPU cost.
- **Nothing overlaps**: guest → (block) executor GPU → (sleep) → compositor GPU, two submits
  serial (**L4/L6**).
- **Static geometry re-uploads every frame** (**L5**), and IR crosses a **fresh socket per frame**
  (**L7**).
- Measure it with a **cross-process, frame-seq-keyed `DD_RENDER_PROF` ledger** and a **four-way
  compare against an in-process GPU-floor loop**, so every lever's win lands on a named hop.
