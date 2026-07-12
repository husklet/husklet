//! Hand-written GLES2 query entry points (`glGetString`, `glGetError`). Values mirror the retiring C
//! shim (`gl_shim.c`). Drawing/state entry points are ported from the C shim incrementally; until then
//! they are generated default stubs (see `build.rs`).

use core::ffi::c_char;

const GL_NO_ERROR: u32 = 0;
const GL_VENDOR: u32 = 0x1F00;
const GL_RENDERER: u32 = 0x1F01;
const GL_VERSION: u32 = 0x1F02;
const GL_EXTENSIONS: u32 = 0x1F03;
const GL_SHADING_LANGUAGE_VERSION: u32 = 0x8B8C;

macro_rules! cstr {
    ($s:literal) => {
        concat!($s, "\0").as_ptr() as *const c_char
    };
}

/// `glGetError` — no error on the covered paths.
#[no_mangle]
pub extern "C" fn glGetError() -> u32 {
    GL_NO_ERROR
}

/// `glGetString` — vendor/renderer/version/GLSL-version/extensions. Returns the ES2 identity by
/// default (glmark2's known-good path); the ES3 identity is selected once context-version tracking is
/// ported (the C shim keyed it on `g_ctx_major`). Returned as `*const u8` per the GL ABI (`GLubyte*`).
#[no_mangle]
pub extern "C" fn glGetString(name: u32) -> *const u8 {
    let s: *const c_char = match name {
        GL_VERSION => cstr!("OpenGL ES 2.0 dd-shim"),
        GL_VENDOR => cstr!("dd"),
        GL_RENDERER => cstr!("dd-metal"),
        GL_SHADING_LANGUAGE_VERSION => cstr!("OpenGL ES GLSL ES 1.00"),
        // A conservative, self-consistent extension set (kept minimal until the ported state machine
        // needs the fuller list the C shim advertises for Chromium's SharedImage path).
        GL_EXTENSIONS => cstr!("GL_OES_element_index_uint GL_OES_texture_npot"),
        _ => cstr!(""),
    };
    s as *const u8
}
