//! The minimal Wayland compositor endpoint: the four core globals plus `xdg_shell`, enough for a
//! `wl_shm` client (`weston-simple-shm`, SDL2) to map a pool, attach a buffer, and commit. Per committed
//! surface we extract a tight BGRA framebuffer and hand it to a [`Presenter`] (a native window on macOS;
//! a PNG dump for headless verification). One [`Server`] drives one client connection.
//!
//! Interface/opcode numbers are the stable Wayland wire opcodes (ordered as in the protocol XML). See
//! `docs/ideas/RENDERING_PLAN.md` §1/§4.

use crate::present::{PopupPlacement, Presenter, SurfaceBuffer};
use crate::wire::{Conn, Message};
use std::collections::HashMap;
use std::os::unix::io::RawFd;

/// Opt-in wire trace (DD_DISPLAY_DEBUG): logs each dispatched request + registry binds, so a complex
/// client (chromium's ozone-wayland) can be watched request-by-request. Zero cost when unset.
pub fn dbg_on() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("DD_DISPLAY_DEBUG").is_ok())
}

// --- global names advertised by wl_registry ---
const G_COMPOSITOR: u32 = 1;
const G_SHM: u32 = 2;
const G_XDG_WM_BASE: u32 = 3;
const G_SEAT: u32 = 4;
const G_OUTPUT: u32 = 5;
const G_DMABUF: u32 = 6;
// Globals chromium's ozone-wayland ALWAYS binds during connection init. Their absence is a real
// divergence from a stock compositor (weston/mutter), so advertise them with minimal/inert handlers so
// chromium's `WaylandConnection::Initialize` sees the same global set it expects.
const G_SUBCOMPOSITOR: u32 = 7;
const G_VIEWPORTER: u32 = 8;
const G_DATA_DEV_MGR: u32 = 9;
const G_SURFACE_AUGMENTER: u32 = 10;
const G_PRESENTATION: u32 = 11;
// wp_cursor_shape_manager_v1: lets the client name a themed pointer shape (default/pointer/text/grab/…)
// instead of committing a cursor buffer. Chrome uses it for its cursors when advertised; we map each
// shape to the matching host NSCursor. See cursor-shape-v1.xml.
const G_CURSOR_SHAPE: u32 = 12;

// wp_presentation clock domain. The `feedback.presented` timestamp is read back by the CLIENT via its own
// clock_gettime(), so this must be the value the *guest* (Linux) libc uses for CLOCK_MONOTONIC (== 1),
// NOT the host macOS libc::CLOCK_MONOTONIC (== 6). Weston reports the compositor's presentation clock the
// same way (libweston `compositor->presentation_clock`).
const CLOCK_MONOTONIC_LINUX: u32 = 1;
// wp_presentation_feedback.kind bit: presentation was vsync'd (tearing-free). We composite/copy on a
// display-synced CADisplayLink-equivalent, so advertise vsync but not zero_copy/hw_clock/hw_completion.
const WP_PRESENTATION_KIND_VSYNC: u32 = 0x1;
// The single mode we advertise on wl_output (mHz). Also drives the `presented.refresh` interval.
const OUTPUT_REFRESH_MHZ: i32 = 60000;
const OUTPUT_WIDTH: i32 = 1920;
const OUTPUT_HEIGHT: i32 = 1080;

// wl_shm formats (the two mandatory ones). Memory byte order is BGRA (little-endian ARGB word).
const FMT_ARGB8888: u32 = 0;
const FMT_XRGB8888: u32 = 1;
// DRM fourccs for zwp_linux_dmabuf format events.
const DRM_FMT_ARGB8888: u32 = 0x3432_5241; // 'AR24'
const DRM_FMT_XRGB8888: u32 = 0x3432_5258; // 'XR24'
                                           // dd-private dmabuf modifier: modifier_lo = IOSurface id; modifier_hi low-16 = magic tag; bit 16 of
                                           // modifier_hi = "the guest asked the host GPU to RENDER into this surface" (rung 3 first slice).
const DD_DMABUF_MOD_MAGIC: u32 = 0x6464;
const DD_DMABUF_RENDER_BIT: u32 = 0x1_0000;

const WL_DISPLAY: u32 = 1;

fn fixed_floor(v: i32) -> i32 {
    v >> 8
}

fn fixed_ceil(v: i32) -> i32 {
    (v + 255) >> 8
}

struct SurfaceMapping {
    src_x: i32,
    src_y: i32,
    src_x2: i32,
    src_y2: i32,
    dst_w: i32,
    dst_h: i32,
    uv_rect: [f32; 4],
}

impl SurfaceMapping {
    fn identity(w: i32, h: i32) -> SurfaceMapping {
        SurfaceMapping::new(w, h, 0, 0, w, h, w, h)
    }

    fn new(
        tex_w: i32,
        tex_h: i32,
        src_x: i32,
        src_y: i32,
        src_x2: i32,
        src_y2: i32,
        dst_w: i32,
        dst_h: i32,
    ) -> SurfaceMapping {
        SurfaceMapping {
            src_x,
            src_y,
            src_x2,
            src_y2,
            dst_w,
            dst_h,
            uv_rect: [
                src_x as f32 / tex_w as f32,
                src_y as f32 / tex_h as f32,
                src_x2 as f32 / tex_w as f32,
                src_y2 as f32 / tex_h as f32,
            ],
        }
    }

