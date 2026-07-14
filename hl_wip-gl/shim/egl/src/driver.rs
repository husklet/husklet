//! The hand-written `egl*` + `gl*` entry points: marshal the GLES2/EGL C ABI into the `hl_gl` lowering
//! services and (only at swap) submit through the process-global [`crate::state`] sink.
//!
//! Two groups: the **EGL lifecycle** (display / config / context / surface bring-up + present) that
//! returns real, sane values so a dlopen + probe accepts the driver, and the **GLES core render set**
//! (buffer / texture / shader / program creation + the bound draw state + `glDraw*`) that RECORDS into
//! the per-context model exactly as the shared `hl_gl::service::record` ops do — the SAME deferred
//! lowering the in-process test exercises — with the whole frame's IR emitted at `eglSwapBuffers`
//! ([`hl_gl::service::swap`]).
//!
//! Every body is panic-free across the C-ABI seam: raw pointers are null-checked, and a lowering
//! [`hl_gpu::GpuError`] at swap is mapped to the accurate `EGL_*` error via [`hl_gl::result`] (never a
//! false success). The crate builds with `panic = "abort"` as a belt-and-braces second guarantee.

use core::ffi::{c_char, c_void};

use hl_gl::model::context::GlSurface;
use hl_gl::model::glconst::*;
use hl_gl::result::{
    egl_error_from_gpu_error, EGL_FALSE, EGL_TRUE, GL_INVALID_ENUM, GL_INVALID_VALUE, GL_OUT_OF_MEMORY,
};
use hl_gl::service::{query, readpixels, record, swap};

use crate::state::{with, CONFIG_TOKEN, DISPLAY_TOKEN};

// ---- EGL query enums the string getters key on (the GL_* query enums live in hl_gl::glconst) ------

const EGL_VENDOR: i32 = 0x3053;
const EGL_VERSION_Q: i32 = 0x3054;
const EGL_EXTENSIONS_Q: i32 = 0x3055;
const EGL_CLIENT_APIS: i32 = 0x308D;

const EGL_ALPHA_SIZE: i32 = 0x3021;
const EGL_BLUE_SIZE: i32 = 0x3022;
const EGL_GREEN_SIZE: i32 = 0x3023;
const EGL_RED_SIZE: i32 = 0x3024;
const EGL_DEPTH_SIZE: i32 = 0x3025;

// ---- small C-ABI marshalling helpers -------------------------------------------------------------

/// Borrow a `const void*` + length as a byte slice (empty if null / non-positive length).
unsafe fn bytes<'a>(p: *const c_void, n: isize) -> &'a [u8] {
    if p.is_null() || n <= 0 {
        &[]
    } else {
        std::slice::from_raw_parts(p as *const u8, n as usize)
    }
}

/// Concatenate a `glShaderSource` string array (`count` NUL-or-length-delimited fragments) into a String.
unsafe fn join_source(count: i32, string: *const *const c_char, length: *const i32) -> String {
    let mut s = String::new();
    if string.is_null() || count <= 0 {
        return s;
    }
    for i in 0..count as isize {
        let frag = *string.offset(i);
        if frag.is_null() {
            continue;
        }
        let len = if length.is_null() { -1 } else { *length.offset(i) };
        if len < 0 {
            if let Ok(t) = std::ffi::CStr::from_ptr(frag).to_str() {
                s.push_str(t);
            }
        } else {
            let raw = std::slice::from_raw_parts(frag as *const u8, len as usize);
            s.push_str(&String::from_utf8_lossy(raw));
        }
    }
    s
}

/// Borrow a NUL-terminated C string as an owned `String` (`None` if null or not valid UTF-8). Used by the
/// `glGet*Location` name lookups.
unsafe fn cstr(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    core::ffi::CStr::from_ptr(p).to_str().ok().map(|s| s.to_string())
}

/// Convert an uploaded `glTexImage2D` image to the RGBA8 (`w*h*4`) plane the frame builder consumes.
/// Handles the common RGBA/BGRA/RGB `UNSIGNED_BYTE` uploads; an unmodeled format uploads no pixels
/// (returns empty — the texture stays data-less and is truthfully skipped at draw time).
unsafe fn to_rgba8(format: u32, type_: u32, w: i32, h: i32, pixels: *const c_void) -> Vec<u8> {
    if pixels.is_null() || w <= 0 || h <= 0 || type_ != GL_UNSIGNED_BYTE {
        return Vec::new();
    }
    let (w, h) = (w as usize, h as usize);
    match format {
        GL_RGBA | GL_BGRA_EXT => bytes(pixels, (w * h * 4) as isize).to_vec(),
        GL_RGB => {
            let src = bytes(pixels, (w * h * 3) as isize);
            let mut out = Vec::with_capacity(w * h * 4);
            for px in src.chunks_exact(3) {
                out.extend_from_slice(&[px[0], px[1], px[2], 0xFF]);
            }
            out
        }
        _ => Vec::new(),
    }
}

/// Default window-surface dimensions (`$HL_GL_SURFACE_W` / `_H`; 1280x720 fallback). The native window
/// handle carries no size in this model, so the surface is sized from the environment.
fn default_surface_wh() -> (u32, u32) {
    let g = |k: &str, d: u32| std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d);
    (g("HL_GL_SURFACE_W", 1280), g("HL_GL_SURFACE_H", 720))
}

// ==================================================================================================
// EGL: display / initialization / query
// ==================================================================================================

