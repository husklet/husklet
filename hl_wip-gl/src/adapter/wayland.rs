//! The Wayland EGL platform adapter — the `wl_egl_window` ABI + the compositor present path.
//!
//! This is the "external, tech-named mechanism" (mirroring [`super::glsl`]) that teaches the GLES/EGL
//! front-end how to speak the **Wayland window system**: a real GUI app (`weston-simple-egl`, GTK) opens
//! its display with `eglGetPlatformDisplay(EGL_PLATFORM_WAYLAND_KHR, wl_display)`, wraps its `wl_surface`
//! in a `wl_egl_window` (via the `libwayland-egl` ABI [`WlEglWindow`]), then `eglCreateWindowSurface`s
//! against it and `eglSwapBuffers` to show a frame. The pieces here are split so the platform-recognition
//! + window-ABI + protocol wire-encoding are **pure, host-testable code** (no sockets), and only the live
//! [`Wayland`] session touches a fd.
//!
//! What lives here:
//!   * the EGL platform enums + `EGL_*_platform_wayland` extension strings the driver advertises,
//!   * the `libwayland-egl` [`WlEglWindow`] handle (the app-visible `wl_egl_window*`) + [`parse_native_window`]
//!     that `eglCreateWindowSurface` reads to size the surface,
//!   * [`rgba_to_xrgb8888`] — the readback→`wl_shm` pixel convert (GL bottom-left → top-left XRGB),
//!   * [`Wayland`] — a dependency-free `wl_shm` present client (discover globals → bring up an
//!     xdg-toplevel → commit a shared-memory `wl_buffer` → pace on the frame callback). It is the
//!     SELF-CONTAINED present (the shim drives its own connection), ported from `hl-shim-gl/src/wayland.rs`
//!     with the dma-buf path swapped for core `wl_shm` so it needs no host buffer-return plumbing.
//!
//! HONEST SCOPE: presenting into an app's OWN `wl_surface` (the one it created on its own
//! `libwayland-client` connection) requires marshalling `wl_surface.attach`/`commit` onto THAT connection
//! (the Mesa `wl_proxy_marshal` path). This module instead drives its own compositor connection — correct
//! for the headless / shim-owned-surface case, and it always fails LOUDLY (never a fake present) when a
//! commit / handshake / pacing step does not complete.

use core::ffi::{c_char, c_int, c_void};

// ==================================================================================================
// EGL platform recognition + advertised extensions
// ==================================================================================================

/// `EGL_PLATFORM_WAYLAND_KHR` (== `EGL_PLATFORM_WAYLAND_EXT`) — the `platform` a Wayland app passes to
/// `eglGetPlatformDisplay` so the driver knows the native display is a `wl_display*`.
pub const EGL_PLATFORM_WAYLAND_KHR: u32 = 0x31D8;
/// `EGL_PLATFORM_WAYLAND_EXT` — the `EGL_EXT_platform_base` spelling (same numeric value as the KHR one).
pub const EGL_PLATFORM_WAYLAND_EXT: u32 = 0x31D8;
/// `EGL_PLATFORM_GBM_KHR` — recognised so a GBM probe gets a truthful "not wayland" answer.
pub const EGL_PLATFORM_GBM_KHR: u32 = 0x31D7;

/// Whether `platform` selects the Wayland window system (the only windowed platform this driver backs).
pub fn is_wayland_platform(platform: u32) -> bool {
    platform == EGL_PLATFORM_WAYLAND_KHR
}

/// The CLIENT extension string (`eglQueryString(EGL_NO_DISPLAY, EGL_EXTENSIONS)`): the toolkits probe this
/// BEFORE opening a display to decide whether `eglGetPlatformDisplay(EGL_PLATFORM_WAYLAND_KHR, …)` is
/// usable, so it must advertise the platform-base + wayland-platform extensions. The device family
/// (`EGL_EXT_device_base`/`device_enumeration`/`device_query`) is queryable with `EGL_NO_DISPLAY`
/// (`eglQueryDevicesEXT` / `eglQueryDeviceStringEXT` take no display), so — matching real Mesa — it is
/// advertised in the CLIENT string as well, letting a toolkit's GL loader (e.g. libepoxy for GTK/GDK)
/// resolve `eglQueryDisplayAttribEXT` & friends before display init.
pub fn egl_client_extensions() -> &'static str {
    "EGL_EXT_client_extensions EGL_EXT_platform_base EGL_EXT_platform_wayland EGL_KHR_platform_wayland \
     EGL_EXT_device_base EGL_EXT_device_enumeration EGL_EXT_device_query"
}

/// The per-DISPLAY extension string (`eglQueryString(dpy, EGL_EXTENSIONS)`), advertising the same
/// wayland-platform support plus the context extensions a GLES app expects. `EGL_EXT_device_base` /
/// `EGL_EXT_device_query` are DISPLAY extensions once a display is initialized (GDK's Wayland EGL
/// bring-up requires one of that set to find `eglQueryDisplayAttribEXT`), so they are advertised here too.
pub fn egl_display_extensions() -> &'static str {
    "EGL_KHR_create_context EGL_KHR_surfaceless_context EGL_KHR_no_config_context \
     EGL_EXT_platform_wayland EGL_KHR_platform_wayland \
     EGL_EXT_device_base EGL_EXT_device_query"
}

// ==================================================================================================
// libwayland-egl ABI: the `wl_egl_window` handle
// ==================================================================================================

/// Magic tag in the first (`version`) field of our [`WlEglWindow`], so `eglCreateWindowSurface` can tell
/// OUR `wl_egl_window*` (created by the staged `libwayland-egl.so.1`) from a stray struct. Positive, so it
/// is a legal `intptr_t version` a stock Mesa parser would just treat as a very large version.
pub const HL_WL_EGL_MAGIC: isize = 0x686c_776c_5f65_676c; // "hlwl_egl"

