use super::*;

impl WaylandAppPresenter {
    pub fn reserve_native_frame(&mut self) -> Option<NativeFrame> {
        let token = self.token?;
        let serial = self.next_serial;
        self.next_serial = self.next_serial.checked_add(1)?;
        Some(NativeFrame {
            token,
            serial: FrameSerial::new(serial).ok()?,
        })
    }

    /// Associate a successfully submitted native GPU frame with the next surface commit.
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

    /// Retire only native pairing after a failed association. The same app-surface presenter remains
    /// available for its SHM compatibility path.
    pub fn retire_native(&mut self) {
        if !self.identity.is_null() {
            self.abi
                .identity_destroy(self.identity, self.identity_version);
            self.identity = core::ptr::null_mut();
            self.identity_version = 0;
            self.token = None;
        }
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
        let _ = self.display;
    }
}