#[no_mangle]
pub extern "C" fn eglGetError() -> i32 {
    with(|s| s.take_egl_error())
}

#[no_mangle]
pub extern "C" fn eglGetDisplay(_display_id: *mut c_void) -> *mut c_void {
    DISPLAY_TOKEN as *mut c_void
}

#[no_mangle]
pub extern "C" fn eglInitialize(_dpy: *mut c_void, major: *mut i32, minor: *mut i32) -> u32 {
    with(|s| s.inited = true);
    // Advertise EGL 1.5.
    unsafe {
        if !major.is_null() {
            *major = 1;
        }
        if !minor.is_null() {
            *minor = 5;
        }
    }
    EGL_TRUE
}

#[no_mangle]
pub extern "C" fn eglTerminate(_dpy: *mut c_void) -> u32 {
    with(|s| s.inited = false);
    EGL_TRUE
}

#[no_mangle]
pub extern "C" fn eglQueryString(_dpy: *mut c_void, name: i32) -> *const c_char {
    let text: &'static [u8] = match name {
        EGL_VENDOR => b"hl-gl\0",
        EGL_VERSION_Q => b"1.5 hl-gl\0",
        EGL_CLIENT_APIS => b"OpenGL_ES\0",
        EGL_EXTENSIONS_Q => b"\0",
        _ => b"\0",
    };
    text.as_ptr() as *const c_char
}

/// `eglGetProcAddress(name)` — resolve a core `egl*`/`gl*` entry point to its real function pointer.
///
/// A GLES app commonly loads its entry points dynamically through this call rather than relying on the
/// dynamic linker; returning null for a core name it then calls would crash it. So we resolve every core
/// `egl*`/`gl*` symbol this object hand-implements to its actual address (cast through `usize` so the
/// `fn` → `*mut c_void` cast is legal). An unknown name (an extension trampoline we do not advertise)
/// still returns null — the spec-legal "not found".
#[no_mangle]
pub extern "C" fn eglGetProcAddress(procname: *const c_char) -> *mut c_void {
    if procname.is_null() {
        return core::ptr::null_mut();
    }
    let name = match unsafe { core::ffi::CStr::from_ptr(procname) }.to_str() {
        Ok(s) => s,
        Err(_) => return core::ptr::null_mut(),
    };
    // Map each core entry point name to its address. `f as usize as *mut c_void` erases the specific
    // `extern "C"` fn type to the opaque function pointer EGL hands back.
    macro_rules! p {
        ($f:path) => {
            $f as usize as *mut c_void
        };
    }
    match name {
        // ---- EGL: display / config / context / surface lifecycle + present ----
        "eglGetError" => p!(eglGetError),
        "eglGetDisplay" => p!(eglGetDisplay),
        "eglInitialize" => p!(eglInitialize),
        "eglTerminate" => p!(eglTerminate),
        "eglQueryString" => p!(eglQueryString),
        "eglGetProcAddress" => p!(eglGetProcAddress),
        "eglBindAPI" => p!(eglBindAPI),
        "eglChooseConfig" => p!(eglChooseConfig),
        "eglGetConfigs" => p!(eglGetConfigs),
        "eglGetConfigAttrib" => p!(eglGetConfigAttrib),
        "eglCreateContext" => p!(eglCreateContext),
        "eglDestroyContext" => p!(eglDestroyContext),
        "eglMakeCurrent" => p!(eglMakeCurrent),
        "eglCreateWindowSurface" => p!(eglCreateWindowSurface),
        "eglDestroySurface" => p!(eglDestroySurface),
        "eglSwapBuffers" => p!(eglSwapBuffers),
        "eglSwapInterval" => p!(eglSwapInterval),
        "eglGetCurrentDisplay" => p!(eglGetCurrentDisplay),
        "eglGetCurrentContext" => p!(eglGetCurrentContext),
        "eglGetCurrentSurface" => p!(eglGetCurrentSurface),
        // ---- GLES: error / string / query ----
        "glGetError" => p!(glGetError),
        "glGetString" => p!(glGetString),
        "glGetIntegerv" => p!(glGetIntegerv),
        "glGetFloatv" => p!(glGetFloatv),
        "glGetBooleanv" => p!(glGetBooleanv),
        "glPixelStorei" => p!(glPixelStorei),
        // ---- GLES: shader / program introspection ----
        "glGetShaderiv" => p!(glGetShaderiv),
        "glGetProgramiv" => p!(glGetProgramiv),
        "glGetShaderInfoLog" => p!(glGetShaderInfoLog),
        "glGetProgramInfoLog" => p!(glGetProgramInfoLog),
        "glGetUniformLocation" => p!(glGetUniformLocation),
        "glGetAttribLocation" => p!(glGetAttribLocation),
        "glBindAttribLocation" => p!(glBindAttribLocation),
        // ---- GLES: buffers ----
        "glGenBuffers" => p!(glGenBuffers),
        "glBindBuffer" => p!(glBindBuffer),
        "glBufferData" => p!(glBufferData),
        "glBufferSubData" => p!(glBufferSubData),
        "glDeleteBuffers" => p!(glDeleteBuffers),
        // ---- GLES: textures ----
        "glGenTextures" => p!(glGenTextures),
        "glActiveTexture" => p!(glActiveTexture),
        "glBindTexture" => p!(glBindTexture),
        "glTexImage2D" => p!(glTexImage2D),
        "glTexParameteri" => p!(glTexParameteri),
        "glDeleteTextures" => p!(glDeleteTextures),
        // ---- GLES: shaders + programs ----
        "glCreateShader" => p!(glCreateShader),
        "glShaderSource" => p!(glShaderSource),
        "glCompileShader" => p!(glCompileShader),
        "glCreateProgram" => p!(glCreateProgram),
        "glAttachShader" => p!(glAttachShader),
        "glLinkProgram" => p!(glLinkProgram),
        "glUseProgram" => p!(glUseProgram),
        "glUniform1i" => p!(glUniform1i),
        "glUniform2i" => p!(glUniform2i),
        "glUniform3i" => p!(glUniform3i),
        "glUniform4i" => p!(glUniform4i),
        "glUniform1iv" => p!(glUniform1iv),
        "glUniform2iv" => p!(glUniform2iv),
        "glUniform3iv" => p!(glUniform3iv),
        "glUniform4iv" => p!(glUniform4iv),
        "glUniform1f" => p!(glUniform1f),
        "glUniform2f" => p!(glUniform2f),
        "glUniform3f" => p!(glUniform3f),
        "glUniform4f" => p!(glUniform4f),
        "glUniform1fv" => p!(glUniform1fv),
        "glUniform2fv" => p!(glUniform2fv),
        "glUniform3fv" => p!(glUniform3fv),
        "glUniform4fv" => p!(glUniform4fv),
        "glUniformMatrix2fv" => p!(glUniformMatrix2fv),
        "glUniformMatrix3fv" => p!(glUniformMatrix3fv),
        "glUniformMatrix4fv" => p!(glUniformMatrix4fv),
        // ---- GLES: vertex attributes + fixed-function state ----
        "glVertexAttribPointer" => p!(glVertexAttribPointer),
        "glEnableVertexAttribArray" => p!(glEnableVertexAttribArray),
        "glDisableVertexAttribArray" => p!(glDisableVertexAttribArray),
        "glClearColor" => p!(glClearColor),
        "glClearDepthf" => p!(glClearDepthf),
        "glViewport" => p!(glViewport),
        "glScissor" => p!(glScissor),
        "glEnable" => p!(glEnable),
        "glDisable" => p!(glDisable),
        "glBlendFunc" => p!(glBlendFunc),
        "glBlendFuncSeparate" => p!(glBlendFuncSeparate),
        "glDepthFunc" => p!(glDepthFunc),
        "glDepthMask" => p!(glDepthMask),
        "glCullFace" => p!(glCullFace),
        "glFrontFace" => p!(glFrontFace),
        // ---- GLES: draw recording ----
        "glClear" => p!(glClear),
        "glDrawArrays" => p!(glDrawArrays),
        "glDrawElements" => p!(glDrawElements),
        "glReadPixels" => p!(glReadPixels),
        // Unknown / extension name we do not advertise → spec-legal "not found".
        _ => core::ptr::null_mut(),
    }
}

