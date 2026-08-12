//! Audited descriptor and shared-trigger adapters for retained-C checkpoints.

use std::ffi::{c_int, c_uint, c_void};
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::os::unix::net::UnixStream;

unsafe extern "C" {
    fn hl_ckpt_broker_pair(parent: *mut u64, child: *mut u64) -> c_int;
    fn hl_ckpt_broker_accept(broker: u64, timeout_ms: c_int, host_pid: *mut u64) -> u64;
    fn hl_ckpt_trigger_create(descriptor: *mut u64, mapping: *mut *mut c_void) -> c_int;
    fn hl_ckpt_trigger_bump(mapping: *mut c_void) -> c_uint;
    fn hl_ckpt_trigger_destroy(mapping: *mut c_void, descriptor: u64);
}

pub(crate) struct Broker(OwnedFd);

impl Broker {
    pub(crate) fn pair() -> std::io::Result<(Self, OwnedFd)> {
        let mut parent = 0_u64;
        let mut child = 0_u64;
        // SAFETY: successful creation returns two uniquely owned descriptors.
        if unsafe { hl_ckpt_broker_pair(&raw mut parent, &raw mut child) } != 0
            || !valid_descriptor(parent)
            || !valid_descriptor(child)
        {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: ownership was transferred by the successful C call above.
        unsafe {
            Ok((
                Self(OwnedFd::from_raw_fd(parent as i32)),
                OwnedFd::from_raw_fd(child as i32),
            ))
        }
    }

    pub(crate) fn accept(&self, timeout: std::time::Duration) -> Option<(UnixStream, u64)> {
        let timeout = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
        let mut host_pid = 0;
        // SAFETY: self keeps the broker live; a nonzero result is a newly owned stream descriptor.
        let channel = unsafe { hl_ckpt_broker_accept(self.0.as_raw_fd() as u64, timeout, &raw mut host_pid) };
        if !valid_descriptor(channel) {
            return None;
        }
        // SAFETY: the accept call transferred unique ownership.
        Some((unsafe { UnixStream::from_raw_fd(channel as i32) }, host_pid))
    }
}

const fn valid_descriptor(descriptor: u64) -> bool {
    descriptor != 0 && descriptor <= i32::MAX as u64
}

pub(crate) struct Trigger {
    descriptor: i32,
    mapping: *mut c_void,
}

// SAFETY: C owns the one-word shared mapping protocol; bump is the only access.
unsafe impl Send for Trigger {}
// SAFETY: capture is serialized by the machine lifecycle; bump is one generation update.
unsafe impl Sync for Trigger {}

impl Trigger {
    pub(crate) fn create() -> std::io::Result<Self> {
        let mut descriptor = 0_u64;
        let mut mapping = std::ptr::null_mut();
        // SAFETY: output pointers are valid and initialized by C on success.
        if unsafe { hl_ckpt_trigger_create(&raw mut descriptor, &raw mut mapping) } != 0
            || !valid_descriptor(descriptor)
            || mapping.is_null()
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self {
            descriptor: descriptor as i32,
            mapping,
        })
    }

    pub(crate) const fn descriptor(&self) -> i32 {
        self.descriptor
    }

    pub(crate) fn bump(&self) -> u32 {
        // SAFETY: mapping remains live for self's lifetime.
        unsafe { hl_ckpt_trigger_bump(self.mapping) }
    }
}

impl Drop for Trigger {
    fn drop(&mut self) {
        // SAFETY: this type owns both resources and drops them exactly once.
        unsafe { hl_ckpt_trigger_destroy(self.mapping, self.descriptor as u64) };
        self.mapping = std::ptr::null_mut();
        self.descriptor = -1;
    }
}
