//! Present a rendered frame into the app's OWN `wl_surface` — the surface a real wayland-egl app created
//! on its OWN `libwayland-client` connection.
//!
//! This is the milestone that turns the shim from "headless readback into a shim-owned toplevel"
//! ([`super::wayland::Wayland`]) into "a real app window": the frame that `eglSwapBuffers` reads back is
//! marshalled as a `wl_shm` `wl_buffer` and `attach`/`damage`/`commit`ed onto the app's `wl_surface`
//! (captured at `eglCreateWindowSurface`). No socket is opened here — the app already owns the connection;
//! we reach it through the surface proxy and drive it via the app's already-mapped `libwayland-client`.
//!
//! ## Why this cannot use a raw socket
//! `libwayland-client` owns the app's connection: the send buffer, the object-id space, the fd-passing
//! ring. A second raw socket (what [`super::wayland::Wayland`] does for its self-owned toplevel) cannot
//! address the app's `wl_surface` (a different id space). So we must marshal through the app's OWN
//! `libwayland-client` — `dlopen(RTLD_NOLOAD)` the already-loaded copy, `dlsym` the proxy/queue ABI, and
//! `wl_proxy_marshal_flags` our requests (the Mesa EGL-Wayland pattern).
//!
//! ## Queue isolation (mandatory)
//! The shim MUST NOT reenter the app's own event listeners. So it creates a PRIVATE `wl_event_queue`
//! (`wl_display_create_queue`) and wraps every proxy it creates/uses with `wl_proxy_create_wrapper` +
//! `wl_proxy_set_queue` so their events dispatch to OUR queue only. We never `roundtrip` the app's default
//! queue and never install a competing frame callback on the app's surface — only `attach`+`commit`+`flush`.
//!
//! ## Honesty
//! Every fallible step returns a typed [`WlAppError`]; a missing library / symbol / global yields a typed
//! error so the caller falls back to the self-owned present ([`super::wayland::Wayland`]) — never a faked
//! success. A live marshal/flush failure is a real error the caller surfaces as `EGL_CONTEXT_LOST`.
//!
//! ## Testability
//! The `libwayland-client` ABI is behind the [`WlAbi`] trait. The live [`SysWlAbi`] is `dlopen`/`dlsym`;
//! a recording backend (in tests) captures every marshalled request so the opcode/arg layout + the
//! private-queue wrapper wiring + the dlsym-fallback path are unit-testable WITHOUT a live compositor.

use core::ffi::{c_char, c_int, c_void};

use super::wayland::ShmBuffer;

/// Whether the readback plane carries a real frame (not the all-zero fill
/// [`super::wayland::rgba_to_xrgb8888`] returns when the readback was too short). A valid convert always
/// stamps the `X` byte `0xFF`, so a genuine frame is never all-zero — this rejects a failed readback
/// rather than committing a blank buffer.
struct FramePlane;
impl FramePlane {
    fn is_present(xrgb: &[u8]) -> bool {
        xrgb.iter().any(|&b| b != 0)
    }
}

/// `WL_SHM_FORMAT_XRGB8888` — the byte order [`super::wayland::rgba_to_xrgb8888`] packs into.
const WL_SHM_FORMAT_XRGB8888: u32 = 1;

/// `WL_MARSHAL_FLAG_DESTROY` — passed to `wl_proxy_marshal_flags` for destructor requests (the proxy is
/// freed as part of the marshal).
const WL_MARSHAL_FLAG_DESTROY: u32 = 1;

// ---- wire opcodes (from wayland.xml; stable across versions) ----
const OP_DISPLAY_GET_REGISTRY: u32 = 1;
const OP_REGISTRY_BIND: u32 = 0;
const OP_SHM_CREATE_POOL: u32 = 0;
const OP_SHM_POOL_CREATE_BUFFER: u32 = 0;
const OP_SHM_POOL_DESTROY: u32 = 1;
const OP_BUFFER_DESTROY: u32 = 0;
const OP_SURFACE_ATTACH: u32 = 1;
const OP_SURFACE_DAMAGE: u32 = 2;
const OP_SURFACE_COMMIT: u32 = 6;

/// A typed outcome for the fallible app-surface present. A library/symbol/global gap is a *soft* failure
/// (the caller falls back to the self-owned present); a live marshal/flush gap is a *hard* failure the
/// caller surfaces as `EGL_CONTEXT_LOST`. Never a fake present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WlAppError {
    /// No app `wl_surface*` was captured (not a wayland window) — soft.
    NoSurface,
    /// `libwayland-client.so.0` is not already mapped in this process (`RTLD_NOLOAD` miss) — soft.
    LibraryMissing,
    /// A required proxy/queue/interface symbol was absent from `libwayland-client` — soft.
    SymbolMissing(&'static str),
    /// `wl_proxy_get_display` on the app surface returned null — soft.
    NoDisplay,
    /// `wl_display_create_queue` / `wl_proxy_create_wrapper` returned null — soft.
    QueueSetup,
    /// The compositor never advertised `wl_shm` on the app's registry — soft.
    NoShmGlobal,
    /// The readback plane was smaller than `w*h*4` — hard.
    BadSize,
    /// Allocating / mapping the shm memfd failed — hard.
    ShmAlloc,
    /// A `wl_proxy_marshal_flags` constructor returned null — hard.
    Marshal,
    /// `wl_display_flush` reported a socket error — hard.
    Flush,
}