#[no_mangle]
pub extern "C" fn eglBindAPI(_api: u32) -> u32 {
    EGL_TRUE
}

// ==================================================================================================
// EGL: config selection
// ==================================================================================================

#[no_mangle]
pub extern "C" fn eglChooseConfig(
    _dpy: *mut c_void,
    _attrib_list: *const i32,
    configs: *mut *mut c_void,
    config_size: i32,
    num_config: *mut i32,
) -> u32 {
    unsafe {
        if !configs.is_null() && config_size >= 1 {
            *configs = CONFIG_TOKEN as *mut c_void;
        }
        if !num_config.is_null() {
            *num_config = if config_size >= 1 || configs.is_null() { 1 } else { 0 };
        }
    }
    EGL_TRUE
}

#[no_mangle]
pub extern "C" fn eglGetConfigs(
    _dpy: *mut c_void,
    configs: *mut *mut c_void,
    config_size: i32,
    num_config: *mut i32,
) -> u32 {
    unsafe {
        if !configs.is_null() && config_size >= 1 {
            *configs = CONFIG_TOKEN as *mut c_void;
        }
        if !num_config.is_null() {
            *num_config = 1;
        }
    }
    EGL_TRUE
}

#[no_mangle]
pub extern "C" fn eglGetConfigAttrib(
    _dpy: *mut c_void,
    _config: *mut c_void,
    attribute: i32,
    value: *mut i32,
) -> u32 {
    if value.is_null() {
        return EGL_FALSE;
    }
    let v = match attribute {
        EGL_RED_SIZE | EGL_GREEN_SIZE | EGL_BLUE_SIZE | EGL_ALPHA_SIZE => 8,
        EGL_DEPTH_SIZE => 24,
        _ => 0,
    };
    unsafe { *value = v };
    EGL_TRUE
}

// ==================================================================================================
// EGL: context / surface lifecycle + present
// ==================================================================================================

#[no_mangle]
pub extern "C" fn eglCreateContext(
    _dpy: *mut c_void,
    _config: *mut c_void,
    _share_context: *mut c_void,
    _attrib_list: *const i32,
) -> *mut c_void {
    with(|s| s.mint_token())
}

#[no_mangle]
pub extern "C" fn eglDestroyContext(_dpy: *mut c_void, ctx: *mut c_void) -> u32 {
    with(|s| {
        if s.current_ctx == ctx as usize {
            s.current_ctx = 0;
        }
    });
    EGL_TRUE
}

