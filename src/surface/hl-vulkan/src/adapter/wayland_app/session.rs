use core::ffi::c_void;

use super::abi::{SysWlAbi, WlAbi};
use super::present::{FramePlane, WlAppError, WlAppResult, WL_SHM_FORMAT_XRGB8888};
use super::shared_memory::ShmBuffer;

/// Presents a readback frame onto the app's OWN `wl_surface`, marshalling through the app's
/// `libwayland-client` on a private event queue.
pub struct WaylandAppPresenter {
    pub(super) abi: Box<dyn WlAbi>,
    display: *mut c_void,
    queue: *mut c_void,
    /// A wrapper of the app's `wl_surface` bound to OUR private queue (we marshal attach/commit on it).
    pub(super) surface_wrapper: *mut c_void,
    surface_version: u32,
    /// The bound `wl_shm` (on our private queue).
    shm: *mut c_void,
    pub(super) shm_version: u32,
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
    /// caller keeps the readback-only present.
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
