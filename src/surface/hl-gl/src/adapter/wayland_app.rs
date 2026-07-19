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
pub trait WlAbi {
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
    /// `wl_display_roundtrip_queue(display, queue)` — dispatch pending events on OUR queue only.
    fn roundtrip_queue(&self, display: *mut c_void, queue: *mut c_void) -> i32;
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

/// Presents a readback frame onto the app's OWN `wl_surface`, marshalling through the app's
/// `libwayland-client` on a private event queue.
pub struct WaylandAppPresenter {
    abi: Box<dyn WlAbi>,
    display: *mut c_void,
    queue: *mut c_void,
    /// A wrapper of the app's `wl_surface` bound to OUR private queue (we marshal attach/commit on it).
    surface_wrapper: *mut c_void,
    surface_version: u32,
    /// The bound `wl_shm` (on our private queue).
    shm: *mut c_void,
    shm_version: u32,
    /// The previous frame's `wl_buffer`, destroyed when the next frame supersedes it (double-buffer safety).
    last_buffer: *mut c_void,
}

// The presenter is only ever touched under the shim's process-global state mutex (one entry point at a
// time), so serialized access to the app's connection is safe even though the raw proxy pointers and the
// boxed ABI are not auto-`Send`.
unsafe impl Send for WaylandAppPresenter {}

impl WaylandAppPresenter {
    /// Bring up the presenter for the app's `wl_surface*` (`0` = not a wayland window) using the live
    /// `dlopen`/`dlsym` [`SysWlAbi`]. A missing library / symbol / global is a typed *soft* error so the
    /// caller falls back to the self-owned present.
    pub fn new(surface_ptr: usize) -> WlAppResult<WaylandAppPresenter> {
        if surface_ptr == 0 {
            return Err(WlAppError::NoSurface);
        }
        let abi = SysWlAbi::load()?;
        Self::with_abi(Box::new(abi), surface_ptr as *mut c_void)
    }

    /// Bring up the presenter over an explicit ABI backend (the seam the recording tests drive).
    pub fn with_abi(abi: Box<dyn WlAbi>, surface: *mut c_void) -> WlAppResult<WaylandAppPresenter> {
        if surface.is_null() {
            return Err(WlAppError::NoSurface);
        }
        // Reach the app's connection through the surface proxy — NEVER open our own socket.
        let display = abi.get_display(surface);
        if display.is_null() {
            return Err(WlAppError::NoDisplay);
        }
        let surface_version = abi.get_version(surface);

        // Private event queue + a display wrapper on it, so the registry (and everything derived from it)
        // dispatches only to OUR queue.
        let queue = abi.create_queue(display);
        if queue.is_null() {
            return Err(WlAppError::QueueSetup);
        }
        let display_wrapper = abi.create_wrapper(display);
        if display_wrapper.is_null() {
            return Err(WlAppError::QueueSetup);
        }
        abi.set_queue(display_wrapper, queue);

        let registry = abi.get_registry(display_wrapper, abi.get_version(display));
        // The display wrapper is only needed to place get_registry on our queue.
        abi.wrapper_destroy(display_wrapper);
        if registry.is_null() {
            return Err(WlAppError::QueueSetup);
        }

        // Discover + bind wl_shm on our private queue.
        let shm_res = abi.discover_shm(registry, display, queue);
        let (shm_name, shm_version) = match shm_res {
            Some(nv) => nv,
            None => {
                abi.destroy(registry);
                return Err(WlAppError::NoShmGlobal);
            }
        };
        let shm = abi.bind_shm(registry, shm_name, shm_version);
        abi.destroy(registry); // no further registry events wanted
        if shm.is_null() {
            return Err(WlAppError::NoShmGlobal);
        }

        // A surface wrapper on our private queue: commits go to the app's surface object, but its events
        // (none from attach/commit) would land on our queue — we never disturb the app's own queue.
        let surface_wrapper = abi.create_wrapper(surface);
        if surface_wrapper.is_null() {
            abi.destroy(shm);
            return Err(WlAppError::QueueSetup);
        }
        abi.set_queue(surface_wrapper, queue);

        Ok(WaylandAppPresenter {
            abi,
            display,
            queue,
            surface_wrapper,
            surface_version,
            shm,
            shm_version,
            last_buffer: core::ptr::null_mut(),
        })
    }

