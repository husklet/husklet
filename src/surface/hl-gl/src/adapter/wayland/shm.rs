use super::*;

/// An anonymous-file-backed shared-memory buffer whose bytes are the pixel plane the compositor maps read-only.
///
/// `pub(crate)` so the app-surface presenter ([`super::wayland_app`]) reuses the SAME allocator to
/// back the `wl_shm` pool it marshals onto the app's own `libwayland-client` connection.
pub(crate) struct ShmBuffer {
    pub(crate) fd: c_int,
}

impl ShmBuffer {
    /// Allocate an anonymous file, map it, copy `pixels` in, then unmap (the fd retains the contents).
    pub(crate) fn new(pixels: &[u8]) -> WlResult<ShmBuffer> {
        let len = pixels.len();
        let fd = hl_fs::AnonymousFile::new(&std::env::temp_dir(), "wayland-shm", len as u64)
            .map_err(|_| WlError::ShmAlloc)?
            .into_file()
            .into_raw_fd();
        let map = unsafe {
            mmap(
                core::ptr::null_mut(),
                len,
                PROT_READ | PROT_WRITE,
                MAP_SHARED,
                fd,
                0,
            )
        };
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