    fn is_identity(&self, w: i32, h: i32) -> bool {
        self.src_x == 0
            && self.src_y == 0
            && self.src_x2 == w
            && self.src_y2 == h
            && self.dst_w == w
            && self.dst_h == h
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FocusedLogicalGeometry {
    pub surface: u32,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub source: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExternalLogicalCrop {
    pub source_client: usize,
    pub source_surface: u32,
    pub source: &'static str,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// What each live object id refers to. The compositor only needs to remember enough per object to route
/// the next request and, for buffers/pools/surfaces, to reconstruct pixels on commit.
/// Real byte length of an open fd (fstat st_size). Used to clamp a wl_shm pool's usable extent to what the
/// guest actually ftruncated the backing fd to, so a read never runs off the end of the file into an
/// unbacked (SIGBUS-on-access) 16 KB host page. Returns `usize::MAX` when fstat fails (unknown length →
/// don't clamp, preserving the pre-guard behaviour); a genuinely empty file yields 0 (reject all reads).
fn fd_byte_len(fd: RawFd) -> usize {
    unsafe {
        let mut st: libc::stat = std::mem::zeroed();
        if libc::fstat(fd, &mut st) == 0 {
            st.st_size.max(0) as usize
        } else {
            usize::MAX
        }
    }
}

enum Obj {
    Registry,
    Compositor,
    Shm,
    ShmPool {
        // The pool's mmap. We keep `fd` for the pool's whole lifetime so `resize` can re-map (portable
        // mremap emulation: mmap-new + munmap-old — Linux `mremap` is unavailable on the macOS target).
        // Matches wl_shm.c, which keeps `pool->mmap_fd` alive for the same reason (wayland-shm.c:69,166).
        fd: RawFd,
        ptr: *mut u8,
        size: usize,
        // The backing fd's real byte length (fstat) at map/resize time. The guest declares the pool `size`
        // independently of how many bytes it actually ftruncated the fd to; a well-formed client makes them
        // equal (wayland-shm's os_create_anonymous_file ftruncates to exactly the pool size), but a lying or
        // racing client can declare a `size` larger than the fd. macOS mmaps whole 16 KB host pages, and a
        // read that lands in a page WHOLLY past the fd's EOF takes a SIGBUS — which dd-display does not catch,
        // so the whole compositor would die. libwayland guards this with wl_shm_buffer_begin_access + a SIGBUS
        // handler; we instead clamp the per-buffer bounds check (see `extract`) to this real length so an
        // out-of-backing buffer is refused rather than read. Equal to `size` for every well-formed client.
        safe_len: usize,
        // Live wl_buffer children. The mapping is torn down only when the pool is destroyed AND no buffer
        // still references it — the spec keeps buffers valid after `wl_shm_pool.destroy` ("The mmapped
        // memory will be released when all buffers … are gone"), mirroring wl_shm.c's shm_pool_unref
        // (wayland-shm.c:145-170).
        buffers: u32,
        zombie: bool, // destroy() seen; free once `buffers` hits 0.
    },
    Buffer {
        pool: u32,
        offset: i32,
        width: i32,
        height: i32,
        stride: i32,
        format: u32,
    },
    Surface(Surface),
    Viewport {
        surface: u32,
    },
    // wl_subsurface: a role object whose requests (set_position/place_*/set_sync) target `surface`.
    Subsurface {
        surface: u32,
    },
    XdgWmBase,
    XdgSurface {
        surface: u32,
    },
    XdgToplevel {
        xdg_surface: u32,
    },
    // xdg_positioner: accrues placement rules; consumed by get_popup to derive the popup geometry.
    XdgPositioner(XdgPositioner),
    // xdg_popup: a menu/dropdown surface. `x,y,w,h` is the geometry computed from its positioner,
    // sent back to the client in the xdg_popup.configure that completes the initial handshake. `parent`
    // is the parent xdg_surface the popup is anchored to (positioner (x,y) is relative to the parent's
    // window-geometry top-left) — threaded to the presenter so the popup window opens at the widget.
    XdgPopup {
        xdg_surface: u32,
        parent: u32,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
    },
    Seat,
    // wl_seat input children. Tracked distinctly (not Obj::Other) so their `release` destructor can
    // stop event routing and free the id.
    Pointer,
    Keyboard,
    Touch,
    // wp_cursor_shape_manager_v1: hands out wp_cursor_shape_device_v1 objects (per wl_pointer/tablet_tool).
    CursorShapeManager,
    // wp_cursor_shape_device_v1: `set_shape(serial, shape)` names a themed pointer; we map it to NSCursor.
    CursorShapeDevice,
    Output,
    // wl_region: an accumulated set of rectangles built by add()/subtract(). Referenced by
    // set_opaque_region / set_input_region; the region object itself is not double-buffered (its rects are
    // snapshotted onto the surface when assigned), but the assignment is applied at commit.
    Region {
        ops: Vec<RegionOp>,
    },
    // Chromium-ozone init globals (inert for the MVP present path).
    Subcompositor,
    Viewporter,
    DataDeviceManager,
    // wl_data_device: the per-seat clipboard/DnD endpoint. The compositor announces the current
    // selection to it (data_offer + selection events) and routes wl_data_offer.receive back to the
    // owning source. Tracked so `set_selection` knows which device(s) to notify.
    DataDevice,
    // wl_data_source: a client-owned advertiser of clipboard/DnD content. `mimes` accrues from
    // wl_data_source.offer(mime_type); when this source becomes the selection, those mimes are
    // replayed to peers as wl_data_offer.offer events.
    DataSource {
        mimes: Vec<String>,
    },
    // wl_data_offer: a SERVER-allocated proxy for reading the current selection. `source` is the
    // wl_data_source it mirrors, so wl_data_offer.receive can be forwarded as wl_data_source.send to
    // the owner's fd. `stale` is set once a newer selection supersedes this offer (its source may be
    // gone), so a late receive on it is dropped instead of hitting a dangling source.
    DataOffer {
        source: u32,
        stale: bool,
    },
    // GPU rung 2: zwp_linux_dmabuf_v1 objects.
    LinuxDmabuf,
    DmabufParams {
        iosurface_id: Option<u32>,
        stride: i32,
        gpu_render: bool,
        /// Allocation generation from `modifier_hi` bits 17..=31 (0 == unversioned); authenticated at
        /// create_immed so a stale reference to a retired/reissued IOSurface id is rejected.
        generation: u32,
    }, // accrues add()
    DmaBuffer {
        width: i32,
        height: i32,
        format: u32,
        iosurface_id: u32,
        gpu_render: bool,
    },
    // Chromium's surface_augmenter protocol. Version 1 provides wl_buffer objects backed only by a
    // solid color; Chrome's surfaceless path uses these during startup before regular buffers flow.
    SurfaceAugmenter,
    AugmentedSurface {
        surface: u32,
    },
    SolidColorBuffer {
        width: i32,
        height: i32,
        bgra: [u8; 4],
    },
    // wp_presentation (presentation-time): the global object + one per-submission feedback object. A
    // feedback object is created by `wp_presentation.feedback(surface, cb)` and delivers exactly one
    // `presented`/`discarded` event (then the server destroys it), so it carries no state of its own.
    Presentation,
    PresentationFeedback,
    Other,
}

/// Accumulated `xdg_positioner` placement rules. Popups derive their on-screen position from an anchor
/// rectangle (a sub-region of the parent's window geometry), an `anchor` point on that rectangle, a
/// `gravity` direction the popup extends toward, and an `offset`. See xdg-shell.xml `xdg_positioner`.
#[derive(Default, Clone, Copy)]
struct XdgPositioner {
    width: i32,
    height: i32,
    anchor_x: i32,
    anchor_y: i32,
    anchor_w: i32,
    anchor_h: i32,
    anchor: u32,  // xdg_positioner.anchor enum
    gravity: u32, // xdg_positioner.gravity enum
    offset_x: i32,
    offset_y: i32,
}

impl XdgPositioner {
    /// Resolve to `(x, y, w, h)` relative to the top-left of the parent's window geometry, following the
    /// same anchor→gravity→offset placement weston/wlroots use (`xdg_positioner_get_geometry`). Constraint
    /// adjustment (flip/slide/resize) is not applied — the unconstrained placement is returned, which is
    /// correct for menus that fit on-screen (the common Chrome case).
    fn geometry(&self) -> (i32, i32, i32, i32) {
        // Placement is only well-defined once the client has set a size; fall back to a 1x1 rect otherwise
        // so we still emit a valid configure rather than a zero-size (protocol-invalid) one.
        let w = if self.width > 0 { self.width } else { 1 };
        let h = if self.height > 0 { self.height } else { 1 };
        // anchor/gravity enum encoding (shared by both): none=0, top=1, bottom=2, left=3, right=4,
        // top_left=5, bottom_left=6, top_right=7, bottom_right=8. "left" set = {3,5,6}; "right" = {4,7,8};
        // "top" = {1,5,7}; "bottom" = {2,6,8}; anything else is centered on that axis.
        let is_left = |v: u32| matches!(v, 3 | 5 | 6);
        let is_right = |v: u32| matches!(v, 4 | 7 | 8);
        let is_top = |v: u32| matches!(v, 1 | 5 | 7);
        let is_bottom = |v: u32| matches!(v, 2 | 6 | 8);
        // anchor point within the anchor rectangle
        let mut x = self.anchor_x;
        let mut y = self.anchor_y;
        if is_right(self.anchor) {
            x += self.anchor_w;
        } else if !is_left(self.anchor) {
            x += self.anchor_w / 2;
        }
        if is_bottom(self.anchor) {
            y += self.anchor_h;
        } else if !is_top(self.anchor) {
            y += self.anchor_h / 2;
        }
        // gravity: which direction the popup extends from the anchor point
        if is_left(self.gravity) {
            x -= w;
        } else if !is_right(self.gravity) {
            x -= w / 2;
        }
        if is_top(self.gravity) {
            y -= h;
        } else if !is_bottom(self.gravity) {
            y -= h / 2;
        }
        (x + self.offset_x, y + self.offset_y, w, h)
    }
}

#[derive(Default)]
struct Surface {
    pending_buffer: Option<u32>, // buffer id from the most recent attach (0 ⇒ detach)
    attached: bool,
    current_buffer: Option<u32>,
    attach_x: i32,
    attach_y: i32,
    // wl_callback ids from wl_surface.frame. A client may request several frame callbacks before a
    // single commit (each schedules one throwaway wl_callback); ALL of them must fire on the next
    // presentation, so this is a queue, not a single slot that later requests would overwrite.
    pending_frame: Vec<u32>,
    xdg_surface: Option<u32>,
    configured: bool,
    // Set once the client has acknowledged a configure serial (xdg_surface.ack_configure). xdg-shell
    // requires an ack before configured content is committed/presented; a latch, so later resize
    // configures don't re-block an already-mapped surface.
    acked: bool,
    title: String,
    buffer_scale: i32,
    window_geometry: Option<(i32, i32, i32, i32)>, // committed x,y,w,h from xdg_surface.set_window_geometry
    pending_window_geometry: Option<(i32, i32, i32, i32)>,
    viewport_source: Option<(i32, i32, i32, i32)>, // wl_fixed 24.8: x,y,w,h in buffer coords
    viewport_destination: Option<(i32, i32)>,      // surface-local output size
    // wp_presentation_feedback callback ids requested (via wp_presentation.feedback) for the NEXT commit's
    // content update. Drained + answered (presented/discarded) when that commit is processed.
    pending_feedback: Vec<u32>,
    // wl_surface.set_buffer_transform: how the client pre-rotated/flipped its buffer; we apply the inverse
    // when reading pixels. Double-buffered: staged in `pending_buffer_transform`, applied at commit
    // (weston applies it in the pending→current state copy, surface-state.c:388).
    buffer_transform: i32,
    pending_buffer_transform: Option<i32>,
    // wl_surface.offset (v5) / attach(x,y): the buffer's placement delta, applied at commit.
    pending_offset: Option<(i32, i32)>,
    // wl_surface.set_opaque_region: rectangles the client guarantees are fully opaque. Where they cover the
    // whole surface we force alpha=1 in the extracted pixels so the over-white composite doesn't bleed the
    // background through undefined/zero alpha bytes (the reported white border). Empty ⇒ nothing opaque.
    opaque_region: Vec<RegionOp>,
    pending_opaque_region: Option<Vec<RegionOp>>,
    // wl_surface.set_input_region: tracked for protocol completeness (our single-window present path treats
    // the whole surface as the input region; no consumer yet).
    #[allow(dead_code)]
    input_region: Option<Vec<RegionOp>>,
    pending_input_region: Option<Option<Vec<RegionOp>>>,
    // ---- wl_subsurface role (Some(parent) ⇒ this surface is a subsurface) ----
    subsurface_parent: Option<u32>,
    subsurface_sync: bool, // synchronized mode; starts true per wl_subcompositor.get_subsurface
    subsurface_x: i32,     // applied position, relative to the parent's origin
    subsurface_y: i32,
    pending_sub_x: i32, // set_position is double-buffered onto the parent commit
    pending_sub_y: i32,
    has_pending_pos: bool,
    // Synchronized-mode commit is cached and applied when the parent commits.
    cached_buffer: Option<u32>,
    has_cached: bool,
    cached_frame: Vec<u32>,
    // Direct child subsurfaces, ordered bottom→top (z-order). Composited above this surface.
    children: Vec<u32>,
    // Buffer id to wl_buffer.release after the next present (the freshly-committed buffer; shm is
    // memcpied in extract() so releasing post-present is legal — see WAYLAND_GAPS §1).
    pending_release: Option<u32>,
    max_size: (i32, i32), // xdg_toplevel.set_max_size (0 ⇒ unbounded on that axis)
    min_size: (i32, i32), // xdg_toplevel.set_min_size (0 ⇒ no minimum on that axis)
}

/// One rectangle contributed to a `wl_region`. `add=false` is a subtract().
#[derive(Clone, Copy, Debug)]
struct RegionOp {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    add: bool,
}

/// Does the region (union of adds, minus subtracts) fully cover `[0,0,w,h]`? Conservative: true iff some
/// single add-rect contains the surface bounds and no subtract-rect intersects them — which is exactly how
/// a toolkit declares a fully-opaque window (one add of the whole surface, no subtracts).
fn region_covers(ops: &[RegionOp], w: i32, h: i32) -> bool {
    if w <= 0 || h <= 0 {
        return false;
    }
    let covered = ops.iter().any(|r| {
        r.add && r.x <= 0 && r.y <= 0 && r.x + r.w >= w && r.y + r.h >= h
    });
    if !covered {
        return false;
    }
    let clipped = ops.iter().any(|r| {
        !r.add && r.w > 0 && r.h > 0 && r.x < w && r.y < h && r.x + r.w > 0 && r.y + r.h > 0
    });
    !clipped
}

pub struct Server<P: Presenter> {
    conn: Conn,
    objs: HashMap<u32, Obj>,
    serial: u32,
    present: P,
    /// Cache surface→title so the presenter can label the window (title is set on xdg_toplevel).
    titles: HashMap<u32, String>,
    // ---- M2 input (wl_seat) ----
    seat_ver: u32, // version the client bound wl_seat at (gates versioned events)
    pointer: Option<u32>, // client's wl_pointer object id (from wl_seat.get_pointer)
    keyboard: Option<u32>, // client's wl_keyboard object id (from wl_seat.get_keyboard)
    touch: Option<u32>, // client's wl_touch object id (from wl_seat.get_touch)
    output: Option<u32>, // client's wl_output object id (from wl_registry.bind)
    focus: Option<u32>, // the surface that has input focus (the toplevel, for MVP)
    ptr_entered: bool, // has wl_pointer.enter been sent for `focus`?
    kbd_entered: bool, // has wl_keyboard.enter been sent for `focus`?
    time_ms: u32,  // monotonic-ish event timestamp
    last_ptr: (i32, i32), // last pointer position (surface-local, integer px)
    last_cfg: Option<(i32, i32)>, // last on-screen window size we sent a configure for (resize debounce)
    external_logical_crop: Option<ExternalLogicalCrop>,
    present_seq: u64, // wp_presentation MSC / vblank counter, incremented once per presented frame
    /// A surface for which the client issued `xdg_toplevel.move` (interactive drag). Drained by the live
    /// presenter loop via [`Server::take_move_request`] to start a HOST window drag — the request-gated
    /// replacement for the blanket `DD_DISPLAY_WINDOW_DRAG` movable-by-background behavior.
    pending_move: Option<u32>,
    /// Monotonic clock start, for frame-callback timestamps (see `frame_time_ms`).
    start: std::time::Instant,
    // ---- wl_data_device (clipboard) ----
    /// wl_data_device object ids created on this connection. `set_selection` notifies each of them.
    data_devices: Vec<u32>,
    /// The client's current selection wl_data_source id (from wl_data_device.set_selection), or None
    /// when the selection is empty. Reads via wl_data_offer.receive are forwarded to this source.
    selection: Option<u32>,
    /// Next SERVER-allocated object id. Wayland reserves ids >= 0xff000000 for server-created objects
    /// (libwayland `WL_SERVER_ID_START`); wl_data_offer is the one object the compositor allocates and
    /// announces to the client, so it must draw from this range to avoid colliding with client ids.
    next_server_id: u32,
    /// Surfaces assigned the CURSOR role via `wl_pointer.set_cursor`. These carry the pointer image and
    /// must never be presented as a native window (doing so is the "spurious tiny window" bug).
    cursor_surfaces: std::collections::HashSet<u32>,
}

/// First id in the server-allocated object namespace (libwayland `WL_SERVER_ID_START`). Client-created
/// ids live below this; anything the compositor mints (e.g. wl_data_offer) lives at or above it.
const WL_SERVER_ID_START: u32 = 0xff00_0000;

impl<P: Presenter> Server<P> {
    pub fn new(fd: RawFd, present: P) -> Server<P> {
        let mut objs = HashMap::new();
        objs.insert(WL_DISPLAY, Obj::Other); // wl_display, id 1, always present
        Server {
            conn: Conn::new(fd),
            objs,
            serial: 0,
            start: std::time::Instant::now(),
            present,
            titles: HashMap::new(),
            seat_ver: 1,
            pointer: None,
            keyboard: None,
            touch: None,
            output: None,
            focus: None,
            ptr_entered: false,
            kbd_entered: false,
            time_ms: 0,
            last_ptr: (0, 0),
            last_cfg: None,
            external_logical_crop: None,
            present_seq: 0,
            pending_move: None,
            data_devices: Vec::new(),
            selection: None,
            next_server_id: WL_SERVER_ID_START,
            cursor_surfaces: std::collections::HashSet::new(),
        }
    }

    /// Mint the next server-allocated object id (for a compositor-created wl_data_offer). Stays within
    /// the `0xff000000+` server range so it can never alias a client-created id.
    fn alloc_server_id(&mut self) -> u32 {
        let id = self.next_server_id;
        self.next_server_id = self.next_server_id.wrapping_add(1).max(WL_SERVER_ID_START);
        id
    }

    /// Drain a pending interactive-move request (from `xdg_toplevel.move`), returning the surface id the
    /// client wants dragged, if any. The live loop feeds this to the presenter to start a native window
    /// drag ONLY when Chrome actually requested one.
    pub fn take_move_request(&mut self) -> Option<u32> {
        self.pending_move.take()
    }

    /// Send `xdg_toplevel.close` to the client owning `surface` — the host window manager asked to close
    /// the native window (the AppKit close button). Per xdg-shell this is a REQUEST to the client, which
    /// then exits or prompts; the compositor must NOT destroy the surface itself. Returns true if a
    /// toplevel was found and the event was queued. surface → xdg_surface → xdg_toplevel; opcode 1, no args.
    pub fn send_close_request(&mut self, surface: u32) -> bool {
        let xdg = match self.objs.get(&surface) {
            Some(Obj::Surface(s)) => s.xdg_surface,
            _ => None,
        };
        let Some(xdg) = xdg else {
            return false;
        };
        let Some(tl) = self.find_toplevel(xdg) else {
            return false;
        };
        self.conn.send(&Message::new(tl, 1)); // xdg_toplevel.close (event opcode 1)
        self.conn.flush().ok();
        true
    }

    /// The presenter this server drives (read-only; used by the headless self-test to assert pixels).
    pub fn presenter(&self) -> &P {
        &self.present
    }

    /// The underlying client socket fd (for a poll-based multiplexer over several clients).
    pub fn raw_fd(&self) -> RawFd {
        self.conn.raw_fd()
    }

    /// The surface id that currently holds input focus (the newest mapped toplevel, MVP), if any. The
    /// live input path uses this to look up the focused window's size for the pointer y-flip.
    pub fn focused_surface(&self) -> Option<u32> {
        self.focus
    }

    /// True once the client has bound input objects and has a focused toplevel surface. Chrome uses a
    /// separate GPU-presenting connection for the IOSurface; that connection owns the native window in our
    /// current presenter, but the browser connection owns wl_seat. The live AppKit router uses this to send
    /// clicks/keys to the client that can actually consume them.
    pub fn can_receive_input(&self) -> bool {
        self.focus.is_some() && (self.pointer.is_some() || self.keyboard.is_some())
    }

    /// Best known logical geometry for the focused input surface. In Chrome's split-client path this
    /// lives on the browser/input connection, while the visible IOSurface is committed by the shim
    /// connection; `run_multi` can mirror this onto that non-input connection for the next present.
    pub fn focused_logical_geometry(&self) -> Option<FocusedLogicalGeometry> {
        let sid = self.focus?;
        let Some(Obj::Surface(surface)) = self.objs.get(&sid) else {
            return None;
        };
        if let Some((x, y, w, h)) = surface.window_geometry {
            if w > 0 && h > 0 {
                return Some(FocusedLogicalGeometry {
                    surface: sid,
                    x,
                    y,
                    w,
                    h,
                    source: "xdg_window_geometry",
                });
            }
        }
        if let Some((w, h)) = self.presenter().window_content_size(sid) {
            if w > 0 && h > 0 {
                return Some(FocusedLogicalGeometry {
                    surface: sid,
                    x: 0,
                    y: 0,
                    w,
                    h,
                    source: "presenter_content_size",
                });
            }
        }
        if let Some((w, h)) = self.presenter().surface_size(sid) {
            if w > 0 && h > 0 {
                return Some(FocusedLogicalGeometry {
                    surface: sid,
                    x: 0,
                    y: 0,
                    w,
                    h,
                    source: "presenter_surface_size",
                });
            }
        }
        if let Some((w, h)) = surface.viewport_destination {
            if w > 0 && h > 0 {
                return Some(FocusedLogicalGeometry {
                    surface: sid,
                    x: 0,
                    y: 0,
                    w,
                    h,
                    source: "viewport_destination",
                });
            }
        }
        let bid = surface.current_buffer.or(surface.pending_buffer)?;
        let (w, h) = self.buffer_logical_size(surface, bid)?;
        Some(FocusedLogicalGeometry {
            surface: sid,
            x: 0,
            y: 0,
            w,
            h,
            source: "buffer_logical_size",
        })
    }

    /// Temporary logical crop supplied by the multi-client presenter path. It is intentionally server-wide:
    /// the shim/GPU connection is expected to own the visible IOSurface surface and not input surfaces.
    pub fn set_external_logical_crop(&mut self, crop: Option<ExternalLogicalCrop>) {
        self.external_logical_crop = crop;
    }

    /// Every IOSurface id this client referenced (via `zwp_linux_dmabuf` buffers/params). The multi-client
    /// loop drops each id's cross-queue fence + cached IOSurface when the client disconnects, so a departed
    /// compositor can't leak `MTLEvent`s/IOSurfaces or leave a stale fence generation that would deadlock a
    /// later client which reuses the same IOSurface id.
    pub fn iosurface_ids(&self) -> Vec<u32> {
        let mut ids = Vec::new();
        for obj in self.objs.values() {
            match obj {
                Obj::DmaBuffer { iosurface_id, .. } => ids.push(*iosurface_id),
                Obj::DmabufParams {
                    iosurface_id: Some(id),
                    ..
                } => ids.push(*id),
                _ => {}
            }
        }
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// The presenter, mutably — the live loop uses it to dump on demand and to size windows.
    pub fn presenter_mut(&mut self) -> &mut P {
        &mut self.present
    }

    /// Observe the focused window's live on-screen size `(w,h)`. The first observation is a baseline (no
    /// send); a later CHANGE (the user dragged the window edge) sends one `configure`. Debounced so we
    /// don't spam the client every loop iteration.
    pub fn maybe_resize(&mut self, w: i32, h: i32) {
        if self.focus.is_none() || w <= 0 || h <= 0 {
            return;
        }
        match self.last_cfg {
            None => self.last_cfg = Some((w, h)),
            Some(prev) if prev == (w, h) => {}
            Some(_) => {
                self.last_cfg = Some((w, h));
                self.resize_focused(w, h);
            }
        }
    }

    /// A live NSWindow was resized by the user: tell the focused toplevel to reconfigure to `(w,h)` so the
    /// client repaints at the new size. Sends `xdg_toplevel.configure(w,h,[activated])` + the paired
    /// `xdg_surface.configure(serial)`; the client acks and commits a resized buffer.
    pub fn resize_focused(&mut self, w: i32, h: i32) {
        let Some(surface) = self.focus else { return };
        let (xdg, min, max) = match self.objs.get(&surface) {
            Some(Obj::Surface(s)) => (s.xdg_surface, s.min_size, s.max_size),
            _ => (None, (0, 0), (0, 0)),
        };
        let Some(xdg) = xdg else { return };
        let Some(tl) = self.find_toplevel(xdg) else {
            return;
        };
        // Honor the client's set_min_size / set_max_size (0 ⇒ unbounded on that axis).
        let w = clamp_axis(w, min.0, max.0);
        let h = clamp_axis(h, min.1, max.1);
        // states array: [4] = XDG_TOPLEVEL_STATE_ACTIVATED (little-endian u32).
        let states = 4u32.to_ne_bytes();
        self.conn
            .send(&Message::new(tl, 0).i32(w).i32(h).array(&states));
        let s = self.next_serial();
        self.conn.send(&Message::new(xdg, 0).u32(s));
        self.conn.flush().ok();
    }

    /// Send `wl_display.error(object, code, message)` — the protocol-level rejection a conformant
    /// client interprets as a fatal error on that object. Used for genuinely invalid requests
    /// (unsupported formats, invalid scale/viewport geometry) that a well-behaved client never sends.
    fn post_error(&mut self, object: u32, code: u32, message: &str) {
        self.conn.send(&Message::new(WL_DISPLAY, 0).u32(object).u32(code).string(message));
    }

    /// Retire a destroyed object id: drop it from the object map and tell the client (via
    /// `wl_display.delete_id`) that the id is free to reuse — the acknowledgement every Wayland
    /// destructor owes the client so libwayland can recycle the id.
    fn retire_id(&mut self, id: u32) {
        self.objs.remove(&id);
        self.conn.send(&Message::new(WL_DISPLAY, 1).u32(id));
    }

    fn next_serial(&mut self) -> u32 {
        self.serial += 1;
        self.serial
    }

    /// Milliseconds since server start, for `wl_callback.done` (wl_surface.frame). Weston sends a
    /// CLOCK_MONOTONIC msec here (compositor.c `wl_callback_send_done`); Chrome/viz paces its frame
    /// clock off this value, so a serial (1,2,3…) gives it a bogus, non-ms time and stalls the frame
    /// callback ("spinner spins then stops"). A monotonic ms counter is what the client needs.
    fn frame_time_ms(&self) -> u32 {
        self.start.elapsed().as_millis() as u32
    }

    /// Drive one readable event: read available bytes + dispatch every complete message, then flush.
    /// Returns `false` when the client has disconnected.
    pub fn pump(&mut self) -> std::io::Result<bool> {
        match self.conn.fill()? {
            0 => return Ok(false), // peer closed
            -1 => {}               // would-block; dispatch whatever's buffered
            _ => {}
        }
        while let Some(msg) = self.conn.next_message() {
            self.dispatch(msg);
        }
        self.conn.flush()?;
        Ok(true)
    }

    fn dispatch(&mut self, m: Message) {
        if dbg_on() {
            let kind = match self.objs.get(&m.object) {
                Some(Obj::Other) if m.object == WL_DISPLAY => "wl_display",
                Some(Obj::Registry) => "wl_registry",
                Some(Obj::Compositor) => "wl_compositor",
                Some(Obj::Shm) => "wl_shm",
                Some(Obj::ShmPool { .. }) => "wl_shm_pool",
                Some(Obj::Buffer { .. }) => "wl_buffer",
                Some(Obj::Surface(_)) => "wl_surface",
                Some(Obj::Viewport { .. }) => "wp_viewport",
                Some(Obj::Subsurface { .. }) => "wl_subsurface",
                Some(Obj::XdgWmBase) => "xdg_wm_base",
                Some(Obj::XdgSurface { .. }) => "xdg_surface",
                Some(Obj::XdgToplevel { .. }) => "xdg_toplevel",
                Some(Obj::XdgPositioner(_)) => "xdg_positioner",
                Some(Obj::XdgPopup { .. }) => "xdg_popup",
                Some(Obj::Seat) => "wl_seat",
                Some(Obj::Pointer) => "wl_pointer",
                Some(Obj::Keyboard) => "wl_keyboard",
                Some(Obj::Touch) => "wl_touch",
                Some(Obj::CursorShapeManager) => "wp_cursor_shape_manager_v1",
                Some(Obj::CursorShapeDevice) => "wp_cursor_shape_device_v1",
                Some(Obj::Subcompositor) => "wl_subcompositor",
                Some(Obj::Viewporter) => "wp_viewporter",
                Some(Obj::DataDeviceManager) => "wl_data_device_manager",
                Some(Obj::DataDevice) => "wl_data_device",
                Some(Obj::DataSource { .. }) => "wl_data_source",
                Some(Obj::DataOffer { .. }) => "wl_data_offer",
                Some(Obj::Output) => "wl_output",
                Some(Obj::Region { .. }) => "wl_region",
                Some(Obj::LinuxDmabuf) => "zwp_linux_dmabuf",
                Some(Obj::DmabufParams { .. }) => "zwp_linux_buffer_params",
                Some(Obj::DmaBuffer { .. }) => "wl_buffer(dmabuf)",
                Some(Obj::SurfaceAugmenter) => "surface_augmenter",
                Some(Obj::AugmentedSurface { .. }) => "augmented_surface",
                Some(Obj::SolidColorBuffer { .. }) => "wl_buffer(solid_color)",
                Some(Obj::Presentation) => "wp_presentation",
                Some(Obj::PresentationFeedback) => "wp_presentation_feedback",
                Some(Obj::Other) => "other",
                None => "UNKNOWN-OBJ",
            };
            eprintln!(
                "[dd-display] req obj={} op={} iface={} ({}b)",
                m.object,
                m.opcode,
                kind,
                m.body.len()
            );
        }
        match self.objs.get(&m.object) {
            Some(Obj::Other) if m.object == WL_DISPLAY => self.wl_display(m),
            Some(Obj::Registry) => self.wl_registry(m),
            Some(Obj::Compositor) => self.wl_compositor(m),
            Some(Obj::Shm) => self.wl_shm(m),
            Some(Obj::ShmPool { .. }) => self.wl_shm_pool(m),
            Some(Obj::Surface(_)) => self.wl_surface(m),
            Some(Obj::Viewport { .. }) => self.wp_viewport(m),
            Some(Obj::Subsurface { .. }) => self.wl_subsurface(m),
            Some(Obj::XdgWmBase) => self.xdg_wm_base(m),
            Some(Obj::XdgSurface { .. }) => self.xdg_surface(m),
            Some(Obj::XdgToplevel { .. }) => self.xdg_toplevel(m),
            Some(Obj::XdgPositioner(_)) => self.xdg_positioner(m),
            Some(Obj::XdgPopup { .. }) => self.xdg_popup(m),
            Some(Obj::Seat) => self.wl_seat(m),
            Some(Obj::Pointer) | Some(Obj::Keyboard) | Some(Obj::Touch) => self.wl_input_device(m),
            Some(Obj::CursorShapeManager) => self.wp_cursor_shape_manager(m),
            Some(Obj::CursorShapeDevice) => self.wp_cursor_shape_device_v1(m),
            Some(Obj::Subcompositor) => self.wl_subcompositor(m),
            Some(Obj::Viewporter) => self.wp_viewporter(m),
            Some(Obj::DataDeviceManager) => self.wl_data_device_manager(m),
            Some(Obj::DataDevice) => self.wl_data_device(m),
            Some(Obj::DataSource { .. }) => self.wl_data_source(m),
            Some(Obj::DataOffer { .. }) => self.wl_data_offer(m),
            Some(Obj::LinuxDmabuf) => self.linux_dmabuf(m),
            Some(Obj::DmabufParams { .. }) => self.dmabuf_params(m),
            Some(Obj::SurfaceAugmenter) => self.surface_augmenter(m),
            Some(Obj::Presentation) => self.wp_presentation(m),
            Some(Obj::Region { .. }) => self.wl_region(m),
            Some(Obj::Buffer { .. }) => self.wl_buffer(m),
            Some(Obj::DmaBuffer { .. })
            | Some(Obj::SolidColorBuffer { .. })
            | Some(Obj::AugmentedSurface { .. })
            | Some(Obj::PresentationFeedback)
            | Some(Obj::Output)
            | Some(Obj::Other) => {
                // destroy-only / inert objects: nothing to do (dmabuf-buffer.destroy, output.*,
                // wp_presentation_feedback has no requests — the server destroys it after presented).
            }
            None => {}
        }
    }

    // ---- wl_display: sync(0), get_registry(1) ----
    fn wl_display(&mut self, m: Message) {
        let mut r = m.reader();
        match m.opcode {
            0 => {
                // sync(callback) — reply done + delete_id so the client's roundtrip completes.
                let cb = r.u32();
                let s = self.next_serial();
                self.conn.send(&Message::new(cb, 0).u32(s)); // wl_callback.done
                self.conn.send(&Message::new(WL_DISPLAY, 1).u32(cb)); // wl_display.delete_id
            }
            1 => {
                // get_registry(registry)
                let reg = r.u32();
                self.objs.insert(reg, Obj::Registry);
                self.advertise(reg);
            }
            _ => {}
        }
    }

    fn advertise(&mut self, reg: u32) {
        // wl_registry.global(name, interface, version)
        let globals: &[(u32, &str, u32)] = &[
            (G_COMPOSITOR, "wl_compositor", 4),
            (G_SHM, "wl_shm", 1),
            // wl_output v4: adds name/description (display identification) + keeps scale (HiDPI). The
            // per-event version gating below still lets a v1/v2 client bind at its own version.
            (G_OUTPUT, "wl_output", 4),
            (G_SEAT, "wl_seat", 5),
            (G_XDG_WM_BASE, "xdg_wm_base", 1),
            // chromium ozone binds all three of these during init; absence is a divergence from a real
            // compositor. Handlers are inert (subsurface/viewport/data objects have no required events).
            (G_SUBCOMPOSITOR, "wl_subcompositor", 1),
            (G_VIEWPORTER, "wp_viewporter", 1),
            (G_DATA_DEV_MGR, "wl_data_device_manager", 3),
            // wp_presentation (presentation-time). Chrome/viz drives frame scheduling off the
            // `presented` feedback; without it, the BeginFrameSource can stall (the "spinner spins then
            // stops" symptom). We advertise v1 (feedback wire format is identical in v1/v2).
            (G_PRESENTATION, "wp_presentation", 1),
            // Themed cursors: Chrome sets named shapes (pointer over links, text over editable text) via
            // this instead of committing a cursor buffer, so we can map them to host NSCursors.
            (G_CURSOR_SHAPE, "wp_cursor_shape_manager_v1", 1),
        ];
        // zwp_linux_dmabuf_v1 (GPU rung 2) is advertised only when DD_DISPLAY_DMABUF is set — until the
        // cross-process IOSurface handle bridge (mach port) is wired, a toolkit that PREFERS dmabuf could
        // commit buffers we can't resolve, so keep the proven shm path the default.
        let dmabuf_on = std::env::var("DD_DISPLAY_DMABUF").is_ok();
        for (name, iface, ver) in globals {
            self.conn
                .send(&Message::new(reg, 0).u32(*name).string(iface).u32(*ver));
        }
        // surface_augmenter is a ChromeOS-only global that stock compositors (Weston) never advertise.
        // The oracle trace shows the real client probes for it and tolerates its absence, so advertising
        // it can steer Chrome onto an augmented path a real compositor never exposes. Default OFF to match
        // Weston; DD_DISPLAY_AUGMENTER=1 re-enables it for debugging the augmented path.
        if std::env::var("DD_DISPLAY_AUGMENTER").is_ok() {
            self.conn.send(
                &Message::new(reg, 0)
                    .u32(G_SURFACE_AUGMENTER)
                    .string("surface_augmenter")
                    .u32(1),
            );
        }
        if dmabuf_on {
            // v4: chromium's ozone GPU derives its DRM render-node path from the dmabuf-feedback
            // `main_device` (get_default_feedback), so advertise v4 and implement feedback below. The v3
            // format/modifier events on the main object are kept (bind) so v3 clients (glmark2) still work.
            self.conn.send(
                &Message::new(reg, 0)
                    .u32(G_DMABUF)
                    .string("zwp_linux_dmabuf_v1")
                    .u32(4),
            );
        }
    }

    // ---- wl_registry: bind(0) ----
    fn wl_registry(&mut self, m: Message) {
        if m.opcode != 0 {
            return;
        }
        // bind(name, interface: string, version, new_id). The client's requested `ver` gates which
        // version-dependent events we may emit — sending a newer event to a proxy the client bound at an
        // older version makes libwayland abort ("listener function for opcode N is NULL").
        let mut r = m.reader();
        let name = r.u32();
        let iface = r.string();
        let ver = r.u32();
        let id = r.u32();
        if dbg_on() {
            eprintln!("[dd-display] bind name={name} iface={iface:?} v{ver} -> id={id}");
        }
        match name {
            G_COMPOSITOR => {
                self.objs.insert(id, Obj::Compositor);
            }
            G_SHM => {
                self.objs.insert(id, Obj::Shm);
                // Advertise supported formats.
                self.conn.send(&Message::new(id, 0).u32(FMT_ARGB8888));
                self.conn.send(&Message::new(id, 0).u32(FMT_XRGB8888));
            }
            G_XDG_WM_BASE => {
                self.objs.insert(id, Obj::XdgWmBase);
            }
            G_SUBCOMPOSITOR => {
                self.objs.insert(id, Obj::Subcompositor);
            }
            G_VIEWPORTER => {
                self.objs.insert(id, Obj::Viewporter);
            }
            G_DATA_DEV_MGR => {
                self.objs.insert(id, Obj::DataDeviceManager);
            }
            G_SURFACE_AUGMENTER => {
                self.objs.insert(id, Obj::SurfaceAugmenter);
            }
            G_CURSOR_SHAPE => {
                self.objs.insert(id, Obj::CursorShapeManager);
            }
            G_SEAT => {
                self.objs.insert(id, Obj::Seat);
                self.seat_ver = ver;
                // capabilities: pointer(1) | keyboard(2) | touch(4). A full seat advertises all three;
                // toolkits (SDL2/GTK/Chrome) require pointer+keyboard to initialize input, and a
                // touch-capable seat is what a reference compositor (weston/wlroots) reports for a laptop.
                // wl_seat.capabilities enum: pointer=1, keyboard=2, touch=4 (wayland.xml wl_seat::capability).
                self.conn.send(&Message::new(id, 0).u32(0b111));
                if ver >= 2 {
                    self.conn.send(&Message::new(id, 1).string("seat0")); // name (wl_seat v2+)
                }
            }
            G_OUTPUT => {
                self.objs.insert(id, Obj::Output);
                self.output = Some(id);
                // Integer scale the compositor is driving (2 on a Retina backing store, else 1). Sizing the
                // physical mm from the mode at a nominal ~96dpi*scale keeps the reported DPI sane. See
                // wlroots `wlr_output_send_geometry`/`wlr_output.scale`.
                let scale = self.present.output_scale().max(1);
                let phys_w = OUTPUT_WIDTH * 254 / (96 * 10); // ~mm at 96dpi
                let phys_h = OUTPUT_HEIGHT * 254 / (96 * 10);
                // geometry(x,y,pw,ph,subpixel,make,model,transform) — v1
                self.conn.send(
                    &Message::new(id, 0)
                        .i32(0)
                        .i32(0)
                        .i32(phys_w)
                        .i32(phys_h)
                        .i32(0)
                        .string("dd")
                        .string("dd-display")
                        .i32(0),
                );
                // mode(flags=current(1)|preferred(2), w, h, refresh) — v1. Reported in physical device
                // pixels; the client derives its logical size by dividing by `scale`.
                self.conn.send(
                    &Message::new(id, 1)
                        .u32(3)
                        .i32(OUTPUT_WIDTH)
                        .i32(OUTPUT_HEIGHT)
                        .i32(OUTPUT_REFRESH_MHZ),
                );
                if ver >= 2 {
                    self.conn.send(&Message::new(id, 3).i32(scale)); // scale (wl_output v2+)
                }
                if ver >= 4 {
                    // name(string) [opcode 4] + description(string) [opcode 5] — wl_output v4. Chrome/GTK
                    // use the stable name to identify the display across hotplugs.
                    self.conn.send(&Message::new(id, 4).string("dd-0"));
                    self.conn
                        .send(&Message::new(id, 5).string("dd virtual display"));
                }
                if ver >= 2 {
                    self.conn.send(&Message::new(id, 2)); // done (wl_output v2+) — must come last.
                }
            }
            G_PRESENTATION => {
                self.objs.insert(id, Obj::Presentation);
                // clock_id(clk_id) [opcode 0] — sent once on bind. The client reads its own
                // clock_gettime(CLOCK_MONOTONIC) to interpret the `presented` timestamps we emit.
                self.conn
                    .send(&Message::new(id, 0).u32(CLOCK_MONOTONIC_LINUX));
            }
            G_DMABUF => {
                self.objs.insert(id, Obj::LinuxDmabuf);
                // Advertise supported (format, modifier) pairs. modifier = our dd tag in the high 32 bits
                // (the low 32 are filled per-buffer with the IOSurface id); also advertise LINEAR(0).
                for fmt in [DRM_FMT_ARGB8888, DRM_FMT_XRGB8888] {
                    // format(format) [v1] — kept for older clients.
                    self.conn.send(&Message::new(id, 0).u32(fmt));
                    if ver >= 3 {
                        // modifier(format, modifier_hi, modifier_lo) [v3]
                        self.conn
                            .send(&Message::new(id, 1).u32(fmt).u32(DD_DMABUF_MOD_MAGIC).u32(0));
                        self.conn.send(&Message::new(id, 1).u32(fmt).u32(0).u32(0));
                        // LINEAR
                    }
                }
            }
            _ => {
                self.objs.insert(id, Obj::Other);
            }
        }
    }

    // ---- wl_compositor: create_surface(0), create_region(1) ----
    fn wl_compositor(&mut self, m: Message) {
        let mut r = m.reader();
        let id = r.u32();
        match m.opcode {
            0 => {
                self.objs.insert(id, Obj::Surface(Surface::default()));
            }
            1 => {
                self.objs.insert(id, Obj::Region { ops: Vec::new() });
            }
            _ => {}
        }
    }

    // ---- wl_shm: create_pool(0, new_id, fd, size) ----
    fn wl_shm(&mut self, m: Message) {
        if m.opcode != 0 {
            return;
        }
        let mut r = m.reader();
        let id = r.u32();
        let fd = self.conn.take_fd();
        let size = r.u32() as usize;
        if let Some(fd) = fd {
            let ptr = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    size.max(1),
                    libc::PROT_READ,
                    libc::MAP_SHARED,
                    fd,
                    0,
                )
            };
            if ptr != libc::MAP_FAILED {
                // Keep `fd` open for the pool's lifetime (needed by resize; closed in `release_pool_ref`).
                let safe_len = fd_byte_len(fd);
                self.objs.insert(
                    id,
                    Obj::ShmPool {
                        fd,
                        ptr: ptr as *mut u8,
                        size,
                        safe_len,
                        buffers: 0,
                        zombie: false,
                    },
                );
                return;
            }
            unsafe { libc::close(fd) };
        }
        self.objs.insert(id, Obj::Other);
    }

    // ---- wl_shm_pool: create_buffer(0), destroy(1), resize(2) ----
    fn wl_shm_pool(&mut self, m: Message) {
        let mut r = m.reader();
        match m.opcode {
            0 => {
                // create_buffer(id, offset, width, height, stride, format)
                let id = r.u32();
                let offset = r.i32();
                let width = r.i32();
                let height = r.i32();
                let stride = r.i32();
                let format = r.u32();
                if let Some(Obj::ShmPool { buffers, .. }) = self.objs.get_mut(&m.object) {
                    *buffers += 1; // pool stays mapped until this buffer is destroyed (spec: buffers pin it).
                }
                self.objs.insert(
                    id,
                    Obj::Buffer {
                        pool: m.object,
                        offset,
                        width,
                        height,
                        stride,
                        format,
                    },
                );
            }
            1 => {
                // destroy(): the pool object is gone, but the mapping outlives it until its buffers are all
                // destroyed. Mark it a zombie and free now iff no buffers remain (wl_shm.c shm_pool_unref).
                if let Some(Obj::ShmPool { zombie, buffers, .. }) = self.objs.get_mut(&m.object) {
                    *zombie = true;
                    if *buffers == 0 {
                        self.free_pool(m.object);
                    }
                }
            }
            2 => {
                // resize(size): pools may only grow. Re-map from the retained fd (portable mremap emulation:
                // map the new size, then unmap the old range). wl_shm.c does the same via
                // wl_os_mremap_maymove where MREMAP_MAYMOVE is unavailable (wayland-shm.c:104-122).
                let new_size = r.i32();
                if new_size <= 0 {
                    return;
                }
                let new_size = new_size as usize;
                let Some(Obj::ShmPool { fd, ptr, size, .. }) = self.objs.get(&m.object) else {
                    return;
                };
                let (fd, old_ptr, old_size) = (*fd, *ptr, *size);
                if new_size <= old_size {
                    return; // never shrink.
                }
                let np = unsafe {
                    libc::mmap(
                        std::ptr::null_mut(),
                        new_size,
                        libc::PROT_READ,
                        libc::MAP_SHARED,
                        fd,
                        0,
                    )
                };
                if np == libc::MAP_FAILED {
                    return; // keep the old (smaller) mapping usable.
                }
                unsafe { libc::munmap(old_ptr as *mut libc::c_void, old_size.max(1)) };
                let safe = fd_byte_len(fd);
                if let Some(Obj::ShmPool { ptr, size, safe_len, .. }) = self.objs.get_mut(&m.object) {
                    *ptr = np as *mut u8;
                    *size = new_size;
                    *safe_len = safe;
                }
            }
            _ => {}
        }
    }

    /// wl_buffer.destroy(0): drop the buffer object and release its hold on the backing shm pool (which
    /// may now be freed if it was already destroyed). Prevents the per-buffer object leak and, together
    /// with pool zombie-tracking, the mmap leak.
    fn wl_buffer(&mut self, m: Message) {
        if m.opcode != 0 {
            return;
        }
        let pool = match self.objs.remove(&m.object) {
            Some(Obj::Buffer { pool, .. }) => Some(pool),
            other => {
                if let Some(o) = other {
                    self.objs.insert(m.object, o);
                }
                None
            }
        };
        if let Some(pool) = pool {
            self.release_pool_ref(pool);
            // Acknowledge the buffer id is free to reuse.
            self.conn.send(&Message::new(WL_DISPLAY, 1).u32(m.object)); // wl_display.delete_id
        }
    }

    /// A buffer that referenced `pool` is gone: drop the pool's refcount and free the mapping if the pool
    /// was already destroyed and no buffers remain (mirrors wl_shm.c shm_pool_unref, wayland-shm.c:145).
    fn release_pool_ref(&mut self, pool: u32) {
        let free = if let Some(Obj::ShmPool { buffers, zombie, .. }) = self.objs.get_mut(&pool) {
            *buffers = buffers.saturating_sub(1);
            *zombie && *buffers == 0
        } else {
            false
        };
        if free {
            self.free_pool(pool);
        }
    }

    /// Unmap a pool's shared memory, close its retained fd, and drop the object.
    fn free_pool(&mut self, pool: u32) {
        if let Some(Obj::ShmPool { fd, ptr, size, .. }) = self.objs.remove(&pool) {
            unsafe {
                libc::munmap(ptr as *mut libc::c_void, size.max(1));
                libc::close(fd);
            }
        }
    }

    // ---- wl_region: destroy(0), add(1, x,y,w,h), subtract(2, x,y,w,h) ----
    fn wl_region(&mut self, m: Message) {
        let mut r = m.reader();
        match m.opcode {
            0 => {
                self.retire_id(m.object); // destroy: drop + wl_display.delete_id
            }
            1 | 2 => {
                let x = r.i32();
                let y = r.i32();
                let w = r.i32();
                let h = r.i32();
                let add = m.opcode == 1;
                if let Some(Obj::Region { ops }) = self.objs.get_mut(&m.object) {
                    ops.push(RegionOp { x, y, w, h, add });
                }
            }
            _ => {}
        }
    }

    // ---- wl_surface: destroy(0), attach(1), damage(2), frame(3), set_opaque_region(4),
    //      set_input_region(5), commit(6), set_buffer_transform(7), set_buffer_scale(8),
    //      damage_buffer(9), offset(10) ----
    fn wl_surface(&mut self, m: Message) {
        let mut r = m.reader();
        match m.opcode {
            0 => self.destroy_surface(m.object),
            1 => {
                // attach(buffer, x, y). x/y are the buffer-placement offset (superseded by offset() in v5;
                // there they must be 0 and offset() carries the delta — we honor both).
                let buffer = r.u32();
                let x = r.i32();
                let y = r.i32();
                if let Some(Obj::Surface(s)) = self.objs.get_mut(&m.object) {
                    s.pending_buffer = if buffer == 0 { None } else { Some(buffer) };
                    s.attach_x = x;
                    s.attach_y = y;
                    s.attached = true;
                }
            }
            3 => {
                // frame(callback)
                let cb = r.u32();
                if let Some(Obj::Surface(s)) = self.objs.get_mut(&m.object) {
                    s.pending_frame.push(cb);
                }
            }
            4 => {
                // set_opaque_region(region): double-buffered. Snapshot the region's rects now; the
                // assignment takes effect at commit. region==0 clears it (nothing opaque).
                let region = r.u32();
                let ops = self.region_ops(region);
                if let Some(Obj::Surface(s)) = self.objs.get_mut(&m.object) {
                    s.pending_opaque_region = Some(ops);
                }
            }
            5 => {
                // set_input_region(region): double-buffered. region==0 ⇒ infinite (whole surface).
                let region = r.u32();
                let val = if region == 0 {
                    None
                } else {
                    Some(self.region_ops(region))
                };
                if let Some(Obj::Surface(s)) = self.objs.get_mut(&m.object) {
                    s.pending_input_region = Some(val);
                }
            }
            6 => self.commit(m.object),
            7 => {
                // set_buffer_transform(transform): double-buffered (applied at commit).
                let transform = r.i32();
                if (0..=7).contains(&transform) {
                    if let Some(Obj::Surface(s)) = self.objs.get_mut(&m.object) {
                        s.pending_buffer_transform = Some(transform);
                    }
                }
            }
            8 => {
                // set_buffer_scale(scale): scale must be positive. A value <= 0 is invalid; reject it
                // with a protocol error (wl_surface.invalid_scale) instead of silently normalizing it to
                // 1 and clobbering the previously-valid scale.
                let scale = r.i32();
                if scale <= 0 {
                    self.post_error(m.object, 0 /* invalid_scale */, "buffer scale must be positive");
                    return;
                }
                if let Some(Obj::Surface(s)) = self.objs.get_mut(&m.object) {
                    s.buffer_scale = scale;
                }
            }
            10 => {
                // offset(x, y) [v5]: double-buffered buffer-placement delta, applied at commit.
                let x = r.i32();
                let y = r.i32();
                if let Some(Obj::Surface(s)) = self.objs.get_mut(&m.object) {
                    s.pending_offset = Some((x, y));
                }
            }
            // damage(2) / damage_buffer(9): we always re-present the whole committed buffer, which is
            // always a correct superset of the damaged sub-rectangles, so accumulating them is unnecessary
            // (weston tracks them only to minimize the GL/pixman blit region — an optimization, not a
            // correctness requirement; compositor.c weston_surface_damage).
            _ => {}
        }
    }

    /// Snapshot a wl_region's accumulated rectangles (empty if the id is not a live region or is null).
    fn region_ops(&self, region: u32) -> Vec<RegionOp> {
        match self.objs.get(&region) {
            Some(Obj::Region { ops }) => ops.clone(),
            _ => Vec::new(),
        }
    }

    /// wl_surface.destroy(0): release any buffer the surface still holds (courtesy release + pool refcount)
    /// and drop the surface object. Without this, a surface and its last buffer leak until disconnect, and
    /// the pool it pinned can never be unmapped.
    fn destroy_surface(&mut self, sid: u32) {
        let held = match self.objs.remove(&sid) {
            Some(Obj::Surface(s)) => s.current_buffer.or(s.pending_buffer),
            other => {
                if let Some(o) = other {
                    self.objs.insert(sid, o);
                }
                return;
            }
        };
        if let Some(bid) = held {
            self.conn.send(&Message::new(bid, 0)); // wl_buffer.release (client owns the buffer object)
        }
        if self.focus == Some(sid) {
            self.focus = None;
            self.ptr_entered = false;
            self.kbd_entered = false;
        }
        // The id is about to be recycled: clear any cursor-role marking so a later surface reusing this id
        // is not wrongly suppressed from presenting.
        self.cursor_surfaces.remove(&sid);
        // Acknowledge the id is free to reuse (the destructor's half of libwayland id recycling).
        self.conn.send(&Message::new(WL_DISPLAY, 1).u32(sid)); // wl_display.delete_id
    }

    /// wl_pointer / wl_keyboard / wl_touch: the only requests are `set_cursor` (pointer) and `release`.
    /// A released input object must stop receiving events and free its id — otherwise later input still
    /// routes to the (possibly reused) id, corrupting the client's protocol stream.
    fn wl_input_device(&mut self, m: Message) {
        // wl_pointer.set_cursor(serial, surface, hotspot_x, hotspot_y) — opcode 0. The named surface is the
        // pointer image; give it the CURSOR role so it is never presented as a native window. Chrome commits
        // the cursor image BEFORE issuing set_cursor, so that surface may ALREADY have been given a tiny
        // window — drop it here so the spurious window disappears (and stays gone on later cursor commits).
        if matches!(self.objs.get(&m.object), Some(Obj::Pointer)) && m.opcode == 0 {
            let mut r = m.reader();
            let _serial = r.u32();
            let surface = r.u32();
            if surface != 0 {
                self.cursor_surfaces.insert(surface);
                self.present.drop_window(surface);
            }
            return;
        }
        let is_release = match self.objs.get(&m.object) {
            // wl_pointer.release is opcode 1 (opcode 0 is set_cursor, handled above).
            Some(Obj::Pointer) => m.opcode == 1,
            // wl_keyboard.release / wl_touch.release are opcode 0.
            Some(Obj::Keyboard) | Some(Obj::Touch) => m.opcode == 0,
            _ => false,
        };
        if !is_release {
            return;
        }
        if self.pointer == Some(m.object) {
            self.pointer = None;
            self.ptr_entered = false;
        }
        if self.keyboard == Some(m.object) {
            self.keyboard = None;
            self.kbd_entered = false;
        }
        if self.touch == Some(m.object) {
            self.touch = None;
        }
        self.retire_id(m.object);
    }

    fn commit(&mut self, sid: u32) {
        // Apply all double-buffered surface state atomically before we read pixels (weston applies the
        // whole pending state in one pass at commit, surface-state.c weston_surface_commit_state).
        if let Some(Obj::Surface(s)) = self.objs.get_mut(&sid) {
            if let Some(geometry) = s.pending_window_geometry.take() {
                s.window_geometry = Some(geometry);
            }
            if let Some(t) = s.pending_buffer_transform.take() {
                s.buffer_transform = t;
            }
            if let Some((x, y)) = s.pending_offset.take() {
                s.attach_x = x;
                s.attach_y = y;
            }
            if let Some(ops) = s.pending_opaque_region.take() {
                s.opaque_region = ops;
            }
            if let Some(region) = s.pending_input_region.take() {
                s.input_region = region;
            }
        }

        // Snapshot the surface's committed state without holding a &mut across the borrows we need.
        // ---- wl_subsurface role ----
        // A subsurface never drives a native window of its own; its pixels are composited into the root
        // ancestor's frame. In *synchronized* mode (the default) a subsurface commit is CACHED and only
        // applied when the parent surface commits — the child's state is atomically coupled to the
        // parent (wl_subsurface spec; wlroots subsurface_handle_surface_client_commit +
        // subsurface_handle_parent_commit, types/wlr_subcompositor.c:262-294). In *desynchronized* mode
        // it applies immediately and re-composites the root.
        if matches!(self.objs.get(&sid), Some(Obj::Surface(s)) if s.subsurface_parent.is_some()) {
            let sync = self.is_effectively_synchronized(sid);
            if let Some(Obj::Surface(s)) = self.objs.get_mut(&sid) {
                if sync {
                    if s.attached {
                        s.cached_buffer = s.pending_buffer;
                        s.has_cached = true;
                        s.attached = false;
                    }
                    s.cached_frame.append(&mut s.pending_frame);
                    return;
                }
                // Desynchronized: apply this commit immediately.
                if s.attached {
                    s.current_buffer = s.pending_buffer;
                    s.pending_release = s.pending_buffer;
                    s.attached = false;
                }
                if s.has_pending_pos {
                    s.subsurface_x = s.pending_sub_x;
                    s.subsurface_y = s.pending_sub_y;
                    s.has_pending_pos = false;
                }
            }
            let root = self.root_surface(sid);
            self.present_root(root);
            return;
        }

        // ---- root / regular surface ----
        let (attached, buffer, xdg_surface, configured, acked) = match self.objs.get(&sid) {
            Some(Obj::Surface(s)) => (s.attached, s.pending_buffer, s.xdg_surface, s.configured, s.acked),
            _ => return,
        };

        // Initial commit of an xdg_surface with no buffer yet -> send the configure handshake.
        if xdg_surface.is_some() && !configured {
            self.maybe_configure_surface(sid);
            return;
        }

        // Apply the root's own freshly-attached buffer, then atomically apply any cached synchronized
        // descendant state, then present the composited frame.
        if attached {
            if let Some(Obj::Surface(s)) = self.objs.get_mut(&sid) {
                s.current_buffer = buffer;
                s.pending_release = buffer;
                s.attached = false;
            }
        }
        // xdg-shell: configured content must not be presented until the client has acknowledged a
        // configure. Apply the buffer state above (so it is ready), but hold the present until ack —
        // ack_configure will drive present_root once the serial is acknowledged.
        if xdg_surface.is_some() && !acked {
            return;
        }
        // A committed wp_viewport source rectangle that falls outside the attached buffer is a protocol
        // error (out_of_buffer), not a silently-clamped crop of different content.
        if let Some(vp) = self.viewport_source_out_of_buffer(sid) {
            self.post_error(vp, 2 /* out_of_buffer */, "wp_viewport source rectangle is outside the buffer");
            return;
        }
        self.apply_cached_subtree(sid);
        // A surface that has been assigned the CURSOR role (via wl_pointer.set_cursor) is the mouse-pointer
        // image, not a window. Presenting it is exactly the "spurious super-small window" bug: Chrome's
        // ~10x16 cursor surface popped as its own tiny NSWindow beside the real content window. Retire its
        // buffer/frame bookkeeping (so the client's frame pacing does not stall) but never present it.
        if self.cursor_surfaces.contains(&sid) {
            self.retire_unpresented(sid);
            return;
        }
        self.present_root(sid);
    }

    /// Advance frame pacing for a committed surface we deliberately do NOT present (a roleless surface such
    /// as a cursor image): release its just-committed buffer and fire its frame callbacks, exactly as
    /// `present_root` would, minus the native-window present. Without this the client would wait forever
    /// for the wl_buffer.release / wl_callback.done it expects after committing the cursor surface.
    fn retire_unpresented(&mut self, sid: u32) {
        let (rel, frames) = match self.objs.get_mut(&sid) {
            Some(Obj::Surface(s)) => (s.pending_release.take(), std::mem::take(&mut s.pending_frame)),
            _ => return,
        };
        if let Some(bid) = rel {
            self.conn.send(&Message::new(bid, 0)); // wl_buffer.release
        }
        for cb in frames {
            let t = self.frame_time_ms();
            self.conn.send(&Message::new(cb, 0).u32(t)); // wl_callback.done
            self.conn.send(&Message::new(WL_DISPLAY, 1).u32(cb)); // delete_id
        }
        self.send_presentation_feedback(sid, false);
    }

    /// Walk up the subsurface parent chain to the root (non-subsurface) surface.
    fn root_surface(&self, mut sid: u32) -> u32 {
        while let Some(Obj::Surface(s)) = self.objs.get(&sid) {
            match s.subsurface_parent {
                Some(p) => sid = p,
                None => break,
            }
        }
        sid
    }

    /// A subsurface is *effectively synchronized* if it, or any of its ancestors, is in sync mode
    /// (wl_subsurface: "the cached state of a sub-surface ... is only applied when its parent's state is
    /// applied"; wlroots `subsurface_is_synchronized`, types/wlr_subcompositor.c:13-24).
    fn is_effectively_synchronized(&self, sid: u32) -> bool {
        let mut cur = sid;
        loop {
            let Some(Obj::Surface(s)) = self.objs.get(&cur) else {
                return false;
            };
            match s.subsurface_parent {
                None => return false, // reached the root toplevel — not synchronized
                Some(p) => {
                    if s.subsurface_sync {
                        return true;
                    }
                    cur = p;
                }
            }
        }
    }

    /// Depth-first over the subsurface tree rooted at `root`, promoting each synchronized child's cached
    /// commit (position + buffer + frame) so it becomes current — the parent-commit half of the
    /// double-buffered subsurface protocol (wlroots `subsurface_handle_parent_commit`).
    fn apply_cached_subtree(&mut self, root: u32) {
        let children = match self.objs.get(&root) {
            Some(Obj::Surface(s)) => s.children.clone(),
            _ => return,
        };
        for c in children {
            if let Some(Obj::Surface(s)) = self.objs.get_mut(&c) {
                if s.has_pending_pos {
                    s.subsurface_x = s.pending_sub_x;
                    s.subsurface_y = s.pending_sub_y;
                    s.has_pending_pos = false;
                }
                if s.has_cached {
                    s.current_buffer = s.cached_buffer;
                    s.pending_release = s.cached_buffer;
                    s.has_cached = false;
                    s.pending_frame.append(&mut s.cached_frame);
                }
            }
            self.apply_cached_subtree(c);
        }
    }

    /// Every subsurface descendant of `root`, in bottom→top composite order, each with its absolute
    /// offset from the root surface's origin (nested positions accumulate).
    fn collect_composite_children(&self, root: u32) -> Vec<(u32, i32, i32)> {
        let mut out = Vec::new();
        self.collect_children_rec(root, 0, 0, &mut out);
        out
    }

    fn collect_children_rec(&self, sid: u32, base_x: i32, base_y: i32, out: &mut Vec<(u32, i32, i32)>) {
        let children = match self.objs.get(&sid) {
            Some(Obj::Surface(s)) => s.children.clone(),
            _ => return,
        };
        for c in children {
            let (cx, cy) = match self.objs.get(&c) {
                Some(Obj::Surface(s)) => (s.subsurface_x, s.subsurface_y),
                _ => continue,
            };
            let (ax, ay) = (base_x + cx, base_y + cy);
            out.push((c, ax, ay));
            self.collect_children_rec(c, ax, ay, out);
        }
    }

    /// Present the root surface, compositing every subsurface descendant onto it at its position in
    /// z-order, then drain pending buffer releases + frame callbacks for the whole subtree, and answer
    /// any wp_presentation_feedback. Subsurface composition is CPU-side, so it applies when the root
    /// carries readable pixels (wl_shm / solid-color); an IOSurface/dmabuf root is presented as a single
    /// zero-copy texture and its subsurfaces are not blended here (documented limitation — WAYLAND_GAPS §7).
    fn present_root(&mut self, root: u32) {
        let (root_bid, title) = match self.objs.get(&root) {
            Some(Obj::Surface(s)) => (s.current_buffer, s.title.clone()),
            _ => return,
        };

        let mut did_present = false;
        // Whether a buffer was actually handed to the presenter this commit. If one was but the present
        // FAILED, we must not release the buffer or fire frame callbacks (that would advance the client's
        // frame pacing for a frame that never reached the screen).
        let mut present_attempted = false;
        if let Some(bid) = root_bid {
            if let Some(mut sb) = self.extract(root, bid, &title) {
                // Thread popup placement so the presenter opens a menu/combobox dropdown at its anchoring
                // widget (parent-content-top-left + positioner offset) instead of a default cascade.
                sb.popup = self.popup_placement(root);
                if sb.iosurface_id.is_none() && !sb.bgra.is_empty() {
                    for (csid, ox, oy) in self.collect_composite_children(root) {
                        let cbid = match self.objs.get(&csid) {
                            Some(Obj::Surface(s)) => s.current_buffer,
                            _ => None,
                        };
                        if let Some(cbid) = cbid {
                            if let Some(csb) = self.extract(csid, cbid, "") {
                                blend_subsurface(&mut sb, &csb, ox, oy);
                            }
                        }
                    }
                }
                present_attempted = true;
                // Only a visibly Delivered frame advances pacing; an Offscreen present or a real
                // output/device error (previously swallowed by a `false`/`true` bool) does not.
                did_present = match self.present.present(&sb) {
                    Ok(crate::present::PresentOutcome::Delivered { .. }) => true,
                    Ok(crate::present::PresentOutcome::Offscreen) => false,
                    Ok(crate::present::PresentOutcome::RetryableFailure) => false,
                    Ok(crate::present::PresentOutcome::TerminalFailure) => false,
                    Err(e) => {
                        eprintln!("dd-display: present failed for sid {}: {e}", sb.sid);
                        false
                    }
                };
            }
        }
        // Advance frame pacing (release buffers + fire frame callbacks) unless a present was attempted
        // and failed. A bufferless commit (nothing to present) still advances, as before.
        let advance = !present_attempted || did_present;

        // Release freshly-committed buffers + fire frame callbacks for the root and every descendant, then
        // answer presentation feedback (the frame is on-screen by the time Presenter::present returns — the
        // analogue of weston sending feedback on the KMS pageflip-complete event; this is what keeps
        // Chrome/viz's frame pacing from stalling).
        let mut subtree = vec![root];
        subtree.extend(self.collect_composite_children(root).into_iter().map(|(c, _, _)| c));
        for s in subtree {
            // On a failed present, leave pending_release / pending_frame in place so the buffer is held
            // and the callbacks fire on a later successful present instead of now.
            let (rel, frames) = match self.objs.get_mut(&s) {
                Some(Obj::Surface(su)) if advance => {
                    (su.pending_release.take(), std::mem::take(&mut su.pending_frame))
                }
                _ => (None, Vec::new()),
            };
            if let Some(bid) = rel {
                self.conn.send(&Message::new(bid, 0)); // wl_buffer.release
            }
            // Fire EVERY frame callback the client queued for this commit (in request order), each with
            // its own wl_callback.done + delete_id.
            for cb in frames {
                let t = self.frame_time_ms();
                self.conn.send(&Message::new(cb, 0).u32(t)); // wl_callback.done (CLOCK_MONOTONIC msec)
                self.conn.send(&Message::new(WL_DISPLAY, 1).u32(cb)); // delete_id
            }
            self.send_presentation_feedback(s, did_present);
        }
    }

    fn find_toplevel(&self, xdg_surface: u32) -> Option<u32> {
        self.objs.iter().find_map(|(id, o)| match o {
            Obj::XdgToplevel { xdg_surface: x } if *x == xdg_surface => Some(*id),
            _ => None,
        })
    }

    /// The `xdg_popup` object id bound to `xdg_surface`, plus its resolved `(x,y,w,h)` geometry.
    fn find_popup(&self, xdg_surface: u32) -> Option<(u32, i32, i32, i32, i32)> {
        self.objs.iter().find_map(|(id, o)| match o {
            Obj::XdgPopup {
                xdg_surface: x,
                x: px,
                y: py,
                w,
                h,
                ..
            } if *x == xdg_surface => Some((*id, *px, *py, *w, *h)),
            _ => None,
        })
    }

    /// If `sid`'s surface carries an `xdg_popup` role, resolve where its native window should open:
    /// the parent wl_surface plus the positioner offset from the parent's window-geometry top-left. The
    /// live presenter uses this to place the popup window at the anchoring widget rather than a default
    /// cascade position. Returns `None` for toplevels / surfaces with no popup role.
    fn popup_placement(&self, sid: u32) -> Option<PopupPlacement> {
        let xdg = match self.objs.get(&sid) {
            Some(Obj::Surface(s)) => s.xdg_surface?,
            _ => return None,
        };
        let (parent_xdg, x, y) = self.objs.values().find_map(|o| match o {
            Obj::XdgPopup {
                xdg_surface,
                parent,
                x,
                y,
                ..
            } if *xdg_surface == xdg => Some((*parent, *x, *y)),
            _ => None,
        })?;
        let parent_sid = self.wl_surface_of_xdg(parent_xdg)?;
        Some(PopupPlacement { parent_sid, x, y })
    }

    fn buffer_logical_size(&self, surface: &Surface, bid: u32) -> Option<(i32, i32)> {
        if let Some((w, h)) = surface.viewport_destination {
            return (w > 0 && h > 0).then_some((w, h));
        }
        let (w, h) = match self.objs.get(&bid) {
            Some(Obj::Buffer { width, height, .. })
            | Some(Obj::DmaBuffer { width, height, .. })
            | Some(Obj::SolidColorBuffer { width, height, .. }) => (*width, *height),
            _ => return None,
        };
        let scale = surface.buffer_scale.max(1);
        let w = w / scale;
        let h = h / scale;
        (w > 0 && h > 0).then_some((w, h))
    }

    fn maybe_configure_surface(&mut self, sid: u32) {
        let xdg = match self.objs.get(&sid) {
            Some(Obj::Surface(s)) if !s.configured => s.xdg_surface,
            _ => None,
        };
        let Some(xdg) = xdg else { return };
        // A popup completes its initial handshake with xdg_popup.configure(x,y,w,h) (the placement its
        // positioner resolved to) followed by the paired xdg_surface.configure(serial). Without this the
        // client's menu/dropdown never maps → Chrome menus break or paint stale.
        if let Some((popup, px, py, pw, ph)) = self.find_popup(xdg) {
            self.conn
                .send(&Message::new(popup, 0).i32(px).i32(py).i32(pw).i32(ph));
            let serial = self.next_serial();
            self.conn.send(&Message::new(xdg, 0).u32(serial)); // xdg_surface.configure(serial)
            if let Some(Obj::Surface(s)) = self.objs.get_mut(&sid) {
                s.configured = true;
            }
            self.conn.flush().ok();
            return;
        }
        let Some(tl) = self.find_toplevel(xdg) else {
            return;
        };
        if let Some(output) = self.output {
            self.conn.send(&Message::new(sid, 0).u32(output)); // wl_surface.enter(output)
        }
        let states = 4u32.to_ne_bytes(); // XDG_TOPLEVEL_STATE_ACTIVATED
        self.conn
            .send(&Message::new(tl, 0).i32(0).i32(0).array(&states));
        self.last_cfg = None;
        let serial = self.next_serial();
        self.conn
            .send(&Message::new(xdg, 0).u32(serial)); // xdg_surface.configure(serial)
        if let Some(Obj::Surface(s)) = self.objs.get_mut(&sid) {
            s.configured = true;
        }
        self.conn.flush().ok();
    }

    /// Read the committed buffer into a `SurfaceBuffer` for the presenter. An shm buffer is unpacked to a
    /// tight BGRA framebuffer; a linux-dmabuf buffer carries only the IOSurface id (the presenter wraps it
    /// as an `MTLTexture` — zero copy, no bytes read here).
    fn extract(&self, sid: u32, bid: u32, title: &str) -> Option<SurfaceBuffer> {
        // GPU rung 2: an IOSurface-backed dmabuf buffer — no CPU pixels, just the id.
        if let Some(Obj::DmaBuffer {
            width,
            height,
            format,
            iosurface_id,
            gpu_render,
        }) = self.objs.get(&bid)
        {
            let map = self.surface_mapping(sid, *width, *height)?;
            return Some(SurfaceBuffer {
                sid,
                width: map.dst_w,
                height: map.dst_h,
                texture_width: *width,
                texture_height: *height,
                stride: map.dst_w * 4,
                format: *format,
                bgra: Vec::new(),
                title: title.to_string(),
                iosurface_id: Some(*iosurface_id),
                gpu_render: *gpu_render,
                uv_rect: map.uv_rect,
                damage: None,
                popup: None,
                overlays: Vec::new(),
            });
        }
        if let Some(Obj::SolidColorBuffer {
            width,
            height,
            bgra,
        }) = self.objs.get(&bid)
        {
            if *width <= 0 || *height <= 0 {
                return None;
            }
            let mut pixels = vec![0u8; *width as usize * *height as usize * 4];
            for px in pixels.chunks_exact_mut(4) {
                px.copy_from_slice(bgra);
            }
            let (bw, bh, pixels) = self.apply_transform(sid, *width, *height, pixels);
            let (width, height, mut pixels) = self.apply_viewport(sid, bw, bh, pixels)?;
            self.force_opaque(sid, width, height, &mut pixels);
            return Some(SurfaceBuffer {
                sid,
                width,
                height,
                texture_width: width,
                texture_height: height,
                stride: width * 4,
                format: FMT_ARGB8888,
                bgra: pixels,
                title: title.to_string(),
                iosurface_id: None,
                gpu_render: false,
                uv_rect: [0.0, 0.0, 1.0, 1.0],
                damage: None,
                popup: None,
                overlays: Vec::new(),
            });
        }
        let (pool, offset, width, height, stride, format) = match self.objs.get(&bid) {
            Some(Obj::Buffer {
                pool,
                offset,
                width,
                height,
                stride,
                format,
            }) => (*pool, *offset, *width, *height, *stride, *format),
            _ => return None,
        };
        let (ptr, size) = match self.objs.get(&pool) {
            // Clamp the pool's usable extent to the fd's real backing length. A read past the fd's EOF lands
            // in a whole 16 KB host page macOS never backed -> unhandled SIGBUS kills the compositor. For a
            // well-formed client `safe_len == size` (wayland-shm ftruncates the fd to exactly the pool size),
            // so this is a no-op; a lying/short-fd client's buffer is refused below instead of crashing us.
            Some(Obj::ShmPool { ptr, size, safe_len, .. }) => (*ptr, (*size).min(*safe_len)),
            _ => return None,
        };
        if width <= 0 || height <= 0 || stride <= 0 || offset < 0 {
            return None;
        }
        // Only the advertised shm formats decode to BGRA pixels; anything else must not be
        // reinterpreted as pixels and presented.
        if format != FMT_ARGB8888 && format != FMT_XRGB8888 {
            return None;
        }
        // A row must physically fit the stride, or rows would overlap and present corrupted pixels.
        let row_bytes = (width as usize).saturating_mul(4);
        if (stride as usize) < row_bytes {
            return None;
        }
        // Bounds-check with wrapping-safe math (a huge/negative-derived offset must not overflow).
        let need = (offset as usize)
            .checked_add((height as usize).saturating_sub(1).saturating_mul(stride as usize))
            .and_then(|v| v.checked_add(row_bytes));
        match need {
            Some(n) if n <= size => {}
            _ => return None,
        }
        let mut bgra = vec![0u8; width as usize * height as usize * 4];
        unsafe {
            let base = ptr.add(offset as usize);
            for row in 0..height as usize {
                let src = base.add(row * stride as usize);
                let dst = bgra.as_mut_ptr().add(row * width as usize * 4);
                std::ptr::copy_nonoverlapping(src, dst, width as usize * 4);
            }
        }
        // Apply buffer_transform (buffer→surface orientation) first, then viewport/scale in surface space,
        // then force alpha where the client declared the surface fully opaque.
        let (bw, bh, bgra) = self.apply_transform(sid, width, height, bgra);
        let (width, height, mut bgra) = self.apply_viewport(sid, bw, bh, bgra)?;
        // XRGB8888 has no alpha channel — the fourth byte is undefined per the wl_shm spec (the "X").
        // A CPU raster client (e.g. Chrome under `--disable-gpu` software compositing, committing web
        // content straight into wl_shm) leaves it 0. The presenter's composite shader blends the
        // surface src-over-white using this alpha, so an undefined 0 would wash opaque content to white.
        // Force it opaque, matching `SurfaceBuffer::to_rgba`. (The GL/IOSurface content path always
        // carried valid premultiplied alpha, so this never bit the dmabuf tile path.)
        if format == FMT_XRGB8888 {
            for px in bgra.chunks_exact_mut(4) {
                px[3] = 0xff;
            }
        }
        self.force_opaque(sid, width, height, &mut bgra);
        Some(SurfaceBuffer {
            sid,
            width,
            height,
            texture_width: width,
            texture_height: height,
            stride: width * 4,
            format,
            bgra,
            title: title.to_string(),
            iosurface_id: None,
            gpu_render: false,
            uv_rect: [0.0, 0.0, 1.0, 1.0],
            damage: None,
            popup: None,
            overlays: Vec::new(),
        })
    }

    /// Apply this surface's committed `buffer_transform` to a tight BGRA buffer, returning the
    /// surface-oriented pixels and their (possibly swapped) dimensions. Identity is a zero-copy passthrough
    /// (the Chrome path, which always uses WL_OUTPUT_TRANSFORM_NORMAL).
    fn apply_transform(&self, sid: u32, w: i32, h: i32, src: Vec<u8>) -> (i32, i32, Vec<u8>) {
        let t = match self.objs.get(&sid) {
            Some(Obj::Surface(s)) => s.buffer_transform,
            _ => 0,
        };
        apply_buffer_transform(w, h, src, t)
    }

    /// If the surface's committed opaque region covers its whole logical bounds, force every pixel opaque.
    /// The over-white composite blends by alpha, so an ARGB buffer with undefined/zero alpha in a region
    /// the client swore was opaque would otherwise bleed the white background through (the white border).
    fn force_opaque(&self, sid: u32, w: i32, h: i32, bgra: &mut [u8]) {
        if let Some(Obj::Surface(s)) = self.objs.get(&sid) {
            if region_covers(&s.opaque_region, w, h) {
                for px in bgra.chunks_exact_mut(4) {
                    px[3] = 0xff;
                }
            }
        }
    }

    /// If the surface's committed `wp_viewport` source rectangle extends past its attached buffer,
    /// return the wp_viewport object id that should receive an `out_of_buffer` protocol error. Valid
    /// (in-buffer) sources — including the fractional-scale case Chrome uses — return `None`.
    fn viewport_source_out_of_buffer(&self, sid: u32) -> Option<u32> {
        let (src, scale, bid) = match self.objs.get(&sid) {
            Some(Obj::Surface(s)) => (s.viewport_source?, s.buffer_scale.max(1), s.current_buffer?),
            _ => return None,
        };
        let (bw, bh) = match self.objs.get(&bid) {
            Some(Obj::Buffer { width, height, .. })
            | Some(Obj::DmaBuffer { width, height, .. })
            | Some(Obj::SolidColorBuffer { width, height, .. }) => (*width, *height),
            _ => return None,
        };
        let (sx, sy, sw, sh) = src;
        // Source coords are surface-local 24.8 fixed; scale them up to buffer pixels (as surface_mapping
        // does) and compare the rectangle's extent to the backing buffer size.
        let left = fixed_floor(sx).saturating_mul(scale);
        let top = fixed_floor(sy).saturating_mul(scale);
        let right = fixed_ceil(sx.saturating_add(sw)).saturating_mul(scale);
        let bottom = fixed_ceil(sy.saturating_add(sh)).saturating_mul(scale);
        if left < 0 || top < 0 || right > bw || bottom > bh {
            return self.objs.iter().find_map(|(id, o)| match o {
                Obj::Viewport { surface } if *surface == sid => Some(*id),
                _ => None,
            });
        }
        None
    }

    fn surface_mapping(&self, sid: u32, src_w: i32, src_h: i32) -> Option<SurfaceMapping> {
        let Some(Obj::Surface(surface)) = self.objs.get(&sid) else {
            return Some(SurfaceMapping::identity(src_w, src_h));
        };
        let scale = surface.buffer_scale.max(1);
        if surface.viewport_source.is_none()
            && surface.viewport_destination.is_none()
            && surface.window_geometry.is_none()
            && self.external_logical_crop.is_none()
            && scale == 1
        {
            return Some(SurfaceMapping::identity(src_w, src_h));
        }

        let (mut src_x, mut src_y, mut src_x2, mut src_y2, mut dst_w, mut dst_h) =
            if surface.viewport_source.is_some() || surface.viewport_destination.is_some() {
                let has_src = surface.viewport_source.is_some();
                let (sx, sy, sw, sh) = surface
                    .viewport_source
                    .unwrap_or((0, 0, src_w << 8, src_h << 8));
                // wp_viewport source coordinates are given AFTER buffer_transform + buffer_scale, i.e.
                // in surface-local units (viewporter.xml lines 97-104). Scale them back up into buffer
                // pixels before cropping the backing texture. (No-op at scale==1, the common Chrome
                // fractional-scale path where the client sets buffer_scale=1 and crops via the viewport.)
                let sc = if has_src { scale } else { 1 };
                let src_x = (fixed_floor(sx) * sc).clamp(0, src_w);
                let src_y = (fixed_floor(sy) * sc).clamp(0, src_h);
                let src_x2 = (fixed_ceil(sx.saturating_add(sw)) * sc).clamp(src_x, src_w);
                let src_y2 = (fixed_ceil(sy.saturating_add(sh)) * sc).clamp(src_y, src_h);
                let crop_w = src_x2 - src_x;
                let crop_h = src_y2 - src_y;
                let (dst_w, dst_h) = surface.viewport_destination.unwrap_or((
                    (crop_w + scale - 1) / scale,
                    (crop_h + scale - 1) / scale,
                ));
                (src_x, src_y, src_x2, src_y2, dst_w, dst_h)
            } else {
                (0, 0, src_w, src_h, src_w / scale, src_h / scale)
            };

        let own_geometry = surface.window_geometry;
        let mirrored_geometry = if own_geometry.is_none() {
            self.external_logical_crop
                .map(|crop| (crop.x, crop.y, crop.w, crop.h, crop))
        } else {
            None
        };
        let logical_geometry =
            own_geometry.or_else(|| mirrored_geometry.map(|(x, y, w, h, _)| (x, y, w, h)));

        if let Some((gx, gy, gw, gh)) = logical_geometry {
            let rel_x = (gx - surface.attach_x).clamp(0, dst_w);
            let rel_y = (gy - surface.attach_y).clamp(0, dst_h);
            let rel_x2 = (gx - surface.attach_x + gw).clamp(rel_x, dst_w);
            let rel_y2 = (gy - surface.attach_y + gh).clamp(rel_y, dst_h);
            let base_src_x = src_x;
            let base_src_y = src_y;
            let base_crop_w = src_x2 - src_x;
            let base_crop_h = src_y2 - src_y;

            src_x = base_src_x + div_floor_i32(rel_x, base_crop_w, dst_w);
            src_y = base_src_y + div_floor_i32(rel_y, base_crop_h, dst_h);
            src_x2 = base_src_x + div_ceil_i32(rel_x2, base_crop_w, dst_w);
            src_y2 = base_src_y + div_ceil_i32(rel_y2, base_crop_h, dst_h);
            dst_w = rel_x2 - rel_x;
            dst_h = rel_y2 - rel_y;

            if let Some((_, _, _, _, crop)) = mirrored_geometry {
                eprintln!(
                    "dd-display[mirror-geometry]: source_client={} source_surface={} source={} \
target_surface={} crop=({},{} {}x{}) backing={}x{} mapped_src=({},{}..{},{}) mapped_dst={}x{}",
                    crop.source_client,
                    crop.source_surface,
                    crop.source,
                    sid,
                    crop.x,
                    crop.y,
                    crop.w,
                    crop.h,
                    src_w,
                    src_h,
                    src_x,
                    src_y,
                    src_x2,
                    src_y2,
                    dst_w,
                    dst_h,
                );
            }
        }

        let crop_w = src_x2 - src_x;
        let crop_h = src_y2 - src_y;
        if crop_w <= 0 || crop_h <= 0 || dst_w <= 0 || dst_h <= 0 {
            return None;
        }
        Some(SurfaceMapping::new(
            src_w,
            src_h,
            src_x,
            src_y,
            src_x2,
            src_y2,
            dst_w,
            dst_h,
        ))
    }

    fn apply_viewport(
        &self,
        sid: u32,
        src_w: i32,
        src_h: i32,
        src: Vec<u8>,
    ) -> Option<(i32, i32, Vec<u8>)> {
        let map = self.surface_mapping(sid, src_w, src_h)?;
        if map.is_identity(src_w, src_h) {
            return Some((src_w, src_h, src));
        }

        let mut out = vec![0u8; map.dst_w as usize * map.dst_h as usize * 4];
        let crop_w = map.src_x2 - map.src_x;
        let crop_h = map.src_y2 - map.src_y;
        for dy in 0..map.dst_h {
            let sy = map.src_y + ((dy as i64 * crop_h as i64) / map.dst_h as i64) as i32;
            for dx in 0..map.dst_w {
                let sx = map.src_x + ((dx as i64 * crop_w as i64) / map.dst_w as i64) as i32;
                let si = ((sy as usize * src_w as usize) + sx as usize) * 4;
                let di = ((dy as usize * map.dst_w as usize) + dx as usize) * 4;
                out[di..di + 4].copy_from_slice(&src[si..si + 4]);
            }
        }
        Some((map.dst_w, map.dst_h, out))
    }

    /// The `wl_surface` id backing an `xdg_surface` object.
    fn wl_surface_of_xdg(&self, xdg: u32) -> Option<u32> {
        match self.objs.get(&xdg) {
            Some(Obj::XdgSurface { surface }) => Some(*surface),
            _ => None,
        }
    }

    /// The `wl_surface` id backing an `xdg_toplevel` object (toplevel → xdg_surface → wl_surface).
    fn wl_surface_of_toplevel(&self, tl: u32) -> Option<u32> {
        let xdg = match self.objs.get(&tl) {
            Some(Obj::XdgToplevel { xdg_surface }) => *xdg_surface,
            _ => return None,
        };
        self.wl_surface_of_xdg(xdg)
    }

    /// Emit an `xdg_toplevel.configure(w,h,states)` + paired `xdg_surface.configure(serial)` handshake for
    /// the toplevel bound to `xdg`. Used for state changes (maximize/fullscreen) the client must ack.
    fn send_toplevel_configure(&mut self, xdg: u32, w: i32, h: i32, states: &[u32]) {
        let Some(tl) = self.find_toplevel(xdg) else {
            return;
        };
        let mut bytes = Vec::with_capacity(states.len() * 4);
        for s in states {
            bytes.extend_from_slice(&s.to_ne_bytes());
        }
        self.conn
            .send(&Message::new(tl, 0).i32(w).i32(h).array(&bytes));
        let serial = self.next_serial();
        self.conn.send(&Message::new(xdg, 0).u32(serial));
        self.conn.flush().ok();
    }

    // ---- xdg_wm_base: destroy(0), create_positioner(1), get_xdg_surface(2), pong(3) ----
    fn xdg_wm_base(&mut self, m: Message) {
        let mut r = m.reader();
        match m.opcode {
            0 => {
                // destroy
                self.objs.remove(&m.object);
            }
            1 => {
                // create_positioner(id)
                let id = r.u32();
                self.objs
                    .insert(id, Obj::XdgPositioner(XdgPositioner::default()));
            }
            2 => {
                // get_xdg_surface(id, surface)
                let id = r.u32();
                let surface = r.u32();
                self.objs.insert(id, Obj::XdgSurface { surface });
                if let Some(Obj::Surface(s)) = self.objs.get_mut(&surface) {
                    s.xdg_surface = Some(id);
                }
            }
            3 => {
                // pong(serial): the client answered our ping. We do not currently originate pings, so this
                // just confirms liveness — consume it (a silent fallthrough would be indistinguishable from
                // an unhandled request).
            }
            _ => {}
        }
    }

    // ---- xdg_positioner: destroy(0), set_size(1), set_anchor_rect(2), set_anchor(3), set_gravity(4),
    //      set_constraint_adjustment(5), set_offset(6), set_reactive(7), set_parent_size(8),
    //      set_parent_configure(9). We accumulate the fields get_popup needs to place the popup. ----
    fn xdg_positioner(&mut self, m: Message) {
        if m.opcode == 0 {
            self.objs.remove(&m.object); // destroy
            return;
        }
        let mut r = m.reader();
        let Some(Obj::XdgPositioner(p)) = self.objs.get_mut(&m.object) else {
            return;
        };
        match m.opcode {
            1 => {
                p.width = r.i32();
                p.height = r.i32();
            }
            2 => {
                p.anchor_x = r.i32();
                p.anchor_y = r.i32();
                p.anchor_w = r.i32();
                p.anchor_h = r.i32();
            }
            3 => p.anchor = r.u32(),
            4 => p.gravity = r.u32(),
            6 => {
                p.offset_x = r.i32();
                p.offset_y = r.i32();
            }
            // set_constraint_adjustment(5) / set_reactive(7) / set_parent_size(8) / set_parent_configure(9):
            // accepted; unconstrained placement is used (see XdgPositioner::geometry).
            _ => {}
        }
    }

    // ---- xdg_surface: destroy(0), get_toplevel(1), get_popup(2), set_window_geometry(3), ack_configure(4) ----
    fn xdg_surface(&mut self, m: Message) {
        let mut r = m.reader();
        match m.opcode {
            0 => {
                // destroy: the role object must be gone already; drop our xdg_surface entry.
                if let Some(surface) = self.wl_surface_of_xdg(m.object) {
                    if let Some(Obj::Surface(s)) = self.objs.get_mut(&surface) {
                        s.xdg_surface = None;
                        s.configured = false;
                    }
                }
                self.objs.remove(&m.object);
            }
            1 => {
                let id = r.u32();
                self.objs.insert(
                    id,
                    Obj::XdgToplevel {
                        xdg_surface: m.object,
                    },
                );
                // Give input focus to this toplevel's surface (MVP: newest toplevel is focused).
                if let Some(surface) = self.wl_surface_of_xdg(m.object) {
                    self.transfer_focus(surface);
                }
            }
            2 => {
                // get_popup(id, parent: xdg_surface?, positioner). Resolve the positioner NOW into a fixed
                // (x,y,w,h) geometry; the initial commit will replay it as xdg_popup.configure.
                let id = r.u32();
                let parent = r.u32();
                let positioner = r.u32();
                let (x, y, w, h) = match self.objs.get(&positioner) {
                    Some(Obj::XdgPositioner(p)) => p.geometry(),
                    _ => (0, 0, 1, 1),
                };
                self.objs.insert(
                    id,
                    Obj::XdgPopup {
                        xdg_surface: m.object,
                        parent,
                        x,
                        y,
                        w,
                        h,
                    },
                );
            }
            3 => {
                // set_window_geometry: double-buffered, takes effect on the next wl_surface.commit.
                let x = r.i32();
                let y = r.i32();
                let w = r.i32();
                let h = r.i32();
                if w > 0 && h > 0 {
                    let Some(surface) = self.wl_surface_of_xdg(m.object) else {
                        return;
                    };
                    if let Some(Obj::Surface(s)) = self.objs.get_mut(&surface) {
                        s.pending_window_geometry = Some((x, y, w, h));
                    }
                }
            }
            4 => {
                // ack_configure(serial): the client acknowledged a configure. Latch it so configured
                // content may now present; if a buffer was committed while we were waiting for the ack,
                // present it now.
                if let Some(surface) = self.wl_surface_of_xdg(m.object) {
                    let present_now = if let Some(Obj::Surface(s)) = self.objs.get_mut(&surface) {
                        let was = s.acked;
                        s.acked = true;
                        !was && s.current_buffer.is_some()
                    } else {
                        false
                    };
                    if present_now {
                        self.present_root(self.root_surface(surface));
                    }
                }
            }
            _ => {}
        }
    }

    // ---- xdg_toplevel: destroy(0), set_parent(1), set_title(2), set_app_id(3), show_window_menu(4),
    //      move(5), resize(6), set_max_size(7), set_min_size(8), set_maximized(9), unset_maximized(10),
    //      set_fullscreen(11), unset_fullscreen(12), set_minimized(13). ----
    fn xdg_toplevel(&mut self, m: Message) {
        // XDG_TOPLEVEL_STATE_*: maximized=1, fullscreen=2, resizing=3, activated=4.
        const ST_MAXIMIZED: u32 = 1;
        const ST_FULLSCREEN: u32 = 2;
        const ST_ACTIVATED: u32 = 4;
        let mut r = m.reader();
        let xdg = match self.objs.get(&m.object) {
            Some(Obj::XdgToplevel { xdg_surface }) => *xdg_surface,
            _ => return,
        };
        match m.opcode {
            0 => {
                // destroy: unmap the toplevel. Clear focus if this was the focused surface.
                if let Some(surface) = self.wl_surface_of_toplevel(m.object) {
                    if self.focus == Some(surface) {
                        self.focus = None;
                    }
                }
                self.objs.remove(&m.object);
            }
            2 => {
                // set_title(title)
                let title = r.string();
                if let Some(surface) = self.wl_surface_of_xdg(xdg) {
                    self.titles.insert(surface, title.clone());
                    if let Some(Obj::Surface(s)) = self.objs.get_mut(&surface) {
                        s.title = title;
                    }
                }
            }
            5 => {
                // move(seat, serial): the client asks the compositor to start an interactive, user-driven
                // window drag. Record the target surface; the live presenter loop turns this into a HOST
                // NSWindow drag ONLY here (the request-gated fix vs. blanket movable-by-window-background).
                if let Some(surface) = self.wl_surface_of_toplevel(m.object) {
                    self.pending_move = Some(surface);
                }
            }
            7 => {
                // set_max_size(width, height)
                let w = r.i32().max(0);
                let h = r.i32().max(0);
                if let Some(surface) = self.wl_surface_of_xdg(xdg) {
                    if let Some(Obj::Surface(s)) = self.objs.get_mut(&surface) {
                        s.max_size = (w, h);
                    }
                }
            }
            8 => {
                // set_min_size(width, height)
                let w = r.i32().max(0);
                let h = r.i32().max(0);
                if let Some(surface) = self.wl_surface_of_xdg(xdg) {
                    if let Some(Obj::Surface(s)) = self.objs.get_mut(&surface) {
                        s.min_size = (w, h);
                    }
                }
            }
            9 => {
                // set_maximized: compositor MUST answer with a configure carrying the maximized state so the
                // client repaints for it. Use the current on-screen size as the hint.
                let (w, h) = self.configure_hint_size(xdg);
                self.send_toplevel_configure(xdg, w, h, &[ST_MAXIMIZED, ST_ACTIVATED]);
            }
            10 => {
                // unset_maximized → back to floating.
                let (w, h) = self.configure_hint_size(xdg);
                self.send_toplevel_configure(xdg, w, h, &[ST_ACTIVATED]);
            }
            11 => {
                // set_fullscreen(output)
                let (w, h) = self.configure_hint_size(xdg);
                self.send_toplevel_configure(xdg, w, h, &[ST_FULLSCREEN, ST_ACTIVATED]);
            }
            12 => {
                // unset_fullscreen
                let (w, h) = self.configure_hint_size(xdg);
                self.send_toplevel_configure(xdg, w, h, &[ST_ACTIVATED]);
            }
            // set_parent(1) / set_app_id(3) / show_window_menu(4) / resize(6) / set_minimized(13):
            // accepted. resize is left to native window chrome (no host-driven interactive resize hook);
            // set_minimized needs no configure per spec.
            _ => {}
        }
    }

    /// Size hint `(w,h)` to put in a state-change configure: the live window content size if the presenter
    /// knows it, else `(0,0)` meaning "client decides" (a valid configure per xdg_toplevel.configure).
    fn configure_hint_size(&self, xdg: u32) -> (i32, i32) {
        self.wl_surface_of_xdg(xdg)
            .and_then(|sid| self.present.window_content_size(sid))
            .unwrap_or((0, 0))
    }

    // ---- xdg_popup: destroy(0), grab(1), reposition(2). ----
    fn xdg_popup(&mut self, m: Message) {
        match m.opcode {
            0 => {
                // destroy: dismiss + unmap the popup.
                self.objs.remove(&m.object);
            }
            // grab(seat, serial): grant the explicit grab (menus). We do not model a separate grab-focus
            // stack in this MVP; accepting keeps Chrome's menu open until it destroys the popup itself.
            _ => {}
        }
    }

    // ---- surface_augmenter: destroy(0), create_solid_color_buffer(1), get_augmented_surface(2) ----
    fn surface_augmenter(&mut self, m: Message) {
        let mut r = m.reader();
        match m.opcode {
            0 => {
                self.objs.remove(&m.object);
            }
            1 => {
                // create_solid_color_buffer(id, color[float r,g,b,a], width, height)
                let id = r.u32();
                let color = r.array();
                let width = r.i32();
                let height = r.i32();
                self.objs.insert(
                    id,
                    Obj::SolidColorBuffer {
                        width,
                        height,
                        bgra: color_to_bgra(&color),
                    },
                );
            }
            2 => {
                let id = r.u32();
                let surface = r.u32();
                self.objs.insert(id, Obj::AugmentedSurface { surface });
            }
            _ => {}
        }
    }

    // ---- zwp_linux_dmabuf_v1: create_params(1), get_default_feedback(2), get_surface_feedback(3) ----
    fn linux_dmabuf(&mut self, m: Message) {
        let mut r = m.reader();
        match m.opcode {
            1 => {
                let id = r.u32();
                self.objs.insert(
                    id,
                    Obj::DmabufParams {
                        iosurface_id: None,
                        stride: 0,
                        gpu_render: false,
                        generation: 0,
                    },
                );
            }
            2 | 3 => {
                // get_default_feedback(id) / get_surface_feedback(id, surface): the v4 render-node
                // discovery hook. chromium reads `main_device` here to find its DRM render node, then
                // takes the normal accelerated path instead of deadlocking in its buffer-manager fallback.
                let id = r.u32();
                self.objs.insert(id, Obj::Other);
                self.send_dmabuf_feedback(id);
            }
            _ => {}
        }
    }

    /// Send one zwp_linux_dmabuf_feedback_v1 round: a format_table fd + the DRM `main_device` (dev_t
    /// matching the engine's synth render node 226:128) + a single tranche advertising ARGB/XRGB8888 +
    /// LINEAR. This is what chromium's ozone GPU uses to resolve the render-node path.
    fn send_dmabuf_feedback(&mut self, fb: u32) {
        // dev_t for /dev/dri/renderD128 == gnu_dev_makedev(226, 128) == (226<<8)|128 == 0xE280 (matches the
        // engine's drm_synth_stat st_rdev, so the client's stat(node) == main_device).
        const DEV_T: u64 = ((226u64) << 8) | 128;
        let dev = DEV_T.to_ne_bytes();
        // format_table: array of {format:u32, pad:u32, modifier:u64} = 16 bytes/entry. LINEAR modifier (0).
        let entries: [(u32, u64); 2] = [(DRM_FMT_ARGB8888, 0), (DRM_FMT_XRGB8888, 0)];
        let mut table = Vec::with_capacity(entries.len() * 16);
        for (fmt, modi) in entries {
            table.extend_from_slice(&fmt.to_ne_bytes());
            table.extend_from_slice(&0u32.to_ne_bytes()); // padding
            table.extend_from_slice(&modi.to_ne_bytes());
        }
        // tranche_formats: le16 indices into the table (both entries).
        let mut idx = Vec::with_capacity(entries.len() * 2);
        for i in 0..entries.len() as u16 {
            idx.extend_from_slice(&i.to_ne_bytes());
        }
        if let Some(fd) = crate::keymap::anon_fd_with(&table) {
            self.conn.queue_fd(fd);
            // format_table(fd(OOB), size)  [opcode 1]
            self.conn.send(&Message::new(fb, 1).u32(table.len() as u32));
        }
        // main_device(device: array)  [opcode 2]
        self.conn.send(&Message::new(fb, 2).array(&dev));
        // tranche_target_device(device: array)  [opcode 4]
        self.conn.send(&Message::new(fb, 4).array(&dev));
        // tranche_formats(indices: array of le16)  [opcode 5]
        self.conn.send(&Message::new(fb, 5).array(&idx));
        // tranche_flags(flags)  [opcode 6] — 0 (no scanout).
        self.conn.send(&Message::new(fb, 6).u32(0));
        // tranche_done  [opcode 3]
        self.conn.send(&Message::new(fb, 3));
        // done  [opcode 0]
        self.conn.send(&Message::new(fb, 0));
    }

    // ---- zwp_linux_buffer_params_v1: add(1), create(2), create_immed(3) ----
    fn dmabuf_params(&mut self, m: Message) {
        let mut r = m.reader();
        match m.opcode {
            1 => {
                // add(fd, plane_idx, offset, stride, modifier_hi, modifier_lo)
                if let Some(fd) = self.conn.take_fd() {
                    unsafe { libc::close(fd) }; // dmabuf handle is a placeholder; we resolve by IOSurface id
                }
                let _plane = r.u32();
                let _offset = r.u32();
                let stride = r.i32();
                let mod_hi = r.u32();
                let mod_lo = r.u32();
                if let Some(Obj::DmabufParams {
                    iosurface_id,
                    stride: s,
                    gpu_render,
                    generation,
                }) = self.objs.get_mut(&m.object)
                {
                    *s = stride;
                    if mod_hi & 0xffff == DD_DMABUF_MOD_MAGIC {
                        *iosurface_id = Some(mod_lo);
                        *gpu_render = mod_hi & DD_DMABUF_RENDER_BIT != 0;
                        // Allocation generation (modifier_hi bits 17..=31); authenticated at create_immed.
                        *generation = (mod_hi >> 17) & 0x7fff;
                    }
                }
            }
            3 => {
                // create_immed(buffer_id, width, height, format, flags): client provides the buffer id.
                let buffer_id = r.u32();
                let w = r.i32();
                let h = r.i32();
                let format = r.u32();
                let (iosurface_id, gpu_render, generation) = match self.objs.get(&m.object) {
                    Some(Obj::DmabufParams {
                        iosurface_id,
                        gpu_render,
                        generation,
                        ..
                    }) => (*iosurface_id, *gpu_render, *generation),
                    _ => (None, false, 0),
                };
                // Authenticate the allocation generation (mirrors the Smithay compositor's import check):
                // a versioned reference (non-zero generation) whose generation no longer matches the id's
                // live host allocation is a stale reference to a retired/reissued IOSurface — reject it.
                // Only macOS resolves real IOSurfaces (and tracks their generations), so the check is a
                // no-op elsewhere.
                #[cfg(target_os = "macos")]
                let stale = iosurface_id.is_some_and(|id| {
                    generation != 0 && generation != crate::metal::iosurface_generation(id)
                });
                #[cfg(not(target_os = "macos"))]
                let stale = {
                    let _ = generation;
                    false
                };
                match iosurface_id {
                    _ if stale => {
                        self.post_error(m.object, 4 /* invalid_format */, "stale dmabuf allocation generation (the IOSurface id was retired and reissued)");
                    }
                    Some(id) => {
                        self.objs.insert(
                            buffer_id,
                            Obj::DmaBuffer {
                                width: w,
                                height: h,
                                format,
                                iosurface_id: id,
                                gpu_render,
                            },
                        );
                    }
                    None => {
                        // No dd IOSurface tag (e.g. the advertised LINEAR modifier): the compositor
                        // cannot back this buffer. create_immed has no `failed` event, so report the
                        // spec's INVALID_FORMAT protocol error rather than handing back an inert object
                        // the client would attach and get missing frames from.
                        self.post_error(m.object, 4 /* invalid_format */, "unsupported dmabuf modifier (only dd IOSurface-tagged buffers are usable)");
                    }
                }
            }
            2 => {
                // create(width, height, format, flags): would need a server-allocated buffer id in a
                // `created`/`failed` event. Not supported for MVP — signal failed so the client falls back.
                self.conn.send(&Message::new(m.object, 1)); // zwp_linux_buffer_params_v1.failed
            }
            _ => {}
        }
    }

    // ---- wp_presentation: destroy(0), feedback(1, surface, callback) ----
    // `feedback` requests presentation timing for the surface's NEXT content update (the commit that
    // follows). We record the callback id against the surface; it is answered in `commit` once the frame
    // is presented. See weston `presentation_feedback` / wlroots `wlr_presentation_surface_textured`.
    fn wp_presentation(&mut self, m: Message) {
        if m.opcode != 1 {
            return; // destroy(0): the global object is inert; existing feedback objects are unaffected.
        }
        let mut r = m.reader();
        let surface = r.u32();
        let callback = r.u32();
        self.objs.insert(callback, Obj::PresentationFeedback);
        if let Some(Obj::Surface(s)) = self.objs.get_mut(&surface) {
            s.pending_feedback.push(callback);
        } else {
            // No such surface (or wrong role): discard immediately so the client's object is cleaned up.
            self.conn.send(&Message::new(callback, 2)); // discarded [opcode 2]
            self.conn.send(&Message::new(WL_DISPLAY, 1).u32(callback)); // delete_id
            self.objs.remove(&callback);
        }
    }

    /// Answer every `wp_presentation_feedback` requested for surface `sid`'s just-processed commit. When
    /// `presented` is true we emit `sync_output` + `presented` (monotonic timestamp, mode refresh interval,
    /// MSC, vsync flag); otherwise `discarded`. Each feedback object is terminal, so we also send the
    /// matching `wl_display.delete_id` (exactly as the frame `wl_callback` is retired). Mirrors weston
    /// `weston_presentation_feedback_present`/`_discard`.
    fn send_presentation_feedback(&mut self, sid: u32, presented: bool) {
        let cbs = match self.objs.get_mut(&sid) {
            Some(Obj::Surface(s)) if !s.pending_feedback.is_empty() => {
                std::mem::take(&mut s.pending_feedback)
            }
            _ => return,
        };
        if presented {
            let (secs, nsec) = monotonic_now();
            let tv_sec_hi = (secs >> 32) as u32;
            let tv_sec_lo = (secs & 0xffff_ffff) as u32;
            let refresh = output_refresh_ns();
            self.present_seq = self.present_seq.wrapping_add(1);
            let seq = self.present_seq;
            let seq_hi = (seq >> 32) as u32;
            let seq_lo = (seq & 0xffff_ffff) as u32;
            let output = self.output;
            for cb in cbs {
                if let Some(out) = output {
                    // sync_output(output) [opcode 0] — precedes presented, names the surface's output.
                    self.conn.send(&Message::new(cb, 0).u32(out));
                }
                // presented(tv_sec_hi, tv_sec_lo, tv_nsec, refresh, seq_hi, seq_lo, flags) [opcode 1]
                self.conn.send(
                    &Message::new(cb, 1)
                        .u32(tv_sec_hi)
                        .u32(tv_sec_lo)
                        .u32(nsec)
                        .u32(refresh)
                        .u32(seq_hi)
                        .u32(seq_lo)
                        .u32(WP_PRESENTATION_KIND_VSYNC),
                );
                self.conn.send(&Message::new(WL_DISPLAY, 1).u32(cb)); // delete_id
                self.objs.remove(&cb);
            }
        } else {
            for cb in cbs {
                self.conn.send(&Message::new(cb, 2)); // discarded [opcode 2]
                self.conn.send(&Message::new(WL_DISPLAY, 1).u32(cb)); // delete_id
                self.objs.remove(&cb);
            }
        }
    }

    // ---- wl_seat: get_pointer(0)/get_keyboard(1)/get_touch(2) ----
    fn wl_seat(&mut self, m: Message) {
        let mut r = m.reader();
        match m.opcode {
            0 => {
                let id = r.u32();
                self.objs.insert(id, Obj::Pointer);
                self.pointer = Some(id);
                self.ptr_entered = false;
            }
            1 => {
                let id = r.u32();
                self.objs.insert(id, Obj::Keyboard);
                self.keyboard = Some(id);
                self.kbd_entered = false;
                self.send_keymap(id);
                if self.seat_ver >= 4 {
                    // repeat_info(rate, delay) — key auto-repeat (keys/s, ms).
                    self.conn.send(&Message::new(id, 5).i32(25).i32(600));
                }
            }
            2 => {
                // get_touch(id): track the object so the touch injection API can deliver events. There is
                // no touch input source on the macOS backend today, but a touch-capable seat must hand out a
                // working wl_touch (the events are wired in touch_down/up/motion/frame/cancel below).
                let id = r.u32();
                self.objs.insert(id, Obj::Touch);
                self.touch = Some(id);
            }
            _ => {}
        }
    }

    // ---- wp_cursor_shape_manager_v1: destroy(0), get_pointer(1, id, pointer),
    //      get_tablet_tool_v2(2, id, tablet_tool) ----
    // Both getters mint a wp_cursor_shape_device_v1 bound to a seat device; we track the device id so its
    // set_shape can drive the host cursor. The `pointer`/`tablet_tool` argument is the seat device the shape
    // applies to — we have a single global pointer, so the association needs no per-device state.
    fn wp_cursor_shape_manager(&mut self, m: Message) {
        let mut r = m.reader();
        match m.opcode {
            1 | 2 => {
                let id = r.u32();
                self.objs.insert(id, Obj::CursorShapeDevice);
            }
            _ => {} // destroy(0): the object is freed by the client; nothing server-side to release.
        }
    }

    // ---- wp_cursor_shape_device_v1: destroy(0), set_shape(1, serial, shape) ----
    // set_shape names a themed pointer (the `shape` enum). Map it to the host cursor via the presenter. The
    // serial identifies the enter that gave us pointer focus; we drive a single host pointer, so it is not
    // needed to disambiguate. An out-of-range shape (0 or > known) is a protocol error in the spec, but we
    // tolerate it by falling back to the default arrow rather than dropping the client.
    fn wp_cursor_shape_device_v1(&mut self, m: Message) {
        if m.opcode != 1 {
            return;
        }
        let mut r = m.reader();
        let _serial = r.u32();
        let shape = r.u32();
        self.present.set_cursor_shape(shape);
    }

    // ---- wl_subcompositor: destroy(0), get_subsurface(1, id, surface, parent) ----
    // get_subsurface gives `surface` the subsurface role: it becomes a child of `parent`, composited
    // into the parent's frame at a client-set position/z-order, initially in synchronized mode
    // (wl_subcompositor.xml get_subsurface; wlroots `subcompositor_handle_get_subsurface`,
    // types/wlr_subcompositor.c:308-354).
    fn wl_subcompositor(&mut self, m: Message) {
        if m.opcode != 1 {
            return;
        }
        let mut r = m.reader();
        let id = r.u32(); // new wl_subsurface id
        let surface = r.u32();
        let parent = r.u32();
        let valid = surface != parent
            && matches!(self.objs.get(&surface), Some(Obj::Surface(_)))
            && matches!(self.objs.get(&parent), Some(Obj::Surface(_)))
            // bad_parent: a surface may not be an ancestor of its own parent (cycle).
            && self.root_surface(parent) != surface;
        if !valid {
            self.objs.insert(id, Obj::Other);
            return;
        }
        self.objs.insert(id, Obj::Subsurface { surface });
        if let Some(Obj::Surface(s)) = self.objs.get_mut(&surface) {
            s.subsurface_parent = Some(parent);
            s.subsurface_sync = true; // subsurfaces start synchronized
        }
        if let Some(Obj::Surface(p)) = self.objs.get_mut(&parent) {
            if !p.children.contains(&surface) {
                p.children.push(surface); // new subsurface goes on top of the stack
            }
        }
    }

    // ---- wl_subsurface: destroy(0), set_position(1), place_above(2), place_below(3), set_sync(4),
    //      set_desync(5) ----
    // Reference: wlroots `subsurface_implementation` (types/wlr_subcompositor.c:184-191).
    fn wl_subsurface(&mut self, m: Message) {
        let surface = match self.objs.get(&m.object) {
            Some(Obj::Subsurface { surface }) => *surface,
            _ => return,
        };
        let mut r = m.reader();
        match m.opcode {
            0 => {
                // destroy: detach the role; the wl_surface itself lives on.
                let parent = match self.objs.get(&surface) {
                    Some(Obj::Surface(s)) => s.subsurface_parent,
                    _ => None,
                };
                if let Some(p) = parent {
                    if let Some(Obj::Surface(ps)) = self.objs.get_mut(&p) {
                        ps.children.retain(|c| *c != surface);
                    }
                }
                if let Some(Obj::Surface(s)) = self.objs.get_mut(&surface) {
                    s.subsurface_parent = None;
                }
                self.objs.remove(&m.object);
            }
            1 => {
                // set_position(x, y): double-buffered onto the parent's commit (wlroots writes
                // subsurface->pending.x/y, applied in surface_state_move at parent commit).
                let x = r.i32();
                let y = r.i32();
                if let Some(Obj::Surface(s)) = self.objs.get_mut(&surface) {
                    s.pending_sub_x = x;
                    s.pending_sub_y = y;
                    s.has_pending_pos = true;
                }
            }
            2 => {
                let sibling = r.u32();
                self.subsurface_place(surface, sibling, true);
            }
            3 => {
                let sibling = r.u32();
                self.subsurface_place(surface, sibling, false);
            }
            4 => {
                if let Some(Obj::Surface(s)) = self.objs.get_mut(&surface) {
                    s.subsurface_sync = true;
                }
            }
            5 => {
                // set_desync: if the subsurface is now effectively desynchronized and holds a cached
                // commit, apply it immediately and recomposite (wlroots subsurface_handle_set_desync).
                if let Some(Obj::Surface(s)) = self.objs.get_mut(&surface) {
                    s.subsurface_sync = false;
                }
                if !self.is_effectively_synchronized(surface) {
                    let mut dirty = false;
                    if let Some(Obj::Surface(s)) = self.objs.get_mut(&surface) {
                        if s.has_cached {
                            s.current_buffer = s.cached_buffer;
                            s.pending_release = s.cached_buffer;
                            s.has_cached = false;
                            s.pending_frame.append(&mut s.cached_frame);
                            dirty = true;
                        }
                        if s.has_pending_pos {
                            s.subsurface_x = s.pending_sub_x;
                            s.subsurface_y = s.pending_sub_y;
                            s.has_pending_pos = false;
                            dirty = true;
                        }
                    }
                    if dirty {
                        let root = self.root_surface(surface);
                        self.present_root(root);
                    }
                }
            }
            _ => {}
        }
    }

    /// Reorder `surface` within its parent's bottom→top child stack relative to `sibling`. All
    /// subsurfaces are modeled as stacking above the parent, so place_above/below the parent itself both
    /// resolve to the bottom of that stack (place_below-the-parent — rendering under the parent — is not
    /// represented; overlays place above). place relative to a real sibling honors true z-order.
    fn subsurface_place(&mut self, surface: u32, sibling: u32, above: bool) {
        let parent = match self.objs.get(&surface) {
            Some(Obj::Surface(s)) => s.subsurface_parent,
            _ => return,
        };
        let Some(parent) = parent else { return };
        if let Some(Obj::Surface(p)) = self.objs.get_mut(&parent) {
            p.children.retain(|c| *c != surface);
            if sibling == parent {
                p.children.insert(0, surface);
            } else if let Some(pos) = p.children.iter().position(|c| *c == sibling) {
                let idx = if above { pos + 1 } else { pos };
                p.children.insert(idx.min(p.children.len()), surface);
            } else {
                p.children.push(surface); // unknown sibling: leave on top
            }
        }
    }

    // ---- wp_viewporter: destroy(0), get_viewport(1, id, surface) ----
    // Chromium uses viewports for logical crop/scale. Track the object so set_source/set_destination can
    // affect the next committed wl_shm buffer instead of letting oversized buffers spill past the window.
    fn wp_viewporter(&mut self, m: Message) {
        if m.opcode == 1 {
            let mut r = m.reader();
            let id = r.u32(); // new wp_viewport id
            let surface = r.u32();
            self.objs.insert(id, Obj::Viewport { surface });
        }
    }

    // ---- wp_viewport: destroy(0), set_source(1), set_destination(2) ----
    fn wp_viewport(&mut self, m: Message) {
        let surface = match self.objs.get(&m.object) {
            Some(Obj::Viewport { surface }) => *surface,
            _ => return,
        };
        let mut r = m.reader();
        match m.opcode {
            0 => {
                if let Some(Obj::Surface(s)) = self.objs.get_mut(&surface) {
                    s.viewport_source = None;
                    s.viewport_destination = None;
                }
                self.objs.remove(&m.object);
            }
            1 => {
                let x = r.i32();
                let y = r.i32();
                let w = r.i32();
                let h = r.i32();
                if x == -1 && y == -1 && w == -1 && h == -1 {
                    if let Some(Obj::Surface(s)) = self.objs.get_mut(&surface) {
                        s.viewport_source = None;
                    }
                } else if w > 0 && h > 0 {
                    if let Some(Obj::Surface(s)) = self.objs.get_mut(&surface) {
                        s.viewport_source = Some((x, y, w, h));
                    }
                } else {
                    // A source rect that is neither the (-1,-1,-1,-1) unset sentinel nor strictly
                    // positive is invalid — reject it rather than silently ignoring it.
                    self.post_error(m.object, 0 /* bad_value */, "wp_viewport.set_source: invalid rectangle");
                }
            }
            2 => {
                let w = r.i32();
                let h = r.i32();
                if w == -1 && h == -1 {
                    if let Some(Obj::Surface(s)) = self.objs.get_mut(&surface) {
                        s.viewport_destination = None;
                    }
                } else if w > 0 && h > 0 {
                    if let Some(Obj::Surface(s)) = self.objs.get_mut(&surface) {
                        s.viewport_destination = Some((w, h));
                    }
                } else {
                    // An invalid destination (e.g. 0x1) must be a protocol error, not silently ignored
                    // while an older valid destination lingers and drives later commits with stale geometry.
                    self.post_error(m.object, 0 /* bad_value */, "wp_viewport.set_destination: invalid size");
                }
            }
            _ => {}
        }
    }

    // ---- wl_data_device_manager: create_data_source(0, id), get_data_device(1, id, seat) ----
    // Clipboard/DnD entry point. chromium binds this unconditionally. The child objects are real: a
    // data_source advertises clipboard content, a data_device receives the current selection, and
    // wl_data_offer bridges a reader's fd back to the source (see wl_data_device / wl_data_source /
    // wl_data_offer below). DnD (start_drag) is accepted but not driven — there is no host drag source.
    fn wl_data_device_manager(&mut self, m: Message) {
        let mut r = m.reader();
        match m.opcode {
            // create_data_source(new_id id)
            0 => {
                let id = r.u32();
                self.objs.insert(id, Obj::DataSource { mimes: Vec::new() });
            }
            // get_data_device(new_id id, object seat)
            1 => {
                let id = r.u32();
                self.objs.insert(id, Obj::DataDevice);
                if !self.data_devices.contains(&id) {
                    self.data_devices.push(id);
                }
                // A data_device created while a selection already exists must be told about it (weston
                // sends the current selection to a newly-bound device with focus). Replay so the client
                // can paste immediately rather than only after the next copy.
                if self.selection.is_some() {
                    self.offer_selection_to(id);
                }
            }
            _ => {}
        }
    }

    // ---- wl_data_source: offer(0, mime), destroy(1), set_actions(2, dnd_actions) [v3] ----
    // A client's advertiser of clipboard/DnD content. `offer` accrues MIME types; the compositor
    // replays them as wl_data_offer.offer events when this source is the active selection.
    fn wl_data_source(&mut self, m: Message) {
        let mut r = m.reader();
        match m.opcode {
            // offer(string mime_type): record an advertised MIME type.
            0 => {
                let mime = r.string();
                if let Some(Obj::DataSource { mimes }) = self.objs.get_mut(&m.object) {
                    if !mime.is_empty() && !mimes.contains(&mime) {
                        mimes.push(mime);
                    }
                }
            }
            // destroy(): the client tears down the source. If it was the current selection, clear it so
            // a subsequent receive can't be forwarded to a dead source.
            1 => {
                if self.selection == Some(m.object) {
                    self.clear_selection();
                }
                self.mark_offers_stale_for(m.object);
                self.objs.remove(&m.object);
                self.retire_id(m.object);
            }
            // set_actions(uint dnd_actions) [v3, DnD-only]: no host drag source, so record nothing.
            _ => {}
        }
    }

    // ---- wl_data_device: start_drag(0), set_selection(1, source, serial), release(2) [v2] ----
    fn wl_data_device(&mut self, m: Message) {
        let mut r = m.reader();
        match m.opcode {
            // start_drag(object? source, object origin, object? icon, uint serial): DnD is not driven on
            // this backend (no host drag session), but the source object stays valid so the client can
            // still cancel/destroy it cleanly. Nothing to emit.
            0 => {}
            // set_selection(object? source, uint serial): make `source` the clipboard owner (source==0
            // clears it), then advertise the new selection to every data_device on this connection.
            1 => {
                let source = r.u32();
                let _serial = r.u32();
                if source == 0 {
                    self.clear_selection();
                    return;
                }
                if !matches!(self.objs.get(&source), Some(Obj::DataSource { .. })) {
                    return; // unknown / wrong-typed source id: ignore rather than fault the client.
                }
                // Any offer minted for the previous selection is now superseded; mark it stale so a late
                // receive on it is dropped (the client is expected to destroy it after the new offer).
                self.mark_all_offers_stale();
                self.selection = Some(source);
                let devices = self.data_devices.clone();
                for dev in devices {
                    self.offer_selection_to(dev);
                }
            }
            // release(): destructor (v2). Drop the device; it no longer receives selection events.
            2 => {
                self.data_devices.retain(|d| *d != m.object);
                self.objs.remove(&m.object);
                self.retire_id(m.object);
            }
            _ => {}
        }
    }

    // ---- wl_data_offer: accept(0), receive(1, mime, fd), destroy(2), finish(3)[v3], set_actions(4)[v3] ----
    // A server-allocated proxy for the current selection. `receive` is the payload transfer: the reader
    // passes a pipe write-fd, which the compositor hands to the owning wl_data_source as `send(mime, fd)`
    // so the source writes the clipboard bytes into it.
    fn wl_data_offer(&mut self, m: Message) {
        let mut r = m.reader();
        match m.opcode {
            // accept(uint serial, string? mime_type): DnD accept handshake; no-op for selection reads.
            0 => {}
            // receive(string mime_type, fd fd): forward to the source as wl_data_source.send(mime, fd).
            1 => {
                let mime = r.string();
                // The fd rides SCM_RIGHTS ahead of the request body; take it even on the error paths so a
                // rejected receive doesn't leak the client's pipe end into this process.
                let fd = self.conn.take_fd();
                let source = match self.objs.get(&m.object) {
                    Some(Obj::DataOffer { source, stale: false }) => Some(*source),
                    _ => None,
                };
                // Only honor a live offer whose source is still the current selection.
                let target = source.filter(|s| self.selection == Some(*s));
                match (target, fd) {
                    (Some(src), Some(fd)) => {
                        // wl_data_source.send(string mime_type, fd fd) — opcode 1. The fd rides the next
                        // flush via SCM_RIGHTS; the source client writes the bytes and closes its end.
                        self.conn.queue_fd(fd);
                        self.conn.send(&Message::new(src, 1).string(&mime));
                        // Our received copy of the pipe fd has been dup'd to the peer by SCM_RIGHTS on
                        // flush; libwayland owns its own copy after receive. We keep ours queued until
                        // flush consumes it (see Conn::flush) and accept the same small per-transfer fd
                        // retention as send_keymap — the compositor is per-client and short-lived.
                    }
                    (_, Some(fd)) => {
                        // No live source (stale offer, cleared/changed selection): close the reader's fd
                        // so its read() sees EOF instead of blocking forever.
                        unsafe {
                            libc::close(fd);
                        }
                    }
                    _ => {}
                }
            }
            // destroy(): client is done with this offer. Server-allocated id ⇒ no delete_id (that ack is
            // only for recycling client-range ids); just drop it.
            2 => {
                self.objs.remove(&m.object);
            }
            // finish(3)/set_actions(4): DnD-only [v3]; nothing to do for a selection offer.
            _ => {}
        }
    }

    /// Advertise the current selection to one data_device: mint a server-side wl_data_offer, announce it
    /// via `wl_data_device.data_offer(new_id)`, replay the source's MIME types as `wl_data_offer.offer`,
    /// then hand it over with `wl_data_device.selection(offer)`. No-op when the selection is empty.
    fn offer_selection_to(&mut self, device: u32) {
        let Some(source) = self.selection else {
            return;
        };
        let mimes = match self.objs.get(&source) {
            Some(Obj::DataSource { mimes }) => mimes.clone(),
            _ => return,
        };
        let offer = self.alloc_server_id();
        self.objs.insert(offer, Obj::DataOffer { source, stale: false });
        // wl_data_device.data_offer(new_id id) — opcode 0. Introduces the offer object id to the client.
        self.conn.send(&Message::new(device, 0).u32(offer));
        // wl_data_offer.offer(string mime_type) — opcode 0. One per advertised type, before selection.
        for mime in &mimes {
            self.conn.send(&Message::new(offer, 0).string(mime));
        }
        // wl_data_device.selection(object? id) — opcode 5. Hands the offer to the client as the current
        // clipboard; the client reads it via wl_data_offer.receive.
        self.conn.send(&Message::new(device, 5).u32(offer));
    }

    /// Clear the clipboard: forget the source and tell every data_device the selection is now empty
    /// (`wl_data_device.selection` with a null offer id).
    fn clear_selection(&mut self) {
        self.mark_all_offers_stale();
        self.selection = None;
        let devices = self.data_devices.clone();
        for dev in devices {
            // wl_data_device.selection(null) — opcode 5, object id 0 ⇒ "no selection".
            self.conn.send(&Message::new(dev, 5).u32(0));
        }
    }

    /// Mark every live wl_data_offer stale so a late `receive` on a superseded offer is dropped rather
    /// than forwarded to a source that may already be gone.
    fn mark_all_offers_stale(&mut self) {
        for obj in self.objs.values_mut() {
            if let Obj::DataOffer { stale, .. } = obj {
                *stale = true;
            }
        }
    }

    /// Mark offers backed by a specific (now-destroyed) source stale.
    fn mark_offers_stale_for(&mut self, source: u32) {
        for obj in self.objs.values_mut() {
            if let Obj::DataOffer { source: s, stale } = obj {
                if *s == source {
                    *stale = true;
                }
            }
        }
    }

    /// Send `wl_keyboard.keymap(format=XKB_V1, fd, size)`: the client mmaps this fd to build its
    /// `xkb_state`. We ship a minimal-but-valid XKB keymap (evdev keycodes, us symbols) over an anonymous
    /// shared fd, so the client's xkbcommon accepts it and input initializes.
    fn send_keymap(&mut self, kbd: u32) {
        let km = crate::keymap::US_XKB_KEYMAP;
        let size = km.len() as u32 + 1; // include NUL, as libwayland expects
        if let Some(fd) = crate::keymap::anon_fd_with(km.as_bytes()) {
            self.conn.queue_fd(fd);
            // keymap(format=1 /*XKB_V1*/, fd(OOB), size)
            self.conn.send(&Message::new(kbd, 0).u32(1).u32(size));
            // The fd is dup'd into the client by SCM_RIGHTS on flush; close our copy after.
            // (Conn takes ownership of the queued fd on flush; we don't close here to avoid a race —
            // libwayland dups on receive. A small fd leak per keyboard is acceptable for the MVP.)
        }
    }

    // ================= M2 input injection API =================
    // The host backend (NSEvent path on macOS; the headless test directly) calls these to deliver input.
    // Events route to the focused/pointed surface; enter is sent lazily before the first event.

    /// Move input focus to `new`, sending `wl_pointer.leave` / `wl_keyboard.leave` to the previously
    /// focused surface first (when it had received the matching enter). Without the leave, a client can
    /// believe it still holds pointer/keyboard focus after focus has moved elsewhere.
    fn transfer_focus(&mut self, new: u32) {
        if self.focus == Some(new) {
            return;
        }
        if let Some(old) = self.focus {
            if self.ptr_entered {
                if let Some(ptr) = self.pointer {
                    let s = self.next_serial();
                    self.conn.send(&Message::new(ptr, 1).u32(s).u32(old)); // wl_pointer.leave
                    if self.seat_ver >= 5 {
                        self.conn.send(&Message::new(ptr, 5)); // frame
                    }
                }
            }
            if self.kbd_entered {
                if let Some(kbd) = self.keyboard {
                    let s = self.next_serial();
                    self.conn.send(&Message::new(kbd, 2).u32(s).u32(old)); // wl_keyboard.leave
                }
            }
        }
        self.focus = Some(new);
        self.ptr_entered = false;
        self.kbd_entered = false;
    }

    fn ensure_pointer_enter(&mut self) {
        if self.ptr_entered {
            return;
        }
        let (Some(ptr), Some(focus)) = (self.pointer, self.focus) else {
            return;
        };
        let s = self.next_serial();
        let (x, y) = self.last_ptr;
        // enter(serial, surface, surface_x(fixed), surface_y(fixed))
        self.conn.send(
            &Message::new(ptr, 0)
                .u32(s)
                .u32(focus)
                .i32(fixed(x))
                .i32(fixed(y)),
        );
        // enter is a logical pointer event and must be closed by a frame at v5+ so the client processes
        // the focus change before the first motion/button/axis group (weston libweston/input.c: every
        // wl_pointer_send_enter is immediately followed by pointer_send_frame; wayland.xml wl_pointer.frame:
        // "The wl_pointer.enter and wl_pointer.leave events ... are also grouped by a wl_pointer.frame").
        if self.seat_ver >= 5 {
            self.conn.send(&Message::new(ptr, 5)); // frame
        }
        self.ptr_entered = true;
    }

    fn ensure_keyboard_enter(&mut self) {
        if self.kbd_entered {
            return;
        }
        let (Some(kbd), Some(focus)) = (self.keyboard, self.focus) else {
            return;
        };
        let s = self.next_serial();
        // enter(serial, surface, keys[] (currently-pressed, empty))
        self.conn
            .send(&Message::new(kbd, 1).u32(s).u32(focus).array(&[]));
        // A keyboard.enter must be paired with the initial modifier state, using the SAME serial, so the
        // client's xkb_state starts consistent (weston libweston/input.c send_enter_to_resource_list:
        // wl_keyboard_send_enter is immediately followed by send_modifiers_to_resource with `serial`).
        // modifiers(serial, depressed, latched, locked, group) — all zero at focus-in.
        self.conn.send(
            &Message::new(kbd, 4)
                .u32(s)
                .u32(0)
                .u32(0)
                .u32(0)
                .u32(0),
        );
        self.kbd_entered = true;
    }

    /// The focused surface's window-geometry origin (`xdg_surface.set_window_geometry` x,y minus the
    /// buffer attach offset), i.e. how far the on-screen (geometry-cropped) content is inset from the
    /// wl_surface's top-left. Clients like GTK render CSD shadow margins around the visible window and set
    /// the geometry to the inner rect; the compositor crops the presented buffer to that rect (see
    /// `surface_mapping`), so the native window shows content starting at (gx,gy) in surface space. The
    /// host derives pointer coordinates from that cropped window (content-local), but `wl_pointer.motion`
    /// must be FULL-surface-local — so this origin is added back. Zero when the client sets no geometry.
    fn focused_geometry_origin(&self) -> (i32, i32) {
        let Some(sid) = self.focus else {
            return (0, 0);
        };
        let Some(Obj::Surface(s)) = self.objs.get(&sid) else {
            return (0, 0);
        };
        match s.window_geometry {
            Some((gx, gy, gw, gh)) if gw > 0 && gh > 0 => {
                ((gx - s.attach_x).max(0), (gy - s.attach_y).max(0))
            }
            _ => (0, 0),
        }
    }

    /// Absolute pointer motion in surface-local integer pixels. `x`/`y` arrive content-local (relative to
    /// the on-screen, geometry-cropped window); the focused surface's window-geometry origin is added so
    /// the coordinate the client receives is full-surface-local (see `focused_geometry_origin`).
    pub fn pointer_motion(&mut self, x: i32, y: i32) {
        let (ox, oy) = self.focused_geometry_origin();
        if std::env::var_os("DD_DISPLAY_INPUT_DEBUG").is_some_and(|v| !v.is_empty() && v != "0") {
            eprintln!(
                "dd-display[input]: pointer_motion content=({x},{y}) geometry_origin=({ox},{oy}) surface_local=({},{}) focus={:?}",
                x + ox,
                y + oy,
                self.focus
            );
        }
        let (x, y) = (x + ox, y + oy);
        self.last_ptr = (x, y);
        self.ensure_pointer_enter();
        let Some(ptr) = self.pointer else { return };
        self.time_ms = self.time_ms.wrapping_add(8);
        let t = self.time_ms;
        // motion(time, x(fixed), y(fixed))
        self.conn
            .send(&Message::new(ptr, 2).u32(t).i32(fixed(x)).i32(fixed(y)));
        if self.seat_ver >= 5 {
            self.conn.send(&Message::new(ptr, 5)); // frame
        }
        self.conn.flush().ok();
    }

    /// Pointer button (evdev code: BTN_LEFT=0x110, RIGHT=0x111, MIDDLE=0x112). `pressed` state.
    pub fn pointer_button(&mut self, button: u32, pressed: bool) {
        self.ensure_pointer_enter();
        let Some(ptr) = self.pointer else { return };
        let s = self.next_serial();
        self.time_ms = self.time_ms.wrapping_add(8);
        let t = self.time_ms;
        // button(serial, time, button, state)
        self.conn.send(
            &Message::new(ptr, 3)
                .u32(s)
                .u32(t)
                .u32(button)
                .u32(pressed as u32),
        );
        if self.seat_ver >= 5 {
            self.conn.send(&Message::new(ptr, 5)); // frame
        }
        self.conn.flush().ok();
    }

    /// One scroll event group. `dx`/`dy` are surface-local scroll amounts in pixels (positive dy = scroll
    /// down/content up, positive dx = scroll right), already sign-corrected by the caller. `precise` marks a
    /// smooth/trackpad source (continuous pixel scrolling) vs a stepped mouse wheel.
    ///
    /// A single logical scroll is delivered as one wl_pointer.frame group: an optional axis_source, then the
    /// per-axis wl_pointer.axis (+ wl_pointer.axis_discrete for a stepped wheel at v5-7), then the frame that
    /// tells the client the group is complete. Missing the axis/frame pairing is why scroll appears dead:
    /// Chrome/viz accumulates axis deltas and only dispatches them on frame (wayland.xml wl_pointer.frame:
    /// "A client is expected to accumulate the data in all events within the frame before proceeding").
    pub fn pointer_scroll(&mut self, dx: i32, dy: i32, precise: bool) {
        self.ensure_pointer_enter();
        let Some(ptr) = self.pointer else { return };
        if dx == 0 && dy == 0 {
            return;
        }
        self.time_ms = self.time_ms.wrapping_add(8);
        let t = self.time_ms;
        // axis_source (v5+): finger/continuous smooth scroll for a trackpad, physical wheel otherwise. Sent
        // once, before the axis events of this frame (wayland.xml wl_pointer.axis_source: "sent before a
        // wl_pointer.frame event and carries the source information for all events within that frame").
        const AXIS_SOURCE_WHEEL: u32 = 0;
        const AXIS_SOURCE_CONTINUOUS: u32 = 2;
        const AXIS_VERTICAL: u32 = 0;
        const AXIS_HORIZONTAL: u32 = 1;
        if self.seat_ver >= 5 {
            let source = if precise {
                AXIS_SOURCE_CONTINUOUS
            } else {
                AXIS_SOURCE_WHEEL
            };
            self.conn.send(&Message::new(ptr, 6).u32(source)); // axis_source
        }
        // For a stepped wheel the axis value is a small multiple of the click count (weston uses ~10 units
        // per detent) and axis_discrete carries the integer click count; for a smooth source the pixel delta
        // is sent verbatim with no discrete steps.
        for (axis, delta) in [(AXIS_VERTICAL, dy), (AXIS_HORIZONTAL, dx)] {
            if delta == 0 {
                continue;
            }
            let value = if precise { delta } else { delta * 10 };
            // axis(time, axis, value(fixed))
            self.conn
                .send(&Message::new(ptr, 4).u32(t).u32(axis).i32(fixed(value)));
            if self.seat_ver >= 5 && !precise {
                // axis_discrete(axis, discrete) — v5..7 stepped-wheel hint (deprecated by value120 at v8; we
                // bind v5 so this is the correct field). One detent per unit of delta.
                self.conn
                    .send(&Message::new(ptr, 8).u32(axis).i32(delta.signum()));
            }
        }
        if self.seat_ver >= 5 {
            self.conn.send(&Message::new(ptr, 5)); // frame — closes the scroll group
        }
        self.conn.flush().ok();
    }

    /// A key press/release. `keycode` is the raw evdev code (e.g. KEY_A=30); the client adds +8 for xkb.
    pub fn key(&mut self, keycode: u32, pressed: bool) {
        self.ensure_keyboard_enter();
        let Some(kbd) = self.keyboard else { return };
        let s = self.next_serial();
        self.time_ms = self.time_ms.wrapping_add(8);
        let t = self.time_ms;
        // key(serial, time, key, state)  state: 0=released 1=pressed
        self.conn.send(
            &Message::new(kbd, 3)
                .u32(s)
                .u32(t)
                .u32(keycode)
                .u32(pressed as u32),
        );
        self.conn.flush().ok();
    }

    /// Keyboard modifier state (xkb masks), drives the client's `xkb_state_update_mask`.
    pub fn modifiers(&mut self, depressed: u32, latched: u32, locked: u32, group: u32) {
        self.ensure_keyboard_enter();
        let Some(kbd) = self.keyboard else { return };
        let s = self.next_serial();
        // modifiers(serial, depressed, latched, locked, group)
        self.conn.send(
            &Message::new(kbd, 4)
                .u32(s)
                .u32(depressed)
                .u32(latched)
                .u32(locked)
                .u32(group),
        );
        self.conn.flush().ok();
    }

    // ---- wl_touch injection (down/motion/up + frame; cancel) ----
    // Touch events are double-grouped like pointer events: a set of down/motion/up events for one logical
    // frame is closed by wl_touch.frame (wayland.xml wl_touch.frame: "indicates the end of a contact point
    // list"). down/motion/up therefore do NOT flush — the caller closes the group with `touch_frame`.

    /// A new touch point `id` at surface-local `(x,y)` pixels. Routed to the focused surface.
    pub fn touch_down(&mut self, id: i32, x: i32, y: i32) {
        let (Some(touch), Some(focus)) = (self.touch, self.focus) else {
            return;
        };
        self.last_ptr = (x, y);
        let s = self.next_serial();
        self.time_ms = self.time_ms.wrapping_add(8);
        let t = self.time_ms;
        // down(serial, time, surface, id, x(fixed), y(fixed))
        self.conn.send(
            &Message::new(touch, 0)
                .u32(s)
                .u32(t)
                .u32(focus)
                .i32(id)
                .i32(fixed(x))
                .i32(fixed(y)),
        );
    }

    /// Movement of an existing touch point `id`.
    pub fn touch_motion(&mut self, id: i32, x: i32, y: i32) {
        let Some(touch) = self.touch else { return };
        self.last_ptr = (x, y);
        self.time_ms = self.time_ms.wrapping_add(8);
        let t = self.time_ms;
        // motion(time, id, x(fixed), y(fixed))
        self.conn.send(
            &Message::new(touch, 2)
                .u32(t)
                .i32(id)
                .i32(fixed(x))
                .i32(fixed(y)),
        );
    }

    /// Release of touch point `id`.
    pub fn touch_up(&mut self, id: i32) {
        let Some(touch) = self.touch else { return };
        let s = self.next_serial();
        self.time_ms = self.time_ms.wrapping_add(8);
        let t = self.time_ms;
        // up(serial, time, id)
        self.conn.send(&Message::new(touch, 1).u32(s).u32(t).i32(id));
    }

    /// Close the current touch contact-point set (wl_touch.frame) and flush.
    pub fn touch_frame(&mut self) {
        let Some(touch) = self.touch else { return };
        self.conn.send(&Message::new(touch, 3)); // frame
        self.conn.flush().ok();
    }

    /// Cancel the whole active touch sequence (e.g. a gesture was recognized by the compositor).
    pub fn touch_cancel(&mut self) {
        let Some(touch) = self.touch else { return };
        self.conn.send(&Message::new(touch, 4)); // cancel
        self.conn.flush().ok();
    }
}

impl<P: Presenter> Server<P> {
    /// Unmap + close every shm pool the client left mapped, removing the objects. Returns how many
    /// pools were released. Called on disconnect (Drop) so a client that never destroys its pools does
    /// not leak the shared mapping + fd for the compositor's lifetime.
    fn release_all_pools(&mut self) -> usize {
        let pools: Vec<u32> = self
            .objs
            .iter()
            .filter_map(|(id, o)| matches!(o, Obj::ShmPool { .. }).then_some(*id))
            .collect();
        let n = pools.len();
        for pool in pools {
            if let Some(Obj::ShmPool { fd, ptr, size, .. }) = self.objs.remove(&pool) {
                unsafe {
                    libc::munmap(ptr as *mut libc::c_void, size.max(1));
                    libc::close(fd);
                }
            }
        }
        n
    }
}

impl<P: Presenter> Drop for Server<P> {
    /// Unmap every shm pool still mapped when the client disconnects. Without this, each pool's `mmap`
    /// (kept alive across the connection for `resize`) would leak the shared mapping + fd until process
    /// exit — a real address-space/fd leak for a long-lived compositor serving many connections.
    fn drop(&mut self) {
        self.release_all_pools();
    }
}

/// Apply a `wl_output` buffer transform (0..=7) to a tight BGRA image, producing the surface-oriented
/// image. The per-pixel mapping matches wlroots' `wlr_box_transform` (util/box.c:129-162): for each
/// destination (surface) pixel we read the corresponding source (buffer) pixel. Transform 0 (NORMAL) is a
/// zero-copy passthrough.
fn apply_buffer_transform(w: i32, h: i32, src: Vec<u8>, transform: i32) -> (i32, i32, Vec<u8>) {
    if transform == 0 || w <= 0 || h <= 0 {
        return (w, h, src);
    }
    // Surface (destination) dimensions: odd transforms (90/270/flipped-90/flipped-270) swap w/h.
    let (sw, sh) = if transform % 2 == 1 { (h, w) } else { (w, h) };
    let bw = w; // buffer width, for indexing source rows.
    let mut out = vec![0u8; (sw as usize) * (sh as usize) * 4];
    for dy in 0..sh {
        for dx in 0..sw {
            // Inverse map: surface pixel (dx,dy) → buffer pixel (bx,by).
            let (bx, by) = match transform {
                1 => (sh - 1 - dy, dx),           // 90
                2 => (sw - 1 - dx, sh - 1 - dy),  // 180
                3 => (dy, sw - 1 - dx),           // 270
                4 => (sw - 1 - dx, dy),           // flipped
                5 => (dy, dx),                    // flipped-90
                6 => (dx, sh - 1 - dy),           // flipped-180
                7 => (sh - 1 - dy, sw - 1 - dx),  // flipped-270
                _ => (dx, dy),
            };
            let si = ((by as usize) * (bw as usize) + bx as usize) * 4;
            let di = ((dy as usize) * (sw as usize) + dx as usize) * 4;
            out[di..di + 4].copy_from_slice(&src[si..si + 4]);
        }
    }
    (sw, sh, out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::present::PngPresenter;

    #[test]
    fn external_logical_crop_maps_presenting_surface() {
        let mut sv = [0i32; 2];
        assert_eq!(
            unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, sv.as_mut_ptr()) },
            0
        );
        let mut server = Server::new(sv[1], PngPresenter::new("/tmp/dd-display-mirror-test"));
        server.objs.insert(7, Obj::Surface(Surface::default()));
        server.set_external_logical_crop(Some(ExternalLogicalCrop {
            source_client: 0,
            source_surface: 42,
            source: "test",
            x: 16,
            y: 8,
            w: 200,
            h: 100,
        }));

        let map = server.surface_mapping(7, 532, 384).expect("mapped crop");
        assert_eq!(
            (map.src_x, map.src_y, map.src_x2, map.src_y2),
            (16, 8, 216, 108)
        );
        assert_eq!((map.dst_w, map.dst_h), (200, 100));
        assert!((map.uv_rect[0] - 16.0 / 532.0).abs() < f32::EPSILON);
        assert!((map.uv_rect[3] - 108.0 / 384.0).abs() < f32::EPSILON);

        drop(server);
        unsafe {
            libc::close(sv[0]);
            libc::close(sv[1]);
        }
    }

    fn test_server() -> (Server<PngPresenter>, [i32; 2]) {
        let mut sv = [0i32; 2];
        assert_eq!(
            unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, sv.as_mut_ptr()) },
            0
        );
        (
            Server::new(sv[1], PngPresenter::new("/tmp/dd-display-unit-test")),
            sv,
        )
    }

