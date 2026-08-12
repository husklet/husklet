//! Raw Linux calls. Safe ownership and policy remain in `native_host`.

use crate::native_host::{
    ClockKind, DescriptorSyscalls, DirectoryEntry, FileMetadata, HostError, HostSyscalls, NativeDescriptor, OwnedFile,
    Protection,
};
use std::ffi::CStr;
use std::sync::Arc;

use self::abi as libc;

#[path = "linux/error.rs"]
mod error;
use error::ErrnoMapper;
#[path = "linux/directory.rs"]
mod directory;
use directory::directory_next;
mod authority;
mod open_how;
#[path = "linux/process.rs"]
mod process;
pub(crate) use open_how::OpenHow;
mod confinement;
pub(crate) use authority::{InheritedDatagram, InheritedFile, InheritedListener, InheritedStream, PinnedRoot};
pub(crate) use confinement::Seccomp;
mod arena;
#[cfg(test)]
mod arena_test;
#[path = "linux/event.rs"]
mod event;
pub(in crate::ffi::linux) mod file_transfer;
mod loader;
mod mapping;
mod mapping_access;
#[cfg(test)]
mod mapping_test;
mod memory_control;
pub(crate) mod network;
#[path = "linux/virtual/backing.rs"]
mod shared_backing;
#[path = "linux/signal.rs"]
mod signal;
#[path = "linux/socket.rs"]
mod socket;
#[path = "linux/transfer.rs"]
pub(crate) mod transfer;
#[path = "linux/virtual/access.rs"]
mod virtual_access;
#[path = "linux/virtual/advice.rs"]
mod virtual_advice;
#[path = "linux/virtual/file.rs"]
mod virtual_file;
#[path = "linux/virtual/host.rs"]
mod virtual_host;
#[path = "linux/virtual/lock.rs"]
mod virtual_lock;
#[path = "linux/virtual/memory.rs"]
mod virtual_memory;
#[path = "linux/virtual/remap.rs"]
mod virtual_remap;
#[path = "linux/virtual/reservation.rs"]
mod virtual_reservation;
#[path = "linux/virtual/sparse.rs"]
mod virtual_sparse;
#[path = "linux/virtual/transaction.rs"]
mod virtual_transaction;
#[path = "linux/watch.rs"]
mod watch;
pub use loader::{AddressSpaceAdapter, Reservation};
pub use mapping::MappingHostAdapter;
pub use virtual_memory::{Memory as VirtualMemory, MemoryError};

/// Shared-backing plumbing is private to this module, so crate tests that need one object
/// mapped through two guest ranges get the wired store and arena from here.
#[cfg(test)]
pub(crate) fn shared_backed_arena(bytes: usize) -> (Arc<hl_memory::SharedObjectStore>, VirtualMemory) {
    let registry = Arc::new(shared_backing::Registry::default());
    let factory = Arc::new(shared_backing::Factory::new(Arc::clone(&registry)));
    let store =
        Arc::new(hl_memory::SharedObjectStore::with_factory(hl_memory::SharedLimits::default(), factory).unwrap());
    let arena = VirtualMemory::reserve(bytes)
        .unwrap()
        .with_shared_store(Arc::clone(&store))
        .with_shared_backings(registry);
    (store, arena)
}

#[derive(Default)]
pub struct LinuxHost;

impl LinuxHost {
    pub fn descriptor_from_file(self: &Arc<Self>, file: &OwnedFile<Self>) -> Result<NativeDescriptor<Self>, HostError> {
        let raw = i32::try_from(file.host_handle()).map_err(|_| HostError::Invalid)?;
        let duplicate = self.duplicate_cloexec(raw, 3)?;
        NativeDescriptor::from_raw(Arc::clone(self), duplicate)
    }
    pub fn open(self: &Arc<Self>, path: &CStr, flags: i32, permissions: u16) -> Result<OwnedFile<Self>, HostError> {
        self.open_at_raw(libc::AT_FDCWD, path, flags, permissions)
    }

    pub fn open_at(
        self: &Arc<Self>,
        directory: &OwnedFile<Self>,
        path: &CStr,
        flags: i32,
        permissions: u16,
    ) -> Result<OwnedFile<Self>, HostError> {
        let directory = i32::try_from(directory.host_handle()).map_err(|_| HostError::Invalid)?;
        self.open_at_raw(directory, path, flags, permissions)
    }

