//! Guest cdylib: libcuda.so.1 — #[no_mangle] cu* C-ABI exports.
//! Thin trampolines forwarding to hl_cuda::driver bodies. Soname/version-script
//! applied by build.rs (was: hl-gpu/cuda/libcuda.map). One guest soname per sub-crate.
//! (was: hl-shim-cuda/src/lib.rs exports.)

/// e.g. cuInit — forwards into the Rust driver.
#[no_mangle]
pub extern "C" fn cuInit(_flags: u32) -> i32 {
    unimplemented!()
}
