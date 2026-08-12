#![allow(unsafe_code)]

use std::{
    ffi::{c_char, c_int, c_uint, c_void},
    ptr::NonNull,
};

use crate::bindings::{self, Backend, SyscallDispatch};

pub const STATUS_OK: i32 = 0;

/// Borrowed, low-level creation arguments for the native engine.
///
/// The safe high-level container adapter owns the strings, arrays and image
/// plan. This package deliberately does not depend on application domain types.
#[derive(Clone, Copy)]
pub struct Create<'a> {
    pub isa: u32,
    pub rootfs: Option<&'a std::ffi::CStr>,
    pub executable_host: Option<&'a std::ffi::CStr>,
    pub executable_fd: i32,
    pub image_plan: *const c_void,
    pub option_names: &'a [*const c_char],
    pub option_values: &'a [*const c_char],
    pub standard_fds: [i32; 3],
    pub provider_fd: i32,
    pub syscall_context: *mut c_void,
    pub syscall_dispatch: Option<SyscallDispatch>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Exit {
    pub kind: u32,
    pub status: i32,
    pub detail: u64,
}

/// Unique owner of a native engine instance.
pub struct Engine(NonNull<Backend>);

// The C lifecycle contract permits request from another thread while run is
// active. The handle remains uniquely owned and destroy joins the active run.
unsafe impl Send for Engine {}
unsafe impl Sync for Engine {}

impl Engine {
    /// Creates an engine through the stable C bridge.
    ///
    /// # Safety
    /// `image_plan`, option pointers and callback state must satisfy the C ABI.
    /// Borrowed create inputs need only remain valid for this call; C copies
    /// configuration. Callback context must remain valid until this value drops.
    pub unsafe fn create(config: Create<'_>) -> Result<Self, i32> {
        if config.option_names.len() != config.option_values.len() {
            return Err(STATUS_OK.wrapping_add(1));
        }
        let count = c_uint::try_from(config.option_names.len()).map_err(|_| 1)?;
        let mut output = std::ptr::null_mut();
        let status = unsafe {
            bindings::hl_c_backend_create(
                config.isa,
                config.rootfs.map_or(std::ptr::null(), |v| v.as_ptr()),
                config.executable_host.map_or(std::ptr::null(), |v| v.as_ptr()),
                config.executable_fd,
                config.image_plan,
                count,
                config.option_names.as_ptr(),
                config.option_values.as_ptr(),
                config.standard_fds.as_ptr(),
                config.provider_fd,
                config.syscall_context,
                config.syscall_dispatch,
                &raw mut output,
            )
        };
        if status != STATUS_OK {
            return Err(status);
        }
        NonNull::new(output).map(Self).ok_or(1)
    }

    pub fn run(&self, arguments: &[*const c_char]) -> Result<(), i32> {
        let count = c_int::try_from(arguments.len()).map_err(|_| 1)?;
        let status = unsafe { bindings::hl_c_backend_run(self.0.as_ptr(), count, arguments.as_ptr()) };
        (status == STATUS_OK).then_some(()).ok_or(status)
    }

    pub fn request(&self, request: u32, signal: i32) -> Result<(), i32> {
        let status = unsafe { bindings::hl_c_backend_request(self.0.as_ptr(), request, signal) };
        (status == STATUS_OK).then_some(()).ok_or(status)
    }

    #[must_use]
    pub fn exit(&self) -> Exit {
        Exit {
            kind: unsafe { bindings::hl_c_backend_exit_kind(self.0.as_ptr()) },
            status: unsafe { bindings::hl_c_backend_exit_status(self.0.as_ptr()) },
            detail: unsafe { bindings::hl_c_backend_exit_detail(self.0.as_ptr()) },
        }
    }

    #[must_use]
    pub fn translation_count(&self) -> u64 {
        unsafe { bindings::hl_c_backend_translation_count(self.0.as_ptr()) }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        unsafe { bindings::hl_c_backend_destroy(self.0.as_ptr()) };
    }
}