/// The `wl_egl_window` the app links against (the `libwayland-egl` ABI). It is a plain
/// `wl_surface` + backing size; our libEGL reads it in `eglCreateWindowSurface`. `#[repr(C)]` with the
/// field order the staged C shim (`shim/wayland-egl/wayland_egl.c`) allocates, so the two agree on the
/// 64-byte layout byte-for-byte (asserted in the tests + the dlopen integration test).
#[repr(C)]
pub struct WlEglWindow {
    /// `HL_WL_EGL_MAGIC` (Mesa keeps an `intptr_t version` here).
    pub version: isize,
    pub width: i32,
    pub height: i32,
    pub dx: i32,
    pub dy: i32,
    pub attached_width: i32,
    pub attached_height: i32,
    driver_private: *mut c_void,
    resize_cb: *mut c_void,
    destroy_cb: *mut c_void,
    /// The app's `wl_surface*` (an opaque `wl_proxy*` on the app's own `libwayland-client` connection).
    pub surface: *mut c_void,
}

impl WlEglWindow {
    /// `wl_egl_window_create(surface, width, height)` — a fresh window wrapping `surface` at `width`×`height`.
    pub fn new(surface: *mut c_void, width: i32, height: i32) -> Self {
        WlEglWindow {
            version: HL_WL_EGL_MAGIC,
            width,
            height,
            dx: 0,
            dy: 0,
            attached_width: 0,
            attached_height: 0,
            driver_private: core::ptr::null_mut(),
            resize_cb: core::ptr::null_mut(),
            destroy_cb: core::ptr::null_mut(),
            surface,
        }
    }

    /// `wl_egl_window_resize(w, width, height, dx, dy)` — update the backing size + attach offset.
    pub fn resize(&mut self, width: i32, height: i32, dx: i32, dy: i32) {
        self.width = width;
        self.height = height;
        self.dx = dx;
        self.dy = dy;
    }

    /// `wl_egl_window_get_attached_size` — the last-attached size (falling back to the current size).
    pub fn attached_size(&self) -> (i32, i32) {
        let w = if self.attached_width != 0 { self.attached_width } else { self.width };
        let h = if self.attached_height != 0 { self.attached_height } else { self.height };
        (w, h)
    }
}

/// The native-window geometry `eglCreateWindowSurface` extracts: the backing size + the app's `wl_surface`.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct WlWindowInfo {
    pub width: u32,
    pub height: u32,
    /// The app's `wl_surface*` as a `usize` (0 if the native window carried none).
    pub wl_surface: usize,
}

/// Parse the app's native window handle to `(width, height, wl_surface)`. Recognises OUR
/// `wl_egl_window` (the `HL_WL_EGL_MAGIC` struct the staged `libwayland-egl` hands the app) AND a REAL
/// `libwayland-egl` `wl_egl_window` (the stable `wayland-egl-backend.h` ABI a bundled/system libwayland-egl
/// — Chrome/ANGLE's — hands us). Sizes are clamped to a sane `1..=8192`, defaulting to 256.
///
/// The real ABI is `intptr_t version; int width, height, dx, dy; struct wl_surface *surface; …`, so on LP64
/// the `version` occupies the FIRST 8 bytes and the size lives at offset 8/12, with the wrapped `wl_surface*`
/// at offset 24 — NOT at offset 0/4. Reading offset 0/4 as `(width, height)` (the old heuristic) misreads a
/// real window's `version` (`WL_EGL_WINDOW_VERSION`, e.g. 3) as `width=3` and its zero high word as
/// `height→256`, which is exactly the bogus 3×256 window Chrome presented. Our OWN magic struct shares the
/// width@8/height@12 offsets but keeps its `surface` at the end, so it is handled by the magic branch.
///
/// # Safety
/// `w` must be null or point at a readable `wl_egl_window`-ABI struct (what `eglCreateWindowSurface` is
/// always handed on Wayland).
pub unsafe fn parse_native_window(w: *const c_void) -> WlWindowInfo {
    let clamp = |v: i32| if v > 0 && v <= 8192 { v as u32 } else { 256 };
    if w.is_null() {
        return WlWindowInfo { width: 256, height: 256, wl_surface: 0 };
    }
    let version = *(w as *const isize);
    if version == HL_WL_EGL_MAGIC {
        let win = &*(w as *const WlEglWindow);
        let (mut ww, mut hh) = (win.width, win.height);
        if win.attached_width > 0 && win.attached_height > 0 && win.attached_width <= 8192 && win.attached_height <= 8192 {
            ww = ww.max(win.attached_width);
            hh = hh.max(win.attached_height);
        }
        return WlWindowInfo { width: clamp(ww), height: clamp(hh), wl_surface: win.surface as usize };
    }
    // A REAL libwayland-egl `wl_egl_window` (Mesa / the reference wayland-egl backend). Its stable ABI puts an
    // `intptr_t version` in the first 8 bytes (LP64), then `int width;`@8, `int height;`@12, `int dx;`@16,
    // `int dy;`@20, and `struct wl_surface *surface;`@24. Read those real fields so the size is the app's
    // actual window and the wrapped `wl_surface*` drives the app-surface present path (present onto the app's
    // OWN toplevel), instead of misreading the version word as a 3×256 window.
    let words = w as *const i32;
    let width = *words.add(2); // offset 8
    let height = *words.add(3); // offset 12
    let surface = *((w as *const u8).add(24) as *const *mut c_void); // offset 24
    WlWindowInfo { width: clamp(width), height: clamp(height), wl_surface: surface as usize }
}

