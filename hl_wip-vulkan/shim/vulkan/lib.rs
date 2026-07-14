//! Guest cdylib: `libvk_hl.so.1` — the deployed drop-in Vulkan ICD.
//!
//! DEFERRED this staging pass (like hl-cuda's `shim/`): the `#[no_mangle]` C-ABI surface — the
//! loader-facing `vk_icd*` negotiation/proc-addr entries plus the `vk*` command tail — trampolines
//! into the `hl_vulkan` lowering crate (`create`/`record`/`submit`/`present` services over a shared
//! `hl_gpu::CommandSink`). The soname `.so.1`, the version-script, and the `icd.json` install are
//! applied by `build.rs` in the shim-cdylib pass. This file is a small stub so the standalone lowering
//! crate builds + tests on its own. See `src/lib.rs` "Scope of this staging pass".

/// The Vulkan-loader ICD-interface negotiation entry — forwards into the Rust ICD layer (later pass).
#[no_mangle]
pub extern "C" fn vk_icdNegotiateLoaderICDInterfaceVersion(_version: *mut u32) -> i32 {
    unimplemented!()
}

/// e.g. vkCreateInstance — forwards into the `hl_vulkan` lowering services (later pass).
#[no_mangle]
pub extern "C" fn vkCreateInstance(
    _p_create_info: *const core::ffi::c_void,
    _p_allocator: *const core::ffi::c_void,
    _p_instance: *mut *mut core::ffi::c_void,
) -> i32 {
    unimplemented!()
}
