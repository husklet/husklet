use super::*;
use std::cell::RefCell;
use std::collections::HashMap;

// ==================================================================================================
// Live `libwayland-client` backend (dlopen RTLD_NOLOAD + dlsym)
// ==================================================================================================

const RTLD_NOW: c_int = 0x2;
const RTLD_NOLOAD: c_int = 0x4;

extern "C" {
    fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlclose(handle: *mut c_void) -> c_int;
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
type DestroyQueueFn = unsafe extern "C" fn(*mut c_void);
type RoundtripQueueFn = unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int;
type FlushFn = unsafe extern "C" fn(*mut c_void) -> c_int;
type AddListenerFn = unsafe extern "C" fn(*mut c_void, *const c_void, *mut c_void) -> c_int;

/// The resolved `libwayland-client` symbols + the exported `wl_interface` pointers we marshal against.
pub(crate) struct SysWlAbi {
    marshal: MarshalFlags,
    /// `wl_proxy_get_display` — Wayland **1.23+** only. Optional: the caller can supply the app's own
    /// `wl_display*` instead. Ubuntu 24.04 / Debian 12 ship libwayland 1.22 and do NOT export it.
    get_display: Option<GetDisplayFn>,
    get_version: GetVersionFn,
    create_queue: CreateQueueFn,
    create_wrapper: CreateWrapperFn,
    wrapper_destroy: WrapperDestroyFn,
    set_queue: SetQueueFn,
    destroy: DestroyFn,
    destroy_queue: DestroyQueueFn,
    roundtrip_queue: RoundtripQueueFn,
    flush: FlushFn,
    add_listener: AddListenerFn,
    iface_registry: *const c_void,
    iface_shm: *const c_void,
    iface_shm_pool: *const c_void,
    iface_buffer: *const c_void,
    protocol: hl_surface_protocol::raw::ClientInterfaces,
    token_listeners: RefCell<HashMap<usize, Box<Token>>>,
    _library: WaylandLibraryHandle,
}

struct WaylandLibraryHandle(*mut c_void);

impl Drop for WaylandLibraryHandle {
    fn drop(&mut self) {
        // SAFETY: this is the successful dlopen handle retained by SysWlAbi.
        unsafe {
            dlclose(self.0);
        }
    }
}

/// `wl_registry_listener.global` — record the globals this adapter can consume.
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
    let st = unsafe { &mut *(data as *mut Globals) };
    let matches = |want: &[u8]| {
        want.iter()
            .enumerate()
            .all(|(index, want)| unsafe { *interface.add(index) as u8 == *want })
            && unsafe { *interface.add(want.len()) == 0 }
    };
    if matches(b"wl_shm") {
        st.shm = Some((name, version.max(1)));
    } else if matches(b"hl_surface_manager_v1") {
        st.identity = Some((name, version.min(1)));
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

#[derive(Default)]
struct Token {
    value: Option<u64>,
}

extern "C" fn on_token(data: *mut c_void, _identity: *mut c_void, high: u32, low: u32) {
    if let Some(token) = unsafe { (data as *mut Token).as_mut() } {
        token.value = Some((u64::from(high) << 32) | u64::from(low));
    }
}

#[repr(C)]
struct IdentityListener {
    token: extern "C" fn(*mut c_void, *mut c_void, u32, u32),
}

static IDENTITY_LISTENER: IdentityListener = IdentityListener { token: on_token };

impl SysWlAbi {
    /// `dlopen(RTLD_NOLOAD)` the already-mapped `libwayland-client.so.0` and `dlsym` the whole ABI. A
    /// missing library or symbol is a typed *soft* error (so the caller falls back to the self-owned
    /// present) — NEVER a faked-up backend.
    pub(crate) fn load() -> WlAppResult<SysWlAbi> {
        let handle = unsafe { dlopen(c"libwayland-client.so.0".as_ptr(), RTLD_NOW | RTLD_NOLOAD) };
        if handle.is_null() {
            return Err(WlAppError::LibraryMissing);
        }
        let library = WaylandLibraryHandle(handle);
        // # Safety: each symbol is transmuted to its known `libwayland-client` prototype.
        unsafe {
            let iface_surface =
                WaylandLibrary::symbol(handle, b"wl_surface_interface\0")? as *const c_void;
            // SAFETY: the `_library` field retains this exact handle until after `protocol` drops.
            let protocol = hl_surface_protocol::raw::ClientInterfaces::new(iface_surface)
                .ok_or(WlAppError::SymbolMissing("wl_surface_interface"))?;
            Ok(SysWlAbi {
                marshal: core::mem::transmute::<*mut c_void, MarshalFlags>(WaylandLibrary::symbol(
                    handle,
                    b"wl_proxy_marshal_flags\0",
                )?),
                // Optional (Wayland 1.23+): absence is NOT a failure — the display can be threaded in.
                get_display: WaylandLibrary::optional_symbol(handle, b"wl_proxy_get_display\0")
                    .map(|p| core::mem::transmute::<*mut c_void, GetDisplayFn>(p)),
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
                destroy_queue: core::mem::transmute::<*mut c_void, DestroyQueueFn>(
                    WaylandLibrary::symbol(handle, b"wl_event_queue_destroy\0")?,
                ),
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
                protocol,
                token_listeners: RefCell::new(HashMap::new()),
                _library: library,
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

    /// `dlsym` a symbol that may legitimately be absent on older `libwayland-client` releases.
    fn optional_symbol(handle: *mut c_void, name: &'static [u8]) -> Option<*mut c_void> {
        let p = unsafe { dlsym(handle, name.as_ptr() as *const c_char) };
        (!p.is_null()).then_some(p)
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
        match self.get_display {
            Some(f) => unsafe { f(surface) },
            None => core::ptr::null_mut(),
        }
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
    fn destroy_queue(&self, queue: *mut c_void) {
        unsafe { (self.destroy_queue)(queue) }
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

    fn discover_globals(
        &self,
        registry: *mut c_void,
        display: *mut c_void,
        queue: *mut c_void,
    ) -> WlAppResult<Globals> {
        let mut st = Globals::default();
        let rc = unsafe {
            (self.add_listener)(
                registry,
                &REGISTRY_LISTENER as *const RegistryListener as *const c_void,
                &mut st as *mut _ as *mut c_void,
            )
        };
        if rc < 0 {
            return Err(WlAppError::Marshal);
        }
        // One roundtrip on OUR queue delivers the initial global burst.
        if unsafe { (self.roundtrip_queue)(display, queue) } < 0 {
            return Err(WlAppError::Flush);
        }
        Ok(st)
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

    fn bind_identity_manager(&self, registry: *mut c_void, name: u32, version: u32) -> *mut c_void {
        unsafe {
            let interface = self.protocol.manager();
            let ifname = WaylandLibrary::interface_name(interface);
            (self.marshal)(
                registry,
                OP_REGISTRY_BIND,
                interface,
                version,
                0,
                name,
                ifname,
                version,
                core::ptr::null::<c_void>(),
            )
        }
    }

    fn identity_for_surface(
        &self,
        manager: *mut c_void,
        version: u32,
        surface: *mut c_void,
    ) -> *mut c_void {
        unsafe {
            (self.marshal)(
                manager,
                0,
                self.protocol.identity(),
                version,
                0,
                core::ptr::null::<c_void>(),
                surface,
            )
        }
    }

    fn identity_token(
        &self,
        identity: *mut c_void,
        display: *mut c_void,
        queue: *mut c_void,
    ) -> WlAppResult<SurfaceToken> {
        let mut token = Box::new(Token::default());
        let result = unsafe {
            (self.add_listener)(
                identity,
                std::ptr::from_ref(&IDENTITY_LISTENER).cast(),
                std::ptr::from_mut(token.as_mut()).cast(),
            )
        };
        if result < 0 {
            return Err(WlAppError::Marshal);
        }
        self.token_listeners
            .borrow_mut()
            .insert(identity as usize, token);
        if unsafe { (self.roundtrip_queue)(display, queue) } < 0 {
            return Err(WlAppError::Flush);
        }
        let value = self
            .token_listeners
            .borrow()
            .get(&(identity as usize))
            .and_then(|token| token.value)
            .unwrap_or(0);
        SurfaceToken::new(value).map_err(|_| WlAppError::NoIdentity)
    }

    fn identity_associate(&self, identity: *mut c_void, version: u32, serial: u64) {
        unsafe {
            (self.marshal)(
                identity,
                0,
                core::ptr::null::<c_void>(),
                version,
                0,
                (serial >> 32) as u32,
                serial as u32,
            );
        }
    }

    fn identity_destroy(&self, identity: *mut c_void, version: u32) {
        unsafe {
            (self.marshal)(
                identity,
                1,
                core::ptr::null::<c_void>(),
                version,
                WL_MARSHAL_FLAG_DESTROY,
            );
        }
        self.token_listeners
            .borrow_mut()
            .remove(&(identity as usize));
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