// ==================================================================================================
// readback → wl_shm pixel convert
// ==================================================================================================

/// Convert a `glReadPixels(GL_RGBA)` plane (tight-packed `w`×`h`, **bottom-left** origin, `[R,G,B,A]`) into
/// the `WL_SHM_FORMAT_XRGB8888` little-endian byte order a `wl_shm` buffer wants (`[B,G,R,X]` per texel,
/// **top-left** origin). The vertical flip turns GL's bottom-up scanlines into wayland's top-down ones.
pub fn rgba_to_xrgb8888(rgba: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut out = vec![0u8; w * h * 4];
    if rgba.len() < w * h * 4 {
        return out;
    }
    for y in 0..h {
        let src_row = (h - 1 - y) * w * 4; // GL bottom-left → top-left
        let dst_row = y * w * 4;
        for x in 0..w {
            let s = src_row + x * 4;
            let d = dst_row + x * 4;
            let (r, g, b) = (rgba[s], rgba[s + 1], rgba[s + 2]);
            // XRGB8888 little-endian in memory is [B, G, R, X].
            out[d] = b;
            out[d + 1] = g;
            out[d + 2] = r;
            out[d + 3] = 0xFF;
        }
    }
    out
}

// ==================================================================================================
// live wl_shm present client (self-contained connection)
// ==================================================================================================

/// Surface geometry the shim advertises to the compositor (logical size + attach offset).
#[derive(Clone, Copy, Default)]
pub struct Geometry {
    pub backing_w: u32,
    pub backing_h: u32,
    pub logical_w: i32,
    pub logical_h: i32,
    pub geom_x: i32,
    pub geom_y: i32,
    pub attach_x: i32,
    pub attach_y: i32,
}

impl Geometry {
    /// A backing-sized geometry (logical == backing, no offset).
    pub fn backing(w: u32, h: u32) -> Self {
        Geometry { backing_w: w, backing_h: h, logical_w: w as i32, logical_h: h as i32, ..Default::default() }
    }

    /// Whether `xdg_surface.set_window_geometry` differs from the backing rectangle and should be sent.
    fn should_send(&self) -> bool {
        self.logical_w > 0
            && self.logical_h > 0
            && !(self.logical_w == self.backing_w as i32
                && self.logical_h == self.backing_h as i32
                && self.geom_x == 0
                && self.geom_y == 0)
    }
}

/// A typed outcome for the fallible wayland present (a commit/handshake/pacing failure must never look
/// like a successful frame).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WlError {
    /// The compositor closed the connection (EOF / write error) — the frame was not delivered.
    Disconnected,
    /// A socket write could not deliver the whole message.
    ShortWrite,
    /// Passing the shm fd (`sendmsg`/`SCM_RIGHTS`) failed.
    FdSend,
    /// Allocating / mapping the shared-memory pool failed.
    ShmAlloc,
    /// A required global (`wl_compositor`/`wl_shm`/`xdg_wm_base`) was not advertised.
    MissingGlobal,
    /// The compositor raised `wl_display.error(object, code)` (a protocol violation).
    Protocol { object: u32, code: u32 },
    /// The compositor never returned the frame callback within the pacing deadline.
    FrameTimeout,
}

pub type WlResult<T> = Result<T, WlError>;

/// One `wl_registry.global` advertisement, used to bind by discovered name/version.
#[derive(Clone)]
pub struct RegistryGlobal {
    pub name: u32,
    pub interface: String,
    pub version: u32,
}

// ---- client-assigned wayland object ids (our id space; server names are discovered) ----
const OBJ_DISPLAY: u32 = 1;
const OBJ_REGISTRY: u32 = 2;
const OBJ_SYNC_CB: u32 = 3;
const OBJ_COMPOSITOR: u32 = 4;
const OBJ_SHM: u32 = 5;
const OBJ_XDG_WM_BASE: u32 = 6;
const OBJ_WL_SURFACE: u32 = 7;
const OBJ_XDG_SURFACE: u32 = 8;
const OBJ_TOPLEVEL: u32 = 9;
const OBJ_SHM_POOL: u32 = 10;
const OBJ_WL_BUFFER: u32 = 11;
const OBJ_FRAME_CB: u32 = 12;

/// `WL_SHM_FORMAT_XRGB8888` (the byte order [`rgba_to_xrgb8888`] packs into).
const WL_SHM_FORMAT_XRGB8888: u32 = 1;

/// How long to wait for the compositor's per-frame callback before reporting a pacing failure.
const FRAME_DEADLINE_MS: u64 = 100;
/// Bound on the initial registry/configure handshake reads.
const HANDSHAKE_DEADLINE_MS: u64 = 400;

// ---- minimal libc surface (dependency-free) ----
#[repr(C)]
struct IoVec {
    base: *mut c_void,
    len: usize,
}
#[repr(C)]
struct MsgHdr {
    name: *mut c_void,
    namelen: u32,
    _pad0: u32,
    iov: *mut IoVec,
    iovlen: usize,
    control: *mut c_void,
    controllen: usize,
    flags: c_int,
}
#[repr(C)]
struct PollFd {
    fd: c_int,
    events: i16,
    revents: i16,
}
extern "C" {
    fn socket(domain: c_int, ty: c_int, proto: c_int) -> c_int;
    fn connect(fd: c_int, addr: *const c_void, len: u32) -> c_int;
    fn write(fd: c_int, buf: *const c_void, n: usize) -> isize;
    fn read(fd: c_int, buf: *mut c_void, n: usize) -> isize;
    fn sendmsg(fd: c_int, msg: *const MsgHdr, flags: c_int) -> isize;
    fn poll(fds: *mut PollFd, n: u64, timeout: c_int) -> c_int;
    fn close(fd: c_int) -> c_int;
    fn memfd_create(name: *const c_char, flags: u32) -> c_int;
    fn ftruncate(fd: c_int, len: i64) -> c_int;
    fn mmap(addr: *mut c_void, len: usize, prot: c_int, flags: c_int, fd: c_int, off: i64) -> *mut c_void;
    fn munmap(addr: *mut c_void, len: usize) -> c_int;
}