#[no_mangle]
pub extern "C" fn eglMakeCurrent(
    _dpy: *mut c_void,
    draw: *mut c_void,
    _read: *mut c_void,
    ctx: *mut c_void,
) -> u32 {
    with(|s| {
        s.current_ctx = ctx as usize;
        s.current_surface = draw as usize;
    });
    EGL_TRUE
}

#[no_mangle]
pub extern "C" fn eglCreateWindowSurface(
    _dpy: *mut c_void,
    _config: *mut c_void,
    _win: *mut c_void,
    _attrib_list: *const i32,
) -> *mut c_void {
    let (width, height) = default_surface_wh();
    with(|s| {
        s.ctx.surf = GlSurface { have: true, width, height };
        let tok = s.mint_token();
        s.current_surface = tok as usize;
        tok
    })
}

#[no_mangle]
pub extern "C" fn eglDestroySurface(_dpy: *mut c_void, surface: *mut c_void) -> u32 {
    with(|s| {
        if s.current_surface == surface as usize {
            s.current_surface = 0;
        }
        s.ctx.surf.have = false;
    });
    EGL_TRUE
}

/// `eglSwapBuffers` — the one sink-touching op: lower + submit + present the recorded frame. On a sink
/// error the frame is retained and `EGL_*` is registered (surfaced by the next `eglGetError`).
#[no_mangle]
pub extern "C" fn eglSwapBuffers(_dpy: *mut c_void, _surface: *mut c_void) -> u32 {
    with(|s| match swap::swap_buffers(&mut s.ctx, &mut s.sink) {
        Ok(_) => EGL_TRUE,
        Err(e) => {
            s.set_egl_error(egl_error_from_gpu_error(&e));
            EGL_FALSE
        }
    })
}

#[no_mangle]
pub extern "C" fn eglSwapInterval(_dpy: *mut c_void, _interval: i32) -> u32 {
    EGL_TRUE
}

#[no_mangle]
pub extern "C" fn eglGetCurrentDisplay() -> *mut c_void {
    with(|s| if s.inited { DISPLAY_TOKEN as *mut c_void } else { core::ptr::null_mut() })
}

#[no_mangle]
pub extern "C" fn eglGetCurrentContext() -> *mut c_void {
    with(|s| s.current_ctx as *mut c_void)
}

#[no_mangle]
pub extern "C" fn eglGetCurrentSurface(_readdraw: i32) -> *mut c_void {
    with(|s| s.current_surface as *mut c_void)
}

// ==================================================================================================
// GLES: error / string
// ==================================================================================================

/// `glGetError` — read + clear the context's error flag. A real app loops on this after batches of GL
/// calls, so it must return the first error raised (GL keeps it until read) and then reset to
/// `GL_NO_ERROR`.
#[no_mangle]
pub extern "C" fn glGetError() -> u32 {
    with(|s| s.ctx.take_gl_error())
}

/// `glGetString(name)` — the driver's GLES3 identity strings (`GL_VERSION` = "OpenGL ES 3.0 …", vendor /
/// renderer / GLSL version / extensions). Served from [`query::gl_string`] so the guest-visible identity
/// is defined once and unit-tested. Never null: a GLES app dereferences the result unconditionally.
#[no_mangle]
pub extern "C" fn glGetString(name: u32) -> *const u8 {
    query::gl_string(name).as_ptr()
}

// ==================================================================================================
// GLES: state / capability queries (glGet*) — read-only; served from the modeled limits + live state
// ==================================================================================================

/// `glGetIntegerv(pname, data)` — capability limits + bound-object / fixed-function state. Writes the
/// modeled value(s) for `pname` (1, 2, or 4 ints); a null `data` or unknown `pname` is handled by
/// [`query::get_integerv`] (unknown → a single `0`).
#[no_mangle]
pub extern "C" fn glGetIntegerv(pname: u32, data: *mut i32) {
    if data.is_null() {
        return;
    }
    let mut buf = [0i32; 4];
    let n = with(|s| query::get_integerv(&s.ctx, pname, &mut buf));
    unsafe {
        for i in 0..n {
            *data.add(i) = buf[i];
        }
    }
}

/// `glGetFloatv(pname, data)` — the float-typed state (clear color, depth-clear value, line width, …).
#[no_mangle]
pub extern "C" fn glGetFloatv(pname: u32, data: *mut f32) {
    if data.is_null() {
        return;
    }
    let mut buf = [0f32; 4];
    let n = with(|s| query::get_floatv(&s.ctx, pname, &mut buf));
    unsafe {
        for i in 0..n {
            *data.add(i) = buf[i];
        }
    }
}

/// `glGetBooleanv(pname, data)` — the boolean-typed state (fixed-function enables + depth write mask).
#[no_mangle]
pub extern "C" fn glGetBooleanv(pname: u32, data: *mut u8) {
    if data.is_null() {
        return;
    }
    let mut buf = [0u8; 4];
    let n = with(|s| query::get_booleanv(&s.ctx, pname, &mut buf));
    unsafe {
        for i in 0..n {
            *data.add(i) = buf[i];
        }
    }
}

/// `glPixelStorei(pname, param)` — record a pack/unpack pixel-store parameter (e.g. `GL_UNPACK_ALIGNMENT`,
/// which affects texture-upload row packing). An out-of-range value raises `GL_INVALID_VALUE` (first-error
/// wins); see [`record::pixel_store`].
#[no_mangle]
pub extern "C" fn glPixelStorei(pname: u32, param: i32) {
    with(|s| record::pixel_store(&mut s.ctx, pname, param));
}

