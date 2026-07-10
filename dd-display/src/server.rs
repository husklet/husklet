//! The minimal Wayland compositor endpoint: the four core globals plus `xdg_shell`, enough for a
//! `wl_shm` client (`weston-simple-shm`, SDL2) to map a pool, attach a buffer, and commit. Per committed
//! surface we extract a tight BGRA framebuffer and hand it to a [`Presenter`] (a native window on macOS;
//! a PNG dump for headless verification). One [`Server`] drives one client connection.
//!
//! Interface/opcode numbers are the stable Wayland wire opcodes (ordered as in the protocol XML). See
//! `docs/ideas/RENDERING_PLAN.md` §1/§4.

use crate::present::{Presenter, SurfaceBuffer};
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
enum Obj {
    Registry,
    Compositor,
    Shm,
    ShmPool {
        ptr: *mut u8,
        size: usize,
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
    XdgWmBase,
    XdgSurface {
        surface: u32,
    },
    XdgToplevel {
        xdg_surface: u32,
    },
    Seat,
    Output,
    Region,
    // Chromium-ozone init globals (inert for the MVP present path).
    Subcompositor,
    Viewporter,
    DataDeviceManager,
    // GPU rung 2: zwp_linux_dmabuf_v1 objects.
    LinuxDmabuf,
    DmabufParams {
        iosurface_id: Option<u32>,
        stride: i32,
        gpu_render: bool,
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
    Other,
}

#[derive(Default)]
struct Surface {
    pending_buffer: Option<u32>, // buffer id from the most recent attach (0 ⇒ detach)
    attached: bool,
    current_buffer: Option<u32>,
    attach_x: i32,
    attach_y: i32,
    pending_frame: Option<u32>, // wl_callback id from wl_surface.frame
    xdg_surface: Option<u32>,
    configured: bool,
    title: String,
    buffer_scale: i32,
    window_geometry: Option<(i32, i32, i32, i32)>, // committed x,y,w,h from xdg_surface.set_window_geometry
    pending_window_geometry: Option<(i32, i32, i32, i32)>,
    viewport_source: Option<(i32, i32, i32, i32)>, // wl_fixed 24.8: x,y,w,h in buffer coords
    viewport_destination: Option<(i32, i32)>,      // surface-local output size
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
    output: Option<u32>, // client's wl_output object id (from wl_registry.bind)
    focus: Option<u32>, // the surface that has input focus (the toplevel, for MVP)
    ptr_entered: bool, // has wl_pointer.enter been sent for `focus`?
    kbd_entered: bool, // has wl_keyboard.enter been sent for `focus`?
    time_ms: u32,  // monotonic-ish event timestamp
    last_ptr: (i32, i32), // last pointer position (surface-local, integer px)
    last_cfg: Option<(i32, i32)>, // last on-screen window size we sent a configure for (resize debounce)
    external_logical_crop: Option<ExternalLogicalCrop>,
}

impl<P: Presenter> Server<P> {
    pub fn new(fd: RawFd, present: P) -> Server<P> {
        let mut objs = HashMap::new();
        objs.insert(WL_DISPLAY, Obj::Other); // wl_display, id 1, always present
        Server {
            conn: Conn::new(fd),
            objs,
            serial: 0,
            present,
            titles: HashMap::new(),
            seat_ver: 1,
            pointer: None,
            keyboard: None,
            output: None,
            focus: None,
            ptr_entered: false,
            kbd_entered: false,
            time_ms: 0,
            last_ptr: (0, 0),
            last_cfg: None,
            external_logical_crop: None,
        }
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
        let xdg = match self.objs.get(&surface) {
            Some(Obj::Surface(s)) => s.xdg_surface,
            _ => None,
        };
        let Some(xdg) = xdg else { return };
        let Some(tl) = self.find_toplevel(xdg) else {
            return;
        };
        // states array: [4] = XDG_TOPLEVEL_STATE_ACTIVATED (little-endian u32).
        let states = 4u32.to_ne_bytes();
        self.conn
            .send(&Message::new(tl, 0).i32(w).i32(h).array(&states));
        let s = self.next_serial();
        self.conn.send(&Message::new(xdg, 0).u32(s));
        self.conn.flush().ok();
    }

    fn next_serial(&mut self) -> u32 {
        self.serial += 1;
        self.serial
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
                Some(Obj::XdgWmBase) => "xdg_wm_base",
                Some(Obj::XdgSurface { .. }) => "xdg_surface",
                Some(Obj::XdgToplevel { .. }) => "xdg_toplevel",
                Some(Obj::Seat) => "wl_seat",
                Some(Obj::Subcompositor) => "wl_subcompositor",
                Some(Obj::Viewporter) => "wp_viewporter",
                Some(Obj::DataDeviceManager) => "wl_data_device_manager",
                Some(Obj::Output) => "wl_output",
                Some(Obj::Region) => "wl_region",
                Some(Obj::LinuxDmabuf) => "zwp_linux_dmabuf",
                Some(Obj::DmabufParams { .. }) => "zwp_linux_buffer_params",
                Some(Obj::DmaBuffer { .. }) => "wl_buffer(dmabuf)",
                Some(Obj::SurfaceAugmenter) => "surface_augmenter",
                Some(Obj::AugmentedSurface { .. }) => "augmented_surface",
                Some(Obj::SolidColorBuffer { .. }) => "wl_buffer(solid_color)",
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
            Some(Obj::XdgWmBase) => self.xdg_wm_base(m),
            Some(Obj::XdgSurface { .. }) => self.xdg_surface(m),
            Some(Obj::XdgToplevel { .. }) => self.xdg_toplevel(m),
            Some(Obj::Seat) => self.wl_seat(m),
            Some(Obj::Subcompositor) => self.wl_subcompositor(m),
            Some(Obj::Viewporter) => self.wp_viewporter(m),
            Some(Obj::DataDeviceManager) => self.wl_data_device_manager(m),
            Some(Obj::LinuxDmabuf) => self.linux_dmabuf(m),
            Some(Obj::DmabufParams { .. }) => self.dmabuf_params(m),
            Some(Obj::SurfaceAugmenter) => self.surface_augmenter(m),
            Some(Obj::Buffer { .. })
            | Some(Obj::DmaBuffer { .. })
            | Some(Obj::SolidColorBuffer { .. })
            | Some(Obj::AugmentedSurface { .. })
            | Some(Obj::Output)
            | Some(Obj::Region)
            | Some(Obj::Other) => {
                // destroy-only / inert objects: nothing to do (buffer.destroy, region.*, output.*).
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
            (G_OUTPUT, "wl_output", 2),
            (G_SEAT, "wl_seat", 5),
            (G_XDG_WM_BASE, "xdg_wm_base", 1),
            // chromium ozone binds all three of these during init; absence is a divergence from a real
            // compositor. Handlers are inert (subsurface/viewport/data objects have no required events).
            (G_SUBCOMPOSITOR, "wl_subcompositor", 1),
            (G_VIEWPORTER, "wp_viewporter", 1),
            (G_DATA_DEV_MGR, "wl_data_device_manager", 3),
            (G_SURFACE_AUGMENTER, "surface_augmenter", 1),
        ];
        // zwp_linux_dmabuf_v1 (GPU rung 2) is advertised only when DD_DISPLAY_DMABUF is set — until the
        // cross-process IOSurface handle bridge (mach port) is wired, a toolkit that PREFERS dmabuf could
        // commit buffers we can't resolve, so keep the proven shm path the default.
        let dmabuf_on = std::env::var("DD_DISPLAY_DMABUF").is_ok();
        for (name, iface, ver) in globals {
            self.conn
                .send(&Message::new(reg, 0).u32(*name).string(iface).u32(*ver));
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
            G_SEAT => {
                self.objs.insert(id, Obj::Seat);
                self.seat_ver = ver;
                // capabilities: pointer(1) | keyboard(2) — toolkits (SDL2/GTK/Chrome) require these to
                // initialize input. Touch(4) is deferred.
                self.conn.send(&Message::new(id, 0).u32(0b11));
                if ver >= 2 {
                    self.conn.send(&Message::new(id, 1).string("seat0")); // name (wl_seat v2+)
                }
            }
            G_OUTPUT => {
                self.objs.insert(id, Obj::Output);
                self.output = Some(id);
                // geometry(x,y,pw,ph,subpixel,make,model,transform) — v1
                self.conn.send(
                    &Message::new(id, 0)
                        .i32(0)
                        .i32(0)
                        .i32(300)
                        .i32(200)
                        .i32(0)
                        .string("dd")
                        .string("dd-display")
                        .i32(0),
                );
                // mode(flags=current(1)|preferred(2), w, h, refresh) — v1
                self.conn
                    .send(&Message::new(id, 1).u32(3).i32(1920).i32(1080).i32(60000));
                if ver >= 2 {
                    self.conn.send(&Message::new(id, 3).i32(1)); // scale (wl_output v2+)
                    self.conn.send(&Message::new(id, 2)); // done (wl_output v2+)
                }
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
                self.objs.insert(id, Obj::Region);
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
                    size,
                    libc::PROT_READ,
                    libc::MAP_SHARED,
                    fd,
                    0,
                )
            };
            unsafe { libc::close(fd) };
            if ptr != libc::MAP_FAILED {
                self.objs.insert(
                    id,
                    Obj::ShmPool {
                        ptr: ptr as *mut u8,
                        size,
                    },
                );
                return;
            }
        }
        self.objs.insert(id, Obj::Other);
    }

    // ---- wl_shm_pool: create_buffer(0), destroy(1), resize(2) ----
    fn wl_shm_pool(&mut self, m: Message) {
        if m.opcode != 0 {
            return;
        }
        let mut r = m.reader();
        let id = r.u32();
        let offset = r.i32();
        let width = r.i32();
        let height = r.i32();
        let stride = r.i32();
        let format = r.u32();
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

    // ---- wl_surface: attach(1), damage(2), frame(3), commit(6) ----
    fn wl_surface(&mut self, m: Message) {
        let mut r = m.reader();
        match m.opcode {
            1 => {
                // attach(buffer, x, y)
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
                    s.pending_frame = Some(cb);
                }
            }
            6 => self.commit(m.object),
            8 => {
                // set_buffer_scale(scale)
                let scale = r.i32().max(1);
                if let Some(Obj::Surface(s)) = self.objs.get_mut(&m.object) {
                    s.buffer_scale = scale;
                }
            }
            _ => {} // damage/opaque/input/transform/offset: no-op for the MVP present path
        }
    }

    fn commit(&mut self, sid: u32) {
        if let Some(Obj::Surface(s)) = self.objs.get_mut(&sid) {
            if let Some(geometry) = s.pending_window_geometry.take() {
                s.window_geometry = Some(geometry);
            }
        }

        // Snapshot the surface's committed state without holding a &mut across the borrows we need.
        let (attached, buffer, frame, xdg_surface, configured, title) = {
            match self.objs.get(&sid) {
                Some(Obj::Surface(s)) => (
                    s.attached,
                    s.pending_buffer,
                    s.pending_frame,
                    s.xdg_surface,
                    s.configured,
                    s.title.clone(),
                ),
                _ => return,
            }
        };

        // Initial commit of an xdg_surface with no buffer yet -> send the configure handshake.
        if xdg_surface.is_some() && !configured {
            self.maybe_configure_surface(sid);
            return;
        }

        let bid_to_present = if attached { buffer } else {
            match self.objs.get(&sid) {
                Some(Obj::Surface(s)) => s.current_buffer,
                _ => None,
            }
        };
        let release_bid = if attached { buffer } else { None };

        // Present the attached buffer, or the current buffer for damage/frame-only commits.
        if let Some(bid) = bid_to_present {
            if let Some(sb) = self.extract(sid, bid, &title) {
                self.present.present(&sb);
            }
        }
        if let Some(bid) = release_bid {
            // Release the buffer so the client may reuse it.
            self.conn.send(&Message::new(bid, 0)); // wl_buffer.release
        }
        // Fire the frame callback so the client draws the next frame.
        if let Some(cb) = frame {
            let t = self.next_serial();
            self.conn.send(&Message::new(cb, 0).u32(t)); // wl_callback.done
            self.conn.send(&Message::new(WL_DISPLAY, 1).u32(cb)); // delete_id
        }
        if let Some(Obj::Surface(su)) = self.objs.get_mut(&sid) {
            if attached {
                su.current_buffer = buffer;
            }
            su.pending_frame = None;
            su.attached = false;
        }
    }

    fn find_toplevel(&self, xdg_surface: u32) -> Option<u32> {
        self.objs.iter().find_map(|(id, o)| match o {
            Obj::XdgToplevel { xdg_surface: x } if *x == xdg_surface => Some(*id),
            _ => None,
        })
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
            let (width, height, pixels) = self.apply_viewport(sid, *width, *height, pixels)?;
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
            Some(Obj::ShmPool { ptr, size }) => (*ptr, *size),
            _ => return None,
        };
        if width <= 0 || height <= 0 || stride <= 0 {
            return None;
        }
        let need = offset as usize
            + (height as usize).saturating_sub(1) * stride as usize
            + width as usize * 4;
        if offset < 0 || need > size {
            return None;
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
        let (width, height, bgra) = self.apply_viewport(sid, width, height, bgra)?;
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
        })
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
                let (sx, sy, sw, sh) = surface
                    .viewport_source
                    .unwrap_or((0, 0, src_w << 8, src_h << 8));
                let src_x = fixed_floor(sx).clamp(0, src_w);
                let src_y = fixed_floor(sy).clamp(0, src_h);
                let src_x2 = fixed_ceil(sx.saturating_add(sw)).clamp(src_x, src_w);
                let src_y2 = fixed_ceil(sy.saturating_add(sh)).clamp(src_y, src_h);
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