    fn open_at_raw(
        self: &Arc<Self>,
        directory: i32,
        path: &CStr,
        flags: i32,
        permissions: u16,
    ) -> Result<OwnedFile<Self>, HostError> {
        // SAFETY: path is a live NUL-terminated C string; directory is AT_FDCWD
        // or owned by the caller. The returned descriptor transfers immediately
        // into OwnedFile, libc retains no pointer, and cannot unwind.
        let descriptor = unsafe {
            libc::openat(
                directory,
                path.as_ptr(),
                flags | libc::O_CLOEXEC,
                libc::mode_t::from(permissions),
            )
        };
        if descriptor < 0 {
            Err(ErrnoMapper::current())
        } else {
            Ok(OwnedFile::from_host_handle(Arc::clone(self), descriptor as u64))
        }
    }
}

impl DescriptorSyscalls for LinuxHost {
    fn duplicate_cloexec(&self, descriptor: i32, minimum: i32) -> Result<i32, HostError> {
        // SAFETY: fcntl receives scalar values only and returns a new descriptor.
        let duplicate = unsafe { libc::fcntl(descriptor, libc::F_DUPFD_CLOEXEC, minimum) };
        (duplicate >= 0).then_some(duplicate).ok_or_else(ErrnoMapper::current)
    }

    fn close_descriptor(&self, descriptor: i32) {
        // SAFETY: the safe owner surrenders the descriptor exactly once.
        let _ = unsafe { libc::close(descriptor) };
    }
}

impl HostSyscalls for LinuxHost {
    fn clock_ns(&self, kind: ClockKind) -> Result<u64, HostError> {
        let clock = match kind {
            ClockKind::Monotonic => libc::CLOCK_MONOTONIC,
            ClockKind::Realtime => libc::CLOCK_REALTIME,
            ClockKind::RawMonotonic => libc::CLOCK_MONOTONIC_RAW,
            ClockKind::ProcessCpu => libc::CLOCK_PROCESS_CPUTIME_ID,
            ClockKind::ThreadCpu => libc::CLOCK_THREAD_CPUTIME_ID,
        };
        let mut value = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        // SAFETY: value is aligned, initialized, uniquely writable, retained by
        // no caller, and libc cannot unwind.
        if unsafe { libc::clock_gettime(clock, &raw mut value) } != 0 {
            return Err(ErrnoMapper::current());
        }
        let seconds = u64::try_from(value.tv_sec).map_err(|_| HostError::Failed)?;
        let nanos = u64::try_from(value.tv_nsec).map_err(|_| HostError::Failed)?;
        seconds
            .checked_mul(1_000_000_000)
            .and_then(|base| base.checked_add(nanos))
            .ok_or(HostError::Failed)
    }

    fn close_file(&self, file: u64) {
        if let Ok(descriptor) = i32::try_from(file) {
            // SAFETY: OwnedFile surrenders this descriptor exactly once; no Rust
            // storage is referenced, and libc cannot unwind.
            let _ = unsafe { libc::close(descriptor) };
        }
    }

    fn read(&self, file: u64, output: &mut [u8]) -> Result<usize, HostError> {
        let descriptor = i32::try_from(file).map_err(|_| HostError::Invalid)?;
        // SAFETY: output is uniquely writable for its exact length; OwnedFile
        // retains the descriptor, libc retains nothing, and cannot unwind.
        let result = unsafe { libc::read(descriptor, output.as_mut_ptr().cast(), output.len()) };
        result.try_into().map_err(|_| ErrnoMapper::current())
    }

    fn write(&self, file: u64, input: &[u8]) -> Result<usize, HostError> {
        let descriptor = i32::try_from(file).map_err(|_| HostError::Invalid)?;
        // SAFETY: input is valid for its exact length and immutably borrowed;
        // OwnedFile retains the descriptor, libc retains nothing, and cannot unwind.
        let result = unsafe { libc::write(descriptor, input.as_ptr().cast(), input.len()) };
        result.try_into().map_err(|_| ErrnoMapper::current())
    }

    fn read_at(&self, file: u64, offset: u64, output: &mut [u8]) -> Result<usize, HostError> {
        let descriptor = i32::try_from(file).map_err(|_| HostError::Invalid)?;
        let offset = i64::try_from(offset).map_err(|_| HostError::Invalid)?;
        // SAFETY: output is uniquely valid for its exact length; OwnedFile keeps
        // the descriptor alive, libc retains nothing, and cannot unwind.
        let result = unsafe { libc::pread(descriptor, output.as_mut_ptr().cast(), output.len(), offset) };
        result.try_into().map_err(|_| ErrnoMapper::current())
    }

