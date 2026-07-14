//! Foreign-connection wayland/dma-buf present: commit the host-rendered swapchain IOSurface to the
//! app's OWN `wl_surface` on the app's OWN `wl_display` — the piece that lands a windowed Vulkan app
//! (vkcube) on hl-display.
//!
//! ## The rendezvous (oracle: `hl-tests/guests/gl_shim.c` `wl_commit`, ~l.1615)
//! hl-display (`server.rs`) composites a dma-buf whose `modifier_hi & 0xffff == HL_DMABUF_MOD_MAGIC`
//! (`0x6464`) and `modifier_lo == <hl surface id>` by pulling the IOSurface that the GPU-exec channel
//! already rendered into (our `ExecConn` `Cmd::Present` in `vkQueuePresentKHR`). So present is:
//!   `zwp_linux_dmabuf_v1.create_params` → `params.add(fd, plane=0, offset=0, stride, mod_hi=0x6464,
//!    mod_lo=surface.id)` → `params.create_immed(w,h,XRGB8888)` → `wl_surface.attach(buffer)` →
//!   `wl_surface.damage` → `wl_surface.frame(cb)` → `wl_surface.commit`.
//!
//! gl_shim.c marshals that raw over ITS OWN socket (it owns the connection). A Vulkan ICD instead gets
//! the app's live `wl_display`/`wl_surface` at `vkCreateWaylandSurfaceKHR`, so we must speak the SAME
//! sequence through the app's `libwayland-client` via `wl_proxy_marshal_flags` (with hand-embedded
//! `zwp_linux_dmabuf_v1`/`zwp_linux_buffer_params_v1` interface tables — the ICD links libwayland but
//! not the protocol bindings, exactly as gl_shim.c hand-rolls them). libwayland is `dlopen`ed so the
//! guest build needs no wayland dev libs; because the app already loaded `libwayland-client.so.0`, the
//! `dlopen` resolves to the SAME instance, so our `wl_proxy` calls act on the app's real proxies.

use hl_shim::transport::{Surface, HL_DMABUF_MOD_MAGIC};
use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_void};
use std::sync::OnceLock;

// ---- libwayland-client ABI (opaque proxies + the wl_interface/wl_message tables) -----------------

#[repr(C)]
struct WlMessage {
    name: *const c_char,
    signature: *const c_char,
    types: *const *const WlInterface,
}
#[repr(C)]
struct WlInterface {
    name: *const c_char,
    version: c_int,
    method_count: c_int,
    methods: *const WlMessage,
    event_count: c_int,
    events: *const WlMessage,
}
unsafe impl Sync for WlInterface {}
unsafe impl Sync for WlMessage {}

const WL_MARSHAL_FLAG_DESTROY: u32 = 1;

// Opcodes (from the wayland / linux-dmabuf protocol).
const WL_DISPLAY_GET_REGISTRY: u32 = 1;
const WL_REGISTRY_BIND: u32 = 0;
const DMABUF_CREATE_PARAMS: u32 = 1;
const PARAMS_DESTROY: u32 = 0;
const PARAMS_ADD: u32 = 1;
const PARAMS_CREATE_IMMED: u32 = 3;
const SURFACE_ATTACH: u32 = 1;
const SURFACE_DAMAGE: u32 = 2;
const SURFACE_FRAME: u32 = 3;
const SURFACE_COMMIT: u32 = 6;
const WL_BUFFER_DESTROY: u32 = 0;

const DRM_FMT_XRGB8888: u32 = 0x3432_5258;

// dlsym'd libwayland-client entry points (variadic marshal is called through a fn pointer).
struct Wl {
    marshal_flags: unsafe extern "C" fn(
        *mut c_void,
        u32,
        *const WlInterface,
        u32,
        u32,
        ...
    ) -> *mut c_void,
    proxy_get_version: unsafe extern "C" fn(*mut c_void) -> u32,
    proxy_add_listener:
        unsafe extern "C" fn(*mut c_void, *mut *mut c_void, *mut c_void) -> c_int,
    display_roundtrip: unsafe extern "C" fn(*mut c_void) -> c_int,
    display_flush: unsafe extern "C" fn(*mut c_void) -> c_int,
    // libwayland's own interface tables (referenced by our create_immed / frame / bind marshals).
    wl_buffer_interface: *const WlInterface,
    wl_callback_interface: *const WlInterface,
    wl_registry_interface: *const WlInterface,
}
unsafe impl Sync for Wl {}
unsafe impl Send for Wl {}

