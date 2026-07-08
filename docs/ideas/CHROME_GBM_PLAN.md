# Chrome accelerated first-frame: the Mesa-GBM wall

Scope: the *next* wall on Chromium's **accelerated** (GPU/dmabuf) path, one step past render-node
discovery. dd already synthesizes `/dev/dri/renderD128` (+ `card0`), answers `DRM_IOCTL_VERSION` /
`GET_CAP` / `SET_CLIENT_CAP`, and Chromium's `drmGetDevices2` now finds the node
(`vfs.c::drm_synth_*`, `fs.c` case 29 — gate-green). The wall now is **Mesa GBM buffer allocation**:

```
MESA-LOADER: failed to open dd_gpu: .../dri/dd_gpu_dri.so  No such file
  → gbm_create_device() returns NULL
  → Chromium NULL-derefs the gbm device  → deterministic guest SIGSEGV
```

Goal: make `gbm_create_device` + `gbm_bo_create` succeed and export a dmabuf that resolves to dd's
existing IOSurface, **without writing a full Mesa DRI GL driver.**

---

## 0. The make-or-break finding: which GBM does Chromium use?

**Chromium uses Mesa's `libgbm` (the DRI-loader GBM), NOT its bundled minigbm.** This is *decisive*
from the error text alone — no image inspection needed:

- The literal string `MESA-LOADER:` is emitted only by Mesa's loader (`src/loader/loader.c`,
  `loader_open_driver`). minigbm never prints it.
- The `<name>_dri.so` dlopen pattern (`dd_gpu_dri.so`) is Mesa's `gbm_dri` backend
  (`src/gbm/backends/dri/gbm_dri.c` → `loader_get_driver_for_fd` → `<kernel_driver>_dri.so`).
  minigbm compiles its backends *in* (dumb/vgem/…); it never dlopens a `_dri.so` and never consults
  `DRM_IOCTL_VERSION.name` to pick one.
- The name it tried, `dd_gpu`, is exactly what our `DRM_IOCTL_VERSION` handler reports
  (`vfs.c::drm_synth_ioctl`, `static const char NM[] = "dd_gpu"`). So Mesa read our version name and
  built `dd_gpu_dri.so` from it — confirming the Mesa-libgbm code path end to end.

**Consequence:** the minigbm route (route 2 below) is *not what this Chromium build does* — rule it
out. The viable answer is to **reuse Mesa's software GBM backend, `kms_swrast`**, which the same
`libgbm` already knows how to load.

### Why `gbm_create_device` returns NULL today (the important subtlety)

Mesa's `gbm_dri` already has an automatic **software fallback**. `dri_device_create()` does roughly:

```c
if (!GBM_ALWAYS_SOFTWARE) ret = dri_screen_create(dri);        // hw: loads <name>_dri.so
if (ret) ret = dri_screen_create_sw(dri);                       // fallback: loads kms_swrast_dri.so
if (ret) return -1;                                             // both failed  → gbm_create_device NULL
```

So the `MESA-LOADER: failed to open dd_gpu` line is only the **hw attempt**. Mesa *should* then fall
back to `kms_swrast`. The fact that `gbm_create_device` still returns NULL means the **fallback also
failed** — overwhelmingly likely because **`kms_swrast_dri.so` is not present in the alpine-chrome
rootfs** (Alpine ships it in `mesa-dri-gallium`; a chromium-only image usually omits it — it only
needs `libgbm.so.1`, which *is* present since the loader ran). If `kms_swrast_dri.so` were present,
`gbm_create_device` would already succeed and the crash would have moved downstream to
`gbm_bo_create` (the first `DRM_IOCTL_MODE_CREATE_DUMB`).

> **The one thing to verify when the image is on-disk** (empty on this host — `~/.dd/images` and
> `~/.dd/gui` are unpopulated, no run): inside the alpine-chrome rootfs, list
> `find / -name 'kms_swrast_dri.so' -o -name 'swrast_dri.so' -o -name 'libgallium*.so*' -o -name
> 'libgbm.so*'` and `nm -D $(which chromium's-libgbm)`. Expectation: `libgbm.so.1` present,
> `kms_swrast_dri.so` **absent**. If it is in fact present, route 1 collapses to "just add the dumb
> ioctls + env override" (no staging step).

---

## 1. Route 1 — reuse Mesa's software GBM (`kms_swrast`) — RECOMMENDED