const AF_UNIX: c_int = 1;
const SOCK_STREAM: c_int = 1;
const SOL_SOCKET: c_int = 1;
const SCM_RIGHTS: c_int = 1;
const POLLIN: i16 = 0x001;
const PROT_READ: c_int = 1;
const PROT_WRITE: c_int = 2;
const MAP_SHARED: c_int = 1;
const MAP_FAILED: isize = -1;

/// A connected `wl_shm` present session to a wayland compositor.
pub struct Wayland {
    fd: c_int,
    tx: Vec<u8>,
    rx: Vec<u8>,
    ready: bool,
    globals: Vec<RegistryGlobal>,
    sync_done: bool,
    configure_serial: Option<u32>,
    frame_done: bool,
}

impl Wayland {
    /// Append one wayland request: `[obj][ (size<<16)|op ][words…]`.
    fn wmsg(&mut self, obj: u32, op: u16, words: &[u32]) {
        let sz = (8 + words.len() * 4) as u32;
        self.tx.extend_from_slice(&obj.to_le_bytes());
        self.tx.extend_from_slice(&((sz << 16) | op as u32).to_le_bytes());
        for w in words {
            self.tx.extend_from_slice(&w.to_le_bytes());
        }
    }

    /// A `wl_registry.bind(name, interface, version, new_id)` — `name` is the DISCOVERED registry name.
    fn bind(&mut self, name: u32, interface: &str, version: u32, new_id: u32) {
        let mut words = vec![name, (interface.len() + 1) as u32];
        let mut sbuf = interface.as_bytes().to_vec();
        sbuf.push(0);
        while sbuf.len() % 4 != 0 {
            sbuf.push(0);
        }
        for chunk in sbuf.chunks(4) {
            let mut wbuf = [0u8; 4];
            wbuf[..chunk.len()].copy_from_slice(chunk);
            words.push(u32::from_le_bytes(wbuf));
        }
        words.push(version);
        words.push(new_id);
        self.wmsg(OBJ_REGISTRY, 0, &words);
    }

    /// Bind a required interface by its advertised name (clamped to the version we speak). Returns false
    /// if the compositor never advertised it.
    fn bind_discovered(&mut self, interface: &str, max_version: u32, new_id: u32) -> bool {
        let Some(g) = self.globals.iter().find(|g| g.interface == interface).cloned() else {
            return false;
        };
        let version = g.version.min(max_version).max(1);
        self.bind(g.name, interface, version, new_id);
        true
    }

    /// Full-write the pending buffer, propagating a short write / disconnect.
    fn wflush(&mut self) -> WlResult<()> {
        let mut sent = 0usize;
        while sent < self.tx.len() {
            let n = unsafe { write(self.fd, self.tx[sent..].as_ptr() as *const c_void, self.tx.len() - sent) };
            if n < 0 {
                self.tx.clear();
                return Err(WlError::Disconnected);
            }
            if n == 0 {
                self.tx.clear();
                return Err(WlError::ShortWrite);
            }
            sent += n as usize;
        }
        self.tx.clear();
        Ok(())
    }

    /// Flush the pending buffer with a single fd attached via `SCM_RIGHTS`, propagating failure.
    fn wflush_fd(&mut self, fd: c_int) -> WlResult<()> {
        let want = self.tx.len();
        let mut iov = IoVec { base: self.tx.as_mut_ptr() as *mut c_void, len: want };
        let mut cbuf = [0u8; 24]; // CMSG_SPACE(sizeof(int)) == 24 on LP64
        cbuf[0..8].copy_from_slice(&20usize.to_ne_bytes()); // cmsg_len = CMSG_LEN(4) = 20
        cbuf[8..12].copy_from_slice(&SOL_SOCKET.to_ne_bytes());
        cbuf[12..16].copy_from_slice(&SCM_RIGHTS.to_ne_bytes());
        cbuf[16..20].copy_from_slice(&fd.to_ne_bytes());
        let mh = MsgHdr {
            name: core::ptr::null_mut(),
            namelen: 0,
            _pad0: 0,
            iov: &mut iov,
            iovlen: 1,
            control: cbuf.as_mut_ptr() as *mut c_void,
            controllen: cbuf.len(),
            flags: 0,
        };
        let n = unsafe { sendmsg(self.fd, &mh, 0) };
        self.tx.clear();
        if n < 0 {
            return Err(WlError::FdSend);
        }
        if (n as usize) < want {
            return Err(WlError::ShortWrite);
        }
        Ok(())
    }

    fn send_geometry(&mut self, g: &Geometry) {
        if g.should_send() {
            self.wmsg(OBJ_XDG_SURFACE, 3, &[g.geom_x as u32, g.geom_y as u32, g.logical_w as u32, g.logical_h as u32]);
        }
    }

    /// Poll + read one batch of compositor events into `rx`. `Ok(true)` if bytes were read, `Ok(false)` on
    /// a poll timeout, `Err(Disconnected)` on EOF / socket error (a closed peer is a real failure).
    fn pump(&mut self, timeout_ms: i32) -> WlResult<bool> {
        if self.fd < 0 {
            return Ok(false);
        }
        let mut pfd = PollFd { fd: self.fd, events: POLLIN, revents: 0 };
        let pr = unsafe { poll(&mut pfd, 1, timeout_ms) };
        if pr < 0 {
            return Err(WlError::Disconnected);
        }
        if pr == 0 {
            return Ok(false);
        }
        if pfd.revents & POLLIN == 0 {
            return Err(WlError::Disconnected);
        }
        let mut buf = [0u8; 8192];
        let n = unsafe { read(self.fd, buf.as_mut_ptr() as *mut c_void, buf.len()) };
        if n <= 0 {
            return Err(WlError::Disconnected);
        }
        self.rx.extend_from_slice(&buf[..n as usize]);
        Ok(true)
    }