fn wl() -> Option<&'static Wl> {
    static WL: OnceLock<Option<Wl>> = OnceLock::new();
    WL.get_or_init(load_wl).as_ref()
}

fn load_wl() -> Option<Wl> {
    unsafe {
        // The app already loaded it; RTLD_NOLOAD would also work, but a plain dlopen resolves to the
        // same in-process instance (libwayland is not statically linked into the app).
        let names = [c"libwayland-client.so.0".as_ptr(), c"libwayland-client.so".as_ptr()];
        let mut h = core::ptr::null_mut();
        for n in names {
            h = libc_dlopen(n, 2 /* RTLD_NOW */);
            if !h.is_null() {
                break;
            }
        }
        if h.is_null() {
            return None;
        }
        macro_rules! sym {
            ($n:literal) => {{
                let p = libc_dlsym(h, concat!($n, "\0").as_ptr() as *const c_char);
                if p.is_null() {
                    return None;
                }
                p
            }};
        }
        Some(Wl {
            marshal_flags: core::mem::transmute(sym!("wl_proxy_marshal_flags")),
            proxy_get_version: core::mem::transmute(sym!("wl_proxy_get_version")),
            proxy_add_listener: core::mem::transmute(sym!("wl_proxy_add_listener")),
            display_roundtrip: core::mem::transmute(sym!("wl_display_roundtrip")),
            display_flush: core::mem::transmute(sym!("wl_display_flush")),
            wl_buffer_interface: sym!("wl_buffer_interface") as *const WlInterface,
            wl_callback_interface: sym!("wl_callback_interface") as *const WlInterface,
            wl_registry_interface: sym!("wl_registry_interface") as *const WlInterface,
        })
    }
}

extern "C" {
    #[link_name = "dlopen"]
    fn libc_dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    #[link_name = "dlsym"]
    fn libc_dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

// ---- hand-embedded interface tables for the two linux-dmabuf protocol objects --------------------
// (Only the requests we marshal need correct opcodes/signatures; opcodes are indices, so the request
//  lists must be complete up to the ones used — mirroring gl_shim.c's hand-rolled protocol.)

fn dmabuf_iface() -> *const WlInterface {
    static NAME: &[u8] = b"zwp_linux_dmabuf_v1\0";
    static DESTROY_N: &[u8] = b"destroy\0";
    static DESTROY_S: &[u8] = b"\0";
    static CREATE_PARAMS_N: &[u8] = b"create_params\0";
    static CREATE_PARAMS_S: &[u8] = b"n\0";
    static TABLE: OnceLock<usize> = OnceLock::new();
    let p = *TABLE.get_or_init(|| {
        // create_params' new-id is a zwp_linux_buffer_params_v1.
        let params_types: &'static [*const WlInterface] = Box::leak(Box::new([params_iface()]));
        let methods: &'static [WlMessage] = Box::leak(Box::new([
            WlMessage { name: DESTROY_N.as_ptr() as _, signature: DESTROY_S.as_ptr() as _, types: core::ptr::null() },
            WlMessage { name: CREATE_PARAMS_N.as_ptr() as _, signature: CREATE_PARAMS_S.as_ptr() as _, types: params_types.as_ptr() },
        ]));
        let iface: &'static WlInterface = Box::leak(Box::new(WlInterface {
            name: NAME.as_ptr() as _,
            version: 3,
            method_count: methods.len() as c_int,
            methods: methods.as_ptr(),
            event_count: 0,
            events: core::ptr::null(),
        }));
        iface as *const WlInterface as usize
    });
    p as *const WlInterface
}