// ==================================================================================================
// GLES: buffers
// ==================================================================================================

#[no_mangle]
pub extern "C" fn glGenBuffers(n: i32, buffers: *mut u32) {
    if buffers.is_null() || n <= 0 {
        return;
    }
    with(|s| unsafe {
        for i in 0..n as isize {
            *buffers.offset(i) = record::gen_buffer(&mut s.ctx);
        }
    });
}

#[no_mangle]
pub extern "C" fn glBindBuffer(target: u32, buffer: u32) {
    with(|s| record::bind_buffer(&mut s.ctx, target, buffer));
}

#[no_mangle]
pub extern "C" fn glBufferData(target: u32, size: isize, data: *const c_void, usage: u32) {
    let d = unsafe { bytes(data, size) }.to_vec();
    with(|s| record::buffer_data(&mut s.ctx, target, &d, usage));
}

#[no_mangle]
pub extern "C" fn glBufferSubData(target: u32, offset: isize, size: isize, data: *const c_void) {
    let d = unsafe { bytes(data, size) }.to_vec();
    with(|s| record::buffer_sub_data(&mut s.ctx, target, offset.max(0) as usize, &d));
}

#[no_mangle]
pub extern "C" fn glDeleteBuffers(n: i32, buffers: *const u32) {
    if buffers.is_null() || n <= 0 {
        return;
    }
    with(|s| unsafe {
        for i in 0..n as isize {
            record::delete_buffer(&mut s.ctx, *buffers.offset(i));
        }
    });
}

// ==================================================================================================
// GLES: textures
// ==================================================================================================

#[no_mangle]
pub extern "C" fn glGenTextures(n: i32, textures: *mut u32) {
    if textures.is_null() || n <= 0 {
        return;
    }
    with(|s| unsafe {
        for i in 0..n as isize {
            *textures.offset(i) = record::gen_texture(&mut s.ctx);
        }
    });
}

#[no_mangle]
pub extern "C" fn glActiveTexture(texture: u32) {
    with(|s| record::active_texture(&mut s.ctx, texture));
}

#[no_mangle]
pub extern "C" fn glBindTexture(target: u32, texture: u32) {
    with(|s| record::bind_texture(&mut s.ctx, target, texture));
}

#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glTexImage2D(
    _target: u32,
    _level: i32,
    _internalformat: i32,
    width: i32,
    height: i32,
    _border: i32,
    format: u32,
    type_: u32,
    pixels: *const c_void,
) {
    let rgba = unsafe { to_rgba8(format, type_, width, height, pixels) };
    with(|s| record::tex_image_2d(&mut s.ctx, width, height, &rgba));
}

#[no_mangle]
pub extern "C" fn glTexParameteri(_target: u32, pname: u32, param: i32) {
    with(|s| record::tex_parameter(&mut s.ctx, pname, param as u32));
}

#[no_mangle]
pub extern "C" fn glDeleteTextures(n: i32, textures: *const u32) {
    if textures.is_null() || n <= 0 {
        return;
    }
    with(|s| unsafe {
        for i in 0..n as isize {
            record::delete_texture(&mut s.ctx, *textures.offset(i));
        }
    });
}

// ==================================================================================================
// GLES: shaders + programs
// ==================================================================================================

#[no_mangle]
pub extern "C" fn glCreateShader(type_: u32) -> u32 {
    with(|s| record::create_shader(&mut s.ctx, type_))
}

#[no_mangle]
pub extern "C" fn glShaderSource(
    shader: u32,
    count: i32,
    string: *const *const c_char,
    length: *const i32,
) {
    let src = unsafe { join_source(count, string, length) };
    with(|s| record::shader_source(&mut s.ctx, shader, &src));
}

#[no_mangle]
pub extern "C" fn glCompileShader(shader: u32) {
    with(|s| record::compile_shader(&mut s.ctx, shader));
}

#[no_mangle]
pub extern "C" fn glCreateProgram() -> u32 {
    with(|s| record::create_program(&mut s.ctx))
}

#[no_mangle]
pub extern "C" fn glAttachShader(program: u32, shader: u32) {
    with(|s| record::attach_shader(&mut s.ctx, program, shader));
}

#[no_mangle]
pub extern "C" fn glLinkProgram(program: u32) {
    with(|s| {
        let _ = record::link_program(&mut s.ctx, program);
    });
}

#[no_mangle]
pub extern "C" fn glUseProgram(program: u32) {
    with(|s| record::use_program(&mut s.ctx, program));
}

// ---- shader / program introspection (glGet*iv / glGet*InfoLog / glGet*Location) -------------------
//
// A real GLES app queries COMPILE_STATUS / LINK_STATUS after every compile+link (and bails on failure),
// and resolves uniform/attribute locations at program bind. These serve the modeled compile/link state +
// the reflected uniform/attribute tables (see `hl_gl::service::query`).

/// `glGetShaderiv(shader, pname, params)` — `GL_COMPILE_STATUS` (TRUE for a compiled shader),
/// `GL_INFO_LOG_LENGTH` (0), `GL_SHADER_SOURCE_LENGTH`, `GL_SHADER_TYPE`, `GL_DELETE_STATUS`.
#[no_mangle]
pub extern "C" fn glGetShaderiv(shader: u32, pname: u32, params: *mut i32) {
    if params.is_null() {
        return;
    }
    let v = with(|s| query::get_shaderiv(&s.ctx, shader, pname));
    unsafe { *params = v };
}