Make our synth render node look, to Mesa's existing `libgbm`, like a dumb-buffer-capable KMS device,
and let Mesa's shipped `kms_swrast_dri.so` do the GBM bookkeeping while dd services the underlying
DRM dumb/PRIME ioctls out of its existing IOSurface path. **No new GL driver — we reuse Mesa's.**

Because dd *fully synthesizes* every ioctl on the node, the usual "render nodes can't do dumb
buffers" kernel rule does not bind us: we simply answer `CREATE_DUMB` on the render-node fd. Whether
Chromium opens `renderD128` or `card0` for GBM is immaterial — the engine controls the replies.

### 1a. Three pieces

**(i) Ship the driver `.so` + point Mesa at it (packaging, gate-neutral).**
Stage `kms_swrast_dri.so` and its shared payload (`libgallium-*.so`, and `libglapi.so.0` if the build
splits it) into a DRI dir — the same staging mechanism already used for the ANGLE EGL shim
(`~/.dd/gui/aarch64/lib.musl-chrome/`). Then set, in the chrome launcher env (alongside the existing
`DD_GPU_IOSURFACE=1`):

```
MESA_LOADER_DRIVER_OVERRIDE=kms_swrast     # step-1 of loader_get_driver_for_fd: skip dd_gpu_dri.so entirely
LIBGL_DRIVERS_PATH=<staged dri dir>        # where Mesa dlopens kms_swrast_dri.so from
GBM_ALWAYS_SOFTWARE=1                       # (belt-and-braces) force the sw path in gbm_dri
```

`MESA_LOADER_DRIVER_OVERRIDE` is honored as the **first** branch of `loader_get_driver_for_fd`, so
Mesa never attempts `dd_gpu_dri.so` and goes straight to `kms_swrast` — which means we don't even
have to change the `DRM_IOCTL_VERSION` name. (These are pure env; zero engine/gate impact.)

Source for `kms_swrast_dri.so`: build it once in arm64 Alpine via orbstack (same route the EGL shim
used — `apk add mesa-dri-gallium`, copy `/usr/lib/xorg/modules/dri/kms_swrast_dri.so` +
`libgallium-*.so`) and stash it in the shim dir. No download on this host; this is a build-when-image-
is-available step.

**(ii) Add the dumb-buffer + PRIME ioctls to `drm_synth_ioctl` (the only engine C).**
Mesa's `kms_swrast` GBM path (`src/gallium/winsys/sw/kms-dri/kms_dri_sw_winsys.c`) uses exactly this
DRM ioctl set. Route every one to the **existing** `dd_gpu_alloc` / `g_gpu_reg` IOSurface machinery
(`vfs.c:3502-3529, 3535`). Numbers are aarch64/LP64 `_IOC` encodings (`'d'`=0x64) — confirm against
`<drm/drm.h>` / `<drm/drm_mode.h>` when wiring:

