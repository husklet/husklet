# Rendering — detailed implementation plan (the next plan)

Companion to [`RENDERING.md`](RENDERING.md) (the architecture). That doc says *what* the display layer
is; this one is the *how* — the DDP wire spec, exact API sequences, per-rung steps, the seam edits, and
the validation order. Grounded in the dd source and a verified technical research pass.

Status: **M0 landed + M1 core landed (software path proven end-to-end, incl. on real macOS).** Locked
decisions (from `RENDERING.md`): unified internal **DDP**; separate host renderer `dd-display`; **GPU
rung 3 = Vulkan-level forwarding** committed; MVP = `weston-simple-shm` → SDL2.

> **Progress (2026-07):**
> - **M0 seam — DONE, gate-green.** `memfd_create` (already impl, `rare.c` case 279) verified; new
>   `wlshm_pool` gate test proves the full CPU foundation: `memfd → ftruncate → mmap(MAP_SHARED) →
>   SCM_RIGHTS fd-pass → cross-process mmap → bidirectional page coherency` (byte-exact, both Linux
>   engines; engine gate **1629/0**). AF_UNIX Wayland routing implemented as a **`--gui`-gated
>   bind-mount + env** in `dd-cli` (mirrors `docker_sock`/nvml — mount-not-bake), incl. optional
>   dd-provided client-lib injection from `~/.dd/gui/<arch>/lib/`.
> - **`dd-display` — NEW crate, builds on Linux (portable core) AND macOS (Cocoa backend).** Decision:
>   **hand-rolled the minimal Wayland wire** instead of Smithay (Smithay drags drm/gbm/libinput/udev/
>   rustix-Linux deps that don't build on macOS). Implements the four core globals + `xdg_shell` +
>   `wl_shm`, enough for `weston-simple-shm`. Cocoa presenter uses the **already-vendored objc2 stack**
>   (no new crate fetch): `NSWindow` + `NSImageView`/`NSBitmapImageRep` blit of the committed buffer.
> - **Software path PROVEN headlessly** (no GPU/eyes): a real-socket `dd-display selftest` composites
>   `weston-simple-shm`'s `(x^y)` pattern → PNG, **pixel-identical on Linux and on real macOS**
>   (retires validation risk #2 on the target OS).
> - **On-screen presenter PROVEN on macOS** (`dd-display selftest-cocoa`): renders the live `NSView`
>   via AppKit's own `cacheDisplayInRect:` → PNG (2× Retina). The window's actual draw path works;
>   "needs the user's eyes" is essentially retired.
> - **REAL `weston-simple-shm` END-TO-END — TRUE M1 first-pixels-from-a-guest. ✅** A stock
>   `ubuntu:24.04` container under the JIT runs the genuine `weston-simple-shm` (libwayland), sourced
>   as **bind-mounts** (`libwayland-client.so.0` + `libffi.so.8` + the binary from Ubuntu debs, staged
>   under `~/.dd/gui/<arch>/{lib,bin}/`, mount-not-bake). It connects over the bind-mounted socket
>   (AF_UNIX routing works), `memfd`-backs its `wl_shm` pool, passes the fd via `SCM_RIGHTS`, and
>   `dd-display` composites its iconic concentric-circles animation → PNG on macOS. Fixed one real bug:
>   version-gate `wl_seat.name`/`wl_output.done` (older-version clients abort otherwise).
> - **Metal present path — DONE + PROVEN (hardware-acceleration foundation).** `dd-display/src/metal.rs`
>   adds `MetalCtx` (the shared system `MTLDevice` + `MTLCommandQueue`). The display path uploads a
>   committed `wl_shm` buffer as a `BGRA8Unorm` `MTLTexture` (`replaceRegion:`) and GPU-blits it.
>   Two proofs: (a) `dd-display selftest-metal` uploads→GPU-blits→**reads back** a frame on the real
>   GPU → PNG, pixel-identical to the CPU path (`target-mac/metal-frame.png`); (b) the live
>   `CAMetalLayer`+drawable present (`--metal`, `MetalPresenter` replacing the `NSImageView` copy-blit)
>   ran **stable for 20 s streaming the real `weston-simple-shm`** through `nextDrawable`/
>   `presentDrawable` (no crash). **dd-gpu seam:** `MetalCtx::from_device` lets `dd-gpu`'s `MetalBackend`
>   share this one device/queue (see `dd-gpu/src/backend.rs`), so a guest-rendered texture/IOSurface
>   composites with no cross-device copy (GPU rung 2). NEXT: `MTLBuffer(bytesNoCopy)` over the pool pages
>   (zero-copy upload) + IOSurface-backed dmabuf.
> - **SDL2 & GTK3 (heavy toolkits) — characterized, deferred (over-linked stock builds).** Both stock
>   Ubuntu SDL2 (2.0.10 *and* 2.30, the latter also `BIND_NOW`) hard-`NEEDED`-link `libwayland-egl` +
>   the full **X11 + ALSA + Pulse (+ libdrm/gbm/libdecor)** stack (~22 libs) at load — empty-SONAME
>   stubs can't satisfy them (address-taken symbols + `BIND_NOW` force eager resolution). `libgtk-3` is
>   deeper still: 23 direct `NEEDED` → ~40-lib closure + runtime data (compiled gsettings schemas,
>   fontconfig+fonts, gdk-pixbuf loaders). **The compositor's `wl_shm` path is NOT the blocker** (weston
>   proves a real libwayland client draws through it). The clean path for heavy toolkits is to let the
>   **image provide the toolkit** (its own `apt`/pre-baked closure) while dd provides only the socket +
>   `libwayland-client` — hand-staging a 22–40-lib closure per toolkit is the wrong layer. (Interim SDL2
>   option if wanted: a custom `--disable-video-x11 --disable-audio` SDL2 build; GTK3 is more
>   Chrome-relevant but needs the image to carry GTK.) This answers validation risk #1.

---

## 1. DDP — the unified wire protocol (concrete v0)

A small length-prefixed message protocol over a per-container `AF_UNIX` `SOCK_STREAM` socket, plus
out-of-band buffer handles (an fd via `SCM_RIGHTS` for shm; a mach-port name for IOSurface). One
`wl_display`/DDP endpoint per container (isolation). Frontend = client of the container; `dd-display` =
server. All ints little-endian; ids are u32; coordinates are i32 in surface-local logical pixels;
scale is a u32 (×120, like `wl_pointer` axis_value120) to avoid floats on the wire.

**Frontend → host (window/surface):**
| msg | fields | notes |
|---|---|---|
| `SURFACE_CREATE` | `sid`, `role`(toplevel/popup/cursor/subsurface), `parent_sid` | |
| `SURFACE_TITLE` | `sid`, `utf8[]` | xdg_toplevel.set_title |
| `SURFACE_GEOMETRY` | `sid`, `x,y,w,h`, `scale120` | window geometry hint |
| `SURFACE_MINMAX` | `sid`, `min_w,min_h,max_w,max_h` | constraints |
| `BUFFER_ATTACH` | `sid`, `buf_kind`(SHM\|IOSURFACE), `w,h,stride,format`(ARGB8888/XRGB8888/BGRA), `offset` | fd or mach-port arrives OOB, correlated by a `buf_seq` |
| `SURFACE_DAMAGE` | `sid`, `x,y,w,h` (repeatable) | dirty rects |
| `SURFACE_COMMIT` | `sid`, `buf_seq`, `frame_token` | atomic apply + request frame callback |
| `SURFACE_DESTROY` | `sid` | |
| `CURSOR_SET` | `sid`, `hotspot_x,hotspot_y` | pointer cursor surface |

**Host → frontend (events + lifecycle):**
| msg | fields | notes |
|---|---|---|
| `FRAME_DONE` | `sid`, `frame_token` | vsync pacing / release prev buffer |
| `BUFFER_RELEASE` | `buf_seq` | client may reuse the buffer |
| `CONFIGURE` | `sid`, `w,h`, `states`(maximized/activated/resizing/fullscreen) | xdg_toplevel.configure |
| `CLOSE` | `sid` | user hit the red button |
| `POINTER_ENTER/LEAVE` | `sid`, `x,y` | focus follows window |
| `POINTER_MOTION` | `sid`, `x,y` (wl_fixed→i32.8) | |
| `POINTER_BUTTON` | `sid`, `button`(BTN_LEFT/RIGHT/MIDDLE), `state` | evdev button codes |
| `POINTER_AXIS` | `sid`, `axis`(vert/horiz), `value120`, `source`(wheel/finger/continuous), `momentum` | precise + momentum |
| `KEY` | `sid`, `evdev_keycode`(already −8-normalized? **no: send raw evdev; frontend adds +8**), `state` | |
| `MODIFIERS` | `depressed,latched,locked,group` | drives `xkb_state_update_mask` |
| `KEYMAP` | size; fd OOB | once per seat; mmap'd `XKB_KEYMAP_FORMAT_TEXT_V1` |

DDP carries **either** an shm fd **or** an IOSurface mach-port as the buffer; everything else is
identical, so the software and GPU paths differ only in `buf_kind`. The Wayland frontend produces DDP
from `wl_*`/`xdg_*`; the Cocoa frontend produces DDP `SURFACE_*` + `IOSURFACE` from adopted windows.

---

## 2. Seam changes in the Linux personality (`os/linux/`)

Exact, code-grounded edits (line refs against current `service.c`).

### 2.1 `memfd_create` (aarch64 nr 279) — REQUIRED for `wl_shm`
Today unhandled → `default: -ENOSYS` (`service.c:2359`). Add a case. Recipe (the canonical portable
`os_create_anonymous_file` fallback, since macOS has no memfd):
```c
case 279: { // memfd_create(name, flags)
    char nm[31];                                 // macOS PSHMNAMLEN ~31 — keep it short
    snprintf(nm, sizeof nm, "/dd%d.%u", getpid(), g_memfd_seq++);
    int oflag = O_RDWR | O_CREAT | O_EXCL;        // (no O_CLOEXEC on shm_open path; set via fcntl)
    int fd = shm_open(nm, oflag, 0600);
    if (fd < 0) { G_RET(c) = (uint64_t)(-m2l_errno(errno)); break; }
    shm_unlink(nm);                              // anonymous: lives as long as fd/mapping
    if ((int)a1 & 1 /*MFD_CLOEXEC*/) fcntl(fd, F_SETFD, FD_CLOEXEC);
    // ignore MFD_ALLOW_SEALING (no F_ADD_SEALS on macOS; toolkits tolerate its absence)
    G_RET(c) = (uint64_t)fd; break;
}
```
The returned fd is `ftruncate`-able and `mmap(MAP_SHARED)`-able (both already handled: `ftruncate`
case 46; `mmap` case 222 → `mmap_flags()` maps `MAP_SHARED`=0x01). **This single syscall unlocks
`wl_shm`.** Validate: a guest creates an fd, `ftruncate`s it, `mmap`s it shared, writes; a second
`mmap` of the same fd sees the bytes.

### 2.2 AF_UNIX Wayland socket reaches `dd-display`
Real `sun_path` `bind`/`connect` currently passes **raw** to the host (`service.c:1899/1950`), not
jail-rewritten. MVP (no personality code): the daemon **bind-mounts** the host DDP socket into the
rootfs and sets `WAYLAND_DISPLAY`/`XDG_RUNTIME_DIR` so the guest's `sun_path` *is* the mount target →
the raw `connect` hits the real socket. Hardening later: jail-translate `sun_path` through the VFS so
arbitrary guest unix sockets stay confined and the socket can live anywhere.

### 2.3 `SCM_RIGHTS` — verify, likely no code
`sendmsg/recvmsg` (cases 211/212) pass ancillary data opaque and guest-fd == host-fd, so a passed shm
pool fd already crosses. **Test** that the fd survives and the receiver `mmap`s it at the right size
(M0 acceptance).

### 2.4 GPU (rungs 2–3) — later
`/dev/dri/renderD128` synthesis (extend the `/dev` handler in `openat` case 56), the Venus **vtest**
socket transport (a new fd kind), and IOSurface-backed memory. §6.

---

## 3. Daemon changes (`dd-jit` `SpawnConfig` + `dd-daemon`)

`SpawnConfig` already injects `env: Vec<(String,String)>` straight into the launch script and
`volumes` via `DDVOL=container:host,...` (`lib.rs`). So:
```rust
// gate on a --gui label / flag
cfg.env.push(("WAYLAND_DISPLAY".into(), "wayland-0".into()));
cfg.env.push(("XDG_RUNTIME_DIR".into(), "/run/user/0".into()));
cfg.volumes.push(Volume { container: "/run/user/0/wayland-0".into(),
                          host: format!("/run/dd/{ctr}/wayland-0") });
```
`dd-daemon` lazily spawns one shared `dd-display` for the session and creates the per-container socket
under `/run/dd/<ctr>/`. Headless containers set no GUI flag and are unaffected.

---

## 4. The renderer `dd-display` (host process, Rust)

Structure (following cocoa-way, corrected):
- **Event loop = winit.** Owns the AppKit main thread; create windows on `Resumed` with
  `MainThreadMarker`. Drive Smithay client dispatch from `Event::AboutToWait`:
  `display.dispatch_clients(&mut state)?; display.flush_clients()?;`.
- **Smithay = protocol only.** Use `wayland::compositor`, `shell::xdg`, `wl_shm`, `seat`, `output`;
  **do not** pull drm/gbm/libinput/udev/libseat. Inject input into the seat manually
  (`keyboard.input(...)`, `pointer.motion(...)`) from winit events. One `Display`/socket per container.
- **Per surface → one `NSWindow` + `CAMetalLayer`** (`BGRA8Unorm`), rootless, with a bidirectional
  `sid ↔ NSWindow` map (quartz-wm's model).
- **Thread rule:** all CoreAnimation/Metal calls on the **main thread** (CA/Metal not thread-safe);
  marshal with `dispatch_sync` if needed.

### 4.1 shm present (MVP path, then zero-copy)
Per `SURFACE_COMMIT` with an SHM buffer:
- **MVP (copy):** `mmap` the pool fd once (`MAP_SHARED`, keyed by pool); on commit, `texture.replace
  (region:mipmapLevel:withBytes: base+offset, bytesPerRow: stride)` into a `BGRA8Unorm` texture sized
  to the surface; handle **stride ≠ w·4** via `bytesPerRow`, and **ARGB↔BGRA** channel order. Then
  `layer.nextDrawable` → blit → `presentDrawable` → `commit`.
- **Zero-copy (later):** wrap the pool directly:
  `device.makeBuffer(bytesNoCopy: base, length: roundUp(size, 16384), options: .storageModeShared,
  deallocator: { munmap })`. **Hard constraints:** base **16 KB page-aligned** (mmap base is), length a
  **page multiple**, memory in a **single VM region**. Then a blit/compute pass to the drawable.

### 4.2 IOSurface present (rungs 2–3)
`device.makeTexture(descriptor:, iosurface:, plane: 0)` → an `MTLTexture` aliasing the IOSurface, zero
copy; set as a layer's content or blit to the drawable. IOSurface pixel format **`'BGRA'`** ↔ Metal
`BGRA8Unorm`. Use `IOSurfaceAlignProperty()` for stride/allocSize (don't hand-compute). Cross-process
handoff: producer `IOSurfaceCreateMachPort(s)` → send the port name in DDP → `dd-display`
`IOSurfaceLookupFromMachPort(port)`; **`mach_port_deallocate()` on both sides or the surface leaks.**
Never `kIOSurfaceIsGlobal` (deprecated, insecure).

### 4.3 input (NSEvent → DDP → xkb)
- **Keymap:** build once with xkbcommon (`xkb_keymap_new_from_names` → `xkb_keymap_get_as_string
  (XKB_KEYMAP_FORMAT_TEXT_V1)`), write to an fd, send via DDP `KEYMAP`; the Wayland frontend hands it to
  `wl_keyboard.keymap` and the client `mmap`s it.
- **Keycodes:** map `NSEvent.keyCode` (`kVK_*`/`CGKeyCode`) → Linux `KEY_*` (evdev). **Send raw evdev
  in DDP; the Wayland frontend feeds xkbcommon with `+8`** (X11 reserves 0–7). The `kVK_*→KEY_*` table
  is **unbuilt** — port from XQuartz `quartzKeyboard` / SDL cocoa.
- **Modifiers:** `NSEventModifierFlags` → `depressed/latched/locked/group` → DDP `MODIFIERS` →
  `xkb_state_update_mask`. Decide Command/Option semantics (map ⌘→Super or Ctrl).
- **Pointer/scroll:** buttons → `BTN_LEFT/RIGHT/MIDDLE`; `scrollingDeltaX/Y` +
  `hasPreciseScrollingDeltas`/momentum → `wl_pointer` axis `value120` + `axis_source` (wheel vs finger)
  + momentum phase. Mapping is **unbuilt**; HiDPI via the window backing scale.

---

## 5. macOS-guest frontend (`os/darwin/display`)

darwinjail runs the guest under the **real host dyld + frameworks**, so its `NSWindow`/`CAMetalLayer`/
IOSurface are genuine host objects in-process. The frontend **adopts** them rather than translating
pixels:
- Interpose/swizzle `NSWindow`/`NSApplication`/`CAMetalLayer`/`CALayer`/`IOSurface` (darwinjail already
  DYLD-`__interpose`s libSystem — extend to AppKit/QuartzCore).
- For each guest top-level: create a `CAContext`, host the window's layer tree, export its
  **`CAContextID`** → emit DDP `SURFACE_CREATE` + the contextID; `dd-display` renders it via a
  **`CALayerHost`** in an NSView (`wantsLayer=YES`). Pixels stay in an IOSurface (its ID/port in DDP).

- Route DDP input back into the guest's event queue; relocate/label/isolate windows in `dd-display`'s
  space. Full AppKit fidelity is a long tail — start with one toplevel + its IOSurface + lifecycle.
- **Risk:** the `contextId`↔windowserver linkage is undocumented; `CAPortalLayer` (private) is the
  fallback. Validate early (§7).

---

## 6. `dd-gpu` — Vulkan command forwarding to host Metal (rung 3)

The committed efficient-rendering path. Forward at the **Vulkan** level (SPIR-V end-to-end; NIR→MSL
once, host-side), reusing Venus.

**Guest side (Linux container):**
- Ship Mesa's **Venus** Vulkan ICD in the guest image; select it with **`VN_DEBUG=vtest`** so its
  transport is the **vtest Unix-socket protocol**, not virtgpu — **no real `/dev/dri` needed** for the
  command path. (GL apps: **Zink → Venus**; `MESA_LOADER_DRIVER_OVERRIDE=zink`,
  `EGL_PLATFORM=surfaceless`.)
- Some clients still probe `/dev/dri` — synthesize a node if needed (Zink "Penny"/`VK_KHR_surfaceless`
  brings up Zink with no DRI/DRM).

**Host side (`dd-display` or a sibling `dd-gpu` service it owns):**
- Run the **vtest server with the Venus backend** (virglrenderer `-Dvenus=true`,
  `virgl_test_server --venus`) replaying serialized Vulkan onto a **host Vulkan driver = KosmicKrisp**
  (Apple Silicon/macOS 26+) **or MoltenVK** (broader), runtime-selected. Borrow **gfxstream's**
  ring-buffer + **1:1 encoder/decoder threading**.
- **Re-implement Venus's memory model for no-VM macOS:** Venus assumes virtio-gpu **blob resources** +
  `VK_EXT_image_drm_format_modifier` + dma-buf/`KVM_SET_USER_MEMORY_REGION` host-visible mapping —
  **none exist here.** Map instead onto: host-visible memory = shared shm regions (we already share
  memory); presentable render targets = **IOSurface-backed `MTLTexture`s** (§4.2), so a guest swapchain
  image *is* the thing `dd-display` composites — zero readback. **This memory-model port is the single
  largest and riskiest piece of the project.**

**The fork, decided:** Vulkan-level, because (a) SPIR-V passes through untranspiled (near-native), (b)
there is **no mature serialization protocol at the Metal level**, (c) the host NIR→MSL happens once in
KosmicKrisp. The most reusable transport is virglrenderer `vtest --venus` over a Unix socket.

**Unknown to spike:** does **virglrenderer build on macOS** with venus + a Metal-backed host Vulkan?
The vtest/venus path is proven on Linux; this port is unproven. Keep **llvmpipe** (software GL, runs on
arm64) as the standing correctness fallback throughout.

---

## 7. Milestones + acceptance tests

- **M0 — seam. ✅ DONE.** `memfd_create` verified; `wlshm_pool` gate test proves memfd→shm→SCM_RIGHTS→
  cross-proc mmap coherency (1629/0, both Linux engines). AF_UNIX routing = `--gui` bind-mount+env in
  `dd-cli` (`ddjit_launcher.rs`, `Workspace::gui`). Retires risk #2 (also re-proven on real macOS via
  `dd-display selftest`).
- **M1 — first pixels. CORE DONE (software path); on-screen window pending user's eyes.** Hand-rolled
  minimal `dd-display` (wl_compositor/shm/xdg_wm_base/wl_seat/wl_output) — Smithay rejected (Linux-only
  deps won't build on macOS). Committed `wl_shm` buffer → tight BGRA framebuffer → PNG (headless, both
  OSes, pixel-identical) **and** → `NSWindow`+`NSImageView` (Cocoa presenter, builds+runs on macOS).
  *Remaining:* (a) confirm the NSWindow visually (needs user — TCC blocks capture); (b) drive a REAL
  guest `weston-simple-shm` over the bind-mounted socket (stage libwayland-client + the binary as
  bind-mounts per the mount-not-bake rule); (c) then SDL2 (`SDL_VIDEODRIVER=wayland`); (d) zero-copy
  `MTLBuffer(bytesNoCopy)` upgrade over the current `NSImageView` copy-blit.
- **M2 — input. INPUT DONE (headless-verified); lifecycle pending.** `dd-display` now advertises a real
  `wl_seat` (pointer+keyboard caps), creates `wl_pointer`/`wl_keyboard`, sends a valid XKB `wl_keyboard.
  keymap` over an anon fd, and routes pointer enter/motion/button/axis + keyboard enter/key/modifiers to
  the focused surface (`server.rs`). Verified by a headless assert-test (`input_events_route_to_the_
  focused_surface`) that drives the seat and checks exact wire contents (surface id, wl_fixed coords,
  `BTN_LEFT`, `KEY_A` evdev code, keymap fd mmaps to `xkb_keymap`). The mac `NSEvent → seat` wiring
  (`present_cocoa.rs`, `kVK_*`→evdev map) is built (needs interactive verification). *Remaining:*
  `xdg_toplevel` lifecycle (resize/close/configure), damage tracking, multi-window; a real toolkit to
  type into (needs the image-provided toolkit).
- **M3 — DDP + macOS guest.** Factor `dd-display` behind DDP; bring up `os/darwin/display` adopting one
  macOS-guest toplevel via CAContext/CALayerHost. *Accept:* a Mac-guest GUI window renders in
  `dd-display`. (Retires risk #6.)
- **M4 — GPU rung 2. ✅ DONE + PROVEN END-TO-END (guest→host zero-copy GPU buffer).** A guest allocates a
  host-IOSurface-backed dmabuf, CPU-fills it, commits via linux-dmabuf; `dd-display` resolves the surface
  via the **mach-port handle bridge** and composites it on the GPU with **no shm copy, no upload, no
  readback** → `target-mac/gpu-frames/surface-6.png` shows the `(x^y)` pattern. This is the no-VM
  GPU-sharing core (Chrome's dmabuf mechanism). Pieces:
  - **Engine (`dd-jit-darwin`, gate-green 1629/0):** a guest opens the synthesized `/dev/dri/renderD128`
    and issues `DD_IOCTL_GPU_ALLOC` (`include/dd_gpu.h`); the engine `IOSurfaceCreate`s a host surface and
    — since guest-VA == host-VA — hands its **base pointer straight back** as the guest buffer (no mmap,
    no copy), plus the IOSurface global id + stride. Proven live: a guest got `id=142 ptr=0x10c608000`,
    CPU-filled the `(x^y)` pattern directly into the IOSurface's pages, and committed it. **Entirely gated
    behind `DD_GPU_IOSURFACE`** (the `--gui` launcher sets it) → inert for every other workload and the
    gate (still 1629/0, 3 engines OK).
  - **`dd-display`:** advertises `zwp_linux_dmabuf_v1` (opt-in `DD_DISPLAY_DMABUF`, so the shm path is
    unaffected), and on a dmabuf commit extracts the IOSurface id from the modifier and composites via
    `MetalCtx::texture_from_iosurface`. **Proven the whole wire works:** the guest's dmabuf commit reached
    dd-display and it attempted the composite with the correct id (142).
  - **The mach-port handle bridge (the crux — DONE).** `IOSurfaceLookup(global_id)` returns null on
    macOS 26 (Apple restricted global surfaces, per the plan's warning), so the handle crosses processes
    as a Mach send-right: the engine `IOSurfaceCreateMachPort(surf)` and `mach_msg`-sends `(port, id)` to
    `dd-display`'s bootstrap service `com.dd.display.gpu` (`vfs.c dd_gpu_send_port`, gated). `dd-display`
    registers the service (`bootstrap_check_in`→`bootstrap_register` fallback) and runs a mach-receive
    thread (`src/mach_bridge.c`, compiled by `dd-display/build.rs`) that `IOSurfaceLookupFromMachPort`s +
    caches `id → IOSurfaceRef`; the compositor resolves from that cache. Proven live: `GPU bridge cached
    IOSurface id=147` → `GPU-composited sid=6 → surface-6.png`. Advertised opt-in via `DD_DISPLAY_DMABUF`
    (shm stays the default). *Follow-ups:* per-buffer IOSurface lifetime/release (currently the engine
    leaks the ref) + surface reuse across frames.
- **M5 — GPU rung 3. REAL COMPLEX GPU APP RENDERS IN-CONTAINER (glmark2 build + texture scenes); full
  Mesa is the long tail.**
  - **🏇 MILESTONE (this session): the stock aarch64 `glmark2 2023.01` renders its `build` (the iconic
    horse model) and `texture` (textured cube) benchmark scenes IN-CONTAINER, end-to-end through the whole
    stack** — GLES2/EGL shim → GLSL-ES→MSL translate → dd-gpu IR over `DD_GPU_EXEC` → Metal executor →
    rung-2 IOSurface → dd-display composite → PNG. Ran at ~48 FPS (Score 46), 190 GPU-composited frames,
    byte-real geometry (1.05 MB vertex IR/frame for the 21.5k-vertex horse). Frames:
    `target-mac/glmark2-build-horse.png` (recognizable horse), `target-mac/glmark2-texture-cube.png`.
    This **retires the old "glmark2 can't run under the JIT" wall** (that was a stale engine; the current
    build reaches `main` and runs the render loop). Five real walls were cleared to get here, ALL in the
    surface stack (`dd-tests/guests/gl_shim.c`), zero engine-C change:
    1. **EGL config suitability.** glmark2 scores each config (`gl-visual-config.cpp score_component`) and
       rejects any component *present but not requested* (`component>0 && target==0` → −1000). Its default
       request has stencil=0/samples=0, so our single config advertising `stencil=8` scored −1000 →
       "Failed to find suitable EGL config". Fix: advertise `EGL_STENCIL_SIZE=0` (real drivers pass because
       they also expose a 0-stencil config).
    2. **`glGetShaderiv(GL_SHADER_SOURCE_LENGTH)`** returned 0; glmark2 verifies it round-trips to
       `strlen(src)+1` before compiling → "Failed to add … shader". Fix: return the stored source length+1.
    3. **Stale `libwayland-egl.so.1`.** The shim is deployed under THREE sonames (libEGL/libGLESv2/
       libwayland-egl, all built from `gl_shim.c`); only libEGL/libGLESv2 had been rebuilt. glmark2 resolves
       GL via `eglGetProcAddress`→`dlsym(RTLD_DEFAULT)` which hits libwayland-egl FIRST (it's in glmark2's
       `DT_NEEDED`), so it ran stale code. Rebuild all three.
    4. **Three-instance split state (the subtle one).** Three copies of the shim = three independent copies
       of the static GL state. glmark2 gets GL entry points via `eglGetProcAddress` (→ one copy) but calls
       `eglSwapBuffers`/`eglCreateWindowSurface` directly (→ another copy), so `glUseProgram`/`glDrawArrays`/
       `glVertexAttribPointer` mutated a DIFFERENT copy's globals than the swap read → every frame was
       clear-only (black), even the clear color defaulted. Fix: collapse to ONE instance — `libGLESv2.so.2`
       and `libwayland-egl.so.1` are now thin **`DT_NEEDED`→`libEGL.so.1`** stubs (empty TU), so all GL+EGL+
       `wl_egl_window` symbols and their state live in a single `libEGL.so.1`. (es2tri/es2tex regression-
       checked — still render.)
    5. **Draw-time state teardown + fixed IR buffer + full vertex descriptor.** glmark2's `Mesh::render_vbo`
       ENABLES attribs, draws, then DISABLES them — all before `eglSwapBuffers`; the shim assembled IR lazily
       at swap from live state → empty vertex layout. Fix: snapshot the attribute array at draw-call time.
       Then the horse's ~258 KB vertex buffer overflowed the 64 KB IR staging buffer (crash) → enlarged to
       8 MB + guarded. Finally, Metal `newRenderPipelineState` failed because the MSL vertex fn declares 3
       attributes (position/normal/texcoord) but glmark2 binds only 2 → emit a descriptor entry for EVERY
       shader-DECLARED attribute (bound → real layout; unbound → placeholder), so the descriptor matches the
       shader and the pipeline compiles.
  - **Multi-vertex-buffer IR — DONE + PROVEN (the old single-buffer approximation is retired).** glmark2/
    ANGLE bind a separate tightly-packed VBO per attribute (position in one buffer, normal in another). The
    IR already modeled this (`RenderPipelineDesc.vertex_buffers: Vec<VertexLayout>`, each layout = one buffer
    binding with its own attrs+stride; `SetVertexBuffer{slot}`) — **no wire change needed**. The two ends
    were the gap, now closed (zero engine-C; `dd-gpu`/`dd-display`/`gl_shim.c` only):
    - **`gl_shim.c` (eglSwapBuffers):** group the enabled attributes by their source `GL_ARRAY_BUFFER` (each
      `glVertexAttribPointer` snapshots its currently-bound VBO) → emit one `CreateBuffer`+`WriteBuffer` +
      one `VertexLayout` + one `SetVertexBuffer` per **distinct** VBO (slot = layout index, IR id = 200+slot),
      each attribute referencing its slot with its in-buffer offset/stride. Interleaved VBOs (es2tri: pos+color
      in one buffer) collapse to a single slot (unchanged). A declared-but-unbound attribute (glmark2's
      `texcoord` in the build scene) stays a placeholder in slot 0 so the Metal pipeline still has every
      shader-declared attribute.
    - **`metal_backend.rs`:** iterate ALL `vertex_buffers`; layout i → Metal buffer index `VBUF_BASE+i`
      (=16.., so vertex streams never collide with the uniform block at `[[buffer(1)]]`); set each attr's
      `MTLVertexAttributeDescriptor` `bufferIndex`/offset + each layout's `MTLVertexBufferLayout` stride;
      `SetVertexBuffer` binds at the same `VBUF_BASE+slot`.
    - **Proof:** the shim's ACTUAL runtime multi-VBO IR (position VBO + a SEPARATE per-vertex normal/color
      VBO) replayed through the real `MetalBackend` (`dd-display selftest-shim-ir`) →
      `target-mac/multivbo-lit-triangle.png` renders a clean **RGB-gradient** triangle whose fragment color
      comes from the *second* stream — if normals still aliased position (the old bug) the colors would be
      muddy/position-derived, not clean corners. Byte-exactness re-verified by decoding the shim's dumped IR
      with dd-gpu's `decode_stream` (2 `vertex_buffers`: pos slot 0, normal slot 1; 2 `SetVertexBuffer`).
      No regression: interleaved es2tri (`regr-interleaved.png`) + selftest texture/indexed/shader/replay all
      still render.
    - **★ IN-CONTAINER LIT HORSE RE-CAPTURED (deliverable, `target-mac/glmark2-lit-horse.png` +
      `-b.png`):** the multi-VBO fix is now confirmed END-TO-END in-container (not just via replay). Stock
      aarch64 `glmark2 build` composited its 21.5k-vtx horse through the full guest→host GPU path and the frame
      shows **correct normal-based diffuse lighting** — light/shadow follows the 3D surface curvature (chest,
      neck, head, raised legs all volumetrically shaded), and two frames at different rotation angles confirm a
      live animated render. This is a decisive change from the OLD flat capture `glmark2-build-horse.png`, whose
      "lighting" was a crude vertical *position* gradient (normals aliased to position — the horse's form washed
      out). The separate per-vertex normal VBO is being bound to its own slot and reaching the Metal executor.
    - **ROOT CAUSE of the "`EXIT=139` during EGL canvas init" flakiness (RESOLVED, was NOT the IOSurface/mach
      path):** measured rate on a freshly-reaped quiet host = 5/10 SIGSEGV + 5/10 `Failed to find suitable EGL
      config` (exit 1) + **0/10 frames** — but the crash is at glmark2's `eglGetConfigs`/config-scoring, i.e.
      *before* `eglCreateWindowSurface` is ever called, so `dd_gpu_alloc`/`dd_gpu_send_port`/IOSurface are NOT
      reached. The real cause is the **persistent per-workspace pcache** (`<storage>/pcache`): translated code
      bakes host-arena-relative absolute addresses that are only valid when the engine re-secures the SAME fixed
      arena base (`g_force_base`) it was written at. A cache written in a PRIOR session, re-loaded in a later
      session, intermittently lands at a MISMATCHED base (fixed VA occupied → NULL-hint fallback that isn't
      latched as `g_force_base_failed`) → stale absolutes → SIGSEGV, or garbage reads of glmark2's config attrs
      → "no suitable config". This is exactly "rendered one session, exit-139'd the next with no code change."
      PROOF: wiping the workspace pcache before each run → **10/10 EXIT=0, ~189 frames**; a cold-once-then-warm
      run → **6/6 EXIT=0** (a cache the *current* engine wrote at the current base reuses safely in-session).
    - **FIX (gate-unaffected — launcher Rust only, `dd-cli/src/ddjit_launcher.rs`, NO engine C):** a fresh
      `--gui` launch now RESETS the persistent pcache first (`ws.gui && !restore`), so every gui launch builds
      the cache cold at the current session's base and reuses it safely in-session (a `restore` keeps it — its
      MAP_FIXED placement needs it). VERIFIED: the original no-manual-wipe repro loop (previously 5/10 SIGSEGV)
      → **10/10 EXIT=0, 189 frames each, zero crashes**. Cost: one-time re-translation at startup (negligible
      for an interactive GUI app; the horse still runs full-speed). The deeper engine-level follow-up (make
      `pcache_load` refuse a base-mismatched cache, or latch `g_force_base_failed` on the NULL-hint fallback so
      LOAD self-invalidates like SAVE already does) is the real root fix but is a flagged pcache-minefield
      change requiring the full cross-arch gate — deferred; the launcher reset makes gui rendering 100% reliable
      today. Repro harnesses: `target-mac/exit139-loop.sh` (no wipe), `-nopcache.sh`, `-warm.sh`.
  - Repro: `~/.dd/gui/aarch64/bin/run-glmark.sh` (mounted via the `glmark2ws` workspace) + driver
    `target-mac/glmark2-run2.sh` (dd-display `--metal --png` on aligned `wayland-0`/`dd-gpu.sock`, then
    `ddcli workspace launch glmark2ws`). Set `DD_SHIM_DEBUG=1` in the guest for per-call shim tracing.
  - **Architecture verdict (decided):** *Do not* port virglrenderer/venus to macOS — they're
    Linux-oriented C (drm/epoll/udev) and don't build on the Mac (MoltenVK isn't even installed; only
    `Metal.framework` is present). Instead **reuse `dd-gpu`'s existing pure-Rust command IR** (already
    built + `cargo test`ed: `Cmd`/`Enc` with `CreateTexture`, `BeginRenderPass`/`Clear`, `Draw`, `Submit`
    + a hand-rolled wire + ring + a `GpuBackend` trait) as the forward protocol, and add a **Metal host
    executor** that renders it — targeting the **rung-2 IOSurface** as the render target (zero readback).
    A future Mesa `virgl`/`venus` frontend can translate to this IR (or we replay a serialized stream),
    but the IR-level forward sidesteps the macOS build wall entirely.
  - **First slice PROVEN end-to-end:** `MetalCtx::render_triangle_into` runs a real Metal render pass
    (compiled MSL vertex+fragment shaders, rasterized RGB triangle) into an IOSurface — verified alone by
    `dd-display selftest-render` → `target-mac/render-frame.png`. Then wired guest→host: the guest allocs
    a host IOSurface (`DD_IOCTL_GPU_ALLOC`), **requests a host GPU render** (a forwarded 1-op command,
    carried in the dmabuf modifier's render bit, `DD_DMABUF_RENDER_BIT`), and commits; dd-display resolves
    the IOSurface (mach bridge), runs the render pass **into the guest's buffer**, and composites →
    `target-mac/gpu-render-frames/surface-6.png` shows the GPU-rendered triangle. The guest triggered host
    GPU *rendering* (not fill/blit) into its shared buffer, zero-copy.
  - **Full IR forward — DONE + PROVEN end-to-end (arbitrary streamed geometry renders on the host GPU).**
    - **`MetalBackend`** (`dd-display/src/metal_backend.rs`, implements `dd_gpu::backend::GpuBackend`):
      `create_buffer`/`write_buffer` (MTLBuffer), `create_texture` (the injected IOSurface render target),
      `create_shader` (ignored — builtin), `create_render_pipeline` (a builtin vertex-color MSL pipeline +
      the guest's vertex layout), and `submit` replaying the encoder: `BeginRenderPass`(clear) /
      `SetPipeline` / `SetVertexBuffer` / `Draw` / `EndRenderPass` into the IOSurface. Shaders stay
      builtin (the IR carries SPIR-V; SPIRV-Cross→MSL is the deferred piece).
    - **Executor transport** (`metal_backend::run_executor`): a unix socket (`dd-gpu.sock`, bind-mounted
      into the guest as `/run/user/0/dd-gpu-0`, advertised via `DD_GPU_EXEC`) receives a framed IR stream
      `[id][w][h][len][bytes]`, resolves the IOSurface (mach bridge), `replay_stream`s it through
      `MetalBackend`, and acks. Started from dd-display's Metal serve loop.
    - **Guest streams real IR**: `gpu_dmabuf_client.c` (`DD_GPU_IR=1`) hand-rolls the dd-gpu wire (matching
      `wire.rs` byte-for-byte — 372 bytes, identical to the Rust `encode_stream`) for a vertex-colored
      QUAD (CreateBuffer+WriteBuffer+CreateShader+CreateRenderPipeline+Submit), streams it, then commits.
    - **Proof**: `target-mac/gpu-ir-frames/surface-6.png` — the composited QUAD (4 corner colors), i.e. the
      *streamed* geometry, distinct from the hardcoded triangle. Also `dd-display selftest-replay` →
      `target-mac/replay-frame.png` unit-tests the replay standalone. Logs: `executor: replayed 372 IR
      bytes into IOSurface 142` → `GPU-composited`.
  - **ICD first slice — a REAL GLES2 app drives the IR (per-app shaders + a guest EGL/GL shim).**
    - **`MetalBackend` per-app shaders (DONE + proven):** `create_shader` now compiles the guest's MSL
      (shipped as bytes in the IR) to an `MTLLibrary`; `create_render_pipeline` binds the app's own
      vertex/fragment functions (builtin only as fallback). Proven by `dd-display selftest-shader` →
      `target-mac/shader-frame.png`: a quad drawn with a CUSTOM fragment shader (`c*0.5+0.5`, washed-out),
      not the builtin — i.e. the app's shader ran.
    - **Guest EGL + GLES2 shim (`dd-tests/guests/gl_shim.c`, built as `libEGL.so.1`+`libGLESv2.so.2`,
      mount-injected):** covers `eglGetDisplay/Initialize/ChooseConfig/CreateContext/CreateWindowSurface`
      (surface = a rung-2 IOSurface via `DD_IOCTL_GPU_ALLOC`) `/MakeCurrent/SwapBuffers/QuerySurface/
      SwapInterval/GetError`; `glClearColor/Clear/Viewport`, `glCreateShader/ShaderSource/CompileShader/
      GetShaderiv`, `glCreateProgram/AttachShader/LinkProgram/GetProgramiv/UseProgram`, `glGetAttribLocation`,
      `glGenBuffers/BindBuffer/BufferData`, `glVertexAttribPointer/EnableVertexAttribArray`, `glDrawArrays`,
      `glGetString`. On `eglSwapBuffers` it translates the accumulated GL state → a dd-gpu IR stream
      (CreateBuffer+WriteBuffer, CreateShader(MSL), CreateRenderPipeline, Submit{clear,draw}), ships it to
      `$DD_GPU_EXEC`, and commits the IOSurface (linux-dmabuf) to dd-display.
    - **Shader path:** a minimal GLSL-ES→MSL translator in the shim handles passthrough shaders
      (`attribute`/`varying`, `gl_Position`/`gl_FragColor`, `vecN`→`floatN`); emits one combined MSL
      (VOut with `[[user(vN)]]` varyings) → `CreateShader`.
    - **Real app:** `dd-tests/guests/es2tri.c` — a stock GLES2 program (no dd calls): compiles a v+f
      shader, uploads an interleaved VBO, draws an animated (per-frame VBO-rotated) colored triangle,
      `eglSwapBuffers` each frame. Runs UNMODIFIED on the shim.
    - **Status: DONE + PROVEN end-to-end.** es2tri runs unmodified → shim → `renderD128` alloc → 3 real
      GL frames → GLSL→MSL → dd-gpu IR (762 bytes) → executor `replayed … into IOSurface 45` → dd-display
      composited. `target-mac/gl-frames/surface-6-00{0,1,2}.png` show the **animated** triangle (per-frame
      VBO rotation: apex up → right → down-left). (Op note: the shim's `Submit` emits 5 encoder ops
      drawing / 2 clearing — an earlier 4/1 miscount caused "short buffer", fixed. Always reap orphaned
      `ddjit`/`dd-daemon` before a launch or it hangs at 0 bytes.)
    - **Coverage extended toward es2gears (DONE + proven, 2 more real apps):**
      - **Uniforms + vec3 attributes + matrices** — `MetalBackend` binds uniform buffers via bind groups
        (`create_bind_group` + `SetBindGroup` → `setVertex/FragmentBuffer` at `[[buffer(1)]]` in both
        stages) and derives the Metal vertex descriptor from the guest attributes (vec2/vec3/vec4). The
        shim maps `glGetUniformLocation`/`glUniform*`/`glUniformMatrix4fv` → a Metal-struct-aligned uniform
        block + bind group; the translator emits a `Uniforms` block at `[[buffer(1)]]` (`u.NAME`). Proven:
        `es2uniform` (`target-mac/gl-uni-frames/`) — a triangle rotating purely via `uniform mat4 uMVP`
        (VBO uploaded ONCE), so the uniform+matrix path drives it.
      - **Depth buffer** — a depth-stencil state (compare Less, write on) + a lazy `Depth32Float` texture;
        `glEnable(GL_DEPTH_TEST)` → pipeline depth attachment + pass depth target. Proven: `es2depth`
        (`target-mac/gl-depth-frames/surface-6-000.png`) — near(red)+far(green) overlap where the near
        wins despite being drawn first (depth testing, not paint order).
    - **Complex-app surface broadening (DONE + proven — the direct Chrome precursor).** Broadened the
      GLES/shader surface toward a complex Wayland-native app (glmark2), since Chrome runs Wayland-native
      (`--ozone-platform=wayland`) with GL from ANGLE — so widening the *shader + texture + indexed-draw*
      surface (not X11) is the Chrome-relevant path. Three things landed:
      - **Textures + samplers (`MetalBackend` + shim + translator).** Backend: `create_texture` honors the
        guest format/usage (a `SAMPLED` texture is Shared-storage `ShaderRead`), `create_sampler` builds an
        `MTLSamplerState` (filter/wrap), `CopyBufferToTexture` uploads a staging buffer's pixels via a blit
        encoder, and bind groups now carry `Texture`/`Sampler` entries → `setVertex/FragmentTexture` +
        `…SamplerState`. Shim: `glGenTextures/BindTexture/ActiveTexture/TexImage2D/TexParameteri/f/
        PixelStorei/GenerateMipmap` (RGBA/RGB/LUMINANCE→RGBA8), `glUniform1i(sampler)` (sentinel location).
        Translator: `sampler2D` uniforms become `texture2d<float> NAME [[texture(i)]] + sampler NAMESmplr
        [[sampler(i)]]` params (per stage that uses them), `texture2D()/texture()` → `NAME.sample(NAMESmplr,
        …)`. Proven: `dd-display selftest-texture` (`target-mac/surface-tests/texture.png`) — a 2×2 RGBA
        texture bilinearly sampled across a quad.
      - **Indexed draw.** Backend: `SetIndexBuffer` + `DrawIndexed` → `drawIndexedPrimitives` (U16/U32,
        `first_index` byte offset). Shim: `glBindBuffer(GL_ELEMENT_ARRAY_BUFFER)` + `glDrawElements` →
        `CreateBuffer(INDEX)` + `WriteBuffer` + `SetIndexBuffer` + `DrawIndexed`. Proven: `dd-display
        selftest-indexed` (`…/surface-tests/indexed.png`) — a vertex-colored quad from **4** vertices + a
        **6**-entry index buffer.
      - **Real shader translator (SPIRV-Cross vs hand — decided: HAND).** SPIRV-Cross/glslang/shaderc are
        **genuinely unbuildable on this host**: no Homebrew, the mac's outbound HTTP is firewalled (so no
        `brew`/tarball), and the project forbids new crates.io deps (the `spirv_cross`/`shaderc-sys` crates
        can't be fetched) — the whole display stack deliberately reuses only the vendored objc2 stack. So
        the **hand GLSL-ES→MSL translator was extended** to cover glmark2's constructs: comment stripping
        (a keyword in a comment no longer mis-parses), `const`-prepended globals (glmark2's `add_const`
        light/material/PI) → MSL `constant`, **`vecN(vecM)` truncation** (`vec3(NormalMatrix*vec4(normal,
        1.0))` → `(…).xyz`), `mat3`, and the sampler path above. Identifier qualification now runs BEFORE
        the `gl_Position`/`gl_FragColor` rewrite so an attribute named `position` can't corrupt
        `out.position`. **Proven against glmark2's own shaders**: the shim's `translate()` (built as the
        host tool `cc -DDD_TR_TOOL gl_shim.c`) turns `light-basic.vert/frag` (build scene) and
        `light-basic-texgen.vert`+`light-basic-tex.frag` (texture scene) into MSL that **Metal actually
        compiles** — `dd-display selftest-msl <file.metal>` → `COMPILED OK (vmain=true fmain=true)` for both.
      - **End-to-end via a real stock GLES2 app (`es2tex.c`, DONE + proven).** A new stock-style app (no dd
        calls) uploads a 4×4 checkerboard `sampler2D` texture and draws a textured quad via `glDrawElements`
        (a 4-vertex VBO + 6-index EBO). It runs UNMODIFIED on the shim; the shim's **actual runtime IR** for
        one frame (1150 bytes: CreateTexture+CreateSampler+staging+CopyBufferToTexture+index buffer+combined
        bind group+DrawIndexed) replays through the **real `MetalBackend`** → `target-mac/surface-tests/
        es2tex-shim-ir.png` shows the checkerboard-textured quad. (Captured with the shim's `DD_IR_DUMP`
        host-tool mode — see below on the container harness gap.)
    - **EGL surface bring-up COMPLETED (the `Could not initialize canvas` blocker is fixed at the shim
      layer) — but glmark2 itself does NOT run under the JIT (a separate engine-C wall).** The rung's
      shim/compositor surface path is done and independently validated:
      - **EGL config attributes — real + self-consistent.** `eglGetConfigAttrib` now returns a genuine
        RGBA8888 + depth24 + stencil8 window config (`EGL_RED/GREEN/BLUE/ALPHA_SIZE=8`, `EGL_DEPTH_SIZE=24`,
        `EGL_STENCIL_SIZE=8`, `EGL_BUFFER_SIZE=32`, `EGL_SURFACE_TYPE=WINDOW|PBUFFER`, `EGL_RENDERABLE_TYPE=
        ES2|ES`, `EGL_CONFIG_ID=1`, `EGL_NATIVE_VISUAL_ID='XR24'`, `EGL_CONFIG_CAVEAT=NONE`, swap-interval
        0..1, samples 0) instead of all-zero; `eglChooseConfig`/`eglGetConfigs` return that one config;
        added `eglQueryString`/`eglGetPlatformDisplay[EXT]`/`eglGetCurrent*`/`eglDestroy*`/`eglReleaseThread`.
        Proven by a host harness that dlopens the shim and runs glmark2's exact config-query sequence — every
        attribute comes back `ok=1` with the right value (this is precisely what glmark2's `get_glvisualconfig`
        + `select_best_config` need, so the canvas-init blocker is retired).
      - **`wl_egl_window` — we now ship our OWN `libwayland-egl.so.1`** (built from `gl_shim.c`, staged over
        Mesa's) so the struct ABI is fully under our control: `wl_egl_window_create/_resize/_destroy/
        _get_attached_size` with a magic-tagged struct (width/height at offsets 8/12, matching Mesa's layout).
        `eglCreateWindowSurface` reads the size from our struct (magic path) OR the stock `{w,h}@0` convention
        (es2tri/es2tex) — proven by an ABI round-trip test (create 1920×1080 → resize 800×600 → attached size
        tracks). (Also fixed a real SIGSEGV: the earlier offset-8/12 read walked off es2tri/es2tex's 8-byte
        `uint32_t win[2]`, crashing the in-process launcher.)
      - **`eglGetProcAddress` + broadened GLES2 surface.** `eglGetProcAddress` now resolves via
        `dlsym(RTLD_DEFAULT)` (was NULL-for-everything → a guaranteed NULL-call crash for any app that loads
        GL through it, e.g. ANGLE/Chrome). Added the entry points glmark2/ANGLE lean on: `glGetIntegerv`
        (real limits + viewport), `glGetFloatv/Booleanv`, depth/blend/stencil/cull/scissor/colormask state
        setters (no-ops), `glUniformMatrix3fv/glUniform2f*`, framebuffer/renderbuffer object stubs
        (`glGenFramebuffers`/`glCheckFramebufferStatus=COMPLETE`/…), `glDelete{Shader,Program,Buffers}`,
        `glIsEnabled/IsTexture/…`, `glReadPixels`. Native es2tri/es2tex/es2depth still produce byte-identical
        IR (no regression).
      - **`wl_output` — already adequate** in `dd-display` (`geometry`+`mode 1920×1080@60`+`scale`+`done`,
        v2), so a windowed 800×600 glmark2 canvas query is satisfied; no change needed.
      - **dd-display now multiplexes CONCURRENT clients (the true architectural fix for a Wayland-native
        app).** A native-Wayland app holds TWO connections to the compositor at once: its own
        libwayland-client connection (wl_compositor/output/seat + `wl_egl_window`) live for the whole run,
        **plus** the GL shim's second connection that commits the rendered IOSurface. The old
        `serve_loop_metal` accepted+pumped **one client to completion**, so the shim's connection was never
        serviced → hard deadlock, nothing composited (this, not the EGL config, was the deeper wall). Replaced
        both serve loops with a single-thread `poll()` multiplexer (`serve_multiplex`) that services every
        client fd independently. Proven by a two-concurrent-client test (both get their registry, compositor
        stays alive); a first index-aliasing bug (accept grows `clients` past the built `pfds`) was found and
        fixed (service by fd-lookup, not pfd index).
      - **RESOLVED — glmark2 now loads AND renders under the JIT.** The earlier `glmark2 -l` `EXC_BAD_ACCESS`
        (exit 139) was a **stale engine**; the current build reaches `main`, lists all 17 scenes, and runs
        full `build`/`texture` benchmark scenes end-to-end (see the M5 milestone above). The stock Feb-2023
        aarch64 C++/PIE glmark2 executes fine; the remaining work is render *fidelity* (multi-vertex-buffer
        IR), not load/execution. This retires the "glmark2 is a JIT wall" framing that blocked the M6 Chrome
        prerequisites list below.
    - **Container executor-socket + render-node path (pre-existing launcher/engine-C, orthogonal to this
      rung) — partially advanced, still the last-mile blocker for in-container pixels.** Current state
      (re-measured this session): BOTH `/run/user/0` bind-mounts now DO reach the guest as socket nodes
      (`wayland-0` AND `dd-gpu-0` — the earlier "second bind dropped" symptom is gone), and `wayland-0`
      **connects** (dd-display logs the shim's client + caches the IOSurface via the mach bridge). But the
      executor path is still not end-to-end: (a) `/dev/dri/renderD128` synth is **flaky** — it fires only
      when `DD_GPU_IOSURFACE=1` is in ddcli's **ambient** env (the typed-config carry via `ddjit_configfd.c`
      is unreliable; `errno=2 ENOENT` on the open otherwise), and even with it exported the guest GPU path is
      **nondeterministic** run-to-run (es2tri sometimes completes with frames, sometimes `EXC_BAD_ACCESS`
      exit 139 right after the IOSurface is cached); (b) when it does run, the shim's `connect()` to the
      bind-mounted `dd-gpu-0` still fails (`gl_shim: exec connect fail`) even though `wayland-0` — bound the
      same way — connects, so the AF_UNIX path→host-socket resolution for the *second* single-file socket
      bind is not equivalent. All engine-C (vfs `atpath`/render-node/IOSurface), no dd-display/shim fix; this
      host is also resource-constrained for the GPU path. The dd-display + shim surface work is proven via the
      GPU-verified selftests + the direct EGL/`wl_egl_window`/multiplex tests above. es2tri DID run
      end-to-end through the launcher earlier (renderD128 alloc + IOSurface bridge + wayland connect), with
      only the executor connect missing.
  - **Beyond glmark2:** `vkcube`/Vulkan via a Mesa-less custom ICD or a Venus-frontend→IR; shader/pipeline
    caching (compiled per call now). SPIRV-Cross/naga stays deferred until it (or a fetchable equivalent)
    can be built here; the hand translator covers the passthrough + uniform + texture + truncation surface.
    IOSurface lifetime + reuse already handled (rung-2 registry).
- **Correctness (rung-2 follow-up, DONE):** the engine now keeps a per-render-node IOSurface registry
  (`vfs.c`) — reuses a same-size surface across frames and releases all of a render node's surfaces when
  the fd closes (`fd_reset_emul`), so a long-running GUI app no longer leaks IOSurfaces. Gate-green.

- **M6 — hardware-accelerated Chrome (the goal). BRING-UP PLAN.** Chrome/Chromium runs **Wayland-native**
  (`--ozone-platform=wayland`, no XWayland) and its GL comes from **ANGLE**, so the Chrome-relevant work is
  the *Wayland + GLES/EGL + dmabuf* surface this milestone has been widening — not X11. Concrete plan:
  1. **Source Chromium FROM THE IMAGE, never a dd-baked image.** Either `apt-get install chromium` in the
     workspace's own image (Debian/Ubuntu ships a Wayland-capable build), or run a published
     `chromium`/`linuxserver/chromium` image directly; dd provides only the socket + `libwayland-client`/
     `-egl` + the `libEGL`/`libGLESv2` shim (mount-not-bake, exactly as for glmark2). Its GL/toolkit closure
     comes from the image's own packages.
  2. **Invocation** (into dd-display, GPU process reaching `/dev/dri` + `DD_GPU_EXEC`):
     `chromium --ozone-platform=wayland --no-sandbox --disable-gpu-sandbox --use-gl=angle
     --use-angle=gl-egl --in-process-gpu --disable-features=Vulkan --window-size=1280,800
     https://example.com` — `--no-sandbox`/`--disable-gpu-sandbox` first (Chrome's seccomp+namespace
     sandbox otherwise blocks the GPU process from `open("/dev/dri/renderD128")` and the `DD_GPU_EXEC`
     connect); `--in-process-gpu` collapses the multiprocess GPU service into the main process for the
     first bring-up (avoids a second render node + socket per child); `--use-angle=gl-egl` routes ANGLE at
     our EGL/GLES2 shim (ANGLE's own Metal backend is macOS-only and unavailable to a Linux guest, so ANGLE
     must target *our* GLES, which is why the broadened shader/texture/indexed surface matters).
  3. **Multiprocess note.** Once single-process works, drop `--in-process-gpu`: Chrome's **GPU process** is
     the one that touches `/dev/dri` + streams to `DD_GPU_EXEC`; each renderer hands textures to it as
     dmabuf — which our **IOSurface-backed `linux-dmabuf`** (rung 2) already models zero-copy, exactly
     Chrome's compositing mechanism. The engine's per-render-node IOSurface registry already handles the
     lifetime.
  4. **First concrete thing to try:** `chromium --ozone-platform=wayland --no-sandbox --in-process-gpu
     --use-gl=angle --use-angle=gl-egl about:blank` — get a single **blank painted window** committed to
     dd-display (the compositor + one ANGLE clear + swap), before any page content. Its GL trace will be a
     `glClear`+`eglSwapBuffers` — the smallest exercise of the shim that a real browser produces.
  - **Prerequisites this milestone must finish first** (updated order after this session):
    - (a) **DONE — the shim's EGL config + `wl_egl_window` ABI + `wl_output` + `eglGetProcAddress` + broader
      GLES2 surface.** All the EGL/EGL-config negotiation ANGLE's GL backend needs is now real (see the M5
      "EGL surface bring-up COMPLETED" bullet): a genuine RGBA8/depth24/stencil8/ES2 config, our own
      `libwayland-egl.so.1`, `eglGetProcAddress` via `dlsym`, `glGetIntegerv` limits, FBO/renderbuffer stubs,
      blend/depth/stencil/scissor state, extra uniform/matrix setters. dd-display multiplexes the two
      concurrent Wayland connections a native app (and Chrome's ozone-wayland) opens.
    - (b) **DONE — complex C++/PIE binaries run under dd-jit.** The stock aarch64 C++/PIE `glmark2 2023.01`
      loads AND renders its benchmark scenes end-to-end (M5 milestone; the old `glmark2 -l` `EXC_BAD_ACCESS`
      was a stale engine). This de-risks Chrome's *load/execute* dimension considerably — a far larger
      multiprocess C++ binary is still heavier, but "the JIT can't run big C++/Wayland binaries at all" is no
      longer true. Chrome-specific next work is now (c)+(d) and Chrome's own launch/sandbox shape.
    - (c) **DONE for the in-container path** — the executor socket + render-node now work end-to-end: both
      `wayland-0` and `dd-gpu-0` bind-mounts reach the guest, the shim `connect()`s to `dd-gpu-0`, streams IR,
      and the Metal executor renders into the rung-2 IOSurface (glmark2 proved 190 composited frames). The
      earlier `dd-gpu-0` connect failure / flaky `renderD128` did not recur with `DD_GPU_IOSURFACE=1`.
    - (d) **Multi-vertex-buffer IR — DONE** (see the M5 "Multi-vertex-buffer IR" bullet): the shim emits one
      IR vertex-buffer per distinct source VBO and `metal_backend.rs` binds each at `VBUF_BASE+slot`. This was
      the highest-value correctness lever (ANGLE, like glmark2, uses a separate tightly-packed VBO per
      attribute). Still open under (d): broader GLES2/EGL coverage ANGLE emits (`glGetString(GL_EXTENSIONS)`
      content, many programs/textures/samplers, MRT/FBO round-trips) — the shader translator, textures,
      indexed draw, FBO stubs, uniform/state coverage are in place; extension-string + MRT are the next gaps.
    - **First Chrome bring-up (exploratory) — chromium arm64 SOURCED + characterized; not yet run (the
      execution wall is the JIT render-node path, not the shim).** Findings, ranked, honestly:
      - **Wall 0 — sourcing (CLEARED, with a caveat).** apt is a dead end on this host: every Debian mirror
        (`deb.debian.org` + `ftp.us/ftp.debian.org`/cloudfront/leaseweb) is firewalled (HTTP 000) and Ubuntu's
        `chromium` is a snap stub (no arm64 `.deb` in `ports.ubuntu.com`). But the **docker.io registry is
        reachable** — pulled `zenika/alpine-chrome:with-node` (arm64) via the oracle and extracted a real
        `usr/lib/chromium/chromium`. So a chromium binary IS obtainable mount-not-bake (image kept in the
        oracle cache; the 936 MB rootfs extraction was cleaned up).
      - **Chrome-scale characterization (from the extracted binary).** chromium = **206 MB PIE, aarch64,
        musl** (`/lib/ld-musl-aarch64.so.1` — NOT the glibc `ubuntu:24.04` glmark2 path), with a **40+ direct
        `NEEDED` closure** (glib/gobject/gio, icu-74, nss/nspr, dbus, atk/atk-bridge/atspi, cups, **libdrm.so.2**,
        fontconfig/freetype/harfbuzz, ffmpeg avcodec/avformat, **libX11/Xcomposite/Xdamage/Xext/Xfixes** even for
        Wayland) → the whole ~936 MB image is the runtime closure. It **bundles ANGLE** (`libEGL.so`/
        `libGLESv2.so` in the chromium dir) + `libwayland-egl.so.1`. Implications, ranked:
        1. **musl workspace, not glibc.** A chromium workspace must run the Alpine image ITSELF under the JIT
           (mount-not-bake: the image provides its own 40-lib closure; dd mounts only the socket + our
           `libEGL`/`libGLESv2` shim) — a new libc/loader path for the JIT at Chrome scale.
        2. **ANGLE is one layer down.** Chrome's own ANGLE (`--use-angle=gl-egl`) targets the *system* libEGL
           (our shim), so the shim stays the GL target but ANGLE emits a far larger GL surface than glmark2
           (many programs, FBO/MRT, `GL_EXTENSIONS` content, multi-VBO — now handled by (d)).
        3. **`libdrm.so.2` + the GPU process** open `/dev/dri/renderD128` (our synth node) + stream to
           `DD_GPU_EXEC` — the exact render-node/executor path that is currently the last-mile blocker.
        4. **Multiprocess + sandbox** (`chrome-sandbox` present, zygote/renderer/gpu fork+exec + seccomp) →
           needs `--no-sandbox --disable-gpu-sandbox --in-process-gpu` for the first bring-up.
      - **Wall 1 — execution (the real blocker; not reached the shim).** The `about:blank` run was NOT reached:
        `ddcli run` needs a dd-daemon that isn't up on this host (daemon-less), and the workspace launcher path
        (glmark2's route) is itself **crashing glmark2 with `EXIT=139` (SIGSEGV during EGL canvas init) run-to-
        run** on this resource-constrained host — a 206 MB multiprocess musl binary (240× glmark2) cannot get
        past that until the JIT's last-mile render-node/executor flakiness + big-binary robustness are solid.
      - **Verdict:** the glmark2 path remains the exact template and Chrome's highest-value GL lever (multi-VBO)
        is now done; the frontier is (1) stabilizing the in-container render-node/executor path (so glmark2 stops
        crashing), then (2) a musl/Alpine chromium workspace + `GL_EXTENSIONS`/MRT coverage for ANGLE, then (3)
        Chrome's multiprocess/sandbox launch shape. None are the shim's EGL-config/`wl_egl_window`/vertex surface
        (those are proven) — they are JIT-execution + ANGLE-breadth.
    - **★ CHROMIUM ACTUALLY RUN (this session) — the full musl/Alpine `--gui` workspace is built and chromium
      executes under the engine; the wall is now precisely pinned and it is DEEPER than expected: the JIT's
      aarch64 `translate_block` NON-TERMINATES on chromium's own code, before ANY GL/Wayland.** The bring-up is
      no longer "not yet run" — everything up to and including chromium process bring-up works; the block
      translator is the hard stop. Reproduction is committed: workspace `chromiumws`
      (`image = zenika/alpine-chrome:with-node`, arm64, `gui = true`, shell `/usr/local/bin/run-chrome.sh`),
      driver `target-mac/chrome-run.sh` (dd-display `--metal --png` on aligned `wayland-0`/`dd-gpu.sock`, swaps
      `~/.dd/gui/aarch64/lib` to the **musl** shim for the run and restores glibc on exit), wrapper
      `target-mac/run-chrome.sh`. The **musl shim** (`gui/aarch64/lib.musl-chrome/{libEGL.so.1,libGLESv2.so.2,
      libwayland-egl.so.1}`) is built inside Alpine arm64 via the oracle (`cc -shared` on `dd-tests/guests/
      gl_shim.c`; stubs forced with `-Wl,--no-as-needed libEGL.so.1` so `dlopen("libGLESv2.so.2")`+dlsym see the
      symbols). **The ranked walls, in the exact order chromium hits them:**
      - **Wall 0 — sourcing + musl workspace (CLEARED).** dd's own image store pulls `zenika/alpine-chrome:
        with-node` from docker.io (936 MB, `~/.dd/images/arm64/docker.io_zenika_alpine-chrome_with-node`) — no
        oracle/apt needed; the image IS the closure (mount-not-bake). Confirmed the **engine runs musl arm64**:
        a trivial Alpine binary (busybox/`cat`/`uname`) runs as pid 1 (`musltest` sanity).
      - **Wall 1 — musl loader + 40-lib closure of the 206 MB chromium (CLEARED).** The musl loader brings the
        binary up: `ldd /usr/lib/chromium/chromium` fully resolves its 40+ `NEEDED` (libgobject/libicu-74/
        libnss3/libdbus/libatk/libcups/**libdrm.so.2**/fontconfig/ffmpeg…) from the image. And a **large musl
        binary from the same image executes correctly**: `node --version` AND `node -e 'console.log(6*7)'`
        (real V8) return rc=0 / `42` under the engine — so the JIT handles big musl binaries; the chromium hang
        is chromium-code-specific, not a musl/loader/closure problem.
      - **Wall 2 — THE HARD STOP: `translate_block` never terminates on chromium's code (JIT-execution, engine-C,
        deep).** chromium `--no-sandbox --version` (a pure no-GL/no-Wayland path) runs the engine at **100% CPU
        with RSS pinned dead-flat at 260 MB for 180–200 s and prints nothing** (timeout SIGTERM, rc=143). Every
        `sample(1)` stack is *entirely* in `translate_block` (`run_guest → translate_block+8676 →
        translate_block+9104/9120`, byte-identical across samples). Flat RSS = **no new translated code is being
        emitted** → this is not slow-but-progressing cold translation (node translated + ran fine); it is a
        **non-terminating decode/region-translation loop** in `dd-jit-darwin/src/runtime/translate/aarch64/
        translate.c:translate_block` — the shallow (~2-frame) self-recursion + pinned inner-loop PC points at the
        region/successor-inlining path (the file's own comment: "intermediate inlined block-starts are left
        unregistered") following a cycle, or an instruction encoding chromium's newer codegen emits that the
        decoder fails to advance past. **This blocks BOTH `--version` and the GL `about:blank` run identically**
        (the GL run also produced zero chromium stderr and dd-display saw no client connect). No GL, no Wayland,
        no ANGLE-vs-shim negotiation is reached — so the shim's EGL/`wl_egl_window`/ANGLE-breadth work is
        **untested against real Chrome** and remains the *next* frontier once Wall 2 falls.
      - **Immediate next diagnostic (cheap, do first):** pin the exact guest PC/opcode where `translate_block`
        loops — enable the engine's `JT`/`JTS` trace (add them to the `spawn_config.rs` forward-list next to
        `CRASHDBG`, rebuild `ddcli` only — Rust/env-forwarding, gate-neutral), then disassemble chromium at that
        gpc. That names the offending instruction/region and turns Wall 2 from "translator loops" into a fix.
      - **Wall size (honest):** Wall 2 is a **deep engine-C translator-correctness fix** (aarch64 `translate_block`
        region-inlining / decode-advance), a flagged minefield requiring the full cross-arch gate — NOT a
        one-session shim/dd-display change. Walls 3+ (ANGLE EGL init against our shim, `GL_EXTENSIONS`/entry-point
        breadth, FBO/MRT, the GPU-process render-node/executor path, multiprocess/sandbox) are all **downstream of
        Wall 2 and currently unreachable/unmapped** because chromium never finishes translating its startup code.
      - **Environment left clean:** glibc `gui/aarch64/lib` restored (glmark2 path intact, 24 libs); musl shim
        stashed in `gui/aarch64/lib.musl-chrome`; `chrome-run.sh` swaps+restores per run; temp `musltest`
        workspace removed, `chromiumws` kept. No engine-C touched → **gate unaffected**.
    - **★★ WALL 2 ROOT-CAUSED + FIXED — `chromium --no-sandbox --version` now EXITS rc=0 in 2 s printing
      `Chromium 124.0.6367.78` (was: 100% CPU forever, rc=143). The wall was NOT what the prior session
      guessed (region-inlining / decode-advance) and NOT the SMC re-translation livelock either.** Diagnosis
      chain (image survived in `~/.dd/images/arm64/docker.io_zenika_alpine-chrome_with-node`, so no re-pull
      needed):
      - **First, the just-landed SMC content-gate fix was VERIFIED to NOT unblock chromium** (its actual
        target was a *different* symptom). Re-ran `--version` under the SMC-fixed engine → identical livelock.
        Decisive tell in the sample: the stack is `run_guest → translate_block` with **zero dispatch/execution
        frames** — the hang is a loop *inside a single translate_block, before any guest code runs*, so the
        guest's `ic ivau` (an execution-time event) is never even reached → SMC could never have been the cause.
      - **lldb attach pinned the real loop:** the hot PC is a **linear probe over `g_txln[]`** (the SMC
        cache-line gate, `engine/cache.c`), `x12 = &g_txln`, **`x13 = 1,503,276` remaining probes mid-scan**.
        `TXLN_N = 2^21 = 2,097,152`; chromium's 206 MB musl binary translates **>2M distinct 64B code lines**
        at startup → the open-addressed set **SATURATES**, and `txln_put`/`txln_has`/`txln_flush_class` had an
        **UNBOUNDED** probe (`for i<TXLN_N`). `txln_put` is on translate_block's hot path (via `txpg_mark`), so
        a full table made **each block's translation an O(TXLN_N) scan per line** → the exact "translate_block
        at 100% CPU, RSS flat, no output" symptom. A hash-set saturation blowup on the *translate* path — a
        class distinct from the SMC *flush*-path livelock.
      - **Fix (`engine/cache.c`, gate-relevant):** bound the probe to `TXLN_PROBE_CAP = 512`. On cap-exhaustion
        each op degrades to the conservative fallback the callers **already documented** ("saturated → assume
        present → wholesale drop"): `txln_put` leaves the line unrecorded, `txln_has`/`txln_flush_class` return
        "present"/"drop". Correctness-preserving by construction — `txln_put` only ever inserts within the cap
        and slots are never individually emptied (only `txln_clear` wholesale-zeroes), so a line that WAS
        inserted is always re-found within the cap; a cap-exhausting probe means the line was never recorded →
        over-approximating it as present never misses stale code. Converts the O(2M) livelock into O(512).
      - **Result:** `--version` 200 s-livelock → **2 s rc=0 + version string**. This is the definitive unblock
        of Wall 2. Gate: `make test` LINUX-side (reap-first) = **1630 passed / 0 failed** (13 xfail), all 3
        engines OK (`linux_aarch64` / `linux_x86_64` / `darwin_aarch64`); every SMC/pcache test green
        (`smc`, `smcthreads`, `smcselfflush`, `smc2`, `ldrsw-literal-pcache`) → no regression.
    - **★ WALL 3 REACHED + PRECISELY MAPPED (new frontier) — chromium boots and EXECUTES real init code, then
      a guest `brk`/SIGTRAP during Chrome's thread/crash-handler setup, still BEFORE any Wayland/GL.** With
      Wall 2 gone, the `about:blank` GL run (`chromium --ozone-platform=wayland --no-sandbox --in-process-gpu
      --use-gl=angle --use-angle=gl-egl about:blank` vs `dd-display --metal --png`) now gets chromium to print
      `This is Chrome version 124.0.6367.78` + `GetCollectStatsConsent()` and run substantial init (syscall
      trace: ~12 `rt_sigaction` handler installs, `mprotect`s, a helper-process fork over a socketpair), then
      `Trace/breakpoint trap (core dumped)` → **rc=133 (SIGTRAP)**; dd-display saw NO client connect (crash is
      pre-Wayland/pre-ANGLE). Discriminated precisely:
      - `--single-process` and `--disable-breakpad` BOTH still SIGTRAP at the identical point → **not** the child
        fork, **not** the crash-uploader.
      - `--headless` gets further and logs the real cause before dying:
        `FATAL:thread_helpers.cc(41) Check failed: . : No such file or directory (2)` +
        `libc_interceptor.cc(240) sendmsg: Connection reset by peer (104)`.
      - **Root cause CONFIRMED directly:** `ls /proc/self/task` under the engine → `No such file or directory`,
        and `/proc/self/` is non-enumerable (empty readdir). Chrome/crashpad's `ThreadHelpers` enumerates the
        process's threads via **`/proc/<pid>/task/`**; our Linux personality's `/proc` doesn't synthesize the
        per-thread `task/` dir (nor a listable `/proc/self/`), so the CHECK fails → `IMMEDIATE_CRASH()` (`brk`)
        in the default path, the logged FATAL in headless. The engine's `CRASHDBG` handler covers SIGSEGV/SIGBUS
        but NOT SIGTRAP, so a guest `brk` kills the engine at host level ("Trace/BPT trap: 5") with no dump.
      - **Wall 3 = a Linux-personality `/proc` gap (os/linux), NOT a translator bug** — synthesize
        `/proc/<pid>/task/<tid>/` (at least one entry per live guest thread) + make `/proc/self/` enumerable.
        Gate-relevant (personality change).
      - **Wall 3 CLEARED (2026-07-07).** The `/proc/<pid>/task/` synth machinery already existed
        (`proc_dir_try_open`/`proc_task_dir_open`/`proc_leaf_dir_open` in `container/vfs.c`, the `synth_stat_raw`
        task-dir block, and the `proc_open` `task/<tid>/<leaf>→<leaf>` fold) but was **numeric-pid-only**: every
        entry point (`proc_dir_try_open`, and the stat block via `proc_any_leaf`) failed to resolve
        `/proc/self/task`, and the fs.c self→pid rewrite fired only for **bare** `/proc/self`, never
        `/proc/self/<leaf>`. Fix = a `proc_deself()` helper (`/proc/self/… , /proc/thread-self/… →
        /proc/<cpid>/…`) applied at the top of `proc_dir_try_open` and in the `synth_stat_raw` task-dir block.
        Verified on the real binary: `ls -la /proc/self/task` now lists `.`/`..`/`1` (main tid == container init
        pid 1) and `/proc/self/task/1/{stat,comm,status,statm,cmdline}` are served (per-tid `exe/cwd/root`
        symlinks still ENOENT — crashpad doesn't read them). Chromium clears the `thread_helpers.cc` FATAL: it
        now prints `This is Chrome version 124.0.6367.78` + `GetCollectStatsConsent()` and runs far past crashpad.
      - **Wall 4 (NEW, mapped 2026-07-07) — chromium Mojo IPC bootstrap over the SEQPACKET socketpair.** Past
        Wall 3, the `about:blank` GL run (`--ozone-platform=wayland --no-sandbox --in-process-gpu --no-zygote
        --use-gl=angle --use-angle=gl-egl`) prints the Chrome banner + `GetCollectStatsConsent()` then a guest
        `brk` → `Trace/breakpoint trap` (rc 133; rc 139 with the new CRASHDBG Mach handler). dd-display saw **no**
        client connect (pre-Wayland/pre-ANGLE). CRASHDBG (now catching `EXC_BREAKPOINT`) reports
        `[MACH] exc=0x6 … gpc=0x12d189978` — a guest `IMMEDIATE_CRASH()` (`brk`). `JTS=1` syscall trace shows the
        crash is preceded by chromium's Mojo channel bring-up: `socketpair(AF_UNIX, SOCK_SEQPACKET, 0)` (args
        `1,5,0`), `sendmsg`/`recvmsg` with SCM_RIGHTS ancillary fds, `epoll_create1`+`eventfd2`+`epoll_ctl`, then a
        `recvmsg` on the IPC fd followed by teardown (`epoll_ctl DEL`, `close`, `exit_group`) — i.e. a child
        aborts during the IPC handshake. This is the same failure headless surfaces as
        `zygote_communication_linux.cc: Did not receive ping from zygote child`. dd **already** emulates SEQPACKET
        socketpairs as `SOCK_DGRAM` (macOS AF_UNIX has no SEQPACKET) and translates SCM_RIGHTS cmsgs
        (`net.c` case 199 / `cmsg_l2m`), so Wall 4 is a **semantic-fidelity gap in that emulation** (message
        framing / SCM_RIGHTS fd delivery over the DGRAM-backed pair), not a missing syscall. It is an
        os/linux `net.c` fix, not a translator bug.
      - **Wall 4 CLEARED (2026-07-07).** Root cause was **two** DGRAM-backed-SEQPACKET fidelity gaps in the
        Mojo NodeChannel handshake, both proven on the real `chromium` binary via `JTS=1`:
        1. **Premature EOF from the close-injected zero-length datagram.** dd wakes a blocked SEQPACKET peer by
           `send(fd,"",0)` on close (macOS DGRAM sockets deliver no EOF on peer close). But chromium's parent
           holds *both* socketpair ends after fork, then `close()`s the child's inherited end (`fd4`) while
           keeping its own (`fd3`). The injected zero-length datagram went to `fd4`'s peer = the parent's **own
           retained `fd3`**, so the parent's next `recvmsg(fd3)` read a bogus 0 → "peer closed" → `IMMEDIATE_CRASH`
           (`brk`, `[MACH] exc=0x6 gpc=…978`). On Linux no EOF is delivered while the fork child still references
           that end. **Fix:** record each pair's partner fd (`g_sock_pair_peer`, set at `socketpair(SEQPACKET)` /
           `pipe2(O_DIRECT)`, carried on dup, reset on close) and **suppress the synthetic EOF when we still hold
           the partner end open** — a genuine last-local close (no partner held, e.g. a jobserver reader) still
           injects. (`netns.c` `seq_send_eof`, `net.c` case 199, `io.c` pipe2.)
        2. **Missing `SO_PASSCRED`/`SCM_CREDENTIALS`.** Past #1 chromium enabled `SO_PASSCRED` (`SOL_SOCKET/16`)
           then `recvmsg`'d the bootstrap message expecting the kernel-auto-attached `SCM_CREDENTIALS` ucred
           record — macOS has neither the option nor the cmsg, so the peer aborted with
           `ERROR:socket.cc(177) missing credentials`. **Fix:** record `SO_PASSCRED` per-fd (`g_sock_passcred`,
           set/get in `net.c` cases 208/209) and **synthesize the `SCM_CREDENTIALS` record on every `recvmsg`**
           (`cmsg_add_cred`: `SOL_SOCKET`/type 2, `ucred{pid,uid,gid}`; uid/gid = container identity, pid =
           `LOCAL_PEERPID` with the init host-pid mapped back to guest 1 / self→container pid; sets Linux
           `MSG_CTRUNC` if the control buffer is short).
        With both fixes the real `chromium about:blank` GL run clears the **entire** Mojo/IPC bootstrap
        (zero `[MACH]`/`IMMEDIATE_CRASH`/`missing credentials`/`Did not receive ping`), **connects to
        dd-display as a live Wayland client** (`dd-display: client connected (fd 5, 1 live)`), **enumerates the
        display** (`wayland_screen.cc:146 Displays updated, count:1`, `Display[5] bounds=[0,0 1920x1080]`), and
        advances into GL init. Regression guard: `dd-tests/guests/ext_ipc/ipc_seqcred.c`.
      - **Wall 5 (NEW, mapped 2026-07-07) — ANGLE EGL bring-up vs the musl GL shim.** Past Wall 4, the GPU
        thread's ANGLE `Display::initialize` fails: `ANGLE Display::initialize error 12289: Could not load EGL
        entry point eglCreatePbufferSurface` → `eglInitialize OpenGLEGL failed EGL_NOT_INITIALIZED` →
        `Initialization of all EGL display types failed` → `gl::init::InitializeGLNoExtensionsOneOff failed`.
        dd-display gets the client + display enumeration but **0 frames** (EGL fails before first pixel). This is
        a **GL/EGL shim gap** (`~/.dd/gui/aarch64/lib.musl-chrome` libEGL missing `eglCreatePbufferSurface` and
        the pbuffer entry-point breadth ANGLE's gl-egl backend probes), **not** an engine/syscall gap — the next
        target is the EGL shim's entry-point coverage → IR over `DD_GPU_EXEC` → paint. Also seen alongside
        (non-fatal): `Failed to find drm render node path` (wayland_buffer_manager_gpu — dmabuf/render-node
        path), which the DD_GPU IOSurface path may need next.
      - **Wall 5 CLEARED (2026-07-07).** Added the EGL entry-point breadth ANGLE's `gl-egl`
        `FunctionsEGL::initialize` resolves-or-aborts on (it loads a FIXED EGL 1.0-1.2 CORE set via
        `eglGetProcAddress` and bails with "Could not load EGL entry point <name>" on the first miss) to
        `dd-tests/guests/gl_shim.c`: **`eglCreatePbufferSurface`** (the reported one) + `eglCreatePixmapSurface`
        + `eglCreatePbufferFromClientBuffer` + `eglQueryContext` + `eglCopyBuffers` + `eglBindTexImage` +
        `eglReleaseTexImage` + `eglSurfaceAttrib` (the already-present `eglBindAPI`/`eglQueryAPI`/`eglSwapInterval`/
        `eglReleaseThread`/`eglWaitClient`/`eglGetCurrent*` complete the set). Pbuffer = a **distinct inert
        handle** (`(EGLSurface)2`, NO `surface_up`) — ANGLE only needs it to make its bootstrap context current
        for GL cap-probing during `Display::initialize`; the browser's real frames still route through the
        window surface (`g_surf`), which a 1x1 pbuffer must not clobber. **Build:** only `libEGL.so.1` carries
        code (the `libGLESv2.so.2`/`libwayland-egl.so.1` sonames are thin `DT_NEEDED->libEGL` stubs, unchanged),
        rebuilt in arm64 alpine via orbstack (`docker -c orbstack run --platform linux/arm64 alpine:3.20`,
        `apk add gcc musl-dev`, `cc -shared -fPIC -O2 -Wl,-soname,libEGL.so.1 -o libEGL.so.1 gl_shim.c`) and
        staged to `~/.dd/gui/aarch64/lib.musl-chrome/`. **Verified on real chromium 124** (`chrome-gl-run.sh`,
        `--use-angle=gl-egl` vs `dd-display --metal --png`): the ANGLE errors are GONE — no
        `Could not load EGL entry point` / `EGL_NOT_INITIALIZED` / `Initialization of all EGL display types
        failed` / `InitializeGLNoExtensionsOneOff failed`. ANGLE now clears `eglInitialize`/`Display::initialize`
        and **forwards chromium's `eglChooseConfig` to the shim** (two calls, seen via `DD_SHIM_DEBUG=1`:
        `SURFACE_TYPE=WINDOW|PBUFFER` then `PBUFFER`-only, both RGBA8/D16/S8, `RENDERABLE_TYPE=EGL_OPENGL_ES3_BIT`),
        which returns the config. **Gate-neutral** (guest `gl_shim.c` only; no engine C).
      - **Wall 6 (NEW, mapped 2026-07-07) — chromium's ozone-wayland GPU has NO DRM render node to back
        GBM/dmabuf buffer allocation → NULL-deref.** Past Wall 5, the `about:blank` GL run crashes with a
        **deterministic** guest `SIGSEGV` (`[CRASH] sig1X fault=0x0 pc=0x5002cdc0a3c`, rc 139 — the SAME pc across
        every run) **immediately after** the two `eglChooseConfig` calls and **before any `eglGetConfigAttrib`/
        `eglCreateWindowSurface`** (the shim's `surface_up` never runs → dd-display gets `client connected` then
        `disconnected (0 frame(s))`). It is co-located with the GPU thread's
        `ozone_platform_wayland.cc(308) Failed to find drm render node path` +
        `wayland_buffer_manager_gpu.cc(487) Failed to initialize drm render node handle`. **DISAMBIGUATED (this
        session): NOT an EGL-config/GL-version breadth gap.** Advertising ES3 end-to-end (config
        `RENDERABLE_TYPE=EGL_OPENGL_ES3_BIT`, `GL_VERSION`="OpenGL ES 3.0", `GL_MAJOR/MINOR/NUM_EXTENSIONS` in
        `glGetIntegerv`) produced a **byte-identical crash** (same pc, same 0 frames) → the crash is independent
        of what the shim reports for the config, so the ES3 experiment was reverted (shim stays ES2 + the new
        entry points). The wall is the **render-node/dmabuf-device path**: chromium's `WaylandBufferManagerGpu`
        can't find a DRM render node to allocate the accelerated widget's buffer, the buffer handle is NULL, and
        chromium NULL-derefs when creating the surface. **Next target** = `dd-display`'s `zwp_linux_dmabuf_v1`
        must advertise a `main_device`/DRM render-node (+ a format/modifier table) the way chromium's ozone GPU
        probes for, and/or the Linux personality must make `/dev/dri/renderD128` discoverable via that probe
        (udev/drm-magic) — the guest already opens `renderD128` for `DD_IOCTL_GPU_ALLOC`, but chromium's ozone
        render-node *discovery* path doesn't find it. Downstream of the EGL shim; **gate-neutral** (dd-display
        Rust + os/linux `/dev/dri` enumeration, NOT the translator). **Debug technique:** `export DD_SHIM_DEBUG=1`
        in `~/.dd/gui/aarch64/bin/run-chrome.sh` logs the last EGL/GL shim call before a crash.
      - **Wall 6 CRASH CLEARED via the software (wl_shm) compositing path (2026-07-07) — the NULL-deref is
        NOT hit when GPU compositing is disabled, confirming Wall 6 is purely the render-node/GBM buffer
        path.** Running chromium with `--disable-gpu --disable-gpu-compositing` (the software `wl_shm`
        output path that already composites weston/SDL2/glmark2) **removes the deterministic guest SIGSEGV
        entirely** — chromium runs the render-node probe (`ozone_platform_wayland.cc(308) Failed to find
        drm render node path` + `wayland_buffer_manager_gpu.cc(487)` still log, now **non-fatal**), enumerates
        the display (`wayland_screen.cc Displays updated count:1`, `Display[5] 1920x1080`), and continues
        past the crash point into audio/media/vaapi init with **no** `[CRASH]`/`[MACH]`/SIGSEGV. So the GPU
        path's NULL-deref is specifically chromium allocating an accelerated (GBM/dmabuf) widget buffer with
        a NULL render-node handle; the software path never allocates one. **Verified across both
        `--in-process-gpu` multiprocess AND `--single-process`, identical.**
      - **Render-node probe pinned (`JTS=1`):** chromium's discovery does NOT `open("/dev/dri/renderD128")`
        or scan `/sys/class/drm` — the "Failed to find drm render node path" warning fires because chromium's
        ozone (`OzonePlatformWayland`/`GetDrmRenderNodePath`) derives the node path from the compositor's
        **`zwp_linux_dmabuf_v1` dmabuf-feedback `main_device`**, which dd-display (v3, no feedback) never
        advertises → empty path. So the *discovery* fix is compositor-side dmabuf feedback (main_device),
        and the *allocation* fix is engine-C GBM/DRM-ioctl emulation on the synth `renderD128` (the guest
        already opens it for `DD_IOCTL_GPU_ALLOC`, but chromium's GBM device creation does `DRM_IOCTL_VERSION`/
        `GET_CAP` + GBM allocs we don't emulate). Both are needed for the zero-copy GPU path; the software
        path needs **neither** — so the first-frame effort should ride the software path, which is blocked
        by Wall 7 below, not Wall 6.
      - **New gate-neutral diagnostic tooling landed in dd-display (Rust only):** `DD_DISPLAY_DEBUG=1` traces
        every dispatched Wayland request + registry bind (interface/version), and the client-disconnect log
        now prints the cause (`EOF (peer closed)` vs `io error`). Zero cost when unset; **no engine C, gate
        untouched.** With it, chromium's exact ozone bring-up over dd-display was captured:
        `get_registry → sync → bind{wl_compositor v4, wl_shm v1, wl_output v2, wl_seat v5, xdg_wm_base v1
        [, zwp_linux_dmabuf_v1 v3]} → sync → get_keyboard/get_pointer → wl_compositor.create_surface →
        clean EOF`. This early ozone connection is **short-lived by design** — chromium closes it right after
        create_surface (identical with/without dmabuf advertised, single- vs multi-process), never having
        attached a buffer, and does not reconnect within the window. dd-display serviced every request
        correctly (glmark2's 170+-frame IOSurface-dmabuf render still passes on the same rebuilt binary — no
        regression).
      - **★ Wall 7 (NEW, mapped 2026-07-07) — chromium startup IDLE-STALL: the browser main thread parks
        forever in its message pump waiting on a cross-thread wakeup that never arrives.** Past the Wall-6
        crash (software path), chromium reaches audio/media/vaapi init (~11 s of guest time) then goes
        **totally silent** — no window, no wl_shm buffer, no further logs for the rest of a 220 s window (so
        it's a hard stall, not slow-but-progressing). `JTS=1` shows the main thread's last syscalls are a
        tight idle loop: `clock_gettime`(113) + `futex`(98, op `0x80` = `FUTEX_WAIT|PRIVATE`, val 2 =
        "locked-with-waiters") + `epoll_pwait`(22, epfd 19) + `getpid`(172), repeating — i.e. chromium is
        **blocked, not crashed**, waiting for an event/futex-wake from another thread (a posted-task / Mojo
        reply / eventfd signal) that never fires. The multiprocess variant shows the same class: the renderer
        child stalls at `child_thread_impl.cc(957) ChildThreadImpl::EnsureConnected()` (waiting on an IPC that
        never completes). **Wall 7 is an engine-level threading/IPC fidelity gap** (a lost cross-thread
        futex/eventfd wakeup under chromium's heavy multi-thread startup), **downstream of Wall 6 and NOT a
        dd-display or Wayland-protocol gap** — dd-display answered every request chromium sent. This is the
        real blocker for a first painted frame (software OR GPU): even a perfect compositor can't paint until
        chromium's own startup unblocks. Next: instrument the engine's futex/eventfd emulation for a dropped
        wake under contention (`futex` case + `eventfd2`/`epoll` in os/linux), or bisect which chromium
        subsystem's async init the main loop is awaiting (attach + guest-thread sample at the stall).
      - **★ Session update (2026-07-07, first-frame push) — Wall 8 (wl_output) RETIRED; a NEW GPU-process
        `/proc` FATAL found + cleared; Wall 7 CONFIRMED on the FRESH engine as the sole remaining blocker.**
        - **Harness consolidated:** the drifted `chrome-shm.sh`/`chrome-shm-jts.sh`/`chrome-run.sh` scripts are
          reconciled into ONE driver `target-mac/chrome-first-frame.sh` — reaps orphans, clears the persistent
          chromium `SingletonLock` + stale `/tmp/.org.chromium.*` in the workspace upper, swaps in the musl
          shim (restores glibc via an EXIT trap), starts `dd-display --metal --png` and launches `chromiumws`
          on the SAME `$WORK` sockets (`DD_DISPLAY_SOCK`/`DD_GPU_EXEC_SOCK` aligned — the old `/tmp`-vs-`$WORK`
          mismatch is gone), runs chromium under a pty with `DD_DISPLAY_DEBUG=1`, self-dumps launch stderr +
          wire trace + frames. `~/.dd/gui/aarch64/bin/run-chrome.sh` now defaults to the software path and is
          documented flag-by-flag.
        - **NEW wall found + cleared — GPU-process `/proc` thread-stop FATAL:** the plain software args
          (`--disable-gpu --disable-gpu-compositing`) make chromium's **GPU process** die at
          `FATAL:thread_helpers.cc(104) Stopped thread does not disappear in /proc (iterations: 30)` during its
          sandbox bring-up (`sandbox::ThreadHelpers::StopThreadAndWatchProcFS` polls `openat(procfd,"self/task/")`
          for a stopped thread to leave `/proc/self/task`; our synth reports a constant 1-entry task dir, so the
          count never satisfies the check). **Cleared (flags, gate-neutral)** by adding
          `--disable-gpu-sandbox --disable-seccomp-filter-sandbox --disable-setuid-sandbox --in-process-gpu`:
          `--in-process-gpu` runs viz in the never-sandboxed browser process so the thread-stop path is never
          taken. (A proper multiprocess fix would need engine `/proc/self/task` to reflect real live-thread
          count — deferred; the in-process path suffices for first-frame bring-up.)
        - **Wall 8 (suspected wl_output gap) RETIRED — dd-display's roundtrip is COMPLETE + correct.** With
          `WAYLAND_DEBUG=1` on chromium (libwayland prints every request/event), chromium's ozone init over
          dd-display completes cleanly: `get_registry → sync → bind{wl_compositor v4, wl_shm v1, wl_output v2,
          wl_seat v5, xdg_wm_base v1} →` receives `wl_output.geometry/mode(1920x1080@60,current|preferred)/
          scale(1)/done`, `wl_seat.capabilities(3)+name`, both `wl_callback.done` roundtrips, `get_keyboard/
          get_pointer`, `create_surface(wl_surface@11)` — every reply chromium waited for arrived. So the
          earlier "chromium never got wl_output.done" hypothesis was WRONG; there is no dd-display/Wayland
          roundtrip gap at init. (The xdg_surface/toplevel configure + shm-buffer commit path is unexercised —
          chromium stalls before reaching it — but reads correct in `server.rs::commit`.)
        - **Wall 7 CONFIRMED on the fresh engine (build 13:55, AFTER the 13:49 proc.c edit → the claimed
          eventfd-wakeup fix IS in this binary).** Past wayland init, chromium hard-stalls in browser/GPU
          bring-up right after audio/ALSA + `media_stream_manager` + `vaapi_wrapper` init — no
          `get_xdg_surface`, no toplevel, no wl_shm buffer, no frame — for a full 270 s window (hard stall, not
          slow). Identical across `--single-process`, `--in-process-gpu` multiprocess, and plain multiprocess.
          The multiprocess renderer child never completes `child_thread_impl.cc(957) EnsureConnected()`. This is
          the documented Wall-7 cross-thread wakeup gap and it is UNCHANGED by the shipped fix — it is now the
          single blocker for a first frame. The engine `futex` (per-address bucket, WAIT re-checks `*uaddr` under
          the same `b->m` the WAKE broadcasts under — lost-wake-safe) and `epoll`/`eventfd` (EVFILT_USER wake
          knote + prime buffers for cross-thread readiness) paths both read correct on static inspection, so the
          lost wake is subtler than a single obvious bug; **next step is a live guest-thread sample at the stall**
          (attach + per-tid backtrace) to identify exactly which posted-task/CV wake is dropped — NOT a blind
          engine patch (gate risk). **No engine C was touched this session; gate remains 1636/0 untouched.**
      - **Repro harnesses added (`target-mac/`):** `chrome-first-frame.sh` (THE consolidated first-frame driver,
        software `wl_shm` + `DD_DISPLAY_DEBUG` + `WAYLAND_DEBUG`), `chrome-shm.sh` (software `wl_shm` path, `DD_DISPLAY_DEBUG`
        wire trace, no dmabuf/iosurface), `chrome-shm-jts.sh` (same + `JTS=1` for the Wall-7 stall syscalls),
        `chrome-dbg.sh` (GL path + wire trace), `chrome-wall6-jts.sh` (render-node probe trace). The shim
        `~/.dd/gui/aarch64/bin/run-chrome.sh` documents both the GL default and the software `CHROME_ARGS_SW`.
      - **SIGTRAP → CRASHDBG (added 2026-07-07).** The `CRASHDBG` crash handler now catches guest `brk`: the POSIX
        path adds `SIGTRAP` (covers forked children), and — because macOS routes a guest `brk` through Mach
        `EXC_BREAKPOINT` *before* POSIX signals — `install_mach_exc` now also registers `EXC_MASK_BREAKPOINT`
        (mapped to `SIGTRAP` for the guest-handler check) and the `[MACH]` report appends the **guest** pc
        (`gpc=`). CRASHDBG-gated, so zero effect on normal runs or the gate.
      - **Repro/diagnosis harness staged** (all under `target-mac/`, fresh engine forced via
        `DDJIT_DIR=target/release/build/dd-jit-darwin-5b0dabfbe6f0af2e/out`; mac-native launchers from
        `target-mac/release`): `chrome-version-test.sh` (A/B `--version` confirmation), `chrome-gl-run.sh` (the
        workspace GL `about:blank` run + dd-display), `chrome-sample.sh` / `chrome-lldb.sh` / `chrome-lldb2.sh`
        (the Wall-2 saturation diagnosis), plus workspace `chromiumws` + `chrome-run.sh`/`run-chrome.sh`.

---

## 8. Validation order — spike these before committing ABIs

1. **Toolkit-via-shm reality** (pull M3's risk into M1): does a *current* GTK4/Qt6 app draw via
   `wl_shm`, or demand dmabuf/EGL even for "software"? Test `weston-simple-shm`→SDL2→GTK3→GTK4. Decides
   MVP scope and how soon rung 2 is mandatory.
2. **shm fd cross-process `mmap` on macOS** (M0) — the whole CPU design.
3. **`makeBuffer(bytesNoCopy)` against a real pool** — 16 KB alignment, length rounding, single-VM-
   region, stride/BGRA/premultiply/Y-flip — with a known test pattern.
4. **IOSurface cross-process handoff on *current* macOS** — confirm `IOSurfaceCreateMachPort`/
   `LookupFromMachPort` + `mach_port_deallocate` against today's headers (one Chromium mach claim was a
   refuted iOS-only path — do not copy it).
5. **virglrenderer+venus on macOS with KosmicKrisp** — the rung-3 make-or-break; prototype the vtest
   server replaying onto a Metal-backed Vulkan before designing the IOSurface memory-model port.
6. **CAContext/CALayerHost guest-window adoption** — can an interposed guest export a contextID that
   `dd-display` hosts, without fighting the guest's own `NSApplication`?
7. **winit + Smithay multi-client on one `NSApp`** — many containers' windows + non-blocking dispatch.

---

## 9. Deliberately deferred

XWayland (after software Wayland), clipboard/drag-drop, multi-output/HiDPI polish, PI/robust edge cases,
full AppKit fidelity for macOS guests, and the in-process same-address-space fast path (optimization).