    fn write_at(&self, file: u64, offset: u64, input: &[u8]) -> Result<usize, HostError> {
        let descriptor = i32::try_from(file).map_err(|_| HostError::Invalid)?;
        let offset = i64::try_from(offset).map_err(|_| HostError::Invalid)?;
        // SAFETY: input is valid and immutably borrowed for its exact length;
        // OwnedFile retains ownership, libc retains nothing, and cannot unwind.
        let result = unsafe { libc::pwrite(descriptor, input.as_ptr().cast(), input.len(), offset) };
        result.try_into().map_err(|_| ErrnoMapper::current())
    }

    fn metadata(&self, file: u64) -> Result<FileMetadata, HostError> {
        let descriptor = i32::try_from(file).map_err(|_| HostError::Invalid)?;
        let mut status = abi::Statx::default();
        // SAFETY: the empty pathname is static and NUL-terminated; status is
        // aligned and uniquely writable. AT_EMPTY_PATH binds the owned descriptor,
        // the kernel retains no pointer, and cannot unwind.
        let result = unsafe {
            libc::syscall(
                libc::SYS_statx,
                descriptor,
                c"".as_ptr(),
                libc::AT_EMPTY_PATH,
                libc::STATX_BASIC_STATS,
                &mut status,
            )
        };
        if result != 0 {
            return Err(ErrnoMapper::current());
        }
        let seconds = u64::try_from(status.modified.seconds).unwrap_or(0);
        let nanos = u64::from(status.modified.nanoseconds);
        Ok(FileMetadata {
            device: (u64::from(status.device_major) << 32) | u64::from(status.device_minor),
            inode: status.inode,
            size: status.size,
            permissions: status.mode & 0o7777,
            links: u64::from(status.links),
            modified_ns: seconds.saturating_mul(1_000_000_000).saturating_add(nanos),
        })
    }

    fn directory_next(
        &self,
        file: u64,
        cookie: u64,
        name: &mut [u8],
    ) -> Result<Option<(DirectoryEntry, usize)>, HostError> {
        directory_next(file, cookie, name)
    }

    fn page_size(&self) -> Result<usize, HostError> {
        // SAFETY: sysconf receives no pointer and cannot unwind.
        let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        let size = usize::try_from(size).map_err(|_| HostError::Failed)?;
        size.is_power_of_two().then_some(size).ok_or(HostError::Failed)
    }

    fn map(&self, size: usize, protection: Protection) -> Result<u64, HostError> {
        let native = protection.native()?;
        // SAFETY: Linux selects an aligned range of validated size. Ownership
        // transfers to OwnedMapping; MAP_FAILED is checked and libc cannot unwind.
        let address = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                size,
                native,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if address == libc::MAP_FAILED {
            Err(ErrnoMapper::current())
        } else {
            Ok(address as usize as u64)
        }
    }

    fn protect(&self, mapping: u64, size: usize, protection: Protection) -> Result<(), HostError> {
        let address = usize::try_from(mapping).map_err(|_| HostError::Invalid)?;
        let native = protection.native()?;
        // SAFETY: address/size belong to a live OwnedMapping with no exposed safe
        // references; Linux retains nothing and libc cannot unwind.
        let result = unsafe { libc::mprotect(address as *mut libc::c_void, size, native) };
        (result == 0).then_some(()).ok_or_else(ErrnoMapper::current)
    }

    fn unmap(&self, mapping: u64, size: usize) -> Result<(), HostError> {
        let address = usize::try_from(mapping).map_err(|_| HostError::Invalid)?;
        // SAFETY: OwnedMapping surrenders the complete mapping exactly once; no
        // safe references exist, Linux retains nothing, and libc cannot unwind.
        let result = unsafe { libc::munmap(address as *mut libc::c_void, size) };
        (result == 0).then_some(()).ok_or_else(ErrnoMapper::current)
    }
}

#[allow(non_camel_case_types, non_upper_case_globals)]
mod abi {
    pub type c_void = core::ffi::c_void;
    pub type mode_t = u32;

    #[repr(C)]
    pub struct timespec {
        pub tv_sec: i64,
        pub tv_nsec: i64,
    }

    #[repr(C)]
    #[derive(Default)]
    pub struct StatxTimestamp {
        pub seconds: i64,
        pub nanoseconds: u32,
        pub reserved: i32,
    }

