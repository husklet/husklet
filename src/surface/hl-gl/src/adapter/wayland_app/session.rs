use super::*;

/// Presents a readback frame onto the app's OWN `wl_surface`, marshalling through the app's
/// `libwayland-client` on a private event queue.
pub struct WaylandAppPresenter {
    pub(super) abi: Box<dyn WlAbi>,
    pub(super) display: *mut c_void,
    pub(super) queue: *mut c_void,
    /// A wrapper of the app's `wl_surface` bound to OUR private queue (we marshal attach/commit on it).
    pub(super) surface_wrapper: *mut c_void,
    pub(super) surface_version: u32,
    /// The bound `wl_shm` (on our private queue).
    pub(super) shm: *mut c_void,
    pub(super) shm_version: u32,
    /// The previous frame's `wl_buffer`, destroyed when the next frame supersedes it (double-buffer safety).
    pub(super) last_buffer: *mut c_void,
}

// The presenter is only ever touched under the shim's process-global state mutex (one entry point at a
// time), so serialized access to the app's connection is safe even though the raw proxy pointers and the
// boxed ABI are not auto-`Send`.
unsafe impl Send for WaylandAppPresenter {}

impl WaylandAppPresenter {
    /// Bring up the presenter for the app's `wl_surface*` (`0` = not a wayland window) using the live
    /// `dlopen`/`dlsym` [`SysWlAbi`]. A missing library / symbol / global is a typed *soft* error so the
    /// caller falls back to the self-owned present.
    /// # Safety
    /// `surface_ptr` must identify a live `wl_surface` whose display connection and proxy remain valid
    /// until the returned presenter is dropped.
    pub unsafe fn new(surface_ptr: usize) -> WlAppResult<WaylandAppPresenter> {
        if surface_ptr == 0 {
            return Err(WlAppError::NoSurface);
        }
        let abi = SysWlAbi::load()?;
        Self::with_abi(Box::new(abi), surface_ptr as *mut c_void)
    }

    /// Bring up the presenter over an explicit ABI backend (the seam the recording tests drive).
    pub(crate) fn with_abi(
        abi: Box<dyn WlAbi>,
        surface: *mut c_void,
    ) -> WlAppResult<WaylandAppPresenter> {
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
}