/// `glGetProgramiv(program, pname, params)` — `GL_LINK_STATUS`/`GL_VALIDATE_STATUS` (TRUE once linked),
/// `GL_INFO_LOG_LENGTH` (0), `GL_ATTACHED_SHADERS`, `GL_ACTIVE_UNIFORMS`, `GL_ACTIVE_ATTRIBUTES`.
#[no_mangle]
pub extern "C" fn glGetProgramiv(program: u32, pname: u32, params: *mut i32) {
    if params.is_null() {
        return;
    }
    let v = with(|s| query::get_programiv(&s.ctx, program, pname));
    unsafe { *params = v };
}

/// `glGetShaderInfoLog(shader, buf_size, length, info_log)` — the shader compiled successfully, so the
/// diagnostic log is empty (an empty NUL-terminated string, length 0).
#[no_mangle]
pub extern "C" fn glGetShaderInfoLog(
    _shader: u32,
    buf_size: i32,
    length: *mut i32,
    info_log: *mut c_char,
) {
    unsafe { write_empty_info_log(buf_size, length, info_log) };
}

/// `glGetProgramInfoLog(program, buf_size, length, info_log)` — the program linked successfully, so the
/// diagnostic log is empty (an empty NUL-terminated string, length 0).
#[no_mangle]
pub extern "C" fn glGetProgramInfoLog(
    _program: u32,
    buf_size: i32,
    length: *mut i32,
    info_log: *mut c_char,
) {
    unsafe { write_empty_info_log(buf_size, length, info_log) };
}

/// Emit an empty info log per the `glGet*InfoLog` contract: write a single NUL into `info_log` (when a
/// buffer of at least one byte is given) and report a written length of `0` (the count excludes the
/// terminator). Null-safe on every pointer.
unsafe fn write_empty_info_log(buf_size: i32, length: *mut i32, info_log: *mut c_char) {
    if !info_log.is_null() && buf_size > 0 {
        *info_log = 0;
    }
    if !length.is_null() {
        *length = 0;
    }
}

/// `glGetUniformLocation(program, name)` — the location of a uniform in the linked program (its reflected
/// declaration index), or `-1` if `name` is not an active uniform. Null-safe.
#[no_mangle]
pub extern "C" fn glGetUniformLocation(program: u32, name: *const c_char) -> i32 {
    let want = match unsafe { cstr(name) } {
        Some(s) => s,
        None => return -1,
    };
    with(|s| query::uniform_location(&s.ctx, program, &want))
}

/// `glGetAttribLocation(program, name)` — the vertex attribute's declaration-order slot in the linked
/// program, or `-1` if `name` is not an active attribute. Null-safe.
#[no_mangle]
pub extern "C" fn glGetAttribLocation(program: u32, name: *const c_char) -> i32 {
    let want = match unsafe { cstr(name) } {
        Some(s) => s,
        None => return -1,
    };
    with(|s| query::attrib_location(&s.ctx, program, &want))
}

/// `glBindAttribLocation(program, index, name)` — a no-op: the GLSL→MSL translator binds attributes by
/// declaration order (`[[attribute(N)]]`), so an app-requested binding cannot be honored without
/// re-linking. This matches the reference shim; `glGetAttribLocation` reports the declaration-order slot.
#[no_mangle]
pub extern "C" fn glBindAttribLocation(_program: u32, _index: u32, _name: *const c_char) {}

/// `glUniform1i` — in this simplified model an integer uniform binds a sampler: `location` selects the
/// sampler's declaration index, `v0` the texture unit (mirrors the lowering test's `uniform_sampler`).
#[no_mangle]
pub extern "C" fn glUniform1i(location: i32, v0: i32) {
    if location < 0 {
        return;
    }
    with(|s| record::uniform_sampler(&mut s.ctx, location as usize, v0));
}

// ---- data uniforms (record into the bound program's uniform block; shipped at binding 1 at draw) -----
//
// `location` is the uniform's declaration index — the same convention `glUniform1i`/`uniform_sampler`
// use for samplers. `glUniform1i` stays sampler-only (above); the integer variants below and every float
// variant write the value's little-endian bytes into the uniform block at the member's reflected offset.

/// Write `bytes` into data uniform `location` of the bound program (no-op for a negative location).
fn set_uniform(location: i32, bytes: &[u8]) {
    if location < 0 {
        return;
    }
    with(|s| record::uniform_at(&mut s.ctx, location as usize, bytes));
}

/// Marshal a slice of scalars into little-endian bytes.
fn le_f32(vs: &[f32]) -> Vec<u8> {
    let mut b = Vec::with_capacity(vs.len() * 4);
    for v in vs {
        b.extend_from_slice(&v.to_le_bytes());
    }
    b
}
fn le_i32(vs: &[i32]) -> Vec<u8> {
    let mut b = Vec::with_capacity(vs.len() * 4);
    for v in vs {
        b.extend_from_slice(&v.to_le_bytes());
    }
    b
}

