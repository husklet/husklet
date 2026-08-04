//! Raw Darwin calls. Safe ownership and policy remain in `native_host`.

#![allow(unsafe_code)]

use crate::native_host::{
    ClockKind, DescriptorSyscalls, DirectoryEntry, FileMetadata, HostError, HostSyscalls, Protection,
};
use std::ffi::CStr;
use std::io;
use std::sync::Arc;

#[path = "macos/event.rs"]
mod event;
#[path = "macos/process.rs"]
mod process;
#[path = "macos/socket.rs"]
mod socket;

#[derive(Default)]
pub struct DarwinHost;

impl DescriptorSyscalls for DarwinHost {
    fn duplicate_cloexec(&self, descriptor: i32, minimum: i32) -> Result<i32, HostError> {
        // SAFETY: fcntl receives scalar arguments and success returns a new,
        // independently owned descriptor. No Rust storage is retained.
        let duplicate = unsafe { libc::fcntl(descriptor, libc::F_DUPFD_CLOEXEC, minimum) };
        (duplicate >= 0).then_some(duplicate).ok_or_else(last_error)
    }

    fn close_descriptor(&self, descriptor: i32) {
        // SAFETY: the RAII owner surrenders this descriptor exactly once.
        let _ = unsafe { libc::close(descriptor) };
    }
}

impl DarwinHost {
    pub fn open(self: &Arc<Self>, path: &CStr, flags: i32) -> Result<crate::native_host::OwnedFile<Self>, HostError> {
        self.open_at_raw(libc::AT_FDCWD, path, flags)
    }

    pub fn open_at(
        self: &Arc<Self>,
        directory: &crate::native_host::OwnedFile<Self>,
        path: &CStr,
        flags: i32,
    ) -> Result<crate::native_host::OwnedFile<Self>, HostError> {
        let descriptor = i32::try_from(directory.host_handle()).map_err(|_| HostError::Invalid)?;
        self.open_at_raw(descriptor, path, flags)
    }

    fn open_at_raw(
        self: &Arc<Self>,
        directory: i32,
        path: &CStr,
        flags: i32,
    ) -> Result<crate::native_host::OwnedFile<Self>, HostError> {
        // SAFETY: `path` is NUL-terminated for the duration of the call. The
        // directory descriptor is either AT_FDCWD or owned by the caller. No Rust
        // reference aliases storage written by `open`; the returned descriptor is
        // immediately transferred to `OwnedFile`, and libc cannot unwind into Rust.
        let descriptor = unsafe {
            // SAFETY: the conditions documented immediately above establish the
            // pathname and descriptor validity for this non-retaining call.
            libc::openat(directory, path.as_ptr(), flags | libc::O_CLOEXEC)
        };
        if descriptor < 0 {
            return Err(last_error());
        }
        Ok(crate::native_host::OwnedFile::from_host_handle(
            Arc::clone(self),
            descriptor as u64,
        ))
    }
}

impl HostSyscalls for DarwinHost {
    fn clock_ns(&self, kind: ClockKind) -> Result<u64, HostError> {
        let clock = match kind {
            ClockKind::Monotonic => libc::CLOCK_MONOTONIC,
            ClockKind::Realtime => libc::CLOCK_REALTIME,
            ClockKind::RawMonotonic => libc::CLOCK_MONOTONIC_RAW,
            ClockKind::ProcessCpu => libc::CLOCK_PROCESS_CPUTIME_ID,
            ClockKind::ThreadCpu => libc::CLOCK_THREAD_CPUTIME_ID,
        };
        let mut value = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        // SAFETY: `value` is aligned, initialized, uniquely borrowed, and lives
        // through the call. libc retains no pointer and cannot unwind.
        if unsafe { libc::clock_gettime(clock, &mut value) } != 0 {
            return Err(last_error());
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
            // SAFETY: ownership is surrendered exactly once by `OwnedFile::drop`;
            // no referenced Rust storage exists and libc cannot unwind.
            let _ = unsafe { libc::close(descriptor) };
        }
    }

    fn read(&self, file: u64, output: &mut [u8]) -> Result<usize, HostError> {
        let descriptor = i32::try_from(file).map_err(|_| HostError::Invalid)?;
        // SAFETY: the output pointer is valid and uniquely borrowed for its exact
        // length. The descriptor lifetime is held by `OwnedFile`; libc retains no
        // pointer and cannot unwind. EINTR/EAGAIN are returned, not retried.
        let result = unsafe { libc::read(descriptor, output.as_mut_ptr().cast(), output.len()) };
        result.try_into().map_err(|_| last_error())
    }

    fn write(&self, file: u64, input: &[u8]) -> Result<usize, HostError> {
        let descriptor = i32::try_from(file).map_err(|_| HostError::Invalid)?;
        // SAFETY: the input pointer is valid and immutably borrowed for its exact
        // length. `OwnedFile` keeps the descriptor alive, libc retains no pointer,
        // concurrent descriptor semantics are delegated to Darwin, and no unwind occurs.
        let result = unsafe { libc::write(descriptor, input.as_ptr().cast(), input.len()) };
        result.try_into().map_err(|_| last_error())
    }

