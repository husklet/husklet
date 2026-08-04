use std::fmt::Debug;

use super::abi;

#[derive(Clone, Copy)]
pub(super) enum MapSource {
    Anonymous,
    File { descriptor: i32, offset: i64 },
}

pub(super) trait GuestVm: Debug + Send + Sync {
    fn reserve(&self, length: usize) -> Result<usize, ()>;
    fn map(&self, address: usize, length: usize, protection: i32, shared: bool, source: MapSource)
        -> Result<(), ()>;
    fn protect(&self, address: usize, length: usize, protection: i32) -> Result<(), ()>;
    fn remap(&self, source: usize, old_length: usize, destination: usize, new_length: usize, keep: bool)
        -> Result<(), ()>;
    fn release(&self, address: usize, length: usize);
}

#[derive(Debug, Default)]
pub(super) struct LinuxGuestVm;

impl GuestVm for LinuxGuestVm {
    fn reserve(&self, length: usize) -> Result<usize, ()> {
        // SAFETY: Linux chooses the address, PROT_NONE exposes no storage, and
        // the returned scalar is transferred immediately to an owner.
        let address = unsafe {
            abi::mmap(
                std::ptr::null_mut(),
                length,
                abi::PROT_NONE,
                abi::MAP_PRIVATE | abi::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        (address != abi::MAP_FAILED).then_some(address as usize).ok_or(())
    }

    fn map(
        &self,
        address: usize,
        length: usize,
        protection: i32,
        shared: bool,
        source: MapSource,
    ) -> Result<(), ()> {
        let (descriptor, offset, anonymous) = match source {
            MapSource::Anonymous => (-1, 0, abi::MAP_ANONYMOUS),
            MapSource::File { descriptor, offset } => (descriptor, offset, 0),
        };
        let sharing = if shared { abi::MAP_SHARED } else { abi::MAP_PRIVATE };
        // SAFETY: the caller owns and validates the fixed destination, and any
        // descriptor remains retained for the duration of this call.
        let mapped = unsafe {
            abi::mmap(
                address as *mut core::ffi::c_void,
                length,
                protection,
                abi::MAP_FIXED | sharing | anonymous,
                descriptor,
                offset,
            )
        };
        (mapped != abi::MAP_FAILED && mapped as usize == address).then_some(()).ok_or(())
    }

    fn protect(&self, address: usize, length: usize, protection: i32) -> Result<(), ()> {
        // SAFETY: the caller validates that the complete range is owned.
        (unsafe { abi::mprotect(address as *mut core::ffi::c_void, length, protection) } == 0)
            .then_some(())
            .ok_or(())
    }

    fn remap(
        &self,
        source: usize,
        old_length: usize,
        destination: usize,
        new_length: usize,
        keep: bool,
    ) -> Result<(), ()> {
        let flags = 1 | 2 | if keep { 4 } else { 0 };
        // SAFETY: both ranges are validated inside the same owned reservation.
        let moved = unsafe {
            mremap(
                source as *mut core::ffi::c_void,
                old_length,
                new_length,
                flags,
                destination as *mut core::ffi::c_void,
            )
        };
        (moved != abi::MAP_FAILED && moved as usize == destination).then_some(()).ok_or(())
    }

    fn release(&self, address: usize, length: usize) {
        // SAFETY: the unique reservation owner surrenders its complete range.
        let _ = unsafe { abi::munmap(address as *mut core::ffi::c_void, length) };
    }
}

unsafe extern "C" {
    fn mremap(
        old_address: *mut core::ffi::c_void,
        old_size: usize,
        new_size: usize,
        flags: i32,
        new_address: *mut core::ffi::c_void,
    ) -> *mut core::ffi::c_void;
}