    #[repr(C)]
    #[derive(Default)]
    pub struct Statx {
        pub mask: u32,
        pub block_size: u32,
        pub attributes: u64,
        pub links: u32,
        pub uid: u32,
        pub gid: u32,
        pub mode: u16,
        pub spare0: u16,
        pub inode: u64,
        pub size: u64,
        pub blocks: u64,
        pub attributes_mask: u64,
        pub accessed: StatxTimestamp,
        pub created: StatxTimestamp,
        pub changed: StatxTimestamp,
        pub modified: StatxTimestamp,
        pub special_major: u32,
        pub special_minor: u32,
        pub device_major: u32,
        pub device_minor: u32,
        pub mount_id: u64,
        pub direct_io_memory_alignment: u32,
        pub direct_io_offset_alignment: u32,
        pub subvolume: u64,
        pub atomic_write_unit_minimum: u32,
        pub atomic_write_unit_maximum: u32,
        pub atomic_write_segments_maximum: u32,
        pub spare1: u32,
        pub spare: [u64; 9],
    }

    pub const AT_FDCWD: i32 = -100;
    pub const AT_EMPTY_PATH: i32 = 0x1000;
    pub const O_CLOEXEC: i32 = 0x80000;
    pub const F_DUPFD_CLOEXEC: i32 = 1030;
    pub const O_RDONLY: i32 = 0;
    #[cfg(test)]
    pub const O_RDWR: i32 = 2;
    #[cfg(test)]
    pub const O_CREAT: i32 = 0x40;
    pub const O_NONBLOCK: i32 = 0x800;
    #[cfg(test)]
    pub const O_TRUNC: i32 = 0x200;
    pub const CLOCK_REALTIME: i32 = 0;
    pub const CLOCK_MONOTONIC: i32 = 1;
    pub const CLOCK_PROCESS_CPUTIME_ID: i32 = 2;
    pub const CLOCK_THREAD_CPUTIME_ID: i32 = 3;
    pub const CLOCK_MONOTONIC_RAW: i32 = 4;
    pub const SEEK_SET: i32 = 0;
    pub const _SC_PAGESIZE: i32 = 30;
    pub const MAP_PRIVATE: i32 = 2;
    pub const MAP_SHARED: i32 = 1;
    pub const MAP_FIXED: i32 = 0x10;
    pub const MAP_ANONYMOUS: i32 = 0x20;
    pub const PROT_NONE: i32 = 0;
    pub const DT_DIR: u8 = 4;
    pub const DT_REG: u8 = 8;
    pub const DT_LNK: u8 = 10;
    pub const STATX_BASIC_STATS: u32 = 0x7ff;
    pub const EINTR: i32 = 4;
    pub const ESRCH: i32 = 3;
    pub const EBADF: i32 = 9;
    pub const EAGAIN: i32 = 11;
    pub const EACCES: i32 = 13;
    pub const EEXIST: i32 = 17;
    pub const ENOENT: i32 = 2;
    pub const ENOMEM: i32 = 12;
    pub const EINVAL: i32 = 22;
    pub const EMFILE: i32 = 24;
    pub const ENFILE: i32 = 23;
    pub const ENOTSUP: i32 = 95;
    pub const EPERM: i32 = 1;
    pub const SIGKILL: i32 = 9;
    pub const WNOHANG: i32 = 1;
    pub const POSIX_SPAWN_SETPGROUP: i16 = 2;
    pub const POSIX_SPAWN_SETSIGDEF: i16 = 4;
    pub const POSIX_SPAWN_SETSIGMASK: i16 = 8;
    /// A libc `sigset_t`: 128 bytes with 8-byte alignment on every Linux target.
    pub type SignalSet = [u64; 16];
    pub const AF_UNIX: i32 = 1;
    pub const SOCK_STREAM: i32 = 1;
    pub const SOCK_NONBLOCK: i32 = 0x800;
    pub const SOCK_CLOEXEC: i32 = 0x80000;
    pub const MAP_FAILED: *mut c_void = usize::MAX as *mut c_void;
    #[cfg(target_arch = "aarch64")]
    pub const SYS_getdents64: i64 = 61;
    #[cfg(target_arch = "x86_64")]
    pub const SYS_getdents64: i64 = 217;
    #[cfg(target_arch = "aarch64")]
    pub const SYS_statx: i64 = 291;
    #[cfg(target_arch = "x86_64")]
    pub const SYS_statx: i64 = 332;
    pub const SYS_pidfd_open: i64 = 434;
    #[cfg(target_arch = "aarch64")]
    pub const SYS_memfd_create: i64 = 279;
    #[cfg(target_arch = "x86_64")]
    pub const SYS_memfd_create: i64 = 319;

    pub struct Memfd;