    fn teardown(server: Server<PngPresenter>, sv: [i32; 2]) {
        drop(server);
        unsafe {
            libc::close(sv[0]);
            libc::close(sv[1]);
        }
    }

    /// Map `fd`'s first `size` bytes read-only; caller stashes it into an `Obj::ShmPool`.
    unsafe fn mmap_ro(fd: i32, size: usize) -> *mut u8 {
        let p = libc::mmap(
            std::ptr::null_mut(),
            size.max(1),
            libc::PROT_READ,
            libc::MAP_SHARED,
            fd,
            0,
        );
        assert_ne!(p, libc::MAP_FAILED, "mmap failed");
        p as *mut u8
    }

    // ---- wl_surface.set_buffer_transform: the CPU orientation matches wlroots' box.c convention ----
    #[test]
    fn buffer_transform_180_reverses_pixels() {
        // 2x1 buffer: red, then green.
        let src = vec![1, 0, 0, 255, 2, 0, 0, 255];
        let (w, h, out) = apply_buffer_transform(2, 1, src, 2); // WL_OUTPUT_TRANSFORM_180
        assert_eq!((w, h), (2, 1));
        assert_eq!(out, vec![2, 0, 0, 255, 1, 0, 0, 255]);
    }

    #[test]
    fn buffer_transform_90_swaps_dimensions() {
        // 2x1 buffer rotated 90°: dims swap to 1x2.
        let src = vec![1, 0, 0, 255, 2, 0, 0, 255];
        let (w, h, out) = apply_buffer_transform(2, 1, src, 1); // WL_OUTPUT_TRANSFORM_90
        assert_eq!((w, h), (1, 2));
        // dst(0,0)->buffer(1,0)=green ; dst(0,1)->buffer(0,0)=red
        assert_eq!(out, vec![2, 0, 0, 255, 1, 0, 0, 255]);
    }

