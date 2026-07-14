//! Guest cdylib: libcudart.so.1 — #[no_mangle] cuda*/__cuda* C-ABI exports.
//! Thin trampolines forwarding to hl_cuda::runtime bodies. Soname/version-script by
//! build.rs (was: hl-gpu/cuda/libcudart.map). (was: hl-shim-cudart/src/lib.rs exports.)

/// e.g. cudaMalloc — forwards into the Rust runtime.
#[no_mangle]
pub extern "C" fn cudaMalloc(_dev_ptr: *mut *mut core::ffi::c_void, _size: usize) -> i32 {
    unimplemented!()
}