    // ---- xdg_wm_base: destroy(0), create_positioner(1), get_xdg_surface(2), pong(3) ----
    fn xdg_wm_base(&mut self, m: Message) {
        let mut r = m.reader();
        match m.opcode {
            2 => {
                // get_xdg_surface(id, surface)
                let id = r.u32();
                let surface = r.u32();
                self.objs.insert(id, Obj::XdgSurface { surface });
                if let Some(Obj::Surface(s)) = self.objs.get_mut(&surface) {
                    s.xdg_surface = Some(id);
                }
            }
            1 => {
                let id = r.u32();
                self.objs.insert(id, Obj::Other); // positioner (popup path, unused in MVP)
            }
            _ => {}
        }
    }

    // ---- xdg_surface: get_toplevel(1), ack_configure(4) ----
    fn xdg_surface(&mut self, m: Message) {
        let mut r = m.reader();
        match m.opcode {
            1 => {
                let id = r.u32();
                self.objs.insert(
                    id,
                    Obj::XdgToplevel {
                        xdg_surface: m.object,
                    },
                );
                // Give input focus to this toplevel's surface (MVP: newest toplevel is focused).
                if let Some(Obj::XdgSurface { surface }) = self.objs.get(&m.object) {
                    let surface = *surface;
                    self.focus = Some(surface);
                    self.ptr_entered = false;
                    self.kbd_entered = false;
                }
            }
            2 => {
                let id = r.u32();
                self.objs.insert(id, Obj::Other); // get_popup
            }
            3 => {
                // xdg_surface state is double-buffered and takes effect on the next wl_surface.commit.
                let x = r.i32();
                let y = r.i32();
                let w = r.i32();
                let h = r.i32();
                if w > 0 && h > 0 {
                    let surface = match self.objs.get(&m.object) {
                        Some(Obj::XdgSurface { surface }) => *surface,
                        _ => return,
                    };
                    if let Some(Obj::Surface(s)) = self.objs.get_mut(&surface) {
                        s.pending_window_geometry = Some((x, y, w, h));
                    }
                }
            }
            _ => {} // ack_configure: accepted
        }
    }