    /// Process every complete message currently buffered in `rx`: records `wl_registry.global`, marks
    /// sync/frame callbacks done, captures the configure serial, answers `xdg_wm_base.ping` with a pong,
    /// and turns `wl_display.error` into [`WlError::Protocol`].
    fn dispatch_pending(&mut self) -> WlResult<()> {
        let mut off = 0usize;
        let mut pong: Option<u32> = None;
        while self.rx.len() - off >= 8 {
            let obj = u32::from_le_bytes(self.rx[off..off + 4].try_into().unwrap());
            let so = u32::from_le_bytes(self.rx[off + 4..off + 8].try_into().unwrap());
            let size = (so >> 16) as usize;
            let op = (so & 0xffff) as u16;
            if size < 8 || self.rx.len() - off < size {
                break;
            }
            let body = &self.rx[off + 8..off + size];
            match (obj, op) {
                (OBJ_DISPLAY, 0) if body.len() >= 8 => {
                    let object = u32::from_le_bytes(body[0..4].try_into().unwrap());
                    let code = u32::from_le_bytes(body[4..8].try_into().unwrap());
                    self.rx.drain(..off + size);
                    return Err(WlError::Protocol { object, code });
                }
                (OBJ_REGISTRY, 0) => {
                    if let Some((name, interface, version)) = parse_registry_global(body) {
                        self.globals.push(RegistryGlobal { name, interface, version });
                    }
                }
                (OBJ_SYNC_CB, 0) => self.sync_done = true,
                (OBJ_FRAME_CB, 0) => self.frame_done = true,
                (OBJ_XDG_SURFACE, 0) if body.len() >= 4 => {
                    self.configure_serial = Some(u32::from_le_bytes(body[0..4].try_into().unwrap()));
                }
                (OBJ_XDG_WM_BASE, 0) if body.len() >= 4 => {
                    pong = Some(u32::from_le_bytes(body[0..4].try_into().unwrap()));
                }
                _ => {}
            }
            off += size;
        }
        if off > 0 {
            self.rx.drain(..off);
        }
        if let Some(serial) = pong {
            self.wmsg(OBJ_XDG_WM_BASE, 3, &[serial]); // xdg_wm_base.pong
            self.wflush()?;
        }
        Ok(())
    }

    /// Send `get_registry` + a `wl_display.sync` barrier, read until the sync callback returns, then bind
    /// each required interface by its discovered name.
    fn discover_and_bind(&mut self) -> WlResult<()> {
        self.wmsg(OBJ_DISPLAY, 1, &[OBJ_REGISTRY]); // wl_display.get_registry
        self.wmsg(OBJ_DISPLAY, 0, &[OBJ_SYNC_CB]); // wl_display.sync — end-of-globals barrier
        self.wflush()?;
        let deadline = now_ms() + HANDSHAKE_DEADLINE_MS;
        while !self.sync_done {
            self.dispatch_pending()?;
            if self.sync_done {
                break;
            }
            let rem = deadline as i64 - now_ms() as i64;
            if rem <= 0 {
                break;
            }
            if !self.pump(rem as c_int + 1)? {
                break;
            }
        }
        if !self.bind_discovered("wl_compositor", 4, OBJ_COMPOSITOR)
            || !self.bind_discovered("wl_shm", 1, OBJ_SHM)
            || !self.bind_discovered("xdg_wm_base", 1, OBJ_XDG_WM_BASE)
        {
            return Err(WlError::MissingGlobal);
        }
        Ok(())
    }

    /// After creating the surface/xdg_surface/toplevel, wait for `configure` and ack it with the RECEIVED
    /// serial (never an invented constant).
    fn ack_first_configure(&mut self) -> WlResult<()> {
        let deadline = now_ms() + HANDSHAKE_DEADLINE_MS;
        while self.configure_serial.is_none() {
            self.dispatch_pending()?;
            if self.configure_serial.is_some() {
                break;
            }
            let rem = deadline as i64 - now_ms() as i64;
            if rem <= 0 {
                break;
            }
            if !self.pump(rem as c_int + 1)? {
                break;
            }
        }
        if let Some(serial) = self.configure_serial {
            self.wmsg(OBJ_XDG_SURFACE, 4, &[serial]); // xdg_surface.ack_configure
            self.wflush()?;
        }
        Ok(())
    }

