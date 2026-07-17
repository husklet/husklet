#![deny(unsafe_op_in_unsafe_fn)]

use std::{
    cell::Cell,
    mem,
    num::NonZeroUsize,
    os::unix::io::{AsFd, AsRawFd, BorrowedFd, OwnedFd},
    ptr,
    sync::{
        mpsc::{channel, Sender},
        LazyLock, OnceLock, RwLock,
    },
    thread,
};

use rustix::mm;
use tracing::{debug, instrument, trace};

// Dropping Pool is actually pretty slow. Unmapping the memory can take 1-2 ms, but the real
// offender is closing the file descriptor, which I've seen take up to 6 ms. It's waiting on some
// spinlock in the kernel.
//
// Blocking the main thread for 6 ms is quite bad. In fact, 6 ms is almost the entire time budget
// for a 165 Hz frame. To make matters worse, some clients will cause repeated creation and
// dropping of shm pools, like Firefox during a focus-out animation. This results in dropped
// frames.
//
// To work around this problem, we spawn a separate thread whose sole purpose is dropping stuff we
// send it through a channel. Conveniently, Pool is already Send, so there's no problem doing this.
static DROP_THIS: LazyLock<Sender<InnerPool>> = LazyLock::new(|| {
    let (tx, rx) = channel();
    thread::Builder::new()
        .name("Shm dropping thread".to_owned())
        .spawn(move || {
            while let Ok(x) = rx.recv() {
                profiling::scope!("dropping Pool");
                drop(x);
            }
        })
        .unwrap();
    tx
});

thread_local!(static SIGBUS_GUARD: Cell<(*const MemMap, bool)> = const { Cell::new((ptr::null_mut(), false)) });

static OLD_SIGBUS_HANDLER: OnceLock<libc::sigaction> = OnceLock::new();

pub struct Pool {
    inner: Option<InnerPool>,
}

impl std::fmt::Debug for Pool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pool")
            .field("size", &self.size())
            .finish_non_exhaustive()
    }
}

struct InnerPool {
    map: RwLock<MemMap>,
    fd: OwnedFd,
    quota: Option<Box<dyn ShmPoolQuota>>,
}

/// Owner-bound accounting token retained for the lifetime of a mapped shm pool.
pub trait ShmPoolQuota: Send + Sync {
    /// Transactionally replace this pool's charged byte count.
    fn resize(&self, new_size: usize) -> bool;
}

// SAFETY: The memmap is owned by the pool and content is only accessible via a reference.
unsafe impl Send for InnerPool {}
// SAFETY: The memmap is guarded by a RwLock, meaning no writers may mutate the memmap when it is being read.
unsafe impl Sync for InnerPool {}

pub enum ResizeError {
    InvalidSize,
    BackingTooSmall,
    BudgetExceeded,
    MremapFailed,
}

impl InnerPool {
    #[instrument(level = "trace", skip_all, name = "wayland_shm")]
    pub fn new(fd: OwnedFd, size: NonZeroUsize, quota: Box<dyn ShmPoolQuota>) -> Result<InnerPool, OwnedFd> {
        if !fd_supports_mapping(fd.as_fd(), size.into()) {
            return Err(fd);
        }
        let memmap = match MemMap::new(fd.as_fd(), size) {
            Ok(memmap) => memmap,
            Err(_) => {
                return Err(fd);
            }
        };
        trace!(fd = ?fd, size = ?size, "Creating new shm pool");
        Ok(InnerPool {
            map: RwLock::new(memmap),
            fd,
            quota: Some(quota),
        })
    }

    pub fn resize(&self, newsize: NonZeroUsize) -> Result<(), ResizeError> {
        let mut guard = self.map.write().unwrap();
        let oldsize = guard.size();

        if oldsize > usize::from(newsize) {
            return Err(ResizeError::InvalidSize);
        }
        if oldsize == usize::from(newsize) {
            return Ok(());
        }
        if !fd_supports_mapping(self.fd.as_fd(), newsize.into()) {
            return Err(ResizeError::BackingTooSmall);
        }
        if !self.quota.as_ref().unwrap().resize(newsize.into()) {
            return Err(ResizeError::BudgetExceeded);
        }

        trace!(fd = ?self.fd, oldsize = oldsize, newsize = ?newsize, "Resizing shm pool");
        guard.remap(self.fd.as_fd(), newsize).map_err(|()| {
            let _ = self.quota.as_ref().unwrap().resize(oldsize);
            debug!(fd = ?self.fd, oldsize = oldsize, newsize = ?newsize, "SHM pool resize failed");
            ResizeError::MremapFailed
        })
    }