impl WlAppError {
    /// Whether this is a *soft* failure (the presenter is simply unavailable and the caller should fall
    /// back to the self-owned present) vs a *hard* live-present failure (surfaced as `EGL_CONTEXT_LOST`).
    pub fn is_unavailable(&self) -> bool {
        matches!(
            self,
            WlAppError::NoSurface
                | WlAppError::LibraryMissing
                | WlAppError::SymbolMissing(_)
                | WlAppError::NoDisplay
                | WlAppError::QueueSetup
                | WlAppError::NoShmGlobal
        )
    }
}

pub type WlAppResult<T> = Result<T, WlAppError>;

/// The `libwayland-client` ABI surface the presenter needs, as a testable seam. The live [`SysWlAbi`]
/// forwards to the `dlsym`'d functions; a recording backend (tests) captures every call.
///
/// Pointers are the app's real `wl_proxy*` / `wl_display*` / `wl_event_queue*` (opaque here). Requests
/// that construct an object return the new proxy (null on failure); void requests return nothing.
///
/// # Safety
/// Implementations must dereference only live Wayland proxies supplied by the presenter or returned by
/// an earlier successful ABI operation, and must preserve libwayland's proxy ownership rules.
pub(crate) unsafe trait WlAbi {
    /// `wl_proxy_get_display(surface)` — the app's `wl_display*` behind its `wl_surface*`.
    fn get_display(&self, surface: *mut c_void) -> *mut c_void;
    /// `wl_proxy_get_version(proxy)`.
    fn get_version(&self, proxy: *mut c_void) -> u32;
    /// `wl_display_create_queue(display)` — a PRIVATE event queue so shim events never reenter the app.
    fn create_queue(&self, display: *mut c_void) -> *mut c_void;
    /// `wl_proxy_create_wrapper(proxy)` — a wrapper whose queue we set without disturbing the original.
    fn create_wrapper(&self, proxy: *mut c_void) -> *mut c_void;
    /// `wl_proxy_wrapper_destroy(wrapper)`.
    fn wrapper_destroy(&self, wrapper: *mut c_void);
    /// `wl_proxy_set_queue(proxy, queue)` — route this proxy's events to our private queue.
    fn set_queue(&self, proxy: *mut c_void, queue: *mut c_void);
    /// `wl_proxy_destroy(proxy)`.
    fn destroy(&self, proxy: *mut c_void);
    /// `wl_display_flush(display)` — push queued requests to the compositor.
    fn flush(&self, display: *mut c_void) -> i32;

    /// `wl_display.get_registry` off `display_wrapper` (so the registry lands on our private queue).
    fn get_registry(&self, display_wrapper: *mut c_void, version: u32) -> *mut c_void;
    /// Add the registry listener + `roundtrip_queue` to discover the `wl_shm` global `(name, version)`.
    fn discover_shm(
        &self,
        registry: *mut c_void,
        display: *mut c_void,
        queue: *mut c_void,
    ) -> Option<(u32, u32)>;
    /// `wl_registry.bind(name, wl_shm, version)` → the bound `wl_shm` proxy (on our private queue).
    fn bind_shm(&self, registry: *mut c_void, name: u32, version: u32) -> *mut c_void;

    /// `wl_shm.create_pool(fd, size)` → a `wl_shm_pool` proxy.
    fn shm_create_pool(&self, shm: *mut c_void, version: u32, fd: i32, size: i32) -> *mut c_void;
    /// `wl_shm_pool.create_buffer(0, w, h, stride, format)` → a `wl_buffer` proxy.
    fn pool_create_buffer(
        &self,
        pool: *mut c_void,
        version: u32,
        w: i32,
        h: i32,
        stride: i32,
        format: u32,
    ) -> *mut c_void;
    /// `wl_shm_pool.destroy()` (destructor).
    fn pool_destroy(&self, pool: *mut c_void, version: u32);
    /// `wl_buffer.destroy()` (destructor).
    fn buffer_destroy(&self, buffer: *mut c_void, version: u32);
    /// `wl_surface.attach(buffer, 0, 0)`.
    fn surface_attach(&self, surface: *mut c_void, version: u32, buffer: *mut c_void);
    /// `wl_surface.damage(0, 0, w, h)`.
    fn surface_damage(&self, surface: *mut c_void, version: u32, w: i32, h: i32);
    /// `wl_surface.commit()`.
    fn surface_commit(&self, surface: *mut c_void, version: u32);
}

mod present;
mod session;
mod sys;

#[cfg(test)]
mod tests;

pub use session::WaylandAppPresenter;
pub(crate) use sys::SysWlAbi;
