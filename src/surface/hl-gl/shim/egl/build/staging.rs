/// Emit the role-specific cdylib link args: this ONE crate is cross-built TWICE by the top-level
/// `hl-gl/build.rs` (once per output object), selected by `$HL_SHIM_ROLE`:
///
///   * `egl`  (default) → `libEGL.so.1`: OWNS the process-global `State` and exports the shared-state
///     accessor `hl_shim_state_ptr` + the `egl*` surface.
///   * `gles`           → `libGLESv2.so.2`: built with `cfg(gles_client)` so it has NO `State` of its own
///     and instead `DT_NEEDED`s `libEGL.so.1` (from `$HL_SHIM_EGL_LIBDIR`) to import `hl_shim_state_ptr`
///     — matching real Mesa's `libGLESv2`/`libEGL` split while keeping ONE shared context. Exports `gl*`.
///
/// Per-object export RESTRICTION is done at the SOURCE, not here: each `gl*`/`egl*` entry point is
/// `#[cfg_attr(<role>, no_mangle)]` (see `emit_stub` + `src/driver.rs`), so rustc's own cdylib export list
/// already contains only this object's half of the surface. Here we set only the soname / cfg / DT_NEEDED.
pub(super) fn emit_role_link_args() {
    println!("cargo:rerun-if-env-changed=HL_SHIM_ROLE");
    println!("cargo:rerun-if-env-changed=HL_SHIM_EGL_LIBDIR");
    let role = std::env::var("HL_SHIM_ROLE").unwrap_or_else(|_| "egl".to_string());

    let soname = match role.as_str() {
        "gles" => {
            // libGLESv2.so.2: NO `hl_shim_state_ptr` of its own (it is imported from libEGL via DT_NEEDED).
            println!("cargo:rustc-cfg=gles_client");
            let libdir = std::env::var("HL_SHIM_EGL_LIBDIR").unwrap_or_else(|_| {
                panic!("HL_SHIM_ROLE=gles requires HL_SHIM_EGL_LIBDIR (dir holding the staged libEGL.so.1)")
            });
            // DT_NEEDED libEGL.so.1 (kept via --no-as-needed) so the imported shared-state accessor binds.
            println!("cargo:rustc-cdylib-link-arg=-L{libdir}");
            println!("cargo:rustc-cdylib-link-arg=-Wl,--no-as-needed");
            println!("cargo:rustc-cdylib-link-arg=-l:libEGL.so.1");
            println!("cargo:rustc-cdylib-link-arg=-Wl,--as-needed");
            "libGLESv2.so.2"
        }
        "egl" => "libEGL.so.1",
        other => panic!("unknown HL_SHIM_ROLE {other:?} (want `egl` or `gles`)"),
    };
    println!("cargo:rustc-cdylib-link-arg=-Wl,-soname,{soname}");
}