    pub fn size(&self) -> usize {
        self.map.read().unwrap().size
    }

    #[instrument(level = "trace", skip_all, name = "wayland_shm")]
    pub fn with_data<T, F: FnOnce(*const u8, usize) -> T>(&self, f: F) -> Result<T, ()> {
        // Place the sigbus handler
        unsafe { place_sigbus_handler() };

        let pool_guard = self.map.read().unwrap();

        trace!(fd = ?self.fd, "Buffer access on shm pool");

        // Prepare the access
        SIGBUS_GUARD.with(|guard| {
            let (p, _) = guard.get();
            if !p.is_null() {
                // Recursive call of this method is not supported
                panic!("Recursive access to a SHM pool content is not supported.");
            }
            guard.set((&*pool_guard as *const MemMap, false))
        });

        let t = f(pool_guard.ptr as *const _, pool_guard.size);

        // Cleanup Post-access
        SIGBUS_GUARD.with(|guard| {
            let (_, triggered) = guard.get();
            guard.set((ptr::null_mut(), false));
            if triggered {
                debug!(fd = ?self.fd, "SIGBUS caught on access on shm pool");
                Err(())
            } else {
                Ok(t)
            }
        })
    }

    #[instrument(level = "trace", skip_all, name = "wayland_shm")]
    pub fn with_data_mut<T, F: FnOnce(*mut u8, usize) -> T>(&self, f: F) -> Result<T, ()> {
        // Place the sigbus handler
        unsafe { place_sigbus_handler() };

        // This is actually a write access.
        #[allow(clippy::readonly_write_lock)]
        let pool_guard = self.map.write().unwrap();

        trace!(fd = ?self.fd, "Mutable buffer access on shm pool");

        // Prepare the access
        SIGBUS_GUARD.with(|guard| {
            let (p, _) = guard.get();
            if !p.is_null() {
                // Recursive call of this method is not supported
                panic!("Recursive access to a SHM pool content is not supported.");
            }
            guard.set((&*pool_guard as *const MemMap, false))
        });

        let t = f(pool_guard.ptr, pool_guard.size);

        // Cleanup Post-access
        SIGBUS_GUARD.with(|guard| {
            let (_, triggered) = guard.get();
            guard.set((ptr::null_mut(), false));
            if triggered {
                debug!(fd = ?self.fd, "SIGBUS caught on access on shm pool");
                Err(())
            } else {
                Ok(t)
            }
        })
    }
}

impl Pool {
    pub fn new(fd: OwnedFd, size: NonZeroUsize, quota: Box<dyn ShmPoolQuota>) -> Result<Self, OwnedFd> {
        InnerPool::new(fd, size, quota).map(|p| Self { inner: Some(p) })
    }

    pub fn resize(&self, newsize: NonZeroUsize) -> Result<(), ResizeError> {
        self.inner.as_ref().unwrap().resize(newsize)
    }

    pub fn size(&self) -> usize {
        self.inner.as_ref().unwrap().size()
    }

    pub fn with_data<T, F: FnOnce(*const u8, usize) -> T>(&self, f: F) -> Result<T, ()> {
        self.inner.as_ref().unwrap().with_data(f)
    }

    pub fn with_data_mut<T, F: FnOnce(*mut u8, usize) -> T>(&self, f: F) -> Result<T, ()> {
        self.inner.as_ref().unwrap().with_data_mut(f)
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        let mut inner = self.inner.take().unwrap();
        // Refund synchronously; only the potentially slow unmap/fd close goes to the worker.
        drop(inner.quota.take());
        let _ = DROP_THIS.send(inner);
    }
}

#[derive(Debug)]
struct MemMap {
    ptr: *mut u8,
    size: usize,
}

