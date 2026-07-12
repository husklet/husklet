# Surface Checklist — the Wayland surface a working Chromium exercises over dd-display

This is the "complete surface" spec: every Wayland global, object, and request a real Chromium
client drives over the dd host compositor, cross-referenced against what the Smithay-native
`dd-compositor` (the `DD_DISPLAY_SMITHAY=1` path) implements today. It is the gap list that drives
"a compositor Chrome can fully run against."

## How this was captured

- **Chrome usage** — decoded from a server-side wire trace of a real Chromium run over the legacy
  `dd-display` (`DD_DISPLAY_DEBUG=1`), a GPU/dmabuf run in which Chromium completed its full
  ozone/wayland bring-up, mapped a toplevel, and presented frames (`client disconnected (25 frame(s))`).
  Every `bind`, every `(interface, opcode)` request pair below is from that trace. A software
  (`--disable-gpu`, `wl_shm`) run is identical minus the `zwp_linux_dmabuf` rung.
- **dd-compositor coverage** — read from `dd-compositor/src/lib.rs` (the `DdState` handlers +
  `delegate_*!` set) and `dd-compositor/src/main.rs`.

Chromium binds globals lazily and tolerates the absence of ones it cannot find (it probes and moves
on), so "used-by-Chrome" means "bound in the trace" unless noted; a couple of globals Chrome only
binds on demand (themed cursor) are marked accordingly.

## Global gap table