/// Borrow a `count`×`n` scalar array (`glUniform{N}{f,i}v` value), empty if null / non-positive count.
unsafe fn slice_f32<'a>(value: *const f32, count: i32, n: usize) -> &'a [f32] {
    if value.is_null() || count <= 0 {
        &[]
    } else {
        std::slice::from_raw_parts(value, count as usize * n)
    }
}
unsafe fn slice_i32<'a>(value: *const i32, count: i32, n: usize) -> &'a [i32] {
    if value.is_null() || count <= 0 {
        &[]
    } else {
        std::slice::from_raw_parts(value, count as usize * n)
    }
}

/// Marshal a column-major GL matrix array into MSL `floatNxN` struct layout: `count` matrices, each `n`
/// columns; every column is padded to 4 floats for `n == 3` (MSL `float3x3` has a 16-byte column stride),
/// else `n` floats. `transpose` swaps row/column when reading the GL source.
unsafe fn mat_bytes(n: usize, count: i32, transpose: bool, value: *const f32) -> Vec<u8> {
    if value.is_null() || count <= 0 {
        return Vec::new();
    }
    let src = std::slice::from_raw_parts(value, count as usize * n * n);
    let col_floats = if n == 3 { 4 } else { n };
    let mut out = Vec::with_capacity(count as usize * n * col_floats * 4);
    for m in 0..count as usize {
        let base = m * n * n;
        for col in 0..n {
            for row in 0..n {
                let v = if transpose { src[base + row * n + col] } else { src[base + col * n + row] };
                out.extend_from_slice(&v.to_le_bytes());
            }
            for _ in n..col_floats {
                out.extend_from_slice(&0f32.to_le_bytes());
            }
        }
    }
    out
}

#[no_mangle]
pub extern "C" fn glUniform1f(location: i32, v0: f32) {
    set_uniform(location, &le_f32(&[v0]));
}
#[no_mangle]
pub extern "C" fn glUniform2f(location: i32, v0: f32, v1: f32) {
    set_uniform(location, &le_f32(&[v0, v1]));
}
#[no_mangle]
pub extern "C" fn glUniform3f(location: i32, v0: f32, v1: f32, v2: f32) {
    set_uniform(location, &le_f32(&[v0, v1, v2]));
}
#[no_mangle]
pub extern "C" fn glUniform4f(location: i32, v0: f32, v1: f32, v2: f32, v3: f32) {
    set_uniform(location, &le_f32(&[v0, v1, v2, v3]));
}

#[no_mangle]
pub extern "C" fn glUniform2i(location: i32, v0: i32, v1: i32) {
    set_uniform(location, &le_i32(&[v0, v1]));
}
#[no_mangle]
pub extern "C" fn glUniform3i(location: i32, v0: i32, v1: i32, v2: i32) {
    set_uniform(location, &le_i32(&[v0, v1, v2]));
}
#[no_mangle]
pub extern "C" fn glUniform4i(location: i32, v0: i32, v1: i32, v2: i32, v3: i32) {
    set_uniform(location, &le_i32(&[v0, v1, v2, v3]));
}

#[no_mangle]
pub extern "C" fn glUniform1fv(location: i32, count: i32, value: *const f32) {
    set_uniform(location, &le_f32(unsafe { slice_f32(value, count, 1) }));
}
#[no_mangle]
pub extern "C" fn glUniform2fv(location: i32, count: i32, value: *const f32) {
    set_uniform(location, &le_f32(unsafe { slice_f32(value, count, 2) }));
}
#[no_mangle]
pub extern "C" fn glUniform3fv(location: i32, count: i32, value: *const f32) {
    set_uniform(location, &le_f32(unsafe { slice_f32(value, count, 3) }));
}
#[no_mangle]
pub extern "C" fn glUniform4fv(location: i32, count: i32, value: *const f32) {
    set_uniform(location, &le_f32(unsafe { slice_f32(value, count, 4) }));
}

#[no_mangle]
pub extern "C" fn glUniform1iv(location: i32, count: i32, value: *const i32) {
    set_uniform(location, &le_i32(unsafe { slice_i32(value, count, 1) }));
}
#[no_mangle]
pub extern "C" fn glUniform2iv(location: i32, count: i32, value: *const i32) {
    set_uniform(location, &le_i32(unsafe { slice_i32(value, count, 2) }));
}
#[no_mangle]
pub extern "C" fn glUniform3iv(location: i32, count: i32, value: *const i32) {
    set_uniform(location, &le_i32(unsafe { slice_i32(value, count, 3) }));
}
#[no_mangle]
pub extern "C" fn glUniform4iv(location: i32, count: i32, value: *const i32) {
    set_uniform(location, &le_i32(unsafe { slice_i32(value, count, 4) }));
}

#[no_mangle]
pub extern "C" fn glUniformMatrix2fv(location: i32, count: i32, transpose: u8, value: *const f32) {
    set_uniform(location, &unsafe { mat_bytes(2, count, transpose != 0, value) });
}
#[no_mangle]
pub extern "C" fn glUniformMatrix3fv(location: i32, count: i32, transpose: u8, value: *const f32) {
    set_uniform(location, &unsafe { mat_bytes(3, count, transpose != 0, value) });
}
#[no_mangle]
pub extern "C" fn glUniformMatrix4fv(location: i32, count: i32, transpose: u8, value: *const f32) {
    set_uniform(location, &unsafe { mat_bytes(4, count, transpose != 0, value) });
}

// ==================================================================================================
// GLES: vertex attributes + fixed-function state
// ==================================================================================================