impl MemMap {
    fn new(fd: BorrowedFd<'_>, size: NonZeroUsize) -> Result<MemMap, ()> {
        Ok(MemMap {
            ptr: unsafe { map(fd, size) }?,
            size: size.into(),
        })
    }

    fn remap(&mut self, fd: BorrowedFd<'_>, newsize: NonZeroUsize) -> Result<(), ()> {
        if self.ptr.is_null() {
            return Err(());
        }
        // Map first, then swap: failure leaves the old valid mapping untouched.
        let new_ptr = unsafe { map(fd, newsize) }?;
        let old_ptr = self.ptr;
        let old_size = self.size;
        self.ptr = new_ptr;
        self.size = usize::from(newsize);
        let _ = unsafe { unmap(old_ptr, old_size) };
        Ok(())
    }

    fn size(&self) -> usize {
        self.size
    }

    fn contains(&self, ptr: *mut u8) -> bool {
        ptr >= self.ptr && ptr < unsafe { self.ptr.add(self.size) }
    }

    fn nullify(&self) -> Result<(), ()> {
        unsafe { nullify_map(self.ptr, self.size) }
    }
}

fn fd_supports_mapping(fd: BorrowedFd<'_>, size: usize) -> bool {
    let mut stat = mem::MaybeUninit::<libc::stat>::zeroed();
    if unsafe { libc::fstat(fd.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return false;
    }
    let stat = unsafe { stat.assume_init() };
    if stat.st_size < 0 || (stat.st_size as u64) < size as u64 {
        return false;
    }
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    flags >= 0 && (flags & libc::O_ACCMODE) != libc::O_RDONLY
}

impl Drop for MemMap {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            let _ = unsafe { unmap(self.ptr, self.size) };
        }
    }
}

/// A simple wrapper with some default arguments for `nix::mman::mmap`.
unsafe fn map(fd: BorrowedFd<'_>, size: NonZeroUsize) -> Result<*mut u8, ()> {
    let ret = unsafe {
        mm::mmap(
            ptr::null_mut(),
            size.into(),
            mm::ProtFlags::READ | mm::ProtFlags::WRITE,
            mm::MapFlags::SHARED,
            fd,
            0,
        )
    };
    ret.map(|p| p as *mut u8).map_err(|_| ())
}

/// A simple wrapper for `nix::mman::munmap`.
#[profiling::function]
unsafe fn unmap(ptr: *mut u8, size: usize) -> Result<(), ()> {
    let ret = unsafe { mm::munmap(ptr as *mut _, size) };
    ret.map_err(|_| ())
}

unsafe fn nullify_map(ptr: *mut u8, size: usize) -> Result<(), ()> {
    let ret = unsafe {
        mm::mmap_anonymous(
            ptr as *mut std::ffi::c_void,
            size,
            mm::ProtFlags::READ | mm::ProtFlags::WRITE,
            mm::MapFlags::PRIVATE | mm::MapFlags::FIXED,
        )
    };
    ret.map(|_| ()).map_err(|_| ())
}

/// The sigbus handler will be placed only once
unsafe fn place_sigbus_handler() {
    let _ = OLD_SIGBUS_HANDLER.get_or_init(|| {
        // create our sigbus handler
        unsafe {
            // We use `mem::zeroed()` because regular struct init as well as struct update syntax require all fields to be public
            // and libc does not guarantee that for all targets
            let mut action: libc::sigaction = mem::zeroed();
            action.sa_sigaction = sigbus_handler as _;
            action.sa_flags = libc::SA_SIGINFO | libc::SA_NODEFER;

            let mut old_action = mem::zeroed();
            if libc::sigaction(libc::SIGBUS, &action, &mut old_action) == -1 {
                let e = rustix::io::Errno::from_raw_os_error(errno::errno().0);
                panic!("sigaction failed for SIGBUS handler: {:?}", e);
            }

            old_action
        }
    });
}

unsafe fn reraise_sigbus() {
    // reset the old sigaction
    unsafe {
        libc::sigaction(libc::SIGBUS, OLD_SIGBUS_HANDLER.get().unwrap(), ptr::null_mut());
        libc::raise(libc::SIGBUS);
    }
}