    #[test]
    fn buffer_transform_normal_is_passthrough() {
        let src = vec![9, 8, 7, 6];
        let (w, h, out) = apply_buffer_transform(1, 1, src.clone(), 0);
        assert_eq!((w, h, out), (1, 1, src));
    }

    // ---- wl_region coverage used by set_opaque_region ----
    #[test]
    fn region_covers_only_when_fully_contained_and_unclipped() {
        let full = vec![RegionOp { x: 0, y: 0, w: 100, h: 50, add: true }];
        assert!(region_covers(&full, 100, 50));
        assert!(region_covers(&full, 80, 40));
        // A subtract punching a hole means it is no longer fully opaque.
        let holed = vec![
            RegionOp { x: 0, y: 0, w: 100, h: 50, add: true },
            RegionOp { x: 10, y: 10, w: 5, h: 5, add: false },
        ];
        assert!(!region_covers(&holed, 100, 50));
        // A partial add does not cover the surface.
        let partial = vec![RegionOp { x: 0, y: 0, w: 50, h: 50, add: true }];
        assert!(!region_covers(&partial, 100, 50));
        assert!(!region_covers(&[], 100, 50));
    }

    // ---- set_opaque_region forces alpha in the extracted shm pixels (white-border fix) ----
    #[test]
    fn opaque_region_forces_alpha_opaque() {
        let (mut server, sv) = test_server();
        // 2x2 ARGB buffer, every pixel with alpha=0 (fully transparent bytes).
        let data = vec![10u8, 20, 30, 0, 11, 21, 31, 0, 12, 22, 32, 0, 13, 23, 33, 0];
        let fd = crate::keymap::anon_fd_with(&data).expect("anon fd");
        let ptr = unsafe { mmap_ro(fd, data.len()) };
        server.objs.insert(
            2,
            Obj::ShmPool { fd, ptr, size: data.len(), safe_len: data.len(), buffers: 1, zombie: false },
        );
        server.objs.insert(
            3,
            Obj::Buffer { pool: 2, offset: 0, width: 2, height: 2, stride: 8, format: FMT_ARGB8888 },
        );
        let mut surface = Surface::default();
        surface.opaque_region = vec![RegionOp { x: 0, y: 0, w: 2, h: 2, add: true }];
        server.objs.insert(7, Obj::Surface(surface));

        let sb = server.extract(7, 3, "t").expect("extracted");
        assert!(sb.bgra.chunks_exact(4).all(|p| p[3] == 0xff), "alpha forced opaque");

        // Without an opaque region the transparent alpha is preserved.
        if let Some(Obj::Surface(s)) = server.objs.get_mut(&7) {
            s.opaque_region.clear();
        }
        let sb = server.extract(7, 3, "t").expect("extracted");
        assert!(sb.bgra.chunks_exact(4).all(|p| p[3] == 0x00), "alpha preserved");
        teardown(server, sv);
    }

