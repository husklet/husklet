/// A completion doorbell for the future shared-memory command ring (the eventfd/futex wake gfxstream
/// uses). The current socket path blocks on the ack instead; this is the forward seam so a ring-mode
/// transport can signal without re-inventing it.
pub struct Doorbell {
    fd: RawFd,
    /// Write end, only on platforms where the doorbell is a self-pipe (no eventfd).
    #[cfg(not(target_os = "linux"))]
    write_fd: RawFd,
}

impl Doorbell {
    /// Create a completion doorbell. On Linux this is a semaphore-mode eventfd
    /// (`EFD_CLOEXEC | EFD_SEMAPHORE`); elsewhere (macOS) a self-pipe, since eventfd/futex are
    /// Linux-only. Portable so the transport crate compiles on the host as well as the guest.
    pub fn new() -> io::Result<Self> {
        #[cfg(target_os = "linux")]
        {
            let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_SEMAPHORE) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Doorbell { fd })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let mut fds = [0 as RawFd; 2];
            if unsafe { libc::pipe(fds.as_mut_ptr()) } < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Doorbell {
                fd: fds[0],
                write_fd: fds[1],
            })
        }
    }
    pub fn raw_fd(&self) -> RawFd {
        self.fd
    }

    /// Wake up to `n` waiters parked on a live, aligned shared futex word.
    ///
    /// # Safety
    /// `addr` must point to a live, correctly aligned `u32` shared with the host for the syscall duration.
    #[cfg(target_os = "linux")]
    pub unsafe fn wake_futex(addr: *mut u32, n: i32) -> i64 {
        // SAFETY: the caller guarantees the futex word is live and aligned; syscall only observes its address.
        unsafe {
            libc::syscall(
                libc::SYS_futex,
                addr,
                libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG,
                n,
            ) as i64
        }
    }

    /// Off Linux the ring transport is unavailable, so waking its futex has no effect.
    ///
    /// # Safety
    /// Kept identical to the Linux signature so platform-neutral ring wiring can compile.
    #[cfg(not(target_os = "linux"))]
    pub unsafe fn wake_futex(_addr: *mut u32, _n: i32) -> i64 {
        0
    }
}

impl Drop for Doorbell {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
        #[cfg(not(target_os = "linux"))]
        unsafe {
            libc::close(self.write_fd)
        };
    }
}
use std::{io, os::fd::RawFd};