    fn read_at(&self, file: u64, offset: u64, output: &mut [u8]) -> Result<usize, HostError> {
        let descriptor = i32::try_from(file).map_err(|_| HostError::Invalid)?;
        let offset = i64::try_from(offset).map_err(|_| HostError::Invalid)?;
        // SAFETY: output is uniquely borrowed for its exact length; the descriptor
        // remains owned, libc retains nothing, and cannot unwind.
        let result = unsafe { libc::pread(descriptor, output.as_mut_ptr().cast(), output.len(), offset) };
        result.try_into().map_err(|_| last_error())
    }

    fn write_at(&self, file: u64, offset: u64, input: &[u8]) -> Result<usize, HostError> {
        let descriptor = i32::try_from(file).map_err(|_| HostError::Invalid)?;
        let offset = i64::try_from(offset).map_err(|_| HostError::Invalid)?;
        // SAFETY: input is valid for its exact length; the descriptor remains
        // owned, libc retains nothing, and cannot unwind.
        let result = unsafe { libc::pwrite(descriptor, input.as_ptr().cast(), input.len(), offset) };
        result.try_into().map_err(|_| last_error())
    }

    fn metadata(&self, file: u64) -> Result<FileMetadata, HostError> {
        let descriptor = i32::try_from(file).map_err(|_| HostError::Invalid)?;
        // SAFETY: zero is a valid initial byte pattern for `stat`; the value is
        // uniquely owned and initialized by `fstat`, which retains nothing.
        let mut status: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: `status` is aligned, writable, and lives through the call.
        // `OwnedFile` retains descriptor ownership and libc cannot unwind.
        if unsafe { libc::fstat(descriptor, &mut status) } != 0 {
            return Err(last_error());
        }
        let seconds = u64::try_from(status.st_mtimespec.tv_sec).unwrap_or(0);
        let nanos = u64::try_from(status.st_mtimespec.tv_nsec).unwrap_or(0);
        Ok(FileMetadata {
            device: status.st_dev,
            inode: status.st_ino,
            size: u64::try_from(status.st_size).unwrap_or(0),
            permissions: (status.st_mode & 0o7777) as u16,
            links: status.st_nlink,
            modified_ns: seconds.saturating_mul(1_000_000_000).saturating_add(nanos),
        })
    }

    fn directory_next(&self, _: u64, _: u64, _: &mut [u8]) -> Result<Option<(DirectoryEntry, usize)>, HostError> {
        Err(HostError::Unsupported)
    }

    fn page_size(&self) -> Result<usize, HostError> {
        // SAFETY: `sysconf` takes no pointers, retains no state, and cannot unwind.
        let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        let size = usize::try_from(size).map_err(|_| HostError::Failed)?;
        size.is_power_of_two().then_some(size).ok_or(HostError::Failed)
    }

    fn map(&self, size: usize, protection: Protection) -> Result<u64, HostError> {
        let native = protection.native()?;
        // SAFETY: Darwin selects an aligned address; size was host-page validated.
        // The returned allocation has no Rust references and transfers to
        // `OwnedMapping`; MAP_FAILED is checked and libc cannot unwind.
        let address = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                size,
                native,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            )
        };
        if address == libc::MAP_FAILED {
            Err(last_error())
        } else {
            Ok(address as usize as u64)
        }
    }

    fn protect(&self, mapping: u64, size: usize, protection: Protection) -> Result<(), HostError> {
        let native = protection.native()?;
        let address = usize::try_from(mapping).map_err(|_| HostError::Invalid)?;
        // SAFETY: the address and size belong to the live `OwnedMapping`; callers
        // hold no safe references into it, Darwin performs the atomic protection
        // transition, retains no pointer, and libc cannot unwind.
        if unsafe { libc::mprotect(address as *mut libc::c_void, size, native) } == 0 {
            Ok(())
        } else {
            Err(last_error())
        }
    }

    fn unmap(&self, mapping: u64, size: usize) -> Result<(), HostError> {
        let address = usize::try_from(mapping).map_err(|_| HostError::Invalid)?;
        // SAFETY: ownership is surrendered exactly once from `OwnedMapping::drop`;
        // no safe references exist, Darwin retains nothing, and libc cannot unwind.
        if unsafe { libc::munmap(address as *mut libc::c_void, size) } == 0 {
            Ok(())
        } else {
            Err(last_error())
        }
    }
}

pub(super) fn last_error() -> HostError {
    match io::Error::last_os_error().raw_os_error() {
        Some(libc::EINTR) => HostError::Interrupted,
        Some(libc::EAGAIN) => HostError::WouldBlock,
        Some(libc::EINVAL) => HostError::Invalid,
        Some(code) if code == libc::EACCES || code == libc::EPERM => HostError::Denied,
        Some(code) if code == libc::ENOENT || code == libc::ESRCH => HostError::NotFound,
        Some(libc::EEXIST) => HostError::Exists,
        Some(code) if code == libc::EMFILE || code == libc::ENFILE || code == libc::ENOMEM => HostError::Exhausted,
        Some(code) if code == libc::ENOTSUP || code == libc::EAFNOSUPPORT => HostError::Unsupported,
        Some(code) if code == libc::EINPROGRESS || code == libc::EALREADY => HostError::WouldBlock,
        _ => HostError::Failed,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn darwin_time_abi() {
        assert_eq!(std::mem::size_of::<libc::timespec>(), 16);
        assert_eq!(std::mem::align_of::<libc::timespec>(), 8);
        assert_eq!(std::mem::size_of::<*mut libc::c_void>(), 8);
    }
}
