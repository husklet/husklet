//! Guest cdylib: libGLESv2.so.2 — the FORWARDING stub (soname baked by build.rs). NOT a second full
//! implementation: it carries a `DT_NEEDED` on libEGL.so.1 so every `gl*` symbol resolves to the primary
//! egl shim object. Exists only so apps that `DT_NEEDED` libGLESv2.so.2 (and libwayland-egl.so.1) link.
//! (was: hl-shim-gl GLESv2 forwarding.) DEFERRED this pass — small stub only.
#![allow(non_snake_case)]

/// e.g. glDrawArrays — the forwarding stub resolves this to the primary libEGL object at load time.
#[no_mangle]
pub extern "C" fn glDrawArrays(_mode: u32, _first: i32, _count: i32) {
    unimplemented!()
}