| ioctl | number | arg struct | dd action |
|---|---|---|---|
| `DRM_IOCTL_MODE_CREATE_DUMB` | `0xc02064b2` | `{u32 height,width,bpp,flags; u32 handle,pitch; u64 size}` | call `dd_gpu_alloc(fd,…)` with w/h → fill `handle`=(our IOSurface id or a small handle keyed to `g_gpu_reg`), `pitch`=stride(=w*4), `size`=stride*h |
| `DRM_IOCTL_MODE_MAP_DUMB` | `0xc01064b3` | `{u32 handle,pad; u64 offset}` | return a synthetic `offset` that encodes the handle (e.g. `handle << 12`); the mmap handler (piece iii) decodes it |
| `DRM_IOCTL_MODE_DESTROY_DUMB` | `0xc00464b4` | `{u32 handle}` | release that IOSurface (`CFRelease` + clear its `g_gpu_reg` slot) |
| `DRM_IOCTL_GEM_CLOSE` | `0x40086409` | `{u32 handle,pad}` | drop the handle ref (same as destroy for our 1:1 handle↔surface model) |
| `DRM_IOCTL_PRIME_HANDLE_TO_FD` | `0xc00c642d` | `{u32 handle,flags; s32 fd}` | return the throwaway dmabuf fd `dd_gpu_alloc` already mints, and tag it so the `zwp_linux_dmabuf` add-param carries the IOSurface **id in the modifier** (dd's existing `DD_DMABUF_MOD_MAGIC` convention, `dd_gpu.h`) |
| `DRM_IOCTL_PRIME_FD_TO_HANDLE` | `0xc00c642e` | `{u32 handle,flags; s32 fd}` | (import — usually unused on the render side; wire only if Chromium imports) |
| `DRM_IOCTL_GET_CAP` DUMB/PRIME/ADDFB2 | `0xc0106c0c` | — | **already handled** (returns DUMB_BUFFER=1, PRIME=3, ADDFB2_MODIFIERS=1) |
| `DRM_IOCTL_SET_CLIENT_CAP` | `0x400c6d0d` | — | **already handled** (no-op OK) |

Note `dd_gpu_alloc` already returns `{ptr,id,stride,fd}` and maintains a per-fd registry keyed by
`(owner_fd,w,h)` with reuse-in-place — so CREATE_DUMB is a thin adapter, not new allocation logic. Use
the IOSurface id (or a compact index into `g_gpu_reg`) as the DRM `handle`; add a
`handle → g_gpu_reg slot` lookup so MAP/DESTROY/PRIME can find the surface.

**(iii) Handle `mmap` of the render-node fd at the MAP_DUMB offset (small engine C).**
`kms_swrast` maps the dumb buffer for CPU software rendering: after `MAP_DUMB` it calls
`mmap(size, PROT_READ|WRITE, MAP_SHARED, drm_fd, offset)`. Intercept `mmap` when `fd` is a synth DRM
node (`g_devdri[fd]`): decode the handle from `offset`, look up the surface, and return its existing
base VA (`g_gpu_reg[i].base`) — **no real mmap** (guest VA == host VA, the buffer already lives at
that address). Integration point: `syscall/mem.c` (mmap handler) with the same `g_devdri[fd]` gate
`fs.c` already uses. On `munmap` of that range, no-op (lifetime is tied to DESTROY_DUMB / fd close).

### 1b. What clears at this wall, and what's the *next* wall

With 1a in place, `gbm_create_device` returns non-NULL, `gbm_bo_create[_with_modifiers]` allocates an
IOSurface-backed dumb buffer, and `gbm_bo_get_fd`/PRIME hands Chromium a dmabuf that dd-display already
resolves to that IOSurface (the glmark2 IOSurface-dmabuf path composites 170+ frames on this exact
mechanism — no regression risk to the compositor). **The deterministic Wall-6 NULL-deref SIGSEGV is
gone.**

The *next* wall (out of scope here, but name it so no one is surprised): Chromium's GPU process wants
the buffer it **rendered into via ANGLE** to be the **same** buffer it exports as the dmabuf — i.e. it
imports the `gbm_bo`'s dmabuf as an `EGLImage` (`EGL_LINUX_DMA_BUF_EXT`) and binds it as the GL render
target. Our ANGLE EGL shim (`gl_shim.c`) would then need `eglCreateImageKHR(EGL_LINUX_DMA_BUF_EXT)` to
map to the same IOSurface. Clearing the GBM wall lets Chromium *proceed past the crash*; full
same-buffer binding is the follow-on. (And independently, the browser-main-thread **Wall 7** idle-stall
still gates a painted frame on either path — see RENDERING_PLAN.md §Wall 7.)

### 1c. Effort / risk

Bounded and low-risk: ~6 ioctl cases + one mmap branch, all behind the existing `DD_GPU_IOSURFACE`
gate (inert otherwise → gate stays byte-exact), reusing the shipped IOSurface path; plus a one-time
`.so` staging + 3 env vars. No new GL driver, no DRI-extension ABI to implement. **This is the
recommendation.**

---

## 2. Route 2 — minigbm — NOT APPLICABLE (ruled out)

Only relevant if Chromium used its own bundled minigbm. The `MESA-LOADER` / `dd_gpu_dri.so` evidence
proves it uses Mesa libgbm, so minigbm is not in this binary's path. (For the record, had it been
minigbm, the matching backend would be `dumb`/`vgem` and it would need the *same* CREATE/MAP/DESTROY
DUMB + PRIME ioctls as route 1 — so the engine work overlaps — but there is no minigbm `.so` to feed
and no `MESA_LOADER_*` lever. Do not pursue.)

---

## 3. Route 3 — Chromium flag to bypass GBM (software `wl_shm`)