    // ---- XRGB8888 (no alpha channel) extracts opaque even without a declared opaque region ----
    // A CPU-raster client (Chrome under `--disable-gpu` software compositing → wl_shm) commits web
    // content as XRGB8888 whose 4th byte is undefined (0). The presenter's composite shader blends
    // src-over-white by that alpha, so an unforced 0 would wash the window white. The format's "X"
    // guarantees opacity regardless of the opaque region.
    #[test]
    fn xrgb_extracts_opaque_without_opaque_region() {
        let (mut server, sv) = test_server();
        // 2x2 XRGB buffer, alpha byte 0 everywhere (undefined "X").
        let data = vec![10u8, 20, 30, 0, 11, 21, 31, 0, 12, 22, 32, 0, 13, 23, 33, 0];
        let fd = crate::keymap::anon_fd_with(&data).expect("anon fd");
        let ptr = unsafe { mmap_ro(fd, data.len()) };
        server.objs.insert(
            2,
            Obj::ShmPool { fd, ptr, size: data.len(), safe_len: data.len(), buffers: 1, zombie: false },
        );
        server.objs.insert(
            3,
            Obj::Buffer { pool: 2, offset: 0, width: 2, height: 2, stride: 8, format: FMT_XRGB8888 },
        );
        // No opaque_region declared — force_opaque would leave alpha untouched; the XRGB rule must not.
        server.objs.insert(7, Obj::Surface(Surface::default()));
        let sb = server.extract(7, 3, "t").expect("extracted");
        assert_eq!(sb.format, FMT_XRGB8888);
        assert!(sb.bgra.chunks_exact(4).all(|p| p[3] == 0xff), "XRGB forced opaque");
        // Color bytes are preserved (only alpha is rewritten).
        assert_eq!(&sb.bgra[0..3], &[10, 20, 30]);

        // An ARGB buffer with the same bytes and no opaque region keeps its (transparent) alpha.
        if let Some(Obj::Buffer { format, .. }) = server.objs.get_mut(&3) {
            *format = FMT_ARGB8888;
        }
        let sb = server.extract(7, 3, "t").expect("extracted");
        assert!(sb.bgra.chunks_exact(4).all(|p| p[3] == 0x00), "ARGB alpha preserved");
        teardown(server, sv);
    }

