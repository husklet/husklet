use super::abi::SysWlAbi;
use super::present::{FramePlane, WL_SHM_FORMAT_XRGB8888};
use super::shared_memory::ShmBuffer;
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
    pub(super) identity: *mut c_void,
    pub(super) identity_version: u32,
    pub(super) token: Option<SurfaceToken>,
    pub(super) next_serial: u64,
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
    pub fn native_token(&self) -> Option<SurfaceToken> {
        self.token
    }

    /// Bring up the presenter for the app's `wl_surface*` (`0` = not a wayland window) using the live
    /// `dlopen`/`dlsym` [`SysWlAbi`]. A missing library / symbol / global is a typed *soft* error so the
    /// caller keeps the readback-only present.
    /// `display_ptr` is the app's OWN `wl_display*` as captured in `VkWaylandSurfaceCreateInfoKHR`
    /// (`0` = unknown, fall back to `wl_proxy_get_display`, which only exists on Wayland 1.23+).
    /// # Safety
    /// `surface_ptr` must identify a live `wl_surface` whose display connection and proxy remain valid
    /// until the returned presenter is dropped.
    pub unsafe fn new(surface_ptr: usize, display_ptr: usize) -> WlAppResult<WaylandAppPresenter> {
        if surface_ptr == 0 {
            return Err(WlAppError::NoSurface);
        }
        let abi = SysWlAbi::load()?;
        Self::with_abi(
            Box::new(abi),
            surface_ptr as *mut c_void,
            display_ptr as *mut c_void,
        )
    }

    pub fn reserve_native_frame(&mut self) -> Option<NativeFrame> {
        let token = self.token?;
        let serial = self.next_serial;
        self.next_serial = self.next_serial.checked_add(1)?;
        Some(NativeFrame {
            token,
            serial: FrameSerial::new(serial).ok()?,
        })
    }

    /// Associate a successfully submitted native GPU frame with the next Wayland commit.
    pub fn commit_native(&mut self, frame: NativeFrame, w: u32, h: u32) -> WlAppResult<()> {
        if self.token != Some(frame.token) || self.identity.is_null() {
            return Err(WlAppError::NoIdentity);
        }
        self.abi
            .identity_associate(self.identity, self.identity_version, frame.serial.get());
        self.abi.surface_damage(
            self.surface_wrapper,
            self.surface_version,
            w.max(1) as i32,
            h.max(1) as i32,
        );
        self.abi
            .surface_commit(self.surface_wrapper, self.surface_version);
        if self.abi.flush(self.display) < 0 {
            return Err(WlAppError::Flush);
        }
        Ok(())
    }

    /// Retire only native pairing; the SHM compatibility path remains available.
    pub fn retire_native(&mut self) {
        if !self.identity.is_null() {
            self.abi
                .identity_destroy(self.identity, self.identity_version);
            self.identity = core::ptr::null_mut();
            self.identity_version = 0;
            self.token = None;
        }
    }

    /// Bring up the presenter over an explicit ABI backend (the seam the recording tests drive).
    pub(crate) fn with_abi(
        abi: Box<dyn WlAbi>,
        surface: *mut c_void,
        display: *mut c_void,
    ) -> WlAppResult<WaylandAppPresenter> {
        if surface.is_null() {
            return Err(WlAppError::NoSurface);
        }
        // Use the app's OWN connection — NEVER open our own socket. It is normally handed to us
        // (`VkWaylandSurfaceCreateInfoKHR.display`); only if it was not do we derive it from the surface
        // proxy via `wl_proxy_get_display` (Wayland 1.23+, absent on 24.04-era guests).
        let display = if display.is_null() {
            abi.get_display(surface)
        } else {
            display
        };
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
            abi.destroy_queue(queue);
            return Err(WlAppError::QueueSetup);
        }
        abi.set_queue(display_wrapper, queue);

        let registry = abi.get_registry(display_wrapper, abi.get_version(display));
        // The display wrapper is only needed to place get_registry on our queue.
        abi.wrapper_destroy(display_wrapper);
        if registry.is_null() {
            abi.destroy_queue(queue);
            return Err(WlAppError::QueueSetup);
        }

        // A surface wrapper on our private queue: commits go to the app's surface object, but its events
        // (none from attach/commit) would land on our queue — we never disturb the app's own queue.
        let surface_wrapper = abi.create_wrapper(surface);
        if surface_wrapper.is_null() {
            abi.destroy(registry);
            abi.destroy_queue(queue);
            return Err(WlAppError::QueueSetup);
        }
        abi.set_queue(surface_wrapper, queue);

        let globals = match abi.discover_globals(registry, display, queue) {
            Ok(globals) => globals,
            Err(error) => {
                abi.wrapper_destroy(surface_wrapper);
                abi.destroy(registry);
                abi.destroy_queue(queue);
                return Err(error);
            }
        };
        let (shm, shm_version) = globals
            .shm
            .map(|(name, version)| (abi.bind_shm(registry, name, version), version))
            .unwrap_or((core::ptr::null_mut(), 0));
        let (identity, identity_version, token) = if let Some((name, version)) = globals.identity {
            let manager = abi.bind_identity_manager(registry, name, version.min(1));
            if manager.is_null() {
                (core::ptr::null_mut(), 0, None)
            } else {
                let identity = abi.identity_for_surface(manager, version.min(1), surface_wrapper);
                abi.destroy(manager);
                if identity.is_null() {
                    (core::ptr::null_mut(), 0, None)
                } else {
                    match abi.identity_token(identity, display, queue) {
                        Ok(token) => (identity, version.min(1), Some(token)),
                        Err(_) => {
                            abi.identity_destroy(identity, version.min(1));
                            (core::ptr::null_mut(), 0, None)
                        }
                    }
                }
            }
        } else {
            (core::ptr::null_mut(), 0, None)
        };
        abi.destroy(registry);
        if shm.is_null() && token.is_none() {
            abi.wrapper_destroy(surface_wrapper);
            abi.destroy_queue(queue);
            return Err(WlAppError::NoShmGlobal);
        }

        Ok(WaylandAppPresenter {
            abi,
            display,
            queue,
            surface_wrapper,
            surface_version,
            identity,
            identity_version,
            token,
            next_serial: 1,
            shm,
            shm_version,
            last_buffer: core::ptr::null_mut(),
        })
    }

    /// Commit one frame's `xrgb` plane (`WL_SHM_FORMAT_XRGB8888`, top-left, tight `w*h*4`) onto the app's
    /// `wl_surface`: wrap it in a fresh `wl_shm` pool+buffer, `attach`+`damage`+`commit`, then `flush`.
    /// Returns a typed *hard* error on any map/marshal/flush failure — never a silent present.
    pub fn present(&mut self, xrgb: &[u8], w: u32, h: u32) -> WlAppResult<()> {
        if self.shm.is_null() {
            return Err(WlAppError::NoShmGlobal);
        }
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
        if !self.identity.is_null() {
            self.retire_native();
        }
        if !self.surface_wrapper.is_null() {
            self.abi.wrapper_destroy(self.surface_wrapper);
        }
        if !self.shm.is_null() {
            self.abi.destroy(self.shm);
        }
        if !self.queue.is_null() {
            self.abi.destroy_queue(self.queue);
        }
    }
}