    /// Commit one frame's `xrgb` plane (`WL_SHM_FORMAT_XRGB8888`, top-left, tight `w*h*4`) onto the app's
    /// `wl_surface`: wrap it in a fresh `wl_shm` pool+buffer, `attach`+`damage`+`commit`, then `flush`.
    /// Returns a typed *hard* error on any map/marshal/flush failure — never a silent present.
    pub fn present(&mut self, xrgb: &[u8], w: u32, h: u32) -> WlAppResult<()> {
        let (w, h) = (w.max(1), h.max(1));
        let stride = (w * 4) as i32;
        let size = (stride as usize) * h as usize;
        if xrgb.len() < size || !FramePlane::is_present(&xrgb[..size]) {
            return Err(WlAppError::BadSize);
        }
        // Reuse the SAME memfd allocator the self-owned present uses.
        let shm = ShmBuffer::new(&xrgb[..size]).map_err(|_| WlAppError::ShmAlloc)?;

        // Retire the previous frame's buffer (the compositor has since shown it) before superseding it.
        if !self.last_buffer.is_null() {
            self.abi.buffer_destroy(self.last_buffer, 1);
            self.last_buffer = core::ptr::null_mut();
        }

        let pool = self
            .abi
            .shm_create_pool(self.shm, self.shm_version, shm.fd, size as i32);
        if pool.is_null() {
            return Err(WlAppError::Marshal);
        }
        let buffer = self.abi.pool_create_buffer(
            pool,
            self.shm_version,
            w as i32,
            h as i32,
            stride,
            WL_SHM_FORMAT_XRGB8888,
        );
        // The pool is no longer needed once the buffer references the mapping.
        self.abi.pool_destroy(pool, self.shm_version);
        if buffer.is_null() {
            return Err(WlAppError::Marshal);
        }

        self.abi
            .surface_attach(self.surface_wrapper, self.surface_version, buffer);
        self.abi.surface_damage(
            self.surface_wrapper,
            self.surface_version,
            w as i32,
            h as i32,
        );
        self.abi
            .surface_commit(self.surface_wrapper, self.surface_version);
        if self.abi.flush(self.display) < 0 {
            return Err(WlAppError::Flush);
        }
        self.last_buffer = buffer;
        // `shm` (the memfd) drops here: the compositor mmap'd the pool at create_pool, so the client fd is
        // safely closed (the canonical wl_shm usage).
        Ok(())
    }
}

impl Drop for WaylandAppPresenter {
    fn drop(&mut self) {
        if !self.last_buffer.is_null() {
            self.abi.buffer_destroy(self.last_buffer, 1);
        }
        if !self.surface_wrapper.is_null() {
            self.abi.wrapper_destroy(self.surface_wrapper);
        }
        if !self.shm.is_null() {
            self.abi.destroy(self.shm);
        }
        let _ = (self.display, self.queue); // the display/queue belong to the app; we don't tear them down.
    }
}

// ==================================================================================================
// Live `libwayland-client` backend (dlopen RTLD_NOLOAD + dlsym)
// ==================================================================================================

const RTLD_NOW: c_int = 0x2;
const RTLD_NOLOAD: c_int = 0x4;

extern "C" {
    fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

/// The variadic `wl_proxy_marshal_flags` — the single marshalling primitive every request funnels through.
/// Trailing args are the request's wire arguments (a `NULL` placeholder for a constructed `new_id`).
type MarshalFlags =
    unsafe extern "C" fn(*mut c_void, u32, *const c_void, u32, u32, ...) -> *mut c_void;
type GetDisplayFn = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
type GetVersionFn = unsafe extern "C" fn(*mut c_void) -> u32;
type CreateQueueFn = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
type CreateWrapperFn = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
type WrapperDestroyFn = unsafe extern "C" fn(*mut c_void);
type SetQueueFn = unsafe extern "C" fn(*mut c_void, *mut c_void);
type DestroyFn = unsafe extern "C" fn(*mut c_void);
type RoundtripQueueFn = unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int;
type FlushFn = unsafe extern "C" fn(*mut c_void) -> c_int;
type AddListenerFn = unsafe extern "C" fn(*mut c_void, *const c_void, *mut c_void) -> c_int;

/// The resolved `libwayland-client` symbols + the exported `wl_interface` pointers we marshal against.
pub struct SysWlAbi {
    marshal: MarshalFlags,
    get_display: GetDisplayFn,
    get_version: GetVersionFn,
    create_queue: CreateQueueFn,
    create_wrapper: CreateWrapperFn,
    wrapper_destroy: WrapperDestroyFn,
    set_queue: SetQueueFn,
    destroy: DestroyFn,
    roundtrip_queue: RoundtripQueueFn,
    flush: FlushFn,
    add_listener: AddListenerFn,
    iface_registry: *const c_void,
    iface_shm: *const c_void,
    iface_shm_pool: *const c_void,
    iface_buffer: *const c_void,
}

/// The mutable data the registry listener writes the discovered `wl_shm` `(name, version)` into.
struct ShmDiscovery {
    name: Option<u32>,
    version: u32,
}

/// `wl_registry_listener.global` — record the `wl_shm` global's name+version (ignoring everything else).
extern "C" fn on_global(
    data: *mut c_void,
    _registry: *mut c_void,
    name: u32,
    interface: *const c_char,
    version: u32,
) {
    if data.is_null() || interface.is_null() {
        return;
    }
    let st = unsafe { &mut *(data as *mut ShmDiscovery) };
    // Compare the C string to "wl_shm" without pulling in std::ffi::CStr allocation semantics.
    let want = b"wl_shm";
    let mut ok = true;
    for (i, &wc) in want.iter().enumerate() {
        let c = unsafe { *interface.add(i) } as u8;
        if c != wc {
            ok = false;
            break;
        }
    }
    if ok && unsafe { *interface.add(want.len()) } == 0 {
        st.name = Some(name);
        st.version = version.max(1);
    }
}

/// `wl_registry_listener.global_remove` — no-op (we bind once at bring-up).
extern "C" fn on_global_remove(_data: *mut c_void, _registry: *mut c_void, _name: u32) {}

/// The `wl_registry_listener` vtable (`global`, `global_remove`) `wl_proxy_add_listener` stores.
#[repr(C)]
struct RegistryListener {
    global: extern "C" fn(*mut c_void, *mut c_void, u32, *const c_char, u32),
    global_remove: extern "C" fn(*mut c_void, *mut c_void, u32),
}

static REGISTRY_LISTENER: RegistryListener = RegistryListener {
    global: on_global,
    global_remove: on_global_remove,
};

impl SysWlAbi {
    /// `dlopen(RTLD_NOLOAD)` the already-mapped `libwayland-client.so.0` and `dlsym` the whole ABI. A
    /// missing library or symbol is a typed *soft* error (so the caller falls back to the self-owned
    /// present) — NEVER a faked-up backend.
    pub fn load() -> WlAppResult<SysWlAbi> {
        let handle = unsafe {
            dlopen(
                b"libwayland-client.so.0\0".as_ptr() as *const c_char,
                RTLD_NOW | RTLD_NOLOAD,
            )
        };
        if handle.is_null() {
            return Err(WlAppError::LibraryMissing);
        }
        // # Safety: each symbol is transmuted to its known `libwayland-client` prototype.
        unsafe {
            Ok(SysWlAbi {
                marshal: core::mem::transmute::<*mut c_void, MarshalFlags>(WaylandLibrary::symbol(
                    handle,
                    b"wl_proxy_marshal_flags\0",
                )?),
                get_display: core::mem::transmute::<*mut c_void, GetDisplayFn>(
                    WaylandLibrary::symbol(handle, b"wl_proxy_get_display\0")?,
                ),
                get_version: core::mem::transmute::<*mut c_void, GetVersionFn>(
                    WaylandLibrary::symbol(handle, b"wl_proxy_get_version\0")?,
                ),
                create_queue: core::mem::transmute::<*mut c_void, CreateQueueFn>(
                    WaylandLibrary::symbol(handle, b"wl_display_create_queue\0")?,
                ),
                create_wrapper: core::mem::transmute::<*mut c_void, CreateWrapperFn>(
                    WaylandLibrary::symbol(handle, b"wl_proxy_create_wrapper\0")?,
                ),
                wrapper_destroy: core::mem::transmute::<*mut c_void, WrapperDestroyFn>(
                    WaylandLibrary::symbol(handle, b"wl_proxy_wrapper_destroy\0")?,
                ),
                set_queue: core::mem::transmute::<*mut c_void, SetQueueFn>(WaylandLibrary::symbol(
                    handle,
                    b"wl_proxy_set_queue\0",
                )?),
                destroy: core::mem::transmute::<*mut c_void, DestroyFn>(WaylandLibrary::symbol(
                    handle,
                    b"wl_proxy_destroy\0",
                )?),
                roundtrip_queue: core::mem::transmute::<*mut c_void, RoundtripQueueFn>(
                    WaylandLibrary::symbol(handle, b"wl_display_roundtrip_queue\0")?,
                ),
                flush: core::mem::transmute::<*mut c_void, FlushFn>(WaylandLibrary::symbol(
                    handle,
                    b"wl_display_flush\0",
                )?),
                add_listener: core::mem::transmute::<*mut c_void, AddListenerFn>(
                    WaylandLibrary::symbol(handle, b"wl_proxy_add_listener\0")?,
                ),
                iface_registry: WaylandLibrary::symbol(handle, b"wl_registry_interface\0")?
                    as *const c_void,
                iface_shm: WaylandLibrary::symbol(handle, b"wl_shm_interface\0")? as *const c_void,
                iface_shm_pool: WaylandLibrary::symbol(handle, b"wl_shm_pool_interface\0")?
                    as *const c_void,
                iface_buffer: WaylandLibrary::symbol(handle, b"wl_buffer_interface\0")?
                    as *const c_void,
            })
        }
    }
}

/// `dlsym` a required symbol, mapping absence to [`WlAppError::SymbolMissing`] (never a null fn pointer).
struct WaylandLibrary;
impl WaylandLibrary {
    fn symbol(handle: *mut c_void, name: &'static [u8]) -> WlAppResult<*mut c_void> {
        let p = unsafe { dlsym(handle, name.as_ptr() as *const c_char) };
        if p.is_null() {
            // Strip the trailing NUL for the error label.
            let label = core::str::from_utf8(&name[..name.len() - 1]).unwrap_or("?");
            return Err(WlAppError::SymbolMissing(label));
        }
        Ok(p)
    }

    /// Read `interface->name` (the first pointer of a `struct wl_interface`) as a `*const c_char`.
    unsafe fn interface_name(iface: *const c_void) -> *const c_char {
        *(iface as *const *const c_char)
    }
}

impl WlAbi for SysWlAbi {
    fn get_display(&self, surface: *mut c_void) -> *mut c_void {
        unsafe { (self.get_display)(surface) }
    }
    fn get_version(&self, proxy: *mut c_void) -> u32 {
        unsafe { (self.get_version)(proxy) }
    }
    fn create_queue(&self, display: *mut c_void) -> *mut c_void {
        unsafe { (self.create_queue)(display) }
    }
    fn create_wrapper(&self, proxy: *mut c_void) -> *mut c_void {
        unsafe { (self.create_wrapper)(proxy) }
    }
    fn wrapper_destroy(&self, wrapper: *mut c_void) {
        unsafe { (self.wrapper_destroy)(wrapper) }
    }
    fn set_queue(&self, proxy: *mut c_void, queue: *mut c_void) {
        unsafe { (self.set_queue)(proxy, queue) }
    }
    fn destroy(&self, proxy: *mut c_void) {
        unsafe { (self.destroy)(proxy) }
    }
    fn roundtrip_queue(&self, display: *mut c_void, queue: *mut c_void) -> i32 {
        unsafe { (self.roundtrip_queue)(display, queue) }
    }
    fn flush(&self, display: *mut c_void) -> i32 {
        unsafe { (self.flush)(display) }
    }

    fn get_registry(&self, display_wrapper: *mut c_void, version: u32) -> *mut c_void {
        // wl_display.get_registry(new_id registry) — NULL placeholder for the constructed proxy.
        unsafe {
            (self.marshal)(
                display_wrapper,
                OP_DISPLAY_GET_REGISTRY,
                self.iface_registry,
                version,
                0,
                core::ptr::null::<c_void>(),
            )
        }
    }

    fn discover_shm(
        &self,
        registry: *mut c_void,
        display: *mut c_void,
        queue: *mut c_void,
    ) -> Option<(u32, u32)> {
        let mut st = ShmDiscovery {
            name: None,
            version: 1,
        };
        let rc = unsafe {
            (self.add_listener)(
                registry,
                &REGISTRY_LISTENER as *const RegistryListener as *const c_void,
                &mut st as *mut _ as *mut c_void,
            )
        };
        if rc < 0 {
            return None;
        }
        // One roundtrip on OUR queue delivers the initial global burst.
        if unsafe { (self.roundtrip_queue)(display, queue) } < 0 {
            return None;
        }
        st.name.map(|n| (n, st.version))
    }

    fn bind_shm(&self, registry: *mut c_void, name: u32, version: u32) -> *mut c_void {
        // wl_registry.bind(name, wl_shm, version): the generic new_id carries interface name + version.
        unsafe {
            let ifname = WaylandLibrary::interface_name(self.iface_shm);
            (self.marshal)(
                registry,
                OP_REGISTRY_BIND,
                self.iface_shm,
                version,
                0,
                name,
                ifname,
                version,
                core::ptr::null::<c_void>(),
            )
        }
    }

    fn shm_create_pool(&self, shm: *mut c_void, version: u32, fd: i32, size: i32) -> *mut c_void {
        // wl_shm.create_pool(new_id pool, fd, size) — the fd rides SCM_RIGHTS inside libwayland.
        unsafe {
            (self.marshal)(
                shm,
                OP_SHM_CREATE_POOL,
                self.iface_shm_pool,
                version,
                0,
                core::ptr::null::<c_void>(),
                fd,
                size,
            )
        }
    }

    fn pool_create_buffer(
        &self,
        pool: *mut c_void,
        version: u32,
        w: i32,
        h: i32,
        stride: i32,
        format: u32,
    ) -> *mut c_void {
        // wl_shm_pool.create_buffer(new_id buffer, offset=0, w, h, stride, format).
        unsafe {
            (self.marshal)(
                pool,
                OP_SHM_POOL_CREATE_BUFFER,
                self.iface_buffer,
                version,
                0,
                core::ptr::null::<c_void>(),
                0i32,
                w,
                h,
                stride,
                format,
            )
        }
    }

    fn pool_destroy(&self, pool: *mut c_void, version: u32) {
        unsafe {
            (self.marshal)(
                pool,
                OP_SHM_POOL_DESTROY,
                core::ptr::null::<c_void>(),
                version,
                WL_MARSHAL_FLAG_DESTROY,
            );
        }
    }

    fn buffer_destroy(&self, buffer: *mut c_void, version: u32) {
        unsafe {
            (self.marshal)(
                buffer,
                OP_BUFFER_DESTROY,
                core::ptr::null::<c_void>(),
                version,
                WL_MARSHAL_FLAG_DESTROY,
            );
        }
    }

    fn surface_attach(&self, surface: *mut c_void, version: u32, buffer: *mut c_void) {
        unsafe {
            (self.marshal)(
                surface,
                OP_SURFACE_ATTACH,
                core::ptr::null::<c_void>(),
                version,
                0,
                buffer,
                0i32,
                0i32,
            );
        }
    }

    fn surface_damage(&self, surface: *mut c_void, version: u32, w: i32, h: i32) {
        unsafe {
            (self.marshal)(
                surface,
                OP_SURFACE_DAMAGE,
                core::ptr::null::<c_void>(),
                version,
                0,
                0i32,
                0i32,
                w,
                h,
            );
        }
    }

    fn surface_commit(&self, surface: *mut c_void, version: u32) {
        unsafe {
            (self.marshal)(
                surface,
                OP_SURFACE_COMMIT,
                core::ptr::null::<c_void>(),
                version,
                0,
            );
        }
    }
}

// ==================================================================================================
// tests — a recording backend proves the request opcodes/args + wrapper wiring without a compositor
// ==================================================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// One recorded ABI interaction (the marshalled request or an infra op).
    #[derive(Debug, Clone, PartialEq)]
    enum Rec {
        GetDisplay(usize),
        CreateQueue(usize),
        CreateWrapper(usize),
        WrapperDestroy(usize),
        SetQueue(usize, usize),
        Destroy(usize),
        Flush(usize),
        GetRegistry {
            on: usize,
        },
        DiscoverShm {
            registry: usize,
        },
        BindShm {
            registry: usize,
            name: u32,
            version: u32,
        },
        ShmCreatePool {
            shm: usize,
            fd_valid: bool,
            size: i32,
        },
        PoolCreateBuffer {
            pool: usize,
            w: i32,
            h: i32,
            stride: i32,
            format: u32,
        },
        PoolDestroy(usize),
        BufferDestroy(usize),
        Attach {
            surface: usize,
            buffer: usize,
        },
        Damage {
            surface: usize,
            w: i32,
            h: i32,
        },
        Commit {
            surface: usize,
        },
    }

    /// A recording `WlAbi`: hands out fresh opaque pointer identities and logs every call so the request
    /// opcodes/args + private-queue wrapper wiring are assertable with no live compositor.
    struct Recorder {
        log: RefCell<Vec<Rec>>,
        next: RefCell<usize>,
        /// Whether `discover_shm` should report a wl_shm global (false models a compositor without shm).
        has_shm: bool,
        /// Force a constructor to return null (models a live marshal failure).
        fail_pool: bool,
    }

    impl Recorder {
        fn new() -> Self {
            Recorder {
                log: RefCell::new(Vec::new()),
                next: RefCell::new(0x1000),
                has_shm: true,
                fail_pool: false,
            }
        }
        fn fresh(&self) -> *mut c_void {
            let mut n = self.next.borrow_mut();
            *n += 0x10;
            *n as *mut c_void
        }
        fn push(&self, r: Rec) {
            self.log.borrow_mut().push(r);
        }
        fn log(&self) -> Vec<Rec> {
            self.log.borrow().clone()
        }
    }

    impl WlAbi for Recorder {
        fn get_display(&self, surface: *mut c_void) -> *mut c_void {
            self.push(Rec::GetDisplay(surface as usize));
            0xD15_9000usize as *mut c_void // a fixed non-null "app display"
        }
        fn get_version(&self, _proxy: *mut c_void) -> u32 {
            4
        }
        fn create_queue(&self, display: *mut c_void) -> *mut c_void {
            self.push(Rec::CreateQueue(display as usize));
            0x0_9EE_0usize as *mut c_void // a fixed non-null "private queue"
        }
        fn create_wrapper(&self, proxy: *mut c_void) -> *mut c_void {
            self.push(Rec::CreateWrapper(proxy as usize));
            self.fresh()
        }
        fn wrapper_destroy(&self, wrapper: *mut c_void) {
            self.push(Rec::WrapperDestroy(wrapper as usize));
        }
        fn set_queue(&self, proxy: *mut c_void, queue: *mut c_void) {
            self.push(Rec::SetQueue(proxy as usize, queue as usize));
        }
        fn destroy(&self, proxy: *mut c_void) {
            self.push(Rec::Destroy(proxy as usize));
        }
        fn roundtrip_queue(&self, _display: *mut c_void, _queue: *mut c_void) -> i32 {
            0
        }
        fn flush(&self, display: *mut c_void) -> i32 {
            self.push(Rec::Flush(display as usize));
            0
        }
        fn get_registry(&self, display_wrapper: *mut c_void, _version: u32) -> *mut c_void {
            self.push(Rec::GetRegistry {
                on: display_wrapper as usize,
            });
            self.fresh()
        }
        fn discover_shm(
            &self,
            registry: *mut c_void,
            _display: *mut c_void,
            _queue: *mut c_void,
        ) -> Option<(u32, u32)> {
            self.push(Rec::DiscoverShm {
                registry: registry as usize,
            });
            if self.has_shm {
                Some((7, 1))
            } else {
                None
            }
        }
        fn bind_shm(&self, registry: *mut c_void, name: u32, version: u32) -> *mut c_void {
            self.push(Rec::BindShm {
                registry: registry as usize,
                name,
                version,
            });
            self.fresh()
        }
        fn shm_create_pool(
            &self,
            shm: *mut c_void,
            _version: u32,
            fd: i32,
            size: i32,
        ) -> *mut c_void {
            self.push(Rec::ShmCreatePool {
                shm: shm as usize,
                fd_valid: fd >= 0,
                size,
            });
            if self.fail_pool {
                core::ptr::null_mut()
            } else {
                self.fresh()
            }
        }
        fn pool_create_buffer(
            &self,
            pool: *mut c_void,
            _version: u32,
            w: i32,
            h: i32,
            stride: i32,
            format: u32,
        ) -> *mut c_void {
            self.push(Rec::PoolCreateBuffer {
                pool: pool as usize,
                w,
                h,
                stride,
                format,
            });
            self.fresh()
        }
        fn pool_destroy(&self, pool: *mut c_void, _version: u32) {
            self.push(Rec::PoolDestroy(pool as usize));
        }
        fn buffer_destroy(&self, buffer: *mut c_void, _version: u32) {
            self.push(Rec::BufferDestroy(buffer as usize));
        }
        fn surface_attach(&self, surface: *mut c_void, _version: u32, buffer: *mut c_void) {
            self.push(Rec::Attach {
                surface: surface as usize,
                buffer: buffer as usize,
            });
        }
        fn surface_damage(&self, surface: *mut c_void, _version: u32, w: i32, h: i32) {
            self.push(Rec::Damage {
                surface: surface as usize,
                w,
                h,
            });
        }
        fn surface_commit(&self, surface: *mut c_void, _version: u32) {
            self.push(Rec::Commit {
                surface: surface as usize,
            });
        }
    }

    const SURFACE: *mut c_void = 0xA9900usize as *mut c_void;

    fn xrgb(w: usize, h: usize) -> Vec<u8> {
        // A non-blank plane (opaque white) so `rgba_is_present` passes.
        vec![0xFFu8; w * h * 4]
    }

    /// Bring-up derives the display FROM the surface proxy (no socket), creates a private queue, and binds
    /// wl_shm off the app registry with the DISCOVERED name — the whole isolation contract in one trace.
    #[test]
    fn bringup_derives_display_and_binds_shm_on_private_queue() {
        let rec = Box::new(Recorder::new());
        let p = WaylandAppPresenter::with_abi(rec, SURFACE).expect("bring-up");
        assert_eq!(p.shm_version, 1);
        let log = unsafe { &*(std::ptr::addr_of!(*p.abi) as *const Recorder) }.log();

        // 1) The display is derived from the app's surface proxy — proving no own socket is opened.
        assert_eq!(log[0], Rec::GetDisplay(SURFACE as usize));
        // 2) A private queue is created off that app display.
        assert_eq!(log[1], Rec::CreateQueue(0xD15_9000));
        // 3) A display wrapper is created + pinned to the private queue, and get_registry runs on it.
        assert!(matches!(log[2], Rec::CreateWrapper(0xD15_9000)));
        assert!(matches!(log[3], Rec::SetQueue(_, 0x0_9EE_0)));
        assert!(matches!(log[4], Rec::GetRegistry { .. }));
        // 4) wl_shm is discovered then bound with the DISCOVERED registry name (7) at the discovered version.
        assert!(log.iter().any(|r| matches!(r, Rec::DiscoverShm { .. })));
        assert!(log.iter().any(|r| matches!(
            r,
            Rec::BindShm {
                name: 7,
                version: 1,
                ..
            }
        )));
        // 5) The app surface is wrapped + pinned to the private queue (never disturbing the app's queue).
        assert!(log
            .iter()
            .any(|r| matches!(r, Rec::CreateWrapper(a) if *a == SURFACE as usize)));
        assert!(log.iter().any(|r| matches!(r, Rec::SetQueue(_, 0x0_9EE_0))));
    }

    /// A present marshals pool → buffer → attach → damage → commit → flush with the right args, onto the
    /// surface WRAPPER (private queue), and passes a valid shm fd.
    #[test]
    fn present_marshals_pool_buffer_attach_damage_commit_flush() {
        let rec = Box::new(Recorder::new());
        let mut p = WaylandAppPresenter::with_abi(rec, SURFACE).expect("bring-up");
        let surface_wrapper = p.surface_wrapper as usize;
        // Clear the bring-up trace to focus on the frame.
        unsafe { &*(std::ptr::addr_of!(*p.abi) as *const Recorder) }
            .log
            .borrow_mut()
            .clear();

        p.present(&xrgb(4, 3), 4, 3).expect("present");
        let log = unsafe { &*(std::ptr::addr_of!(*p.abi) as *const Recorder) }.log();

        // create_pool with a real fd + full byte size (4*3*4 = 48).
        let pool_rec = log.iter().find_map(|r| match r {
            Rec::ShmCreatePool {
                shm: _,
                fd_valid,
                size,
            } => Some((*fd_valid, *size)),
            _ => None,
        });
        assert_eq!(pool_rec, Some((true, 48)));
        // create_buffer at 4x3, stride 16, XRGB8888.
        assert!(log.iter().any(|r| matches!(
            r,
            Rec::PoolCreateBuffer {
                w: 4,
                h: 3,
                stride: 16,
                format: 1,
                ..
            }
        )));
        // The pool is destroyed once the buffer holds the mapping.
        assert!(log.iter().any(|r| matches!(r, Rec::PoolDestroy(_))));
        // attach/damage/commit all target the SURFACE WRAPPER (the private-queue proxy), not the raw surface.
        assert!(log
            .iter()
            .any(|r| matches!(r, Rec::Attach { surface, .. } if *surface == surface_wrapper)));
        assert!(log.iter().any(
            |r| matches!(r, Rec::Damage { surface, w: 4, h: 3 } if *surface == surface_wrapper)
        ));
        assert!(log
            .iter()
            .any(|r| matches!(r, Rec::Commit { surface } if *surface == surface_wrapper)));
        assert_ne!(
            surface_wrapper, SURFACE as usize,
            "commit must go via a wrapper, not the app's raw surface"
        );
        // A flush ends the frame.
        assert!(matches!(log.last(), Some(Rec::Flush(_))));
    }

    /// Frame N+1 retires frame N's buffer before superseding it (double-buffer safety, no unbounded leak).
    #[test]
    fn second_frame_retires_the_previous_buffer() {
        let rec = Box::new(Recorder::new());
        let mut p = WaylandAppPresenter::with_abi(rec, SURFACE).expect("bring-up");
        p.present(&xrgb(2, 2), 2, 2).expect("frame 1");
        unsafe { &*(std::ptr::addr_of!(*p.abi) as *const Recorder) }
            .log
            .borrow_mut()
            .clear();
        p.present(&xrgb(2, 2), 2, 2).expect("frame 2");
        let log = unsafe { &*(std::ptr::addr_of!(*p.abi) as *const Recorder) }.log();
        // The very first op of frame 2 destroys frame 1's buffer.
        assert!(
            matches!(log.first(), Some(Rec::BufferDestroy(_))),
            "frame 2 must retire the prior buffer first"
        );
    }

    /// A compositor without wl_shm fails bring-up LOUDLY (soft error → caller falls back), never fakes it.
    #[test]
    fn missing_shm_global_is_a_soft_error() {
        let mut rec = Recorder::new();
        rec.has_shm = false;
        let err = WaylandAppPresenter::with_abi(Box::new(rec), SURFACE)
            .err()
            .unwrap();
        assert_eq!(err, WlAppError::NoShmGlobal);
        assert!(
            err.is_unavailable(),
            "a missing global must be a soft (fall-back) failure"
        );
    }

    /// A null surface pointer (not a wayland app) is a soft NoSurface — the caller keeps its self-owned path.
    #[test]
    fn null_surface_is_soft_no_surface() {
        let rec = Box::new(Recorder::new());
        let err = WaylandAppPresenter::with_abi(rec, core::ptr::null_mut())
            .err()
            .unwrap();
        assert_eq!(err, WlAppError::NoSurface);
        assert!(err.is_unavailable());
    }

    /// A too-small readback plane is a hard BadSize (never a silent/blank present).
    #[test]
    fn short_plane_is_hard_bad_size() {
        let rec = Box::new(Recorder::new());
        let mut p = WaylandAppPresenter::with_abi(rec, SURFACE).expect("bring-up");
        let err = p.present(&[0u8; 4], 4, 4).unwrap_err();
        assert_eq!(err, WlAppError::BadSize);
        assert!(
            !err.is_unavailable(),
            "a live present failure is hard (EGL_CONTEXT_LOST), not a fall-back"
        );
    }

    /// A constructor returning null (a live marshal failure) is a hard Marshal error.
    #[test]
    fn null_constructor_is_hard_marshal_error() {
        let mut rec = Recorder::new();
        rec.fail_pool = true;
        let mut p = WaylandAppPresenter::with_abi(Box::new(rec), SURFACE).expect("bring-up");
        let err = p.present(&xrgb(2, 2), 2, 2).unwrap_err();
        assert_eq!(err, WlAppError::Marshal);
        assert!(!err.is_unavailable());
    }

    /// The soft/hard classification is the exact contract the caller keys its fall-back vs CONTEXT_LOST on.
    #[test]
    fn error_softness_classification() {
        for e in [
            WlAppError::NoSurface,
            WlAppError::LibraryMissing,
            WlAppError::SymbolMissing("wl_proxy_marshal_flags"),
            WlAppError::NoDisplay,
            WlAppError::QueueSetup,
            WlAppError::NoShmGlobal,
        ] {
            assert!(e.is_unavailable(), "{e:?} must be soft");
        }
        for e in [
            WlAppError::BadSize,
            WlAppError::ShmAlloc,
            WlAppError::Marshal,
            WlAppError::Flush,
        ] {
            assert!(!e.is_unavailable(), "{e:?} must be hard");
        }
    }

    /// The live `dlopen(RTLD_NOLOAD)` load path: with `libwayland-client` NOT mapped into this test
    /// process, `SysWlAbi::load()` returns a typed soft error (never a null-fn backend, never a fake up).
    #[test]
    fn sys_abi_load_without_libwayland_is_a_soft_error() {
        // The test harness does not link/load libwayland-client, so RTLD_NOLOAD must miss.
        match SysWlAbi::load() {
            Err(e) => assert!(
                e.is_unavailable(),
                "absent libwayland must be a soft error, got {e:?}"
            ),
            Ok(_) => { /* if a host happens to have it mapped, the load simply succeeded — also valid. */
            }
        }
    }
}
