use core::ffi::{c_int, c_void};
use std::os::fd::IntoRawFd;

// ==================================================================================================
// anonymous-file-backed shm plane
// ==================================================================================================

extern "C" {
    fn close(fd: c_int) -> c_int;
    fn mmap(
        addr: *mut c_void,
        len: usize,
        prot: c_int,
        flags: c_int,
        fd: c_int,
        off: i64,
    ) -> *mut c_void;
    fn munmap(addr: *mut c_void, len: usize) -> c_int;
}

const PROT_READ: c_int = 1;
const PROT_WRITE: c_int = 2;
const MAP_SHARED: c_int = 1;
const MAP_FAILED: isize = -1;

/// An anonymous-file-backed shared-memory buffer whose bytes are the pixel plane the compositor maps.
pub(super) struct ShmBuffer {
    pub(super) fd: c_int,
}

impl ShmBuffer {
    /// Allocate an anonymous file, map it, copy `pixels` in, then unmap (the fd retains the contents).
    pub(super) fn new(pixels: &[u8]) -> Result<ShmBuffer, ()> {
        let len = pixels.len();
        let fd = hl_fs::AnonymousFile::new(
            &std::env::temp_dir(),
            "vulkan-wayland-shm",
            len as u64,
        )
            .map_err(|_| ())?
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
            return Err(());
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