fn params_iface() -> *const WlInterface {
    static NAME: &[u8] = b"zwp_linux_buffer_params_v1\0";
    static DESTROY_N: &[u8] = b"destroy\0";
    static EMPTY_S: &[u8] = b"\0";
    static ADD_N: &[u8] = b"add\0";
    static ADD_S: &[u8] = b"huuuuu\0";
    static CREATE_N: &[u8] = b"create\0";
    static CREATE_S: &[u8] = b"iiuu\0";
    static CREATE_IMMED_N: &[u8] = b"create_immed\0";
    static CREATE_IMMED_S: &[u8] = b"niiuu\0";
    static TABLE: OnceLock<usize> = OnceLock::new();
    let p = *TABLE.get_or_init(|| {
        // create_immed's new-id is a wl_buffer (libwayland's own interface).
        let immed_types: &'static [*const WlInterface] = Box::leak(Box::new([
            wl().map(|w| w.wl_buffer_interface).unwrap_or(core::ptr::null()),
            core::ptr::null(),
            core::ptr::null(),
            core::ptr::null(),
            core::ptr::null(),
        ]));
        let none6: &'static [*const WlInterface] = Box::leak(Box::new([core::ptr::null(); 6]));
        let none4: &'static [*const WlInterface] = Box::leak(Box::new([core::ptr::null(); 4]));
        let methods: &'static [WlMessage] = Box::leak(Box::new([
            WlMessage { name: DESTROY_N.as_ptr() as _, signature: EMPTY_S.as_ptr() as _, types: core::ptr::null() },
            WlMessage { name: ADD_N.as_ptr() as _, signature: ADD_S.as_ptr() as _, types: none6.as_ptr() },
            WlMessage { name: CREATE_N.as_ptr() as _, signature: CREATE_S.as_ptr() as _, types: none4.as_ptr() },
            WlMessage { name: CREATE_IMMED_N.as_ptr() as _, signature: CREATE_IMMED_S.as_ptr() as _, types: immed_types.as_ptr() },
        ]));
        let iface: &'static WlInterface = Box::leak(Box::new(WlInterface {
            name: NAME.as_ptr() as _,
            version: 3,
            method_count: methods.len() as c_int,
            methods: methods.as_ptr(),
            event_count: 0,
            events: core::ptr::null(),
        }));
        iface as *const WlInterface as usize
    });
    p as *const WlInterface
}

// ---- registry bind: catch the zwp_linux_dmabuf_v1 global, bind it once ---------------------------

static DMABUF_PROXY: OnceLock<usize> = OnceLock::new();

extern "C" fn on_global(
    _data: *mut c_void,
    registry: *mut c_void,
    name: u32,
    interface: *const c_char,
    version: u32,
) {
    if interface.is_null() {
        return;
    }
    let ifc = unsafe { core::ffi::CStr::from_ptr(interface) };
    if std::env::var_os("HL_SHIM_DEBUG").is_some() {
        eprintln!("[hl-shim-vk] wl global: name={name} iface={:?} v{version}", ifc);
    }
    if ifc.to_bytes() != b"zwp_linux_dmabuf_v1" {
        return;
    }
    let Some(w) = wl() else { return };
    let bind_ver = version.min(3);
    let target = dmabuf_iface();
    let iname = c"zwp_linux_dmabuf_v1";
    // wl_registry.bind(name, interface->name, version, new_id) — the generic-new-id "sun" form.
    let proxy = unsafe {
        (w.marshal_flags)(
            registry,
            WL_REGISTRY_BIND,
            target,
            bind_ver,
            0,
            name,
            iname.as_ptr(),
            bind_ver,
            core::ptr::null::<c_void>(),
        )
    };
    if !proxy.is_null() {
        let _ = DMABUF_PROXY.set(proxy as usize);
    }
}

fn ensure_dmabuf(w: &Wl, display: *mut c_void) -> Option<*mut c_void> {
    if let Some(p) = DMABUF_PROXY.get() {
        return Some(*p as *mut c_void);
    }
    unsafe {
        // registry = wl_display.get_registry()
        let registry = (w.marshal_flags)(
            display,
            WL_DISPLAY_GET_REGISTRY,
            w.wl_registry_interface,
            (w.proxy_get_version)(display),
            0,
            core::ptr::null::<c_void>(),
        );
        let dbg = std::env::var_os("HL_SHIM_DEBUG").is_some();
        if registry.is_null() {
            if dbg {
                eprintln!("[hl-shim-vk] ensure_dmabuf: get_registry returned NULL");
            }
            return None;
        }
        // listener = { global, global_remove }
        static LISTENER: OnceLock<[usize; 2]> = OnceLock::new();
        let l = LISTENER.get_or_init(|| [on_global as usize, on_global_remove as usize]);
        let added = (w.proxy_add_listener)(registry, l.as_ptr() as *mut *mut c_void, core::ptr::null_mut());
        let rt = (w.display_roundtrip)(display); // deliver the globals + our bind
        if dbg {
            eprintln!("[hl-shim-vk] ensure_dmabuf: add_listener={added} roundtrip={rt} bound={}", DMABUF_PROXY.get().is_some());
        }
    }
    DMABUF_PROXY.get().map(|p| *p as *mut c_void)
}