    // ---- shm pool whose declared `size` exceeds the fd's real backing must refuse an out-of-backing
    // buffer, not read past EOF. A read that lands in a 16 KB host page wholly past the fd's EOF takes an
    // uncatchable SIGBUS that kills the whole compositor (macOS has no per-read shm access guard); the
    // per-pool `safe_len` clamp (fstat length at map time) turns that crash into a rejected buffer. This is
    // the host-side memfd-mapping hardening for the wl_shm path (libwayland's wl_shm_buffer_begin_access). ----
    #[test]
    fn shm_buffer_past_fd_backing_is_refused_not_read() {
        let (mut server, sv) = test_server();
        // fd really holds only 8 bytes, but the pool DECLARES a large size (a lying/short-fd client). A
        // 2x2 ARGB buffer needs offset0 + (2-1)*8 + 8 = 16 bytes — beyond the 8-byte backing.
        let data = vec![1u8, 2, 3, 4, 5, 6, 7, 8];
        let fd = crate::keymap::anon_fd_with(&data).expect("anon fd");
        let ptr = unsafe { mmap_ro(fd, 64) }; // map the declared size; only [0,8) is real backing
        server.objs.insert(
            2,
            Obj::ShmPool { fd, ptr, size: 64, safe_len: data.len(), buffers: 1, zombie: false },
        );
        server.objs.insert(
            3,
            Obj::Buffer { pool: 2, offset: 0, width: 2, height: 2, stride: 8, format: FMT_ARGB8888 },
        );
        server.objs.insert(7, Obj::Surface(Surface::default()));
        // Needs 16 bytes but only 8 are backed → clamp (min(size,safe_len)=8) refuses it instead of reading
        // past the fd's EOF.
        assert!(server.extract(7, 3, "t").is_none(), "out-of-backing buffer must be refused");

        // Control: a well-formed pool (safe_len == size == the fd's real 8-byte backing) with a 1x2 buffer
        // that needs exactly 0 + (2-1)*4 + 4 = 8 bytes extracts fine — the clamp is a no-op here.
        if let Some(Obj::ShmPool { safe_len, size, .. }) = server.objs.get_mut(&2) {
            *safe_len = 8;
            *size = 8;
        }
        if let Some(Obj::Buffer { width, height, stride, .. }) = server.objs.get_mut(&3) {
            *width = 1;
            *height = 2;
            *stride = 4;
        }
        assert!(server.extract(7, 3, "t").is_some(), "in-backing buffer must extract");
        teardown(server, sv);
    }

    // ---- wl_shm_pool.resize: a buffer in the grown region extracts only after resize ----
    #[test]
    fn shm_pool_resize_grows_mapping() {
        let (mut server, sv) = test_server();
        // Back the fd with the full 4x4 image, but map only the first row (16 bytes) initially.
        let full: Vec<u8> = (0..64).map(|i| i as u8).collect();
        let fd = crate::keymap::anon_fd_with(&full).expect("anon fd");
        let ptr = unsafe { mmap_ro(fd, 16) };
        server.objs.insert(
            2,
            Obj::ShmPool { fd, ptr, size: 16, safe_len: full.len(), buffers: 0, zombie: false },
        );
        // create_buffer(4x4) via the real handler so refcount is exercised.
        server.wl_shm_pool(
            Message::new(2, 0).u32(3).i32(0).i32(4).i32(4).i32(16).u32(FMT_ARGB8888),
        );
        server.objs.insert(7, Obj::Surface(Surface::default()));
        // Before resize the buffer's rows spill past the 16-byte mapping → rejected.
        assert!(server.extract(7, 3, "t").is_none(), "oversized buffer rejected pre-resize");
        // resize(64) re-maps.
        server.wl_shm_pool(Message::new(2, 2).i32(64));
        let sb = server.extract(7, 3, "t").expect("extracts after resize");
        assert_eq!((sb.width, sb.height), (4, 4));
        teardown(server, sv);
    }

    // ---- pool + buffer refcount: mapping is freed only once buffers are gone ----
    #[test]
    fn pool_freed_after_buffers_destroyed() {
        let (mut server, sv) = test_server();
        let data = vec![0u8; 16];
        let fd = crate::keymap::anon_fd_with(&data).expect("anon fd");
        let ptr = unsafe { mmap_ro(fd, data.len()) };
        server.objs.insert(
            2,
            Obj::ShmPool { fd, ptr, size: data.len(), safe_len: data.len(), buffers: 0, zombie: false },
        );
        server.wl_shm_pool(
            Message::new(2, 0).u32(3).i32(0).i32(2).i32(2).i32(8).u32(FMT_ARGB8888),
        );
        // Destroying the pool while a buffer is alive must NOT unmap it (spec: buffers keep it alive).
        server.wl_shm_pool(Message::new(2, 1));
        assert!(matches!(server.objs.get(&2), Some(Obj::ShmPool { zombie: true, .. })));
        // Destroying the last buffer frees the pool.
        server.wl_buffer(Message::new(3, 0));
        assert!(server.objs.get(&2).is_none(), "pool unmapped after last buffer");
        assert!(server.objs.get(&3).is_none(), "buffer object removed");
        teardown(server, sv);
    }

    // ---- wl_surface.destroy releases the held buffer and drops the surface + focus ----
    #[test]
    fn surface_destroy_cleans_up() {
        let (mut server, sv) = test_server();
        let mut surface = Surface::default();
        surface.current_buffer = Some(3);
        server.objs.insert(7, Obj::Surface(surface));
        server.focus = Some(7);
        server.wl_surface(Message::new(7, 0)); // destroy
        assert!(server.objs.get(&7).is_none(), "surface removed");
        assert_eq!(server.focus, None, "focus cleared");
        teardown(server, sv);
    }

    // ---- double-buffered transform/offset/opaque only take effect at commit ----
    #[test]
    fn buffer_transform_is_double_buffered() {
        let (mut server, sv) = test_server();
        server.objs.insert(7, Obj::Surface(Surface::default()));
        // Configure so commit() doesn't short-circuit into the xdg handshake.
        if let Some(Obj::Surface(s)) = server.objs.get_mut(&7) {
            s.configured = true;
        }
        server.wl_surface(Message::new(7, 7).i32(2)); // set_buffer_transform(180)
        // Not applied until commit.
        match server.objs.get(&7) {
            Some(Obj::Surface(s)) => {
                assert_eq!(s.buffer_transform, 0);
                assert_eq!(s.pending_buffer_transform, Some(2));
            }
            _ => panic!(),
        }
        server.commit(7);
        match server.objs.get(&7) {
            Some(Obj::Surface(s)) => {
                assert_eq!(s.buffer_transform, 2);
                assert_eq!(s.pending_buffer_transform, None);
            }
            _ => panic!(),
        }
        teardown(server, sv);
    }

    // ---- wl_subsurface composition ----

    fn solid(bgra: [u8; 4], w: i32, h: i32) -> Obj {
        Obj::SolidColorBuffer {
            width: w,
            height: h,
            bgra,
        }
    }

    // rgba pixel at (x,y) in the last presented frame.
    fn px(last: &(u32, i32, i32, Vec<u8>), x: i32, y: i32) -> [u8; 4] {
        let (_, w, _, rgba) = last;
        let i = ((y * *w + x) * 4) as usize;
        [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
    }

    // BGRA constants (memory order) and their RGBA-after-present equivalents.
    const RED_BGRA: [u8; 4] = [0, 0, 255, 255];
    const BLUE_BGRA: [u8; 4] = [255, 0, 0, 255];
    const GREEN_BGRA: [u8; 4] = [0, 255, 0, 255];
    const RED_RGBA: [u8; 4] = [255, 0, 0, 255];
    const BLUE_RGBA: [u8; 4] = [0, 0, 255, 255];
    const GREEN_RGBA: [u8; 4] = [0, 255, 0, 255];

    #[test]
    fn subsurface_composited_at_position_after_parent_commit() {
        let (mut server, sv) = test_server();
        // Parent: red 4x4. Child subsurface: blue 2x2 at (1,1).
        server.objs.insert(10, Obj::Surface(Surface::default()));
        server.objs.insert(11, solid(RED_BGRA, 4, 4));
        server.objs.insert(20, Obj::Surface(Surface::default()));
        server.objs.insert(21, solid(BLUE_BGRA, 2, 2));
        server.objs.insert(5, Obj::Subcompositor);
        // get_subsurface(id=30, surface=20, parent=10)
        server.wl_subcompositor(Message::new(5, 1).u32(30).u32(20).u32(10));
        // set_position(1, 1)
        server.wl_subsurface(Message::new(30, 1).i32(1).i32(1));

        // Commit the child first (synchronized ⇒ cached, NOT presented, NOT applied).
        if let Some(Obj::Surface(s)) = server.objs.get_mut(&20) {
            s.pending_buffer = Some(21);
            s.attached = true;
        }
        server.commit(20);
        assert_eq!(server.present.frames, 0, "sync child commit must not present");
        assert!(
            matches!(server.objs.get(&20), Some(Obj::Surface(s)) if s.current_buffer.is_none()),
            "sync child commit must be cached, not applied"
        );

        // Commit the parent ⇒ cached child state applied + composited.
        if let Some(Obj::Surface(s)) = server.objs.get_mut(&10) {
            s.pending_buffer = Some(11);
            s.attached = true;
        }
        server.commit(10);
        assert_eq!(server.present.frames, 1);
        let last = server.present.last.clone().expect("presented");
        assert_eq!((last.1, last.2), (4, 4));
        // Corners are the parent (red); the 2x2 block at (1,1)..(2,2) is the child (blue).
        assert_eq!(px(&last, 0, 0), RED_RGBA);
        assert_eq!(px(&last, 3, 3), RED_RGBA);
        assert_eq!(px(&last, 1, 1), BLUE_RGBA);
        assert_eq!(px(&last, 2, 2), BLUE_RGBA);
        assert_eq!(px(&last, 0, 3), RED_RGBA);

        teardown(server, sv);
    }

    #[test]
    fn subsurface_place_above_controls_z_order() {
        let (mut server, sv) = test_server();
        // Parent red 4x4; two full-cover children A(blue) and B(green) both at (0,0).
        server.objs.insert(10, Obj::Surface(Surface::default()));
        server.objs.insert(11, solid(RED_BGRA, 4, 4));
        server.objs.insert(20, Obj::Surface(Surface::default())); // A
        server.objs.insert(21, solid(BLUE_BGRA, 4, 4));
        server.objs.insert(40, Obj::Surface(Surface::default())); // B
        server.objs.insert(41, solid(GREEN_BGRA, 4, 4));
        server.objs.insert(5, Obj::Subcompositor);
        server.wl_subcompositor(Message::new(5, 1).u32(30).u32(20).u32(10)); // A obj 30
        server.wl_subcompositor(Message::new(5, 1).u32(50).u32(40).u32(10)); // B obj 50
        // Default stack order = insertion order [A, B] ⇒ B (green) on top.
        for (sid, bid) in [(20, 21), (40, 41)] {
            if let Some(Obj::Surface(s)) = server.objs.get_mut(&sid) {
                s.pending_buffer = Some(bid);
                s.attached = true;
            }
            server.commit(sid); // cached (sync)
        }
        if let Some(Obj::Surface(s)) = server.objs.get_mut(&10) {
            s.pending_buffer = Some(11);
            s.attached = true;
        }
        server.commit(10);
        let last = server.present.last.clone().unwrap();
        assert_eq!(px(&last, 2, 2), GREEN_RGBA, "B (green) starts on top");

        // place_above A over sibling B ⇒ A (blue) now on top.
        server.wl_subsurface(Message::new(30, 2).u32(40)); // A.place_above(sibling surface B=40)
        server.commit(10);
        let last = server.present.last.clone().unwrap();
        assert_eq!(px(&last, 2, 2), BLUE_RGBA, "after place_above, A (blue) is on top");

        // place_below A under B ⇒ B (green) back on top.
        server.wl_subsurface(Message::new(30, 3).u32(40)); // A.place_below(B)
        server.commit(10);
        let last = server.present.last.clone().unwrap();
        assert_eq!(px(&last, 2, 2), GREEN_RGBA, "after place_below, B (green) is on top");

        teardown(server, sv);
    }

    #[test]
    fn subsurface_desync_applies_immediately() {
        let (mut server, sv) = test_server();
        server.objs.insert(10, Obj::Surface(Surface::default()));
        server.objs.insert(11, solid(RED_BGRA, 4, 4));
        server.objs.insert(20, Obj::Surface(Surface::default()));
        server.objs.insert(21, solid(BLUE_BGRA, 2, 2));
        server.objs.insert(5, Obj::Subcompositor);
        server.wl_subcompositor(Message::new(5, 1).u32(30).u32(20).u32(10));
        // Map the parent first.
        if let Some(Obj::Surface(s)) = server.objs.get_mut(&10) {
            s.pending_buffer = Some(11);
            s.attached = true;
        }
        server.commit(10);
        let base_frames = server.present.frames;
        // Switch child to desync; a subsequent child commit recomposites the root immediately.
        server.wl_subsurface(Message::new(30, 5)); // set_desync
        if let Some(Obj::Surface(s)) = server.objs.get_mut(&20) {
            s.pending_buffer = Some(21);
            s.attached = true;
        }
        server.commit(20);
        assert_eq!(
            server.present.frames,
            base_frames + 1,
            "desync child commit recomposites the root"
        );
        let last = server.present.last.clone().unwrap();
        assert_eq!(px(&last, 0, 0), BLUE_RGBA, "desync child applied immediately");

        teardown(server, sv);
    }

    #[test]
    fn viewport_source_crop_and_dest_scale_mapping() {
        let (mut server, sv) = test_server();
        server.objs.insert(7, Obj::Surface(Surface::default()));
        // Source crop only (dst unset): surface size becomes the source-rect size.
        if let Some(Obj::Surface(s)) = server.objs.get_mut(&7) {
            s.viewport_source = Some((fixed(10), fixed(20), fixed(100), fixed(50)));
        }
        let map = server.surface_mapping(7, 200, 100).expect("crop");
        assert_eq!((map.src_x, map.src_y, map.src_x2, map.src_y2), (10, 20, 110, 70));
        assert_eq!((map.dst_w, map.dst_h), (100, 50));

        // Add a destination size ⇒ the cropped region scales to dst.
        if let Some(Obj::Surface(s)) = server.objs.get_mut(&7) {
            s.viewport_destination = Some((50, 25));
        }
        let map = server.surface_mapping(7, 200, 100).expect("crop+scale");
        assert_eq!((map.src_x, map.src_y, map.src_x2, map.src_y2), (10, 20, 110, 70));
        assert_eq!((map.dst_w, map.dst_h), (50, 25));

        teardown(server, sv);
    }

    #[test]
    fn viewport_source_scales_by_buffer_scale() {
        let (mut server, sv) = test_server();
        server.objs.insert(7, Obj::Surface(Surface::default()));
        // wp_viewport source is in post-buffer-scale surface coords; at buffer_scale=2 it maps to 2x
        // buffer pixels, and the (dst-unset) surface size is the source size in surface coords.
        if let Some(Obj::Surface(s)) = server.objs.get_mut(&7) {
            s.buffer_scale = 2;
            s.viewport_source = Some((fixed(5), fixed(5), fixed(20), fixed(10)));
        }
        let map = server.surface_mapping(7, 100, 100).expect("scaled crop");
        assert_eq!((map.src_x, map.src_y, map.src_x2, map.src_y2), (10, 10, 50, 30));
        assert_eq!((map.dst_w, map.dst_h), (20, 10));

        teardown(server, sv);
    }

    // ---- xdg_shell helpers ----

    /// A server wired to a socketpair; `peer` is the client end we read the compositor's events from.
    struct Harness {
        server: Server<PngPresenter>,
        peer: RawFd,
        srv: RawFd,
    }

    impl Harness {
        fn new() -> Harness {
            let mut sv = [0i32; 2];
            assert_eq!(
                unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, sv.as_mut_ptr()) },
                0
            );
            // Non-blocking peer so draining never hangs.
            unsafe {
                let fl = libc::fcntl(sv[0], libc::F_GETFL);
                libc::fcntl(sv[0], libc::F_SETFL, fl | libc::O_NONBLOCK);
            }
            let server = Server::new(sv[1], PngPresenter::new("/tmp/dd-display-xdg-test"));
            Harness {
                server,
                peer: sv[0],
                srv: sv[1],
            }
        }

