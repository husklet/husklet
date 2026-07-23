use super::*;

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
pub(crate) struct SysWlAbi {
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
    pub(crate) fn load() -> WlAppResult<SysWlAbi> {
        let handle = unsafe { dlopen(c"libwayland-client.so.0".as_ptr(), RTLD_NOW | RTLD_NOLOAD) };
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

// SAFETY: every function pointer is resolved from the already-loaded libwayland-client, and callers
// must uphold WlAbi's live-proxy contract when constructing a presenter.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
unsafe impl WlAbi for SysWlAbi {
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