    // ---- xdg_toplevel: set_title(2) ----
    fn xdg_toplevel(&mut self, m: Message) {
        if m.opcode == 2 {
            let mut r = m.reader();
            let title = r.string();
            // Route the title back to the owning surface.
            if let Some(Obj::XdgToplevel { xdg_surface }) = self.objs.get(&m.object) {
                let xdg = *xdg_surface;
                if let Some(Obj::XdgSurface { surface }) = self.objs.get(&xdg) {
                    let surface = *surface;
                    self.titles.insert(surface, title.clone());
                    if let Some(Obj::Surface(s)) = self.objs.get_mut(&surface) {
                        s.title = title;
                    }
                }
            }
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
                }) = self.objs.get_mut(&m.object)
                {
                    *s = stride;
                    if mod_hi & 0xffff == DD_DMABUF_MOD_MAGIC {
                        *iosurface_id = Some(mod_lo);
                        *gpu_render = mod_hi & DD_DMABUF_RENDER_BIT != 0;
                    }
                }
            }
            3 => {
                // create_immed(buffer_id, width, height, format, flags): client provides the buffer id.
                let buffer_id = r.u32();
                let w = r.i32();
                let h = r.i32();
                let format = r.u32();
                let (iosurface_id, gpu_render) = match self.objs.get(&m.object) {
                    Some(Obj::DmabufParams {
                        iosurface_id,
                        gpu_render,
                        ..
                    }) => (*iosurface_id, *gpu_render),
                    _ => (None, false),
                };
                match iosurface_id {
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
                        self.objs.insert(buffer_id, Obj::Other); // no dd IOSurface tag → unusable
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

    // ---- wl_seat: get_pointer(0)/get_keyboard(1)/get_touch(2) ----
    fn wl_seat(&mut self, m: Message) {
        let mut r = m.reader();
        match m.opcode {
            0 => {
                let id = r.u32();
                self.objs.insert(id, Obj::Other);
                self.pointer = Some(id);
                self.ptr_entered = false;
            }
            1 => {
                let id = r.u32();
                self.objs.insert(id, Obj::Other);
                self.keyboard = Some(id);
                self.kbd_entered = false;
                self.send_keymap(id);
                if self.seat_ver >= 4 {
                    // repeat_info(rate, delay) — key auto-repeat (keys/s, ms).
                    self.conn.send(&Message::new(id, 5).i32(25).i32(600));
                }
            }
            2 => {
                let id = r.u32();
                self.objs.insert(id, Obj::Other); // wl_touch (deferred)
            }
            _ => {}
        }
    }

    // ---- wl_subcompositor: destroy(0), get_subsurface(1, id, surface, parent) ----
    // A subsurface is a *role* attached to an existing wl_surface; the surface keeps committing buffers
    // through the normal wl_surface path. We only need to consume the new_id so the client's object map
    // stays in sync — positioning/sync semantics are a no-op for the single-window MVP present path.
    fn wl_subcompositor(&mut self, m: Message) {
        if m.opcode == 1 {
            let mut r = m.reader();
            let id = r.u32(); // new wl_subsurface id
            self.objs.insert(id, Obj::Other);
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
                if let Some(Obj::Surface(s)) = self.objs.get_mut(&surface) {
                    if x == -1 && y == -1 && w == -1 && h == -1 {
                        s.viewport_source = None;
                    } else if w > 0 && h > 0 {
                        s.viewport_source = Some((x, y, w, h));
                    }
                }
            }
            2 => {
                let w = r.i32();
                let h = r.i32();
                if let Some(Obj::Surface(s)) = self.objs.get_mut(&surface) {
                    if w == -1 && h == -1 {
                        s.viewport_destination = None;
                    } else if w > 0 && h > 0 {
                        s.viewport_destination = Some((w, h));
                    }
                }
            }
            _ => {}
        }
    }

    // ---- wl_data_device_manager: create_data_source(0, id), get_data_device(1, id, seat) ----
    // Clipboard/DnD plumbing. chromium binds this unconditionally; with no clipboard activity there are no
    // required events, so both child objects are inert (we just track the ids).
    fn wl_data_device_manager(&mut self, m: Message) {
        match m.opcode {
            0 | 1 => {
                let mut r = m.reader();
                let id = r.u32();
                self.objs.insert(id, Obj::Other);
            }
            _ => {}
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
        self.kbd_entered = true;
    }

    /// Absolute pointer motion in surface-local integer pixels.
    pub fn pointer_motion(&mut self, x: i32, y: i32) {
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

    /// Scroll axis (0=vertical, 1=horizontal), `value` in surface coords (fixed).
    pub fn pointer_axis(&mut self, axis: u32, value: i32) {
        self.ensure_pointer_enter();
        let Some(ptr) = self.pointer else { return };
        self.time_ms = self.time_ms.wrapping_add(8);
        let t = self.time_ms;
        // axis(time, axis, value(fixed))
        self.conn
            .send(&Message::new(ptr, 4).u32(t).u32(axis).i32(fixed(value)));
        if self.seat_ver >= 5 {
            self.conn.send(&Message::new(ptr, 5)); // frame
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
}

/// Convert an integer pixel coordinate to a `wl_fixed` (24.8 fixed-point).
fn fixed(v: i32) -> i32 {
    v * 256
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