        fn req(&mut self, object: u32, opcode: u16, body: Message) {
            let m = Message {
                object,
                opcode,
                body: body.body,
            };
            self.server.dispatch(m);
        }

        /// Flush the server's outbound queue and decode every complete message the client would receive.
        fn drain(&mut self) -> Vec<(u32, u16, Vec<u8>)> {
            self.server.conn.flush().ok();
            let mut buf = [0u8; 8192];
            let n = unsafe { libc::read(self.peer, buf.as_mut_ptr() as *mut _, buf.len()) };
            let mut out = Vec::new();
            if n <= 0 {
                return out;
            }
            let bytes = &buf[..n as usize];
            let mut pos = 0;
            while pos + 8 <= bytes.len() {
                let object = u32::from_ne_bytes(bytes[pos..pos + 4].try_into().unwrap());
                let word = u32::from_ne_bytes(bytes[pos + 4..pos + 8].try_into().unwrap());
                let size = (word >> 16) as usize;
                let opcode = (word & 0xffff) as u16;
                if size < 8 || pos + size > bytes.len() {
                    break;
                }
                out.push((object, opcode, bytes[pos + 8..pos + size].to_vec()));
                pos += size;
            }
            out
        }

        /// Deliver a request from the client end carrying a single `SCM_RIGHTS` fd (as a real client
        /// does for wl_data_offer.receive), then let the server harvest + dispatch it via `pump`.
        fn req_with_fd(&mut self, object: u32, opcode: u16, body: Message, fd: RawFd) {
            let m = Message { object, opcode, body: body.body };
            let mut bytes = Vec::new();
            m.encode(&mut bytes);
            let mut iov = libc::iovec {
                iov_base: bytes.as_ptr() as *mut _,
                iov_len: bytes.len(),
            };
            let mut cbuf = vec![0u8; unsafe { libc::CMSG_SPACE(4) } as usize];
            let mut mh: libc::msghdr = unsafe { std::mem::zeroed() };
            mh.msg_iov = &mut iov;
            mh.msg_iovlen = 1;
            mh.msg_control = cbuf.as_mut_ptr() as *mut _;
            mh.msg_controllen = cbuf.len() as _;
            unsafe {
                let cmsg = libc::CMSG_FIRSTHDR(&mh);
                (*cmsg).cmsg_level = libc::SOL_SOCKET;
                (*cmsg).cmsg_type = libc::SCM_RIGHTS;
                (*cmsg).cmsg_len = libc::CMSG_LEN(4) as _;
                *(libc::CMSG_DATA(cmsg) as *mut RawFd) = fd;
                let n = libc::sendmsg(self.peer, &mh, 0);
                assert!(n > 0, "sendmsg with fd failed");
            }
            self.server.pump().expect("server pump");
        }