    /// Connect to `$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY` and run discovery + surface bring-up. Returns None
    /// if the socket is unavailable or the handshake fails (an honest "no compositor" — never a fake up).
    pub fn connect_and_handshake(g: &Geometry) -> Option<Wayland> {
        let disp = std::env::var("WAYLAND_DISPLAY").ok()?;
        let rd = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/0".to_string());
        let path = if disp.starts_with('/') { disp } else { format!("{rd}/{disp}") };

        let fd = unsafe { socket(AF_UNIX, SOCK_STREAM, 0) };
        if fd < 0 {
            return None;
        }
        let mut sa = [0u8; 110]; // sockaddr_un: family (u16) + path (108 bytes)
        sa[0..2].copy_from_slice(&(AF_UNIX as u16).to_ne_bytes());
        let pb = path.as_bytes();
        let n = pb.len().min(107);
        sa[2..2 + n].copy_from_slice(&pb[..n]);
        if unsafe { connect(fd, sa.as_ptr() as *const c_void, sa.len() as u32) } != 0 {
            unsafe { close(fd) };
            return None;
        }
        let mut w = Wayland {
            fd,
            tx: Vec::new(),
            rx: Vec::new(),
            ready: false,
            globals: Vec::new(),
            sync_done: false,
            configure_serial: None,
            frame_done: false,
        };
        if w.discover_and_bind().is_err() {
            return None;
        }
        w.wmsg(OBJ_COMPOSITOR, 0, &[OBJ_WL_SURFACE]); // wl_compositor.create_surface
        w.wmsg(OBJ_XDG_WM_BASE, 2, &[OBJ_XDG_SURFACE, OBJ_WL_SURFACE]); // xdg_wm_base.get_xdg_surface
        w.wmsg(OBJ_XDG_SURFACE, 1, &[OBJ_TOPLEVEL]); // xdg_surface.get_toplevel
        w.send_geometry(g);
        w.wmsg(OBJ_WL_SURFACE, 6, &[]); // wl_surface.commit (initial)
        if w.wflush().is_err() {
            return None;
        }
        if w.ack_first_configure().is_err() {
            return None;
        }
        w.ready = true;
        Some(w)
    }

    /// Commit one frame's `xrgb` pixels (`WL_SHM_FORMAT_XRGB8888`, top-left, tight `w*h*4`) as a `wl_shm`
    /// `wl_buffer` to the surface, then pace on the frame callback. Returns a typed error on any
    /// map/delivery/protocol/pacing failure — never a silent "presented".
    pub fn commit(&mut self, xrgb: &[u8], g: &Geometry) -> WlResult<()> {
        if !self.ready {
            return Err(WlError::Disconnected);
        }
        let (w, h) = (g.backing_w.max(1), g.backing_h.max(1));
        let stride = w * 4;
        let size = (stride * h) as usize;
        if xrgb.len() < size {
            return Err(WlError::ShmAlloc);
        }
        let shm = ShmBuffer::new(&xrgb[..size])?;
        self.frame_done = false;

        // wl_shm.create_pool(new_id=pool, fd, size) — the fd rides SCM_RIGHTS on the flush.
        self.wmsg(OBJ_SHM, 0, &[OBJ_SHM_POOL, size as u32]);
        self.wflush_fd(shm.fd)?;
        // wl_shm_pool.create_buffer(new_id=buffer, offset=0, width, height, stride, format).
        self.wmsg(OBJ_SHM_POOL, 0, &[OBJ_WL_BUFFER, 0, w, h, stride, WL_SHM_FORMAT_XRGB8888]);
        self.wmsg(OBJ_WL_SURFACE, 1, &[OBJ_WL_BUFFER, g.attach_x as u32, g.attach_y as u32]); // attach
        self.wmsg(OBJ_WL_SURFACE, 2, &[0, 0, w, h]); // damage
        self.send_geometry(g);
        self.wmsg(OBJ_WL_SURFACE, 3, &[OBJ_FRAME_CB]); // frame(callback)
        self.wmsg(OBJ_WL_SURFACE, 6, &[]); // commit
        self.wflush()?;
        self.wmsg(OBJ_WL_BUFFER, 0, &[]); // wl_buffer.destroy (the compositor keeps its own dup)
        self.wmsg(OBJ_SHM_POOL, 1, &[]); // wl_shm_pool.destroy
        self.wflush()?;
        self.await_frame()
    }

    /// Drain events until the frame callback fires, bounded by the pacing deadline.
    fn await_frame(&mut self) -> WlResult<()> {
        let deadline = now_ms() + FRAME_DEADLINE_MS;
        loop {
            self.dispatch_pending()?;
            if self.frame_done {
                return Ok(());
            }
            let rem = deadline as i64 - now_ms() as i64;
            if rem <= 0 {
                return Err(WlError::FrameTimeout);
            }
            if !self.pump(rem as c_int + 1)? {
                continue;
            }
        }
    }
}

impl Drop for Wayland {
    fn drop(&mut self) {
        if self.fd >= 0 {
            unsafe { close(self.fd) };
        }
    }
}

/// A memfd-backed shared-memory buffer whose bytes are the pixel plane the compositor maps read-only.
///
/// `pub(crate)` so the app-surface presenter ([`super::wayland_app`]) reuses the SAME memfd allocator to
/// back the `wl_shm` pool it marshals onto the app's own `libwayland-client` connection.
pub(crate) struct ShmBuffer {
    pub(crate) fd: c_int,
}

impl ShmBuffer {
    /// Allocate a `memfd`, size it, map it, copy `pixels` in, then unmap (the fd retains the contents).
    pub(crate) fn new(pixels: &[u8]) -> WlResult<ShmBuffer> {
        let name = b"hl-wl-shm\0";
        let fd = unsafe { memfd_create(name.as_ptr() as *const c_char, 0) };
        if fd < 0 {
            return Err(WlError::ShmAlloc);
        }
        let len = pixels.len();
        if unsafe { ftruncate(fd, len as i64) } != 0 {
            unsafe { close(fd) };
            return Err(WlError::ShmAlloc);
        }
        let map = unsafe { mmap(core::ptr::null_mut(), len, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0) };
        if map as isize == MAP_FAILED || map.is_null() {
            unsafe { close(fd) };
            return Err(WlError::ShmAlloc);
        }
        unsafe {
            core::ptr::copy_nonoverlapping(pixels.as_ptr(), map as *mut u8, len);
            munmap(map, len);
        }
        Ok(ShmBuffer { fd })
    }
}

