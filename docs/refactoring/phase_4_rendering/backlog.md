# Phase 4 rendering backlog

Status: reconciled with current main on 2026-07-13. This is a residual-only plan: completed work is absent
and remains discoverable in Git. Inventory/symbol tests are supporting evidence; closure requires Rust/C
behavior through public APIs, transport, backend, compositor, presentation or application pixels.

## Current supported baseline

- EGL 1.4 and GLES 2.0 mandatory surfaces have real implementations and behavioral coverage.
- Opt-in GLES 3.0 now has real mandatory command bodies, including object families, mapped buffers,
  instancing, first-instance and base-vertex transport/execution. GLES 3.1/3.2 remain outside the supported
  advertised profile unless separately enabled.
- Vulkan advertises 1.4 and the generated inventory reports real bodies for all 234 mandatory core commands
  across 1.0–1.4. Truthful-failure stubs remain only for unadvertised extension commands.
- CUDA Driver/Runtime shims expose classified full/partial/unsupported surfaces and execute the modeled PTX
  subset through the shared IR and software backend, including deployed-library C ABI tests.
- Shared GPU IR carries instanced/base-vertex draws, texture copy/blit/resolve and capability negotiation;
  malformed streams are rejected before backend mutation.
- Smithay composition, modern protocol groups, dmabuf-v4 feedback/generation, explicit synchronization,
  output/resource accounting and Cocoa presentation have substantial behavioral tests. The legacy
  compositor remains a fallback until live Smithay application parity is proven.

This baseline corrects two stale claims in the former ledger: instanced/base-vertex work landed in
`60ffa1ec`, and Vulkan was raised from the old reduced profile to a tested 1.4 core in `1a49beff`.

## Open rendering objectives

| ID | Residual | Current evidence | Required closure |
|---|---|---|---|
| R1 | asynchronous GPU submission completion | GL submission serials advance after synchronous submit/ack; sync objects can observe that serial, but accepted-versus-completed is not a live asynchronous executor contract | versioned accepted/completed ACKs, GL sync wait/status tests, Metal/wgpu/software parity, disconnect/device-loss behavior |
| R2 | real GLES query results | query objects enforce typed lifecycle, availability and completion; result is deliberately zero because no occlusion/transform-feedback counter reaches the backend | query IR/capability negotiation, backend query allocation/resolve, serial ownership, nonzero pixel/primitive behavioral result and unsupported-backend truthfulness |
| R3 | visible Cocoa presentation timing | command completion retains the drawable and reads measured `presentedTime`/target-screen refresh; unit tests cover ordering but not a visible device journey | bounded multi-frame visible macOS test proving monotonic presentation serial/time, plausible refresh, retry/occlusion behavior and no fabricated completion |
| R4 | supervised XWayland | feature-independent X11 window adoption/focus/input model is tested; feature-gated server/XWM code is unavailable in the default dependency set and the runtime activation point does not call `start_xwayland` | build declared feature, start/supervise Xwayland+XWM+shell, X11 render/input/clipboard/resize/close journey, crash/restart cleanup |
| R5 | default multi-process Chrome content | level-triggered epoll re-registration fix implemented; renderer-pump gate passes 50/50 and full `ext_ipc` is 114/0 across both architectures; no Chromium binary is installed on the current host for the live capstone | repeated no-argument Chrome content/input/menu/resize/recovery/shutdown journey on macOS with image digest and pixel evidence |
| R6 | default Smithay application parity | protocol/pixel units are broad, but they do not prove unmodified desktop workloads or justify deleting the legacy compositor | repeated GTK3/4, Qt5/6, SDL, GLFW, Electron, Firefox, Chrome and Vulkan application matrix with compositor/backend/profile recorded and zero required skips |
| R7 | accelerated production shader/query breadth | supported GLES2/ES3.0 and Vulkan core calls can still encounter truthful unsupported extension or backend-semantic gaps used by real applications | drive failures from application traces into narrowly advertised capability fixes; never close from export counts alone |

## Application acceptance matrix

Each run uses the normal launch command with no single-process, software, PIXMAN, X11 or tracing workaround
unless that row explicitly tests a fallback. Record image digest, guest architecture, compositor, GPU
backend, API/profile, scale, windows, screenshots and executed/skip counts.

| Class | Minimum journey |
|---|---|
| Chrome/Chromium | multi-process page content, GPU-process recovery, keyboard/pointer, menus/popups, resize/HiDPI, clean shutdown |
| GTK3/GTK4 | native Wayland accelerated window, text/widgets, popup/menu, clipboard, resize/scale and repeated open/close |
| Qt5/Qt6 | native Wayland accelerated widget/window, input, popup, clipboard and resize |
| Vulkan/Zed-style | standard loader discovers the ICD, creates window/swapchain, renders multiple frames, resizes and exits cleanly |
| SDL/GLFW | EGL/Vulkan window creation, animation/input and teardown |
| Electron/Firefox | multi-process content plus browser chrome, input, clipboard and lifecycle |
| X11-only | XWayland render, focus, keyboard/pointer, clipboard, resize and server failure handling |

## Closure rules

1. Capability claims equal the intersection of shim, IR, executor, compositor and presenter.
2. Required platform/device/tool/image absence fails preflight; tests do not return early as success.
3. C fixtures remain appropriate for public C ABI calls; orchestration/assertions are Rust or focused C.
4. Tests assert results, pixels, protocol bytes, timing or resource ownership—not implementation text.
5. A completed row is deleted from this backlog after merge. Historical hashes and narratives stay in Git,
   not in the active document.
6. Phase 2 test ownership applies: GPU core tests live in `dd-gpu`, API tests in their shim, Metal in
   `dd-gpu-wgpu`/display, compositor behavior in `dd-compositor`, and engine IPC in `dd-jit-darwin`.