        /// Drain the server's outbound stream, collecting messages AND any `SCM_RIGHTS` fds (as a real
        /// client's recvmsg would). Returns (messages, received_fds).
        fn drain_with_fds(&mut self) -> (Vec<(u32, u16, Vec<u8>)>, Vec<RawFd>) {
            self.server.conn.flush().ok();
            let mut buf = [0u8; 8192];
            let mut cbuf = [0u8; 256];
            let mut iov = libc::iovec {
                iov_base: buf.as_mut_ptr() as *mut _,
                iov_len: buf.len(),
            };
            let mut mh: libc::msghdr = unsafe { std::mem::zeroed() };
            mh.msg_iov = &mut iov;
            mh.msg_iovlen = 1;
            mh.msg_control = cbuf.as_mut_ptr() as *mut _;
            mh.msg_controllen = cbuf.len() as _;
            let n = unsafe { libc::recvmsg(self.peer, &mut mh, 0) };
            let mut msgs = Vec::new();
            let mut fds = Vec::new();
            if n <= 0 {
                return (msgs, fds);
            }
            unsafe {
                let mut cmsg = libc::CMSG_FIRSTHDR(&mh);
                while !cmsg.is_null() {
                    if (*cmsg).cmsg_level == libc::SOL_SOCKET
                        && (*cmsg).cmsg_type == libc::SCM_RIGHTS
                    {
                        let data = libc::CMSG_DATA(cmsg) as *const RawFd;
                        let payload = (*cmsg).cmsg_len as usize - libc::CMSG_LEN(0) as usize;
                        for i in 0..payload / std::mem::size_of::<RawFd>() {
                            fds.push(*data.add(i));
                        }
                    }
                    cmsg = libc::CMSG_NXTHDR(&mh, cmsg);
                }
            }
            let bytes = &buf[..n as usize];
            let mut pos = 0;
            while pos + 8 <= bytes.len() {
                let object = u32::from_ne_bytes(bytes[pos..pos + 4].try_into().unwrap());
                let word = u32::from_ne_bytes(bytes[pos + 4..pos + 8].try_into().unwrap());
                let size = (word >> 16) as usize;
                let opcode = (word & 0xffff) as u16;
                if size < 8 || pos + size > bytes.len() {
                    break;
                }
                msgs.push((object, opcode, bytes[pos + 8..pos + size].to_vec()));
                pos += size;
            }
            (msgs, fds)
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            unsafe {
                libc::close(self.peer);
                libc::close(self.srv);
            }
        }
    }

    fn body() -> Message {
        Message::new(0, 0)
    }

    #[test]
    fn positioner_geometry_anchor_gravity() {
        // anchor bottom_left of rect (10,20,100,40) ⇒ (10,60); gravity bottom_right ⇒ no shift; offset 0.
        let p = XdgPositioner {
            width: 50,
            height: 30,
            anchor_x: 10,
            anchor_y: 20,
            anchor_w: 100,
            anchor_h: 40,
            anchor: 6,  // bottom_left
            gravity: 8, // bottom_right
            offset_x: 0,
            offset_y: 0,
        };
        assert_eq!(p.geometry(), (10, 60, 50, 30));

        // anchor none (centered) of rect (0,0,100,20) ⇒ (50,10); gravity none (centered) ⇒ (-25,-5)+(size)
        let c = XdgPositioner {
            width: 40,
            height: 10,
            anchor_x: 0,
            anchor_y: 0,
            anchor_w: 100,
            anchor_h: 20,
            anchor: 0,
            gravity: 0,
            offset_x: 5,
            offset_y: -2,
        };
        assert_eq!(c.geometry(), (50 - 20 + 5, 10 - 5 - 2, 40, 10));
    }

    #[test]
    fn commit_fires_all_queued_frame_callbacks() {
        // A client may request several wl_surface.frame callbacks before one commit; each is a distinct
        // wl_callback that must receive its own .done. A single-slot pending_frame would drop all but
        // the last.
        let mut h = Harness::new();
        h.server.objs.insert(10, Obj::Surface(Surface::default()));
        // two frame() requests (opcode 3) with callback ids 101 and 102, then commit (opcode 6).
        h.req(10, 3, body().u32(101));
        h.req(10, 3, body().u32(102));
        h.req(10, 6, body());
        let msgs = h.drain();
        let done101 = msgs.iter().any(|(o, op, _)| *o == 101 && *op == 0);
        let done102 = msgs.iter().any(|(o, op, _)| *o == 102 && *op == 0);
        assert!(done101 && done102, "both frame callbacks must fire, got {msgs:?}");
        // each callback id is also released via wl_display.delete_id.
        let del: Vec<u32> = msgs
            .iter()
            .filter(|(o, op, _)| *o == WL_DISPLAY && *op == 1)
            .map(|(_, _, b)| u32::from_ne_bytes(b[0..4].try_into().unwrap()))
            .collect();
        assert!(del.contains(&101) && del.contains(&102), "delete_id for both, got {del:?}");
    }

    #[test]
    fn popup_gets_configure_handshake() {
        let mut h = Harness::new();
        // wl_surface(10), xdg_wm_base(5)
        h.server.objs.insert(10, Obj::Surface(Surface::default()));
        h.server.objs.insert(5, Obj::XdgWmBase);
        // create_positioner(40) then set_size + set_anchor_rect.
        h.req(5, 1, body().u32(40));
        // set_size(120, 80)
        h.req(40, 1, body().i32(120).i32(80));
        // set_anchor_rect(4, 6, 20, 10)
        h.req(40, 2, body().i32(4).i32(6).i32(20).i32(10));
        // get_xdg_surface(20, surface=10)
        h.req(5, 2, body().u32(20).u32(10));
        // get_popup(30, parent=0, positioner=40)
        h.req(20, 2, body().u32(30).u32(0).u32(40));
        let _ = h.drain();
        // Initial bufferless commit → popup configure handshake.
        h.req(10, 6, body());
        let msgs = h.drain();
        // xdg_popup.configure(30) carries the resolved (x,y,w,h).
        let popup_cfg = msgs.iter().find(|(o, op, _)| *o == 30 && *op == 0);
        assert!(
            popup_cfg.is_some(),
            "expected xdg_popup.configure, got {msgs:?}"
        );
        let (_, _, b) = popup_cfg.unwrap();
        let w = i32::from_ne_bytes(b[8..12].try_into().unwrap());
        let hh = i32::from_ne_bytes(b[12..16].try_into().unwrap());
        assert_eq!((w, hh), (120, 80));
        // followed by xdg_surface.configure(20, serial).
        assert!(
            msgs.iter().any(|(o, op, _)| *o == 20 && *op == 0),
            "expected xdg_surface.configure, got {msgs:?}"
        );
    }

    // ---- BUG 1: pointer_motion offsets content-local coords by the focused surface's window-geometry
    // origin, so the coordinate the client receives is FULL-surface-local. A GTK/CSD client renders a
    // shadow margin around the visible window and sets its window geometry to the inner rect; the
    // compositor crops the presented buffer to that rect, so on-screen clicks are content-local (relative
    // to the cropped window) and must have (gx,gy) added back. Without this a click lands off-target by
    // exactly the shadow margin (the ~10px-x/~20px-y bug). ----
    #[test]
    fn pointer_motion_adds_window_geometry_origin() {
        let mut h = Harness::new();
        h.server.seat_ver = 5;
        h.server.pointer = Some(60);
        // Focused toplevel with a CSD shadow margin: window geometry inset (gx=12, gy=23) into a larger
        // surface. attach_x/attach_y are 0 (the buffer is committed at the surface origin).
        let mut surf = Surface::default();
        surf.window_geometry = Some((12, 23, 400, 300));
        h.server.objs.insert(10, Obj::Surface(surf));
        h.server.focus = Some(10);
        let _ = h.drain();

        // A click landing at content-local (100, 30) (relative to the on-screen cropped window).
        h.server.pointer_motion(100, 30);
        let msgs = h.drain();
        let motion = msgs
            .iter()
            .find(|(o, op, _)| *o == 60 && *op == 2)
            .unwrap_or_else(|| panic!("no wl_pointer.motion, got {msgs:?}"));
        // motion body: time(u32), x(wl_fixed 24.8), y(wl_fixed 24.8). Decode surface-local integer coords.
        let b = &motion.2;
        let x = i32::from_ne_bytes(b[4..8].try_into().unwrap()) / 256;
        let y = i32::from_ne_bytes(b[8..12].try_into().unwrap()) / 256;
        assert_eq!(
            (x, y),
            (112, 53),
            "content (100,30) + geometry origin (12,23) must be full-surface-local (112,53), got ({x},{y})"
        );

        // Control: with NO window geometry the coordinate passes through unchanged.
        if let Some(Obj::Surface(s)) = h.server.objs.get_mut(&10) {
            s.window_geometry = None;
        }
        h.server.pointer_motion(100, 30);
        let msgs = h.drain();
        let motion = msgs
            .iter()
            .find(|(o, op, _)| *o == 60 && *op == 2)
            .unwrap_or_else(|| panic!("no wl_pointer.motion, got {msgs:?}"));
        let b = &motion.2;
        let x = i32::from_ne_bytes(b[4..8].try_into().unwrap()) / 256;
        let y = i32::from_ne_bytes(b[8..12].try_into().unwrap()) / 256;
        assert_eq!((x, y), (100, 30), "no geometry ⇒ passthrough, got ({x},{y})");
    }

    // ---- BUG 2: popup_placement resolves an xdg_popup's parent surface and the positioner (x,y) offset
    // relative to that parent's window-geometry top-left, which the presenter turns into the popup window's
    // screen position (parent-content-top-left + offset). Without it the popup opened at a default cascade
    // (bottom of screen) instead of at its anchoring widget. ----
    #[test]
    fn popup_placement_resolves_parent_and_positioner_offset() {
        let mut h = Harness::new();
        h.server.objs.insert(5, Obj::XdgWmBase);
        // Parent (10) and popup (11) wl_surfaces.
        h.server.objs.insert(10, Obj::Surface(Surface::default()));
        h.server.objs.insert(11, Obj::Surface(Surface::default()));
        // Positioner(40): size 120x80, anchor rect (10,20,100,40), anchor bottom_left, gravity bottom_right,
        // offset (3,7). geometry() ⇒ anchor point (10, 20+40)=(10,60); gravity bottom_right ⇒ no shift;
        // + offset ⇒ (13, 67).
        h.req(5, 1, body().u32(40)); // create_positioner
        h.req(40, 1, body().i32(120).i32(80)); // set_size
        h.req(40, 2, body().i32(10).i32(20).i32(100).i32(40)); // set_anchor_rect
        h.req(40, 3, body().u32(6)); // set_anchor bottom_left
        h.req(40, 4, body().u32(8)); // set_gravity bottom_right
        h.req(40, 6, body().i32(3).i32(7)); // set_offset
        // xdg_surfaces for parent (20→surface 10) and popup (21→surface 11).
        h.req(5, 2, body().u32(20).u32(10)); // get_xdg_surface parent
        h.req(5, 2, body().u32(21).u32(11)); // get_xdg_surface popup
        // get_popup(id=31, parent=xdg_surface 20, positioner=40) on the popup's xdg_surface (21).
        h.req(21, 2, body().u32(31).u32(20).u32(40));

        let placement = h
            .server
            .popup_placement(11)
            .expect("popup surface 11 must resolve a placement");
        assert_eq!(
            placement,
            PopupPlacement { parent_sid: 10, x: 13, y: 67 },
            "popup anchored to parent surface 10 at positioner offset (13,67), got {placement:?}"
        );
        // A plain toplevel surface (no popup role) resolves nothing.
        assert!(
            h.server.popup_placement(10).is_none(),
            "the parent toplevel is not a popup"
        );
    }

    #[test]
    fn toplevel_move_records_request() {
        let mut h = Harness::new();
        h.server.objs.insert(10, Obj::Surface(Surface::default()));
        h.server.objs.insert(20, Obj::XdgSurface { surface: 10 });
        h.server
            .objs
            .insert(30, Obj::XdgToplevel { xdg_surface: 20 });
        assert_eq!(h.server.take_move_request(), None);
        // move(seat=4, serial=7)
        h.req(30, 5, body().u32(4).u32(7));
        assert_eq!(h.server.take_move_request(), Some(10));
        // drained: a second take yields nothing.
        assert_eq!(h.server.take_move_request(), None);
    }

    #[test]
    fn close_request_emits_xdg_toplevel_close() {
        // The AppKit close button maps to xdg_toplevel.close (event opcode 1, no args) on the toplevel
        // bound to the given wl_surface, so the client exits/prompts instead of being silently orphaned.
        let mut h = Harness::new();
        let mut surf = Surface::default();
        surf.xdg_surface = Some(20);
        h.server.objs.insert(10, Obj::Surface(surf));
        h.server.objs.insert(20, Obj::XdgSurface { surface: 10 });
        h.server
            .objs
            .insert(30, Obj::XdgToplevel { xdg_surface: 20 });
        assert!(h.server.send_close_request(10));
        let msgs = h.drain();
        assert!(
            msgs.iter().any(|(o, op, b)| *o == 30 && *op == 1 && b.is_empty()),
            "expected xdg_toplevel.close(obj=30, op=1, no args), got {msgs:?}"
        );
        // An unknown surface (or one with no toplevel) is a no-op, not a panic.
        assert!(!h.server.send_close_request(999));
    }

    #[test]
    fn set_maximized_sends_configure_with_state() {
        let mut h = Harness::new();
        h.server.objs.insert(10, Obj::Surface(Surface::default()));
        h.server.objs.insert(20, Obj::XdgSurface { surface: 10 });
        h.server
            .objs
            .insert(30, Obj::XdgToplevel { xdg_surface: 20 });
        // set_maximized
        h.req(30, 9, body());
        let msgs = h.drain();
        let cfg = msgs
            .iter()
            .find(|(o, op, _)| *o == 30 && *op == 0)
            .expect("xdg_toplevel.configure");
        // states array is the 3rd arg: i32 w, i32 h, then array(len, bytes...).
        let b = &cfg.2;
        let arr_len = u32::from_ne_bytes(b[8..12].try_into().unwrap()) as usize;
        let states: Vec<u32> = b[12..12 + arr_len]
            .chunks_exact(4)
            .map(|c| u32::from_ne_bytes(c.try_into().unwrap()))
            .collect();
        assert!(states.contains(&1), "MAXIMIZED state missing: {states:?}");
        assert!(states.contains(&4), "ACTIVATED state missing: {states:?}");
        assert!(msgs.iter().any(|(o, op, _)| *o == 20 && *op == 0));
    }

    #[test]
    fn min_max_size_clamps_resize_configure() {
        let mut h = Harness::new();
        let mut s = Surface::default();
        s.xdg_surface = Some(20);
        s.min_size = (200, 150);
        s.max_size = (800, 600);
        h.server.objs.insert(10, Obj::Surface(s));
        h.server.objs.insert(20, Obj::XdgSurface { surface: 10 });
        h.server
            .objs
            .insert(30, Obj::XdgToplevel { xdg_surface: 20 });
        h.server.focus = Some(10);
        // Ask for a size below the minimum → clamped up to min.
        h.server.resize_focused(50, 50);
        let msgs = h.drain();
        let cfg = msgs
            .iter()
            .find(|(o, op, _)| *o == 30 && *op == 0)
            .expect("configure");
        let w = i32::from_ne_bytes(cfg.2[0..4].try_into().unwrap());
        let hh = i32::from_ne_bytes(cfg.2[4..8].try_into().unwrap());
        assert_eq!((w, hh), (200, 150));
    }

    #[test]
    fn pong_and_destroy_are_inert() {
        let mut h = Harness::new();
        h.server.objs.insert(5, Obj::XdgWmBase);
        // pong(serial) must not panic and must not remove the wm_base.
        h.req(5, 3, body().u32(99));
        assert!(matches!(h.server.objs.get(&5), Some(Obj::XdgWmBase)));
        // wm_base.destroy removes it.
        h.req(5, 0, body());
        assert!(h.server.objs.get(&5).is_none());
    }

    // ---- shm buffer validation: no crash, no corrupted present ----

    fn shm_pool_and_buffer(server: &mut Server<PngPresenter>, offset: i32, w: i32, h: i32, stride: i32, format: u32) {
        let data = vec![0u8; 4096];
        let fd = crate::keymap::anon_fd_with(&data).expect("anon fd");
        let ptr = unsafe { mmap_ro(fd, data.len()) };
        server.objs.insert(2, Obj::ShmPool { fd, ptr, size: data.len(), safe_len: data.len(), buffers: 1, zombie: false });
        server.objs.insert(3, Obj::Buffer { pool: 2, offset, width: w, height: h, stride, format });
        server.objs.insert(7, Obj::Surface(Surface::default()));
    }

    #[test]
    fn shm_negative_offset_is_rejected_without_panic() {
        let (mut server, sv) = test_server();
        shm_pool_and_buffer(&mut server, -4, 2, 2, 8, FMT_ARGB8888);
        assert!(server.extract(7, 3, "t").is_none(), "negative offset rejected, no panic");
        teardown(server, sv);
    }

    #[test]
    fn shm_stride_smaller_than_row_is_rejected() {
        let (mut server, sv) = test_server();
        // 2px row needs 8 bytes; stride=4 would overlap rows.
        shm_pool_and_buffer(&mut server, 0, 2, 2, 4, FMT_ARGB8888);
        assert!(server.extract(7, 3, "t").is_none(), "stride < row rejected");
        teardown(server, sv);
    }

    #[test]
    fn shm_unsupported_format_is_rejected() {
        let (mut server, sv) = test_server();
        shm_pool_and_buffer(&mut server, 0, 2, 2, 8, 0xdead_beef);
        assert!(server.extract(7, 3, "t").is_none(), "unadvertised format not presented");
        teardown(server, sv);
    }

    #[test]
    fn destroyed_buffer_is_not_presented() {
        // A buffer that was destroyed must not be extracted/presented on a later no-attach commit.
        let (mut server, sv) = test_server();
        shm_pool_and_buffer(&mut server, 0, 2, 2, 8, FMT_ARGB8888);
        assert!(server.extract(7, 3, "t").is_some(), "live buffer extracts");
        server.wl_buffer(Message::new(3, 0)); // destroy the buffer
        assert!(server.extract(7, 3, "t").is_none(), "destroyed buffer must not present");
        teardown(server, sv);
    }

    // ---- protocol errors on invalid state ----

    fn has_display_error(msgs: &[(u32, u16, Vec<u8>)]) -> bool {
        msgs.iter().any(|(o, op, _)| *o == WL_DISPLAY && *op == 0)
    }

    #[test]
    fn set_buffer_scale_zero_is_protocol_error_and_keeps_state() {
        let mut h = Harness::new();
        h.server.objs.insert(10, Obj::Surface(Surface::default()));
        h.req(10, 8, body().i32(2)); // valid scale 2
        let _ = h.drain();
        h.req(10, 8, body().i32(0)); // invalid scale 0
        let msgs = h.drain();
        assert!(has_display_error(&msgs), "scale 0 must be a protocol error, got {msgs:?}");
        let scale = match h.server.objs.get(&10) {
            Some(Obj::Surface(s)) => s.buffer_scale,
            _ => panic!(),
        };
        assert_eq!(scale, 2, "valid scale must not be clobbered by the invalid one");
    }

    #[test]
    fn viewport_invalid_destination_is_protocol_error() {
        let mut h = Harness::new();
        h.server.objs.insert(10, Obj::Surface(Surface::default()));
        h.server.objs.insert(50, Obj::Viewport { surface: 10 });
        // set_destination(0, 1) — invalid.
        h.req(50, 2, body().i32(0).i32(1));
        let msgs = h.drain();
        assert!(has_display_error(&msgs), "invalid destination must error, got {msgs:?}");
    }

    #[test]
    fn viewport_source_out_of_buffer_is_detected() {
        let (mut server, sv) = test_server();
        server.objs.insert(3, Obj::Buffer { pool: 2, offset: 0, width: 4, height: 1, stride: 16, format: FMT_ARGB8888 });
        let mut s = Surface::default();
        s.buffer_scale = 1;
        s.current_buffer = Some(3);
        s.viewport_source = Some((3 << 8, 0, 4 << 8, 1 << 8)); // x=3,w=4 → right=7 > buffer width 4
        server.objs.insert(7, Obj::Surface(s));
        server.objs.insert(50, Obj::Viewport { surface: 7 });
        assert_eq!(server.viewport_source_out_of_buffer(7), Some(50), "out-of-buffer source flagged");
        // A source that fits returns None.
        if let Some(Obj::Surface(s)) = server.objs.get_mut(&7) {
            s.viewport_source = Some((0, 0, 4 << 8, 1 << 8));
        }
        assert_eq!(server.viewport_source_out_of_buffer(7), None, "in-buffer source is fine");
        teardown(server, sv);
    }

    // ---- destructors free ids / stop routing ----

    #[test]
    fn surface_destroy_emits_delete_id() {
        let mut h = Harness::new();
        h.server.objs.insert(10, Obj::Surface(Surface::default()));
        h.req(10, 0, body()); // wl_surface.destroy
        let msgs = h.drain();
        let deleted: Vec<u32> = msgs
            .iter()
            .filter(|(o, op, _)| *o == WL_DISPLAY && *op == 1)
            .map(|(_, _, b)| u32::from_ne_bytes(b[0..4].try_into().unwrap()))
            .collect();
        assert!(deleted.contains(&10), "destroy must emit delete_id(10), got {msgs:?}");
        assert!(h.server.objs.get(&10).is_none());
    }

    #[test]
    fn pointer_release_stops_routing_and_frees_id() {
        let mut h = Harness::new();
        h.server.objs.insert(3, Obj::Seat);
        // get_pointer(id=60)
        h.req(3, 0, body().u32(60));
        assert_eq!(h.server.pointer, Some(60));
        let _ = h.drain();
        // wl_pointer.release (opcode 1)
        h.req(60, 1, body());
        assert_eq!(h.server.pointer, None, "released pointer no longer routes");
        assert!(h.server.objs.get(&60).is_none(), "released pointer id freed");
        let msgs = h.drain();
        assert!(
            msgs.iter().any(|(o, op, b)| *o == WL_DISPLAY && *op == 1 && u32::from_ne_bytes(b[0..4].try_into().unwrap()) == 60),
            "release emits delete_id(60), got {msgs:?}"
        );
    }

    #[test]
    fn focus_transfer_emits_pointer_and_keyboard_leave() {
        let mut h = Harness::new();
        h.server.objs.insert(10, Obj::Surface(Surface::default()));
        h.server.objs.insert(20, Obj::Surface(Surface::default()));
        h.server.pointer = Some(60);
        h.server.keyboard = Some(61);
        h.server.seat_ver = 5;
        h.server.focus = Some(10);
        h.server.ptr_entered = true;
        h.server.kbd_entered = true;
        let _ = h.drain();
        h.server.transfer_focus(20);
        let msgs = h.drain();
        // wl_pointer.leave (obj 60, opcode 1) naming old focus 10
        let ptr_leave = msgs.iter().find(|(o, op, _)| *o == 60 && *op == 1);
        assert!(ptr_leave.is_some(), "pointer leave to old focus, got {msgs:?}");
        let (_, _, b) = ptr_leave.unwrap();
        assert_eq!(u32::from_ne_bytes(b[4..8].try_into().unwrap()), 10, "leave names old surface");
        // wl_keyboard.leave (obj 61, opcode 2)
        assert!(msgs.iter().any(|(o, op, _)| *o == 61 && *op == 2), "keyboard leave, got {msgs:?}");
        assert_eq!(h.server.focus, Some(20));
        assert!(!h.server.ptr_entered && !h.server.kbd_entered);
    }

    // A synthetic pointer-down injected into a bound seat with a focused surface must emit, ON THE WIRE
    // and in order, the full sequence a Wayland client (Chrome/viz) needs to process a click: an initial
    // wl_pointer.enter (naming the focused surface, with a serial) BEFORE any button, the motion, the
    // button press (state=1), and a wl_pointer.frame closing the group — with monotonically increasing
    // serials. Most clients discard button/motion received before enter, so this ordering is the crux of
    // "clicks reach the app". This is the deterministic wire proof for the live NSEvent→wl_seat path.
    #[test]
    fn pointer_down_emits_enter_before_button_with_frame() {
        let mut h = Harness::new();
        h.server.seat_ver = 5;
        h.server.pointer = Some(60);
        h.server.keyboard = Some(61);
        // A mapped toplevel surface holds focus (as after get_toplevel → transfer_focus).
        h.server.objs.insert(10, Obj::Surface(Surface::default()));
        h.server.focus = Some(10);
        let _ = h.drain();

        // The exact calls inject_nsevent makes for a LeftMouseDown at surface-local (100,30).
        h.server.pointer_motion(100, 30);
        h.server.pointer_button(0x110, true);
        let msgs = h.drain();

        let idx = |op: u16| msgs.iter().position(|(o, p, _)| *o == 60 && *p == op);
        let enter_i = idx(0).unwrap_or_else(|| panic!("no wl_pointer.enter, got {msgs:?}"));
        let motion_i = idx(2).unwrap_or_else(|| panic!("no wl_pointer.motion, got {msgs:?}"));
        let button_i = idx(3).unwrap_or_else(|| panic!("no wl_pointer.button, got {msgs:?}"));
        // enter MUST precede motion and button (clients drop input received before enter).
        assert!(enter_i < motion_i, "enter must precede motion, got {msgs:?}");
        assert!(motion_i < button_i, "motion must precede button, got {msgs:?}");
        // enter names the focused surface (arg after the serial).
        let enter_body = &msgs[enter_i].2;
        assert_eq!(
            u32::from_ne_bytes(enter_body[4..8].try_into().unwrap()),
            10,
            "enter names the focused surface, got {msgs:?}"
        );
        // button carries state=1 (pressed) at arg offset 12 (serial,time,button,state).
        let button_body = &msgs[button_i].2;
        assert_eq!(
            u32::from_ne_bytes(button_body[12..16].try_into().unwrap()),
            1,
            "button is a press, got {msgs:?}"
        );
        assert_eq!(
            u32::from_ne_bytes(button_body[8..12].try_into().unwrap()),
            0x110,
            "button is BTN_LEFT, got {msgs:?}"
        );
        // Serials strictly increase: enter's serial < button's serial.
        let enter_serial = u32::from_ne_bytes(enter_body[0..4].try_into().unwrap());
        let button_serial = u32::from_ne_bytes(button_body[0..4].try_into().unwrap());
        assert!(
            enter_serial < button_serial,
            "serials must increase (enter {enter_serial} < button {button_serial}), got {msgs:?}"
        );
        // A wl_pointer.frame (v5+) must close the button group AFTER the button, or clients buffer it.
        let frame_after_button = msgs[button_i + 1..]
            .iter()
            .any(|(o, p, _)| *o == 60 && *p == 5);
        assert!(frame_after_button, "button group must be closed by a frame, got {msgs:?}");

        // Now the matching LeftMouseUp (inject_nsevent calls motion then button-release): the client must
        // receive a button RELEASE (state=0) closed by its own frame, with a serial strictly greater than
        // the press — the release half of the click. Without it a client sees a stuck-pressed button.
        h.server.pointer_button(0x110, false);
        let up = h.drain();
        let rel_i = up
            .iter()
            .position(|(o, p, _)| *o == 60 && *p == 3)
            .unwrap_or_else(|| panic!("no wl_pointer.button (release), got {up:?}"));
        let rel_body = &up[rel_i].2;
        assert_eq!(
            u32::from_ne_bytes(rel_body[12..16].try_into().unwrap()),
            0,
            "button is a release (state=0), got {up:?}"
        );
        let rel_serial = u32::from_ne_bytes(rel_body[0..4].try_into().unwrap());
        assert!(
            rel_serial > button_serial,
            "release serial must exceed press serial (press {button_serial} < release {rel_serial}), got {up:?}"
        );
        let frame_after_release = up[rel_i + 1..].iter().any(|(o, p, _)| *o == 60 && *p == 5);
        assert!(frame_after_release, "release group must be closed by a frame, got {up:?}");
    }

    // A surface assigned the CURSOR role via wl_pointer.set_cursor (Chrome's pointer image) must NOT be
    // presented as a native window: presenting it is the "spurious super-small window" bug (a ~10x16 cursor
    // surface popped as its own tiny NSWindow). Its frame pacing must still advance so the client is not
    // stalled: the committed buffer is released and the frame callback fires.
    #[test]
    fn cursor_surface_commit_is_not_presented_but_advances() {
        let mut h = Harness::new();
        let data = vec![0u8; 64];
        let fd = crate::keymap::anon_fd_with(&data).expect("anon fd");
        let ptr = unsafe { mmap_ro(fd, data.len()) };
        h.server.objs.insert(2, Obj::ShmPool { fd, ptr, size: data.len(), safe_len: data.len(), buffers: 1, zombie: false });
        h.server.objs.insert(3, Obj::Buffer { pool: 2, offset: 0, width: 2, height: 2, stride: 8, format: FMT_ARGB8888 });
        // A pointer + a surface that Chrome dedicates to the cursor image.
        h.server.objs.insert(60, Obj::Pointer);
        h.server.objs.insert(10, Obj::Surface(Surface::default()));
        // wl_pointer.set_cursor(serial, surface=10, hotspot_x, hotspot_y) → surface 10 gets the cursor role.
        h.req(60, 0, body().u32(1).u32(10).i32(0).i32(0));
        assert!(h.server.cursor_surfaces.contains(&10), "set_cursor assigns the cursor role");
        let _ = h.drain();
        // attach buffer 3, request a frame callback, commit.
        h.req(10, 1, body().u32(3).i32(0).i32(0)); // attach
        h.req(10, 3, body().u32(101)); // frame(101)
        h.req(10, 6, body()); // commit
        // The presenter was NOT asked to present (a cursor image is not a window).
        assert_eq!(h.server.presenter().frames, 0, "cursor surface must not be presented as a window");
        let msgs = h.drain();
        // Frame pacing still advances: the frame callback completes and the buffer is released.
        assert!(msgs.iter().any(|(o, op, _)| *o == 101 && *op == 0), "frame callback must fire, got {msgs:?}");
        assert!(msgs.iter().any(|(o, op, _)| *o == 3 && *op == 0), "buffer must be released, got {msgs:?}");
    }

    #[test]
    fn xdg_buffer_commit_before_ack_is_not_presented() {
        let mut h = Harness::new();
        // shm pool + buffer 3
        let data = vec![0u8; 64];
        let fd = crate::keymap::anon_fd_with(&data).expect("anon fd");
        let ptr = unsafe { mmap_ro(fd, data.len()) };
        h.server.objs.insert(2, Obj::ShmPool { fd, ptr, size: data.len(), safe_len: data.len(), buffers: 1, zombie: false });
        h.server.objs.insert(3, Obj::Buffer { pool: 2, offset: 0, width: 2, height: 2, stride: 8, format: FMT_ARGB8888 });
        // xdg surface that has been configured but not yet acked.
        let mut s = Surface::default();
        s.xdg_surface = Some(20);
        s.configured = true;
        s.acked = false;
        h.server.objs.insert(10, Obj::Surface(s));
        h.server.objs.insert(20, Obj::XdgSurface { surface: 10 });
        // attach buffer 3, request a frame callback, commit — content is configured but unacked, so it
        // must be held: the frame callback (which only fires on present) does NOT complete yet.
        h.req(10, 1, body().u32(3).i32(0).i32(0)); // attach
        h.req(10, 3, body().u32(101)); // frame(101)
        h.req(10, 6, body()); // commit
        let msgs = h.drain();
        assert!(!msgs.iter().any(|(o, op, _)| *o == 101 && *op == 0), "unacked buffer must not present, got {msgs:?}");
        // ack the configure — now the held buffer presents and the frame callback fires.
        h.req(20, 4, body().u32(1)); // ack_configure(serial=1)
        let msgs = h.drain();
        assert!(msgs.iter().any(|(o, op, _)| *o == 101 && *op == 0), "ack releases the held present, got {msgs:?}");
    }

    struct FailPresenter;
    impl Presenter for FailPresenter {
        fn present(
            &mut self,
            _surf: &SurfaceBuffer,
        ) -> Result<crate::present::PresentOutcome, crate::present::PresentError> {
            // simulate an IOSurface/drawable acquisition failure
            Err(crate::present::PresentError::Device(
                "simulated acquisition failure".into(),
            ))
        }
    }

    #[test]
    fn failed_present_holds_buffer_release_and_frame_callbacks() {
        let mut sv = [0i32; 2];
        assert_eq!(unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, sv.as_mut_ptr()) }, 0);
        unsafe {
            let fl = libc::fcntl(sv[0], libc::F_GETFL);
            libc::fcntl(sv[0], libc::F_SETFL, fl | libc::O_NONBLOCK);
        }
        let mut server = Server::new(sv[1], FailPresenter);
        // shm pool + buffer 3
        let data = vec![0u8; 64];
        let fd = crate::keymap::anon_fd_with(&data).expect("anon fd");
        let ptr = unsafe { mmap_ro(fd, data.len()) };
        server.objs.insert(2, Obj::ShmPool { fd, ptr, size: data.len(), safe_len: data.len(), buffers: 1, zombie: false });
        server.objs.insert(3, Obj::Buffer { pool: 2, offset: 0, width: 2, height: 2, stride: 8, format: FMT_ARGB8888 });
        let mut s = Surface::default();
        s.current_buffer = Some(3);
        s.pending_release = Some(3);
        s.pending_frame = vec![101];
        server.objs.insert(10, Obj::Surface(s));
        // present fails → the frame did not reach the screen, so the buffer is NOT released and the
        // frame callback does NOT fire (frame pacing must not advance).
        server.present_root(10);
        server.conn.flush().ok();
        let mut buf = [0u8; 4096];
        let n = unsafe { libc::read(sv[0], buf.as_mut_ptr() as *mut _, buf.len()) };
        let got = if n > 0 { &buf[..n as usize] } else { &[][..] };
        // decode messages
        let mut released = false;
        let mut done = false;
        let mut pos = 0;
        while pos + 8 <= got.len() {
            let object = u32::from_ne_bytes(got[pos..pos + 4].try_into().unwrap());
            let word = u32::from_ne_bytes(got[pos + 4..pos + 8].try_into().unwrap());
            let size = (word >> 16) as usize;
            let opcode = (word & 0xffff) as u16;
            if size < 8 || pos + size > got.len() { break; }
            if object == 3 && opcode == 0 { released = true; } // wl_buffer.release
            if object == 101 && opcode == 0 { done = true; } // wl_callback.done
            pos += size;
        }
        assert!(!released, "failed present must not release the buffer");
        assert!(!done, "failed present must not fire the frame callback");
        // state is retained for a later successful present
        if let Some(Obj::Surface(su)) = server.objs.get(&10) {
            assert_eq!(su.pending_release, Some(3));
            assert_eq!(su.pending_frame, vec![101]);
        } else {
            panic!("surface gone");
        }
        drop(server);
        unsafe { libc::close(sv[0]); libc::close(sv[1]); }
    }

    #[test]
    fn disconnect_releases_shm_pool_mappings() {
        // A client that disconnects without destroying its pools must not leak the mappings; the
        // disconnect path walks and releases every pool.
        let (mut server, sv) = test_server();
        let data = vec![0u8; 64];
        for pid in [2u32, 4u32] {
            let fd = crate::keymap::anon_fd_with(&data).expect("anon fd");
            let ptr = unsafe { mmap_ro(fd, data.len()) };
            server.objs.insert(pid, Obj::ShmPool { fd, ptr, size: data.len(), safe_len: data.len(), buffers: 1, zombie: false });
        }
        let released = server.release_all_pools();
        assert_eq!(released, 2, "both leaked pools released on disconnect");
        assert!(server.release_all_pools() == 0, "idempotent: nothing left to release");
        assert!(!server.objs.values().any(|o| matches!(o, Obj::ShmPool { .. })));
        teardown(server, sv);
    }

    #[test]
    fn dmabuf_untagged_create_immed_is_protocol_error() {
        // A dmabuf buffer created without the dd IOSurface tag (e.g. the advertised LINEAR modifier)
        // cannot be presented — create_immed must post a protocol error, not hand back an inert object.
        let mut h = Harness::new();
        h.server.objs.insert(50, Obj::DmabufParams { iosurface_id: None, stride: 0, gpu_render: false, generation: 0 });
        // create_immed(buffer_id=60, w=4, h=4, format=0x3432_5241, flags=0)
        h.req(50, 3, body().u32(60).i32(4).i32(4).u32(0x3432_5241).u32(0));
        let msgs = h.drain();
        assert!(has_display_error(&msgs), "untagged dmabuf create_immed must error, got {msgs:?}");
        // and it must NOT create an inert buffer object under that id.
        assert!(h.server.objs.get(&60).is_none(), "no inert buffer object created");
    }

    #[test]
    fn keymap_enables_key_repeat() {
        // The keymap must agree with the repeat_info the compositor advertises (repeat enabled).
        assert!(crate::keymap::US_XKB_KEYMAP.contains("interpret.repeat = True"));
    }

    // ---- wl_data_device (clipboard) ----

    /// The full selection round-trip: a source advertises a MIME type, set_selection announces a
    /// server-allocated wl_data_offer to the client's data_device, and wl_data_offer.receive pipes the
    /// reader's fd back to the source as wl_data_source.send so the clipboard bytes actually flow. This is
    /// the exact path Chrome's ozone clipboard uses; before this the child objects were inert and the whole
    /// sequence was silently swallowed.
    #[test]
    fn data_device_set_selection_offers_and_receive_pipes_fd() {
        let mut h = Harness::new();
        h.server.objs.insert(20, Obj::DataDeviceManager);
        h.server.objs.insert(4, Obj::Seat);

        // create_data_source(31); get_data_device(30, seat=4); source.offer(mime).
        h.req(20, 0, body().u32(31));
        h.req(20, 1, body().u32(30).u32(4));
        h.req(31, 0, body().string("text/plain;charset=utf-8"));
        let _ = h.drain();

        // set_selection(source=31, serial=1) — must NOT be silently swallowed.
        h.req(30, 1, body().u32(31).u32(1));
        let msgs = h.drain();

        // data_device.data_offer(new_id) — the id must come from the server-allocated range.
        let data_offer = msgs
            .iter()
            .find(|(o, op, _)| *o == 30 && *op == 0)
            .unwrap_or_else(|| panic!("wl_data_device.data_offer expected, got {msgs:?}"));
        let offer_id = u32::from_ne_bytes(data_offer.2[0..4].try_into().unwrap());
        assert!(
            offer_id >= WL_SERVER_ID_START,
            "offer id must be server-allocated, got {offer_id:#x}"
        );
        // wl_data_offer.offer(mime) on the offer object, before the selection event.
        let offer_mime = msgs
            .iter()
            .find(|(o, op, _)| *o == offer_id && *op == 0)
            .unwrap_or_else(|| panic!("wl_data_offer.offer expected on {offer_id}, got {msgs:?}"));
        let offer_body = Message { object: 0, opcode: 0, body: offer_mime.2.clone() };
        let mut mr = offer_body.reader();
        assert_eq!(mr.string(), "text/plain;charset=utf-8");
        // wl_data_device.selection(offer) hands the offer over as the current clipboard.
        let selection = msgs
            .iter()
            .find(|(o, op, _)| *o == 30 && *op == 5)
            .unwrap_or_else(|| panic!("wl_data_device.selection expected, got {msgs:?}"));
        assert_eq!(
            u32::from_ne_bytes(selection.2[0..4].try_into().unwrap()),
            offer_id
        );

        // ---- offer → receive fd flow ----
        let mut pipefd = [0 as RawFd; 2];
        assert_eq!(unsafe { libc::pipe(pipefd.as_mut_ptr()) }, 0);
        let (read_fd, write_fd) = (pipefd[0], pipefd[1]);
        // receive(mime, write_fd) on the offer: the reader hands the compositor its pipe write end.
        h.req_with_fd(offer_id, 1, body().string("text/plain;charset=utf-8"), write_fd);
        unsafe {
            libc::close(write_fd);
        }

        // The source (31) is asked to fulfil the read via wl_data_source.send(mime, fd).
        let (smsgs, sfds) = h.drain_with_fds();
        let send = smsgs
            .iter()
            .find(|(o, op, _)| *o == 31 && *op == 1)
            .unwrap_or_else(|| panic!("wl_data_source.send expected on 31, got {smsgs:?}"));
        let send_body = Message { object: 0, opcode: 0, body: send.2.clone() };
        let mut sr = send_body.reader();
        assert_eq!(sr.string(), "text/plain;charset=utf-8");
        assert_eq!(sfds.len(), 1, "send must carry exactly one fd, got {sfds:?}");
        // The source writes the clipboard payload into the pipe and closes.
        let payload = b"dd-clipboard";
        let w = unsafe { libc::write(sfds[0], payload.as_ptr() as *const _, payload.len()) };
        assert_eq!(w, payload.len() as isize);
        unsafe {
            libc::close(sfds[0]);
        }
        // The reader receives exactly those bytes off its pipe — the clipboard actually transferred.
        let mut got = [0u8; 32];
        let n = unsafe { libc::read(read_fd, got.as_mut_ptr() as *mut _, got.len()) };
        assert!(n > 0, "reader should receive clipboard bytes");
        assert_eq!(&got[..n as usize], payload);
        unsafe {
            libc::close(read_fd);
        }
    }

    /// A data_device created while a selection already exists is told about it immediately (so a client
    /// can paste right after binding), and clearing the selection (set_selection with a null source)
    /// emits selection(null).
    #[test]
    fn data_device_replays_and_clears_selection() {
        let mut h = Harness::new();
        h.server.objs.insert(20, Obj::DataDeviceManager);

        // A source with a selection set on a first device.
        h.req(20, 0, body().u32(31)); // create_data_source(31)
        h.req(20, 1, body().u32(30).u32(0)); // get_data_device(30)
        h.req(31, 0, body().string("text/plain")); // offer(mime)
        h.req(30, 1, body().u32(31).u32(1)); // set_selection(31)
        let _ = h.drain();

        // A second device bound afterwards must be handed the existing selection without a new copy.
        h.req(20, 1, body().u32(40).u32(0)); // get_data_device(40)
        let msgs = h.drain();
        let off = msgs
            .iter()
            .find(|(o, op, _)| *o == 40 && *op == 0)
            .unwrap_or_else(|| panic!("late data_device must get current selection, got {msgs:?}"));
        let late_offer = u32::from_ne_bytes(off.2[0..4].try_into().unwrap());
        assert!(msgs
            .iter()
            .any(|(o, op, b)| *o == 40
                && *op == 5
                && u32::from_ne_bytes(b[0..4].try_into().unwrap()) == late_offer));

        // Clearing the selection (null source) notifies every device with selection(null).
        h.req(30, 1, body().u32(0).u32(2)); // set_selection(null)
        let msgs = h.drain();
        let null_sel: Vec<u32> = msgs
            .iter()
            .filter(|(o, op, _)| (*o == 30 || *o == 40) && *op == 5)
            .map(|(_, _, b)| u32::from_ne_bytes(b[0..4].try_into().unwrap()))
            .collect();
        assert!(
            !null_sel.is_empty() && null_sel.iter().all(|id| *id == 0),
            "clearing selection must emit selection(null) to all devices, got {null_sel:?} in {msgs:?}"
        );
    }

    /// A receive on a stale offer (its selection was superseded) must not forward to the old source; the
    /// reader's fd is closed so its read sees EOF instead of hanging.
    #[test]
    fn data_offer_receive_after_selection_change_does_not_forward() {
        let mut h = Harness::new();
        h.server.objs.insert(20, Obj::DataDeviceManager);
        h.req(20, 1, body().u32(30).u32(0)); // get_data_device(30)
        h.req(20, 0, body().u32(31)); // create_data_source(31)
        h.req(31, 0, body().string("text/plain"));
        h.req(30, 1, body().u32(31).u32(1)); // set_selection(31)
        let msgs = h.drain();
        let first_offer = u32::from_ne_bytes(
            msgs.iter()
                .find(|(o, op, _)| *o == 30 && *op == 0)
                .unwrap()
                .2[0..4]
                .try_into()
                .unwrap(),
        );

        // A new selection supersedes the first offer.
        h.req(20, 0, body().u32(32)); // create_data_source(32)
        h.req(32, 0, body().string("text/plain"));
        h.req(30, 1, body().u32(32).u32(2)); // set_selection(32)
        let _ = h.drain();

        // receive() on the now-stale first offer: no wl_data_source.send, and our read end hits EOF.
        let mut pipefd = [0 as RawFd; 2];
        assert_eq!(unsafe { libc::pipe(pipefd.as_mut_ptr()) }, 0);
        let (read_fd, write_fd) = (pipefd[0], pipefd[1]);
        h.req_with_fd(first_offer, 1, body().string("text/plain"), write_fd);
        unsafe {
            libc::close(write_fd);
        }
        let (smsgs, sfds) = h.drain_with_fds();
        assert!(
            !smsgs.iter().any(|(o, op, _)| *o == 31 && *op == 1),
            "stale offer must not forward to the old source, got {smsgs:?}"
        );
        assert!(sfds.is_empty(), "no fd should be forwarded for a stale offer");
        // Both the compositor's copy and the reader's write_fd are closed ⇒ read returns EOF (0).
        let mut got = [0u8; 8];
        let n = unsafe { libc::read(read_fd, got.as_mut_ptr() as *mut _, got.len()) };
        assert_eq!(n, 0, "stale receive must close the reader's fd (EOF)");
        unsafe {
            libc::close(read_fd);
        }
    }
}

/// Convert an integer pixel coordinate to a `wl_fixed` (24.8 fixed-point).
fn fixed(v: i32) -> i32 {
    v * 256
}

/// Current monotonic clock as `(whole_seconds, nanoseconds)` for a `wp_presentation.presented` timestamp.
/// Read from the host's `CLOCK_MONOTONIC`; the wire `clock_id` we advertise is the guest's value (1).
fn monotonic_now() -> (u64, u32) {
    let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
    unsafe {
        libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts);
    }
    (ts.tv_sec as u64, (ts.tv_nsec as u32) % 1_000_000_000)
}

/// The `presented.refresh` interval (ns until the next output refresh) derived from the advertised mode.
/// 60000 mHz ⇒ 16_666_666 ns. Clients add multiples of this to predict future vblanks.
fn output_refresh_ns() -> u32 {
    if OUTPUT_REFRESH_MHZ <= 0 {
        return 0;
    }
    (1_000_000_000_000u64 / OUTPUT_REFRESH_MHZ as u64) as u32
}

/// Composite a subsurface's BGRA framebuffer `src` onto the root frame `dst` at offset `(ox, oy)`,
/// clipping to the root bounds. Wayland buffers carry premultiplied alpha, so blending is straight
/// src-over: `out = src + dst·(1 - a_src)`. XRGB (format 1) is treated as opaque.
fn blend_subsurface(dst: &mut SurfaceBuffer, src: &SurfaceBuffer, ox: i32, oy: i32) {
    if src.bgra.is_empty() || dst.bgra.is_empty() {
        return;
    }
    let (dw, dh) = (dst.width, dst.height);
    let (sw, sh) = (src.width, src.height);
    for sy in 0..sh {
        let dy = oy + sy;
        if dy < 0 || dy >= dh {
            continue;
        }
        for sx in 0..sw {
            let dx = ox + sx;
            if dx < 0 || dx >= dw {
                continue;
            }
            let si = ((sy as usize * sw as usize) + sx as usize) * 4;
            let di = ((dy as usize * dw as usize) + dx as usize) * 4;
            if si + 4 > src.bgra.len() || di + 4 > dst.bgra.len() {
                continue;
            }
            let sa = if src.format == FMT_XRGB8888 {
                255u32
            } else {
                src.bgra[si + 3] as u32
            };
            if sa == 0 {
                continue; // fully transparent
            }
            if sa == 255 {
                dst.bgra[di..di + 4].copy_from_slice(&src.bgra[si..si + 4]);
                dst.bgra[di + 3] = 255;
                continue;
            }
            let inv = 255 - sa;
            for c in 0..4 {
                let s = if c == 3 { sa } else { src.bgra[si + c] as u32 };
                let d = dst.bgra[di + c] as u32;
                dst.bgra[di + c] = (s + d * inv / 255).min(255) as u8;
            }
        }
    }
}

/// Clamp a configure dimension to a client's `[min, max]` (a bound of 0 ⇒ unconstrained on that side).
fn clamp_axis(v: i32, min: i32, max: i32) -> i32 {
    let v = if min > 0 { v.max(min) } else { v };
    if max > 0 {
        v.min(max)
    } else {
        v
    }
}

fn div_floor_i32(n: i32, mul: i32, div: i32) -> i32 {
    if div <= 0 {
        return 0;
    }
    ((n as i64 * mul as i64) / div as i64) as i32
}

fn div_ceil_i32(n: i32, mul: i32, div: i32) -> i32 {
    if div <= 0 {
        return 0;
    }
    ((n as i64 * mul as i64 + div as i64 - 1) / div as i64) as i32
}

fn color_to_bgra(bytes: &[u8]) -> [u8; 4] {
    fn channel(bytes: &[u8], idx: usize, fallback: f32) -> u8 {
        let start = idx * 4;
        let value = if start + 4 <= bytes.len() {
            f32::from_ne_bytes(bytes[start..start + 4].try_into().unwrap())
        } else {
            fallback
        };
        let value = if value.is_finite() {
            value.clamp(0.0, 1.0)
        } else {
            fallback
        };
        (value * 255.0 + 0.5) as u8
    }

    let r = channel(bytes, 0, 0.0);
    let g = channel(bytes, 1, 0.0);
    let b = channel(bytes, 2, 0.0);
    let a = channel(bytes, 3, 1.0);
    [b, g, r, a]
}