impl Drop for ShmBuffer {
    fn drop(&mut self) {
        if self.fd >= 0 {
            unsafe { close(self.fd) };
        }
    }
}

/// Decode a `wl_registry.global` event body: `name(u32), interface(string), version(u32)`.
fn parse_registry_global(body: &[u8]) -> Option<(u32, String, u32)> {
    if body.len() < 8 {
        return None;
    }
    let name = u32::from_le_bytes(body[0..4].try_into().ok()?);
    let slen = u32::from_le_bytes(body[4..8].try_into().ok()?) as usize;
    let padded = (slen + 3) & !3;
    if 8 + padded + 4 > body.len() {
        return None;
    }
    let raw = &body[8..8 + slen.saturating_sub(1)]; // exclude the NUL terminator
    let interface = String::from_utf8_lossy(raw).into_owned();
    let version = u32::from_le_bytes(body[8 + padded..8 + padded + 4].try_into().ok()?);
    Some((name, interface, version))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

// ==================================================================================================
// tests
// ==================================================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn blank() -> Wayland {
        Wayland {
            fd: -1,
            tx: Vec::new(),
            rx: Vec::new(),
            ready: false,
            globals: Vec::new(),
            sync_done: false,
            configure_serial: None,
            frame_done: false,
        }
    }

    fn global_event(name: u32, interface: &str, version: u32) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&name.to_le_bytes());
        body.extend_from_slice(&((interface.len() + 1) as u32).to_le_bytes());
        let mut s = interface.as_bytes().to_vec();
        s.push(0);
        while s.len() % 4 != 0 {
            s.push(0);
        }
        body.extend_from_slice(&s);
        body.extend_from_slice(&version.to_le_bytes());
        let size = (8 + body.len()) as u32;
        let mut msg = Vec::new();
        msg.extend_from_slice(&OBJ_REGISTRY.to_le_bytes());
        msg.extend_from_slice(&((size << 16) | 0u32).to_le_bytes());
        msg.extend_from_slice(&body);
        msg
    }

    #[test]
    fn platform_recognition_and_extensions() {
        assert!(is_wayland_platform(EGL_PLATFORM_WAYLAND_KHR));
        assert!(is_wayland_platform(EGL_PLATFORM_WAYLAND_EXT));
        assert!(!is_wayland_platform(EGL_PLATFORM_GBM_KHR));
        assert!(egl_client_extensions().contains("EGL_EXT_platform_wayland"));
        assert!(egl_client_extensions().contains("EGL_KHR_platform_wayland"));
        assert!(egl_display_extensions().contains("EGL_KHR_platform_wayland"));
        // The device family GDK/epoxy require to find eglQueryDisplayAttribEXT — advertised on both
        // the client (EGL_NO_DISPLAY) and per-display strings, matching real Mesa.
        assert!(egl_client_extensions().contains("EGL_EXT_device_base"));
        assert!(egl_client_extensions().contains("EGL_EXT_device_query"));
        assert!(egl_display_extensions().contains("EGL_EXT_device_base"));
        assert!(egl_display_extensions().contains("EGL_EXT_device_query"));
    }

    /// The `wl_egl_window` ABI struct is the exact 64-byte C layout the staged `libwayland-egl` allocates.
    #[test]
    fn wl_egl_window_layout_is_the_c_abi() {
        assert_eq!(core::mem::size_of::<WlEglWindow>(), 64, "wl_egl_window is 64 bytes on LP64");
        assert_eq!(core::mem::align_of::<WlEglWindow>(), 8);
        let s = 0xABCD_1234usize as *mut c_void;
        let w = WlEglWindow::new(s, 800, 600);
        assert_eq!(w.version, HL_WL_EGL_MAGIC);
        assert_eq!((w.width, w.height), (800, 600));
        assert_eq!(w.attached_size(), (800, 600));
    }

    #[test]
    fn wl_egl_window_resize_updates_size_and_offset() {
        let mut w = WlEglWindow::new(core::ptr::null_mut(), 100, 100);
        w.resize(640, 480, 3, 4);
        assert_eq!((w.width, w.height, w.dx, w.dy), (640, 480, 3, 4));
        assert_eq!(w.attached_size(), (640, 480));
    }

    /// `eglCreateWindowSurface` reads OUR magic `wl_egl_window` for size + the app's `wl_surface`.
    #[test]
    fn parse_native_window_reads_the_magic_wl_egl_window() {
        let surf = 0x7777_0000usize as *mut c_void;
        let win = WlEglWindow::new(surf, 1024, 768);
        let info = unsafe { parse_native_window(&win as *const _ as *const c_void) };
        assert_eq!(info, WlWindowInfo { width: 1024, height: 768, wl_surface: 0x7777_0000 });
    }

    /// A REAL `libwayland-egl` `wl_egl_window` (the bundled/system one Chrome/ANGLE hands us): the stable
    /// `wayland-egl-backend.h` ABI is `intptr_t version; int width, height, dx, dy; struct wl_surface*;`, so
    /// the size lives at offset 8/12 and the wrapped surface at offset 24 — NOT at offset 0/4. The fallback
    /// must read those real fields (a regression guard for the 3×256 Chrome window: `version`=3 must NOT be
    /// read as width).
    #[test]
    fn parse_native_window_reads_the_real_libwayland_egl_window() {
        // Mirror the real ABI in a byte buffer: version(3)@0, width(800)@8, height(600)@12, dx@16, dy@20,
        // surface(ptr)@24. `#[repr(C)]` so the field offsets are exactly the C ABI's.
        #[repr(C)]
        struct MesaWlEglWindow {
            version: isize,
            width: i32,
            height: i32,
            dx: i32,
            dy: i32,
            surface: *mut c_void,
        }
        let win = MesaWlEglWindow {
            version: 3, // WL_EGL_WINDOW_VERSION — the value the old heuristic misread as width=3
            width: 800,
            height: 600,
            dx: 0,
            dy: 0,
            surface: 0x5150_0000usize as *mut c_void,
        };
        let info = unsafe { parse_native_window(&win as *const _ as *const c_void) };
        assert_eq!((info.width, info.height, info.wl_surface), (800, 600, 0x5150_0000));
        // A null window is the clamped default.
        let d = unsafe { parse_native_window(core::ptr::null()) };
        assert_eq!((d.width, d.height), (256, 256));
    }

    /// The readback→shm convert flips vertically and packs XRGB8888 little-endian ([B,G,R,X]).
    #[test]
    fn rgba_to_xrgb_flips_and_reorders() {
        // 1x2 image: bottom row red, top row green (GL bottom-left order: row0 = bottom).
        let rgba = [/*row0 bottom, red*/ 255, 0, 0, 255, /*row1 top, green*/ 0, 255, 0, 255];
        let out = rgba_to_xrgb8888(&rgba, 1, 2);
        // top-left output row0 is the GL TOP row (green) → [B,G,R,X] = [0,255,0,255].
        assert_eq!(&out[0..4], &[0, 255, 0, 255]);
        // output row1 is the GL BOTTOM row (red) → [0,0,255,255].
        assert_eq!(&out[4..8], &[0, 0, 255, 255]);
    }

    /// Binds use the DISCOVERED registry name (not an assumed constant), and require wl_shm.
    #[test]
    fn binds_use_discovered_registry_names() {
        let mut w = blank();
        w.rx.extend_from_slice(&global_event(7, "wl_compositor", 5));
        w.rx.extend_from_slice(&global_event(9, "wl_shm", 1));
        w.rx.extend_from_slice(&global_event(4, "xdg_wm_base", 2));
        w.dispatch_pending().unwrap();
        assert_eq!(w.globals.len(), 3);

        assert!(w.bind_discovered("wl_shm", 1, OBJ_SHM));
        assert_eq!(&w.tx[0..4], &OBJ_REGISTRY.to_le_bytes(), "bind targets wl_registry");
        assert_eq!(&w.tx[8..12], &9u32.to_le_bytes(), "bind must use the discovered name");

        assert!(!w.bind_discovered("wl_seat", 1, 99), "a missing interface is not bound");
    }

    /// A compositor missing `wl_shm` fails discovery loudly (no fake present).
    #[test]
    fn missing_shm_is_a_missing_global() {
        let mut w = blank();
        w.rx.extend_from_slice(&global_event(1, "wl_compositor", 4));
        w.rx.extend_from_slice(&global_event(2, "xdg_wm_base", 1));
        w.dispatch_pending().unwrap();
        assert_eq!(w.discover_and_bind_after_sync(), Err(WlError::MissingGlobal));
    }

    /// The configure ack echoes the RECEIVED serial (not an invented `1`).
    #[test]
    fn ack_configure_echoes_received_serial() {
        let mut w = blank();
        let serial = 4242u32;
        let mut msg = Vec::new();
        msg.extend_from_slice(&OBJ_XDG_SURFACE.to_le_bytes());
        msg.extend_from_slice(&((12u32 << 16) | 0).to_le_bytes());
        msg.extend_from_slice(&serial.to_le_bytes());
        w.rx.extend_from_slice(&msg);
        w.dispatch_pending().unwrap();
        assert_eq!(w.configure_serial, Some(serial));
        w.wmsg(OBJ_XDG_SURFACE, 4, &[w.configure_serial.unwrap()]);
        let n = w.tx.len();
        assert_eq!(&w.tx[n - 4..n], &serial.to_le_bytes(), "ack_configure must echo the received serial");
    }

    /// `wl_display.error` is surfaced as a protocol failure, not swallowed.
    #[test]
    fn display_error_is_reported_as_protocol_failure() {
        let mut w = blank();
        let mut msg = Vec::new();
        msg.extend_from_slice(&OBJ_DISPLAY.to_le_bytes());
        msg.extend_from_slice(&((16u32 << 16) | 0).to_le_bytes());
        msg.extend_from_slice(&OBJ_WL_SURFACE.to_le_bytes());
        msg.extend_from_slice(&3u32.to_le_bytes());
        w.rx.extend_from_slice(&msg);
        assert_eq!(w.dispatch_pending(), Err(WlError::Protocol { object: OBJ_WL_SURFACE, code: 3 }));
    }

    /// `commit` on a not-ready session is an honest disconnect, never a fake success.
    #[test]
    fn commit_without_handshake_fails() {
        let mut w = blank();
        let g = Geometry::backing(2, 2);
        let px = vec![0u8; 2 * 2 * 4];
        assert_eq!(w.commit(&px, &g), Err(WlError::Disconnected));
    }

    #[test]
    fn geometry_full_size_is_not_sent() {
        let g = Geometry::backing(100, 100);
        assert!(!g.should_send());
        let g2 = Geometry { backing_w: 100, backing_h: 100, logical_w: 80, logical_h: 60, geom_x: 10, ..Default::default() };
        assert!(g2.should_send());
    }

    impl Wayland {
        /// Test helper: run only the bind half of `discover_and_bind` (globals already dispatched).
        fn discover_and_bind_after_sync(&mut self) -> WlResult<()> {
            if !self.bind_discovered("wl_compositor", 4, OBJ_COMPOSITOR)
                || !self.bind_discovered("wl_shm", 1, OBJ_SHM)
                || !self.bind_discovered("xdg_wm_base", 1, OBJ_XDG_WM_BASE)
            {
                return Err(WlError::MissingGlobal);
            }
            Ok(())
        }
    }
}