#[no_mangle]
pub extern "C" fn glVertexAttribPointer(
    index: u32,
    size: i32,
    type_: u32,
    normalized: u8,
    stride: i32,
    pointer: *const c_void,
) {
    with(|s| {
        record::vertex_attrib_pointer(
            &mut s.ctx,
            index as usize,
            size,
            type_,
            normalized != 0,
            stride,
            pointer as usize,
        )
    });
}

#[no_mangle]
pub extern "C" fn glEnableVertexAttribArray(index: u32) {
    with(|s| record::enable_vertex_attrib(&mut s.ctx, index as usize));
}

#[no_mangle]
pub extern "C" fn glDisableVertexAttribArray(index: u32) {
    with(|s| record::disable_vertex_attrib(&mut s.ctx, index as usize));
}

#[no_mangle]
pub extern "C" fn glClearColor(red: f32, green: f32, blue: f32, alpha: f32) {
    with(|s| record::clear_color(&mut s.ctx, [red, green, blue, alpha]));
}

#[no_mangle]
pub extern "C" fn glViewport(x: i32, y: i32, width: i32, height: i32) {
    with(|s| record::viewport(&mut s.ctx, [x, y, width, height]));
}

#[no_mangle]
pub extern "C" fn glScissor(x: i32, y: i32, width: i32, height: i32) {
    with(|s| record::scissor(&mut s.ctx, [x, y, width, height]));
}

#[no_mangle]
pub extern "C" fn glEnable(cap: u32) {
    with(|s| record::enable(&mut s.ctx, cap));
}

#[no_mangle]
pub extern "C" fn glDisable(cap: u32) {
    with(|s| record::disable(&mut s.ctx, cap));
}

#[no_mangle]
pub extern "C" fn glClearDepthf(d: f32) {
    with(|s| record::clear_depth(&mut s.ctx, d));
}

#[no_mangle]
pub extern "C" fn glBlendFunc(sfactor: u32, dfactor: u32) {
    with(|s| record::blend_func(&mut s.ctx, sfactor, dfactor));
}

#[no_mangle]
pub extern "C" fn glBlendFuncSeparate(src_rgb: u32, dst_rgb: u32, src_alpha: u32, dst_alpha: u32) {
    with(|s| record::blend_func_separate(&mut s.ctx, src_rgb, dst_rgb, src_alpha, dst_alpha));
}

#[no_mangle]
pub extern "C" fn glDepthFunc(func: u32) {
    with(|s| record::depth_func(&mut s.ctx, func));
}

#[no_mangle]
pub extern "C" fn glDepthMask(flag: u8) {
    with(|s| record::depth_mask(&mut s.ctx, flag != 0));
}

#[no_mangle]
pub extern "C" fn glCullFace(mode: u32) {
    with(|s| record::cull_face(&mut s.ctx, mode));
}

#[no_mangle]
pub extern "C" fn glFrontFace(mode: u32) {
    with(|s| record::front_face(&mut s.ctx, mode));
}

// ==================================================================================================
// GLES: draw recording (frame draw-list; IR lowered at eglSwapBuffers)
// ==================================================================================================

#[no_mangle]
pub extern "C" fn glClear(_mask: u32) {
    with(|s| record::clear(&mut s.ctx));
}

#[no_mangle]
pub extern "C" fn glDrawArrays(mode: u32, first: i32, count: i32) {
    with(|s| record::draw_arrays(&mut s.ctx, mode, first, count));
}

#[no_mangle]
pub extern "C" fn glDrawElements(mode: u32, count: i32, type_: u32, indices: *const c_void) {
    with(|s| record::draw_elements(&mut s.ctx, mode, count, type_, indices as usize));
}

// ==================================================================================================
// GLES: readback (device→host — the GL equivalent of cuMemcpyDtoH)
// ==================================================================================================

/// `glReadPixels(x, y, w, h, format, type, pixels)` — render the recorded frame and read the requested
/// rectangle of the resulting render target back into `pixels`. Only `GL_UNSIGNED_BYTE` RGBA/BGRA/RGB is
/// modeled; the readback goes through the `hl_gl` service (render → `CopyTextureToBuffer` → `read_buffer`),
/// the same device→host port as cuda's DtoH.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glReadPixels(
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    format: u32,
    type_: u32,
    pixels: *mut c_void,
) {
    // Record the first GL error and bail (GL keeps the first error until glGetError clears it).
    let fail = |e: u32| with(|s| s.ctx.set_gl_error(e));
    if type_ != GL_UNSIGNED_BYTE {
        fail(GL_INVALID_ENUM);
        return;
    }
    let bpp = match format {
        GL_RGBA | GL_BGRA_EXT => 4usize,
        GL_RGB => 3,
        _ => {
            fail(GL_INVALID_ENUM);
            return;
        }
    };
    if width < 0 || height < 0 {
        fail(GL_INVALID_VALUE);
        return;
    }
    if width == 0 || height == 0 {
        return;
    }
    if pixels.is_null() {
        fail(GL_INVALID_VALUE);
        return;
    }
    let packed = with(|s| readpixels::read_pixels(&mut s.ctx, &mut s.sink, x, y, width, height, format));
    match packed {
        Ok(bytes) => {
            let n = bytes.len().min(width as usize * height as usize * bpp);
            unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), pixels as *mut u8, n) };
        }
        Err(e) => with(|s| {
            s.ctx.set_gl_error(GL_OUT_OF_MEMORY);
            s.set_egl_error(egl_error_from_gpu_error(&e));
        }),
    }
}