    impl Memfd {
        #[allow(unsafe_code)]
        pub fn create(name: &std::ffi::CStr) -> Result<std::fs::File, ()> {
            use std::os::fd::FromRawFd;
            // SAFETY: the name is terminated, MFD_CLOEXEC requests an owned fd,
            // and a successful descriptor is transferred immediately to File.
            let descriptor = unsafe { syscall(SYS_memfd_create, name.as_ptr(), 3_u32) };
            let descriptor = i32::try_from(descriptor).map_err(|_| ())?;
            if descriptor < 0 {
                return Err(());
            }
            // SAFETY: memfd_create returned a new descriptor owned by this call.
            Ok(unsafe { std::fs::File::from_raw_fd(descriptor) })
        }
    }

    pub struct PageLock;

    impl PageLock {
        pub fn lock(address: *const c_void, size: usize) -> bool {
            // SAFETY: callers prove the range is a retained mapping; libc keeps no pointer.
            unsafe { mlock(address, size) == 0 }
        }

        pub fn unlock(address: *const c_void, size: usize) -> bool {
            // SAFETY: callers prove the range is retained; libc keeps no pointer.
            unsafe { munlock(address, size) == 0 }
        }
    }

    unsafe extern "C" {
        pub fn openat(directory: i32, path: *const core::ffi::c_char, flags: i32, ...) -> i32;
        pub fn close(descriptor: i32) -> i32;
        pub fn fcntl(descriptor: i32, command: i32, ...) -> i32;
        pub fn read(descriptor: i32, output: *mut c_void, length: usize) -> isize;
        pub fn write(descriptor: i32, input: *const c_void, length: usize) -> isize;
        pub fn pread(descriptor: i32, output: *mut c_void, length: usize, offset: i64) -> isize;
        pub fn pwrite(descriptor: i32, input: *const c_void, length: usize, offset: i64) -> isize;
        pub fn clock_gettime(clock: i32, output: *mut timespec) -> i32;
        pub fn sysconf(name: i32) -> i64;
        pub fn lseek(descriptor: i32, offset: i64, whence: i32) -> i64;
        pub fn mmap(
            address: *mut c_void,
            size: usize,
            protection: i32,
            flags: i32,
            descriptor: i32,
            offset: i64,
        ) -> *mut c_void;
        pub fn mprotect(address: *mut c_void, size: usize, protection: i32) -> i32;
        pub fn mlock(address: *const c_void, size: usize) -> i32;
        pub fn munlock(address: *const c_void, size: usize) -> i32;
        pub fn munmap(address: *mut c_void, size: usize) -> i32;
        pub fn syscall(number: i64, ...) -> i64;
        pub fn posix_spawnp(
            pid: *mut i32,
            path: *const core::ffi::c_char,
            actions: *const c_void,
            attributes: *const c_void,
            arguments: *const *mut core::ffi::c_char,
            environment: *const *mut core::ffi::c_char,
        ) -> i32;
        pub fn posix_spawn_file_actions_init(actions: *mut c_void) -> i32;
        pub fn posix_spawn_file_actions_destroy(actions: *mut c_void) -> i32;
        pub fn posix_spawn_file_actions_adddup2(actions: *mut c_void, source: i32, target: i32) -> i32;
        pub fn posix_spawn_file_actions_addclose(actions: *mut c_void, descriptor: i32) -> i32;
        pub fn posix_spawnattr_init(attributes: *mut c_void) -> i32;
        pub fn posix_spawnattr_destroy(attributes: *mut c_void) -> i32;
        pub fn posix_spawnattr_setflags(attributes: *mut c_void, flags: i16) -> i32;
        pub fn posix_spawnattr_setpgroup(attributes: *mut c_void, group: i32) -> i32;
        pub fn posix_spawnattr_setsigdefault(attributes: *mut c_void, set: *const SignalSet) -> i32;
        pub fn posix_spawnattr_setsigmask(attributes: *mut c_void, set: *const SignalSet) -> i32;
        pub fn sigfillset(set: *mut SignalSet) -> i32;
        pub fn sigemptyset(set: *mut SignalSet) -> i32;
        pub fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
        pub fn kill(pid: i32, signal: i32) -> i32;
        pub fn socketpair(domain: i32, kind: i32, protocol: i32, output: *mut i32) -> i32;
        pub fn send(descriptor: i32, input: *const c_void, length: usize, flags: i32) -> isize;
        pub fn recv(descriptor: i32, output: *mut c_void, length: usize, flags: i32) -> isize;
    }
}

#[cfg(test)]
#[path = "linux/test.rs"]
mod tests;