`--disable-gpu --disable-gpu-compositing` (+ the sandbox-disable flags already documented) makes ozone
use `wl_shm` and never allocate a GBM buffer — this **already removes the Wall-6 SIGSEGV** (confirmed,
RENDERING_PLAN.md §"Wall 6 CRASH CLEARED"). But this is the **software** output path, not the
accelerated path this task targets, and it then hits the separate **Wall 7** ~270 s browser-thread
idle-stall before any frame. Useful as a *diagnostic control* (isolates GBM from everything else); not
a solution for the accelerated first frame. There is no known flag that keeps GPU compositing on yet
bypasses GBM once a render node is present — GBM is how ozone-wayland's GPU path allocates its buffers.

---

## 4. Route 4 — tiny custom `dd_gpu_dri.so` (GBM-only DRI backend)

If `kms_swrast_dri.so` genuinely cannot be shipped, write a minimal DRI driver named `dd_gpu` so
Mesa's `loader_open_driver` finds `dd_gpu_dri.so`. **Assessment: this is a rabbit hole, not a bounded
task** — worse than route 1. Mesa's `gbm_dri` does not talk to a "gbm ABI"; it consumes the DRI
extension mechanism: the `.so` must export `__driDriverGetExtensions_dd_gpu` returning a versioned
`__DRIextension*` array, and `gbm_dri` requires a working subset of `__DRI_CORE`, `__DRI_MESA`,
`__DRI_IMAGE` (`createImageFromFds`, `queryImage`, `createImageFromDmaBufs`, `mapImage`/`unmapImage`,
`destroyImage`), and a swrast/kopper screen-create extension — each a struct of function pointers with
version negotiation that shifts across Mesa releases. Reimplementing that surface correctly (and
keeping it matched to the exact Mesa the image ships) is substantially more code and more fragile than
reusing Mesa's own `kms_swrast`. **Only if route 1's `.so` is truly unavailable, and even then prefer
building `kms_swrast` from source over authoring a DRI driver.**

---

## 5. Recommendation & exact engine change

**Do route 1.** It reuses Mesa's software GBM, so the engine never learns the DRI/GL driver ABI.

Concretely, three deliverables, in order:

1. **Verify** (needs the image on disk): `kms_swrast_dri.so` presence in the alpine-chrome rootfs. If
   present → skip staging. If absent → build it in arm64 Alpine (`mesa-dri-gallium`) and stage it
   beside the EGL shim in `~/.dd/gui/aarch64/lib.musl-chrome/dri/`.

2. **Env only** (launcher, gate-neutral): add `MESA_LOADER_DRIVER_OVERRIDE=kms_swrast`,
   `LIBGL_DRIVERS_PATH=<staged dri dir>`, `GBM_ALWAYS_SOFTWARE=1` to the chrome launch env next to
   `DD_GPU_IOSURFACE=1`.

3. **Engine C** (`drm_synth_ioctl` in `vfs.c`, all behind the existing `gpu_iosurface_on()` gate, so
   the test gate is untouched):
   - `DRM_IOCTL_MODE_CREATE_DUMB (0xc02064b2)` → `dd_gpu_alloc`, return `handle/pitch/size`.
   - `DRM_IOCTL_MODE_MAP_DUMB (0xc01064b3)` → return `offset = handle<<12` (or any reversible tag).
   - `DRM_IOCTL_MODE_DESTROY_DUMB (0xc00464b4)` + `DRM_IOCTL_GEM_CLOSE (0x40086409)` → release the
     surface / drop the handle.
   - `DRM_IOCTL_PRIME_HANDLE_TO_FD (0xc00c642d)` → return the `dd_gpu_alloc` dmabuf fd, carry the
     IOSurface id in the dmabuf modifier (`DD_DMABUF_MOD_MAGIC`).
   - Keep `DRM_IOCTL_VERSION` and `GET_CAP` as-is (DUMB_BUFFER=1, PRIME=3, ADDFB2_MODIFIERS=1 already
     correct). The version `name` no longer matters once `MESA_LOADER_DRIVER_OVERRIDE` is set.
   - Add a `handle → g_gpu_reg` map (small static table) so MAP/DESTROY/PRIME resolve the surface.
   - **`mmap` branch** in `syscall/mem.c`: when `g_devdri[fd]` and `offset` is a MAP_DUMB tag, return
     the surface's existing base VA (no real mapping).

Net new engine surface: ~6 ioctl cases + one mmap branch, all IOSurface-backed and gate-inert — the
smallest change that turns `gbm_create_device` from NULL into a working IOSurface-backed GBM device on
the accelerated path.