extern "C" fn on_global_remove(_data: *mut c_void, _registry: *mut c_void, _name: u32) {}

// ---- the present itself --------------------------------------------------------------------------

/// Commit the executor-rendered IOSurface `surf` (its dma-buf fd + hl surface id) to the app's
/// `wl_surface` on the app's `wl_display`. Returns false if wayland/dmabuf is unavailable (off-guest).
pub fn present(display: usize, surface: usize, surf: &Surface) -> bool {
    let dbg = std::env::var_os("HL_SHIM_DEBUG").is_some();
    if display == 0 || surface == 0 || surf.fd < 0 {
        if dbg {
            eprintln!("[hl-shim-vk] wl_present: bad args display={display:#x} surface={surface:#x} fd={}", surf.fd);
        }
        return false;
    }
    let Some(w) = wl() else {
        if dbg {
            eprintln!("[hl-shim-vk] wl_present: libwayland-client dlopen/dlsym FAILED");
        }
        return false;
    };
    let display = display as *mut c_void;
    let wl_surface = surface as *mut c_void;
    let Some(dmabuf) = ensure_dmabuf(w, display) else {
        if dbg {
            eprintln!("[hl-shim-vk] wl_present: zwp_linux_dmabuf_v1 global NOT bound");
        }
        return false;
    };
    let ver = unsafe { (w.proxy_get_version)(dmabuf) };
    let nullp = core::ptr::null::<c_void>();
    unsafe {
        // params = zwp_linux_dmabuf_v1.create_params()  — the 'n' new-id takes a NULL va_list
        // placeholder (as libwayland's generated stubs pass), else every following arg shifts by one.
        let params = (w.marshal_flags)(dmabuf, DMABUF_CREATE_PARAMS, params_iface(), ver, 0, nullp);
        if params.is_null() {
            return false;
        }
        // params.add(fd, plane_idx=0, offset=0, stride, modifier_hi=magic|generation, modifier_lo=id).
        // The generation (modifier_hi bits 17..=31) lets the compositor reject a stale reference whose
        // id was retired and reissued; 0 == unversioned (see transport::Surface::generation).
        let modifier_hi = HL_DMABUF_MOD_MAGIC | ((surf.generation & 0x7fff) << 17);
        (w.marshal_flags)(
            params,
            PARAMS_ADD,
            core::ptr::null(),
            ver,
            0,
            surf.fd as c_int,
            0u32,
            0u32,
            surf.stride,
            modifier_hi,
            surf.id,
        );
        // buffer = params.create_immed(width, height, DRM_FORMAT_XRGB8888, flags=0) — NULL placeholder
        // for the wl_buffer 'n' new-id, then the i,i,u,u args.
        let buffer = (w.marshal_flags)(
            params,
            PARAMS_CREATE_IMMED,
            w.wl_buffer_interface,
            ver,
            0,
            nullp,
            surf.width as c_int,
            surf.height as c_int,
            DRM_FMT_XRGB8888,
            0u32,
        );
        // wl_surface.attach(buffer, 0, 0); damage; frame(cb); commit
        let sver = (w.proxy_get_version)(wl_surface);
        (w.marshal_flags)(wl_surface, SURFACE_ATTACH, core::ptr::null(), sver, 0, buffer, 0i32, 0i32);
        (w.marshal_flags)(
            wl_surface,
            SURFACE_DAMAGE,
            core::ptr::null(),
            sver,
            0,
            0i32,
            0i32,
            surf.width as c_int,
            surf.height as c_int,
        );
        // frame(callback) — NULL placeholder for the wl_callback 'n' new-id.
        let _cb = (w.marshal_flags)(wl_surface, SURFACE_FRAME, w.wl_callback_interface, sver, 0, nullp);
        (w.marshal_flags)(wl_surface, SURFACE_COMMIT, core::ptr::null(), sver, 0);
        // Release the per-frame protocol objects (the compositor already holds the dma-buf import).
        (w.marshal_flags)(params, PARAMS_DESTROY, core::ptr::null(), ver, WL_MARSHAL_FLAG_DESTROY);
        (w.marshal_flags)(buffer, WL_BUFFER_DESTROY, core::ptr::null(), ver, WL_MARSHAL_FLAG_DESTROY);
        (w.display_flush)(display);
    }
    true
}

// Keep CString referenced (used only in some build configs) without warnings.
const _: fn() = || {
    let _ = CString::new("");
};