extern "C" fn sigbus_handler(_signum: libc::c_int, info: *mut libc::siginfo_t, _context: *mut libc::c_void) {
    let faulty_ptr = unsafe { siginfo_si_addr(info) } as *mut u8;
    SIGBUS_GUARD.with(|guard| {
        let (memmap, _) = guard.get();
        match unsafe { memmap.as_ref() }.map(|m| (m, m.contains(faulty_ptr))) {
            Some((m, true)) => {
                // we are in a faulty memory pool !
                // remember that it was faulty
                guard.set((memmap, true));
                // nullify the pool
                if m.nullify().is_err() {
                    // something terrible occurred !
                    unsafe { reraise_sigbus() }
                }
            }
            _ => {
                // something else occurred, let's die honorably
                unsafe { reraise_sigbus() }
            }
        }
    });
}

/// This was shamelessly stolen from rustc's source
/// so I expect it to work whenever rust works
/// I guess it's good enough?
///
/// SAFETY:
/// The returned pointer points to a struct. Make sure that you use it
/// appropriately.
#[cfg(any(target_os = "linux", target_os = "android"))]
unsafe fn siginfo_si_addr(info: *mut libc::siginfo_t) -> *mut libc::c_void {
    #[repr(C)]
    #[allow(non_camel_case_types)]
    struct siginfo_t {
        a: [libc::c_int; 3], // si_signo, si_errno, si_code
        si_addr: *mut libc::c_void,
    }

    unsafe { (*(info as *const siginfo_t)).si_addr }
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
unsafe fn siginfo_si_addr(info: *mut libc::siginfo_t) -> *mut libc::c_void {
    unsafe { (*info).si_addr as _ }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{ffi::CString, os::fd::FromRawFd};

    struct Unlimited;
    impl ShmPoolQuota for Unlimited {
        fn resize(&self, _new_size: usize) -> bool {
            true
        }
    }

    fn memfd(size: usize) -> OwnedFd {
        let name = CString::new("smithay-shm-pool-test").unwrap();
        let raw = unsafe { libc::memfd_create(name.as_ptr(), 0) };
        assert!(raw >= 0);
        assert_eq!(unsafe { libc::ftruncate(raw, size as libc::off_t) }, 0);
        unsafe { OwnedFd::from_raw_fd(raw) }
    }

    #[test]
    fn pool_rejects_short_backing_and_failed_resize_preserves_mapping() {
        assert!(Pool::new(memfd(16), NonZeroUsize::new(32).unwrap(), Box::new(Unlimited)).is_err());
        let pool = Pool::new(memfd(64), NonZeroUsize::new(32).unwrap(), Box::new(Unlimited)).unwrap();
        assert!(matches!(
            pool.resize(NonZeroUsize::new(16).unwrap()),
            Err(ResizeError::InvalidSize)
        ));
        assert_eq!(pool.size(), 32);
        assert!(matches!(
            pool.resize(NonZeroUsize::new(128).unwrap()),
            Err(ResizeError::BackingTooSmall)
        ));
        assert_eq!(pool.size(), 32);
        assert!(pool.with_data(|ptr, len| unsafe { (*ptr, len) }).is_ok());
    }

    #[test]
    fn truncation_sigbus_is_contained_in_an_isolated_child() {
        let fd = memfd(4096);
        let child_fd = fd.try_clone().unwrap();
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            let pool = Pool::new(child_fd, NonZeroUsize::new(4096).unwrap(), Box::new(Unlimited)).unwrap();
            let raw = pool.inner.as_ref().unwrap().fd.as_raw_fd();
            if unsafe { libc::ftruncate(raw, 0) } != 0 {
                unsafe { libc::_exit(2) };
            }
            let result = pool.with_data(|ptr, len| unsafe { std::ptr::read_volatile(ptr.add(len - 1)) });
            unsafe { libc::_exit(if result.is_err() { 0 } else { 3 }) };
        }
        drop(fd);
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
        assert!(libc::WIFEXITED(status));
        assert_eq!(libc::WEXITSTATUS(status), 0);
    }
}