| Global | Ver (Chrome binds) | Used by Chrome | In dd-compositor | Notes |
|---|---|---|---|---|
| `wl_compositor` | 4 | yes | **yes** (v5) | `delegate_compositor!`. Chrome binds v4; compositor offers v5. |
| `wl_subcompositor` | 1 | yes | **yes** | Same `delegate_compositor!`. Chrome binds it during ozone init; handlers inert until a client actually nests subsurfaces. |
| `wl_shm` | 1 | yes | **yes** | `ShmState` advertises Argb8888/Xrgb8888. The CPU-raster present path. |
| `wl_output` | 4 | yes | **yes** (v4 + `xdg_output`) | `OutputManagerState::new_with_xdg_output`. Advertises HiDPI `scale` from `Presenter::output_scale()` (= `backingScaleFactor`, 2 on Retina). Legacy path advertises `wl_output` only; compositor adds `xdg_output` (superset). |
| `wl_seat` | 5 | yes | **yes** (v5) | Keyboard (XKB via libxkbcommon) + pointer. |
| `xdg_wm_base` | 1 | yes | **yes** | `XdgShellState`: `xdg_surface` / `xdg_toplevel` / `xdg_popup` / `xdg_positioner`. |
| `wp_viewporter` | 1 | yes | **yes** | `ViewporterState`. Source-crop + dst-size honoured in `build_surface_buffer` (Chrome's fractional-scale path). |
| `wp_presentation` | 1 | yes | **yes** | `PresentationState`, clock = Linux `CLOCK_MONOTONIC` (1). Drives viz's BeginFrame vsync estimator; `presented`/`discarded` answered per commit. |
| `wp_cursor_shape_manager_v1` | 1 | on demand | **yes** | `CursorShapeManagerState` + `delegate_cursor_shape!`. Chrome binds it only when it first needs a themed cursor (pointer-over-link/text); not bound in the captured trace. `set_shape` → `NSCursor` proven by the compositor integration test. |
| `wl_data_device_manager` | 3 | **yes** | **NO** ❌ | Chrome binds it during init and calls `get_data_device`. **Gap**: no clipboard / drag-and-drop on the Smithay path. Inert on legacy too, but at least advertised so Chrome's bind succeeds; on dd-compositor the bind currently fails (global absent). |
| `zwp_linux_dmabuf_v1` | 3 (v4 for feedback) | **yes (GPU path)** | **NO** ❌ | The accelerated path: Chrome's GPU process imports its ANGLE/Metal render target as a dmabuf/IOSurface and commits it zero-copy. **Biggest gap** — without it, hardware-composited Chrome (and every GLES client: glmark2/es2tri/es2tex) cannot present on the Smithay path; only `wl_shm` (CPU raster) works. Legacy `dd-display` advertises it under `DD_DISPLAY_DMABUF=1`. |
| `surface_augmenter` | — | probes, tolerates absence | n/a | ChromeOS-only (exo). Stock Weston never advertises it; legacy keeps it OFF by default (`DD_DISPLAY_AUGMENTER=1` to force). Not needed. |

### Summary of gaps (Smithay path, in priority order)

1. **`zwp_linux_dmabuf_v1`** — blocks the entire accelerated/zero-copy present path. Required for
   GPU-composited Chrome and for every GLES demo client. This is the single change that unblocks
   "Chrome renders through dd-compositor with the GPU on." The legacy path's dmabuf + mach-port
   IOSurface bridge (`DD_DISPLAY_DMABUF=1`) is the reference implementation to port behind the
   Smithay `DmabufState` + a `DmabufHandler`.
2. **`wl_data_device_manager`** — Chrome binds it every launch; absence means the bind fails and
   there is no clipboard/DnD. Smithay ships `DataDeviceState` + `delegate_data_device!`; wiring it
   is mechanical (no platform seam needed for a stub selection).
3. **`wl_pointer.set_cursor` (client-surface cursor)** — see request table: Chrome drove
   `wl_pointer.set_cursor` 20× in the trace (classic buffer cursor, distinct from
   `wp_cursor_shape`). dd-compositor's `SeatHandler::cursor_image` handles `Named` (themed) but
   ignores `Surface` (client-provided cursor buffer), so a client bitmap cursor falls back to the
   host default. Minor, but it is a real request Chrome sends.

## Object / request coverage (what Chrome actually calls)

Every `(interface, request)` observed in the trace. All of these dispatch correctly on dd-compositor
**for the globals it implements** (Smithay's `delegate_*!` generate the request handlers); the rows
under a ❌ global are unreachable on the Smithay path until that global lands.

| Interface | Requests Chrome issued | dd-compositor |
|---|---|---|
| `wl_display` | `sync`, `get_registry` | yes |
| `wl_registry` | `bind` | yes |
| `wl_compositor` | `create_surface`, `create_region` | yes |
| `wl_surface` | `attach`, `damage`, `frame`, `set_opaque_region`, `set_input_region`, `commit`, `set_buffer_scale`, `damage_buffer` | yes (frame callbacks + viewport/buffer_scale honoured; opaque/input regions parsed by Smithay) |
| `wl_region` | `add`, `destroy` | yes |
| `wl_shm` | `create_pool` | yes |
| `wl_shm_pool` | `create_buffer`, `resize`, `destroy` | yes |
| `wl_buffer` | `destroy` | yes |
| `wl_seat` | `get_pointer`, `get_keyboard`, `get_touch` | pointer + keyboard yes; touch focus type declared, no touch device |
| `wl_pointer` | `set_cursor` (20×) | partial — themed (`wp_cursor_shape`) yes; client-surface cursor ignored |
| `xdg_wm_base` | `get_xdg_surface`, `pong` | yes |
| `xdg_surface` | `get_toplevel`, `set_window_geometry`, `ack_configure` | yes |
| `xdg_toplevel` | `set_title`, `set_app_id`, `set_max_size`, `set_min_size`, `unset_maximized` | title captured (window label); min/max/maximize state parsed by Smithay, not enforced by the single-window backend. **`move`/`resize` grabs not wired** → no interactive window drag on Smithay yet (legacy has `perform_window_drag`). |
| `wp_viewporter` / `wp_viewport` | `get_viewport`, `set_source`, `set_destination`, `destroy` | yes |
| `wl_data_device_manager` | `get_data_device` | ❌ global absent |
| `zwp_linux_dmabuf_v1` | `create_params` (+ `get_default_feedback` on v4) | ❌ global absent |
| `zwp_linux_buffer_params` | `add`, `create_immed`, `destroy` | ❌ (dmabuf) |

## Bottom line

dd-compositor already covers the **core surface** — `wl_compositor`/`wl_subcompositor`, `wl_shm`,
`xdg_wm_base` (surface/toplevel/popup), `wl_seat`, `wl_output`(+`xdg_output`), `wp_viewporter`,
`wp_presentation`, `wp_cursor_shape` — i.e. everything a **software-rendered** (`wl_shm`) Chromium
needs to bring up, map a toplevel, present, and pace frames. Reaching a **fully working accelerated
Chromium** requires, in order: `zwp_linux_dmabuf_v1` (GPU present), `wl_data_device_manager`
(clipboard/DnD), the `wl_pointer.set_cursor` client-cursor case, and the `xdg_toplevel` move/resize
grabs for interactive window management.
