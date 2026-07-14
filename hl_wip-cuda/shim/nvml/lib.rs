//! Guest cdylib: libnvidia-ml.so.1 — #[no_mangle] nvml* C-ABI exports.
//! Thin trampolines forwarding to hl_cuda::nvml bodies. Soname/version-script by build.rs.
//! (was: C-only hl-gpu/nvml/nvml_shim.c — now the parity oracle.)

/// e.g. nvmlInit_v2 — forwards into the Rust nvml layer.
#[no_mangle]
pub extern "C" fn nvmlInit_v2() -> i32 {
    unimplemented!()
}
