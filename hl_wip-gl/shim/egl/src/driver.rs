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
use hl_gl::service::{compute, es3, map, query, readpixels, record, swap, sync};

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
        "glGetStringi" => p!(glGetStringi),
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
        "glGetActiveUniform" => p!(glGetActiveUniform),
        "glGetActiveAttrib" => p!(glGetActiveAttrib),
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
        "glTexParameterf" => p!(glTexParameterf),
        "glTexParameterfv" => p!(glTexParameterfv),
        "glTexParameteriv" => p!(glTexParameteriv),
        "glGenerateMipmap" => p!(glGenerateMipmap),
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
        "glVertexAttribDivisor" => p!(glVertexAttribDivisor),
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
        "glDrawArraysInstanced" => p!(glDrawArraysInstanced),
        "glDrawElementsInstanced" => p!(glDrawElementsInstanced),
        "glReadPixels" => p!(glReadPixels),
        // ---- GLES: vertex array objects ----
        "glGenVertexArrays" => p!(glGenVertexArrays),
        "glBindVertexArray" => p!(glBindVertexArray),
        "glDeleteVertexArrays" => p!(glDeleteVertexArrays),
        "glIsVertexArray" => p!(glIsVertexArray),
        // ---- GLES: framebuffer + renderbuffer objects (offscreen render targets) ----
        "glGenFramebuffers" => p!(glGenFramebuffers),
        "glBindFramebuffer" => p!(glBindFramebuffer),
        "glDeleteFramebuffers" => p!(glDeleteFramebuffers),
        "glIsFramebuffer" => p!(glIsFramebuffer),
        "glCheckFramebufferStatus" => p!(glCheckFramebufferStatus),
        "glFramebufferTexture2D" => p!(glFramebufferTexture2D),
        "glGenRenderbuffers" => p!(glGenRenderbuffers),
        "glBindRenderbuffer" => p!(glBindRenderbuffer),
        "glDeleteRenderbuffers" => p!(glDeleteRenderbuffers),
        "glIsRenderbuffer" => p!(glIsRenderbuffer),
        "glRenderbufferStorage" => p!(glRenderbufferStorage),
        "glFramebufferRenderbuffer" => p!(glFramebufferRenderbuffer),
        "glBlitFramebuffer" => p!(glBlitFramebuffer),
        // ---- GLES3.1: compute dispatch ----
        "glDispatchCompute" => p!(glDispatchCompute),
        "glDispatchComputeIndirect" => p!(glDispatchComputeIndirect),
        // ---- GLES3.0: sync objects (GLsync) ----
        "glFenceSync" => p!(glFenceSync),
        "glClientWaitSync" => p!(glClientWaitSync),
        "glWaitSync" => p!(glWaitSync),
        "glDeleteSync" => p!(glDeleteSync),
        "glIsSync" => p!(glIsSync),
        "glGetSynciv" => p!(glGetSynciv),
        // ---- GLES3.0: indexed buffer bindings (UBO/SSBO) ----
        "glBindBufferBase" => p!(glBindBufferBase),
        "glBindBufferRange" => p!(glBindBufferRange),
        // ---- GLES3.0: PBO-style buffer mapping ----
        "glMapBufferRange" => p!(glMapBufferRange),
        "glUnmapBuffer" => p!(glUnmapBuffer),
        "glFlushMappedBufferRange" => p!(glFlushMappedBufferRange),
        // ---- GLES3.0: MRT draw/read buffer selection ----
        "glDrawBuffers" => p!(glDrawBuffers),
        "glReadBuffer" => p!(glReadBuffer),
        // ---- GLES3.0: sampler objects ----
        "glGenSamplers" => p!(glGenSamplers),
        "glDeleteSamplers" => p!(glDeleteSamplers),
        "glBindSampler" => p!(glBindSampler),
        "glIsSampler" => p!(glIsSampler),
        "glSamplerParameteri" => p!(glSamplerParameteri),
        "glSamplerParameterf" => p!(glSamplerParameterf),
        "glSamplerParameteriv" => p!(glSamplerParameteriv),
        "glSamplerParameterfv" => p!(glSamplerParameterfv),
        "glSamplerParameterIiv" => p!(glSamplerParameterIiv),
        "glSamplerParameterIuiv" => p!(glSamplerParameterIuiv),
        "glGetSamplerParameteriv" => p!(glGetSamplerParameteriv),
        "glGetSamplerParameterfv" => p!(glGetSamplerParameterfv),
        "glGetSamplerParameterIiv" => p!(glGetSamplerParameterIiv),
        "glGetSamplerParameterIuiv" => p!(glGetSamplerParameterIuiv),
        // ---- GLES3.0: query objects ----
        "glGenQueries" => p!(glGenQueries),
        "glDeleteQueries" => p!(glDeleteQueries),
        "glBeginQuery" => p!(glBeginQuery),
        "glEndQuery" => p!(glEndQuery),
        "glIsQuery" => p!(glIsQuery),
        "glGetQueryiv" => p!(glGetQueryiv),
        "glGetQueryObjectuiv" => p!(glGetQueryObjectuiv),
        // ---- GLES3.0: transform-feedback objects ----
        "glGenTransformFeedbacks" => p!(glGenTransformFeedbacks),
        "glDeleteTransformFeedbacks" => p!(glDeleteTransformFeedbacks),
        "glBindTransformFeedback" => p!(glBindTransformFeedback),
        "glIsTransformFeedback" => p!(glIsTransformFeedback),
        "glBeginTransformFeedback" => p!(glBeginTransformFeedback),
        "glEndTransformFeedback" => p!(glEndTransformFeedback),
        "glPauseTransformFeedback" => p!(glPauseTransformFeedback),
        "glResumeTransformFeedback" => p!(glResumeTransformFeedback),
        "glTransformFeedbackVaryings" => p!(glTransformFeedbackVaryings),
        "glGetTransformFeedbackVarying" => p!(glGetTransformFeedbackVarying),
        // ---- GLES3.1: program pipeline objects ----
        "glGenProgramPipelines" => p!(glGenProgramPipelines),
        "glDeleteProgramPipelines" => p!(glDeleteProgramPipelines),
        "glBindProgramPipeline" => p!(glBindProgramPipeline),
        "glIsProgramPipeline" => p!(glIsProgramPipeline),
        "glUseProgramStages" => p!(glUseProgramStages),
        "glActiveShaderProgram" => p!(glActiveShaderProgram),
        "glProgramParameteri" => p!(glProgramParameteri),
        "glGetProgramPipelineiv" => p!(glGetProgramPipelineiv),
        "glGetProgramPipelineInfoLog" => p!(glGetProgramPipelineInfoLog),
        "glValidateProgramPipeline" => p!(glValidateProgramPipeline),
        "glCreateShaderProgramv" => p!(glCreateShaderProgramv),
        // ---- GLES3.0: texture storage / 3D / sub-image / copy ----
        "glTexStorage2D" => p!(glTexStorage2D),
        "glTexStorage3D" => p!(glTexStorage3D),
        "glTexImage3D" => p!(glTexImage3D),
        "glTexSubImage2D" => p!(glTexSubImage2D),
        "glTexSubImage3D" => p!(glTexSubImage3D),
        "glCopyTexSubImage2D" => p!(glCopyTexSubImage2D),
        "glCopyTexSubImage3D" => p!(glCopyTexSubImage3D),
        "glCopyTexImage2D" => p!(glCopyTexImage2D),
        "glCompressedTexImage2D" => p!(glCompressedTexImage2D),
        "glCompressedTexImage3D" => p!(glCompressedTexImage3D),
        "glCompressedTexSubImage2D" => p!(glCompressedTexSubImage2D),
        "glCompressedTexSubImage3D" => p!(glCompressedTexSubImage3D),
        // ---- GLES3.0: buffer / texture / vertex-attribute queries ----
        "glGetBufferParameteriv" => p!(glGetBufferParameteriv),
        "glGetBufferParameteri64v" => p!(glGetBufferParameteri64v),
        "glGetTexParameteriv" => p!(glGetTexParameteriv),
        "glGetTexParameterfv" => p!(glGetTexParameterfv),
        "glGetTexParameterIiv" => p!(glGetTexParameterIiv),
        "glGetTexParameterIuiv" => p!(glGetTexParameterIuiv),
        "glGetVertexAttribfv" => p!(glGetVertexAttribfv),
        "glGetVertexAttribiv" => p!(glGetVertexAttribiv),
        "glGetVertexAttribIiv" => p!(glGetVertexAttribIiv),
        "glGetVertexAttribIuiv" => p!(glGetVertexAttribIuiv),
        "glGetVertexAttribPointerv" => p!(glGetVertexAttribPointerv),
        // ---- GLES3.0: buffer copy + bounded readback ----
        "glCopyBufferSubData" => p!(glCopyBufferSubData),
        "glReadnPixels" => p!(glReadnPixels),
        // ---- GLES3.0: constant + integer vertex attributes ----
        "glVertexAttrib1f" => p!(glVertexAttrib1f),
        "glVertexAttrib2f" => p!(glVertexAttrib2f),
        "glVertexAttrib3f" => p!(glVertexAttrib3f),
        "glVertexAttrib4f" => p!(glVertexAttrib4f),
        "glVertexAttrib1fv" => p!(glVertexAttrib1fv),
        "glVertexAttrib2fv" => p!(glVertexAttrib2fv),
        "glVertexAttrib3fv" => p!(glVertexAttrib3fv),
        "glVertexAttrib4fv" => p!(glVertexAttrib4fv),
        "glVertexAttribI4i" => p!(glVertexAttribI4i),
        "glVertexAttribI4ui" => p!(glVertexAttribI4ui),
        "glVertexAttribI4iv" => p!(glVertexAttribI4iv),
        "glVertexAttribI4uiv" => p!(glVertexAttribI4uiv),
        "glVertexAttribIPointer" => p!(glVertexAttribIPointer),
        "glVertexAttribIFormat" => p!(glVertexAttribIFormat),
        // ---- GLES3.0: invalidation hints + separate-face stencil ----
        "glInvalidateFramebuffer" => p!(glInvalidateFramebuffer),
        "glInvalidateSubFramebuffer" => p!(glInvalidateSubFramebuffer),
        "glStencilFuncSeparate" => p!(glStencilFuncSeparate),
        "glStencilMaskSeparate" => p!(glStencilMaskSeparate),
        "glStencilOpSeparate" => p!(glStencilOpSeparate),
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

/// `glGetStringi(name, index)` — the ES3 indexed extension enumeration. Served from the SAME inventory as
/// `glGetString(GL_EXTENSIONS)` + `glGetIntegerv(GL_NUM_EXTENSIONS)` (see [`query::string_i`]), so the
/// three never disagree. An out-of-range index raises `GL_INVALID_VALUE` and returns null (never a
/// dangling pointer); a non-`GL_EXTENSIONS` name raises `GL_INVALID_ENUM`. With no extensions advertised
/// an app that honors the count of `0` never calls this.
#[no_mangle]
pub extern "C" fn glGetStringi(name: u32, index: u32) -> *const u8 {
    match query::string_i(name, index) {
        Some(bytes) => bytes.as_ptr(),
        None => {
            let e = if name == GL_EXTENSIONS { GL_INVALID_VALUE } else { GL_INVALID_ENUM };
            with(|s| s.ctx.set_gl_error(e));
            core::ptr::null()
        }
    }
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

/// `glTexParameterf(target, pname, param)` — the float-typed setter. GL's texture filter/wrap parameters
/// are enum-valued; the app passes the enum as a float, so it is truncated back to the `GLenum` the
/// integer path records (`glTexParameteri` parity).
#[no_mangle]
pub extern "C" fn glTexParameterf(_target: u32, pname: u32, param: f32) {
    with(|s| record::tex_parameter(&mut s.ctx, pname, param as u32));
}

/// `glTexParameterfv(target, pname, params)` — the single-element vector form; reads `params[0]`.
#[no_mangle]
pub extern "C" fn glTexParameterfv(_target: u32, pname: u32, params: *const f32) {
    if params.is_null() {
        return;
    }
    let v = unsafe { *params };
    with(|s| record::tex_parameter(&mut s.ctx, pname, v as u32));
}

/// `glTexParameteriv(target, pname, params)` — the single-element integer vector form; reads `params[0]`.
#[no_mangle]
pub extern "C" fn glTexParameteriv(_target: u32, pname: u32, params: *const i32) {
    if params.is_null() {
        return;
    }
    let v = unsafe { *params };
    with(|s| record::tex_parameter(&mut s.ctx, pname, v as u32));
}

/// `glGenerateMipmap(target)` — validate + record (an honest no-op on the pixel data; this model samples
/// the base level only). See [`record::generate_mipmap`].
#[no_mangle]
pub extern "C" fn glGenerateMipmap(target: u32) {
    with(|s| record::generate_mipmap(&mut s.ctx, target));
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

/// Write an active-variable reflection into the `glGetActive{Uniform,Attrib}` out-params: `size`, GL
/// `type`, the NUL-terminated `name` truncated to `buf_size`, and `length` = chars written (excl. NUL).
/// Every pointer is null-safe.
unsafe fn write_active_var(
    var: &query::ActiveVar,
    buf_size: i32,
    length: *mut i32,
    size: *mut i32,
    type_: *mut u32,
    name: *mut c_char,
) {
    if !size.is_null() {
        *size = var.size;
    }
    if !type_.is_null() {
        *type_ = var.gl_type;
    }
    let mut written = 0i32;
    if !name.is_null() && buf_size > 0 {
        let bytes = var.name.as_bytes();
        let cap = (buf_size - 1) as usize; // reserve one byte for the terminator
        let n = bytes.len().min(cap);
        for (i, &b) in bytes.iter().take(n).enumerate() {
            *name.add(i) = b as c_char;
        }
        *name.add(n) = 0;
        written = n as i32;
    }
    if !length.is_null() {
        *length = written;
    }
}

/// `glGetActiveUniform(program, index, …)` — the `index`-th active uniform's name/type/size from the
/// reflected tables (data uniforms first, then samplers — matching `glGetUniformLocation`). An
/// out-of-range index raises `GL_INVALID_VALUE` and reports an empty name.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glGetActiveUniform(
    program: u32,
    index: u32,
    buf_size: i32,
    length: *mut i32,
    size: *mut i32,
    type_: *mut u32,
    name: *mut c_char,
) {
    let var = with(|s| query::active_uniform(&s.ctx, program, index));
    emit_active_var(var, buf_size, length, size, type_, name);
}

/// `glGetActiveAttrib(program, index, …)` — the `index`-th active vertex attribute's name/type/size, in
/// the declaration order `glGetAttribLocation` resolves against. Out-of-range index → `GL_INVALID_VALUE`.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glGetActiveAttrib(
    program: u32,
    index: u32,
    buf_size: i32,
    length: *mut i32,
    size: *mut i32,
    type_: *mut u32,
    name: *mut c_char,
) {
    let var = with(|s| query::active_attrib(&s.ctx, program, index));
    emit_active_var(var, buf_size, length, size, type_, name);
}

/// Shared tail for `glGetActive{Uniform,Attrib}`: write the reflection, or (on an out-of-range index)
/// raise `GL_INVALID_VALUE` and report an empty variable.
fn emit_active_var(
    var: Option<query::ActiveVar>,
    buf_size: i32,
    length: *mut i32,
    size: *mut i32,
    type_: *mut u32,
    name: *mut c_char,
) {
    match var {
        Some(v) => unsafe { write_active_var(&v, buf_size, length, size, type_, name) },
        None => {
            with(|s| s.ctx.set_gl_error(GL_INVALID_VALUE));
            let empty = query::ActiveVar { name: String::new(), gl_type: 0, size: 0 };
            unsafe { write_active_var(&empty, buf_size, length, size, type_, name) };
        }
    }
}

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

/// `glVertexAttribDivisor(index, divisor)` — the instance-step rate for attribute `index` (`0` =
/// per-vertex, `>0` = per-instance). See [`record::vertex_attrib_divisor`].
#[no_mangle]
pub extern "C" fn glVertexAttribDivisor(index: u32, divisor: u32) {
    with(|s| record::vertex_attrib_divisor(&mut s.ctx, index as usize, divisor));
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

/// `glDrawArraysInstanced(mode, first, count, instancecount)` — record an instanced array draw; the
/// frame builder lowers the recorded instance count into `Draw { instance_count }`.
#[no_mangle]
pub extern "C" fn glDrawArraysInstanced(mode: u32, first: i32, count: i32, instancecount: i32) {
    with(|s| record::draw_arrays_instanced(&mut s.ctx, mode, first, count, instancecount));
}

/// `glDrawElementsInstanced(mode, count, type, indices, instancecount)` — record an instanced indexed
/// draw; lowered into `DrawIndexed { instance_count }`.
#[no_mangle]
pub extern "C" fn glDrawElementsInstanced(
    mode: u32,
    count: i32,
    type_: u32,
    indices: *const c_void,
    instancecount: i32,
) {
    with(|s| record::draw_elements_instanced(&mut s.ctx, mode, count, type_, indices as usize, instancecount));
}

// ==================================================================================================
// GLES: vertex array objects (GLES3 requires a bound VAO to draw)
// ==================================================================================================

#[no_mangle]
pub extern "C" fn glGenVertexArrays(n: i32, arrays: *mut u32) {
    if arrays.is_null() || n <= 0 {
        return;
    }
    with(|s| unsafe {
        for i in 0..n as isize {
            *arrays.offset(i) = record::gen_vertex_array(&mut s.ctx);
        }
    });
}

#[no_mangle]
pub extern "C" fn glBindVertexArray(array: u32) {
    with(|s| record::bind_vertex_array(&mut s.ctx, array));
}

#[no_mangle]
pub extern "C" fn glDeleteVertexArrays(n: i32, arrays: *const u32) {
    if arrays.is_null() || n <= 0 {
        return;
    }
    with(|s| unsafe {
        for i in 0..n as isize {
            record::delete_vertex_array(&mut s.ctx, *arrays.offset(i));
        }
    });
}

/// `glIsVertexArray(array)` — `GL_TRUE`/`GL_FALSE`. Returns the `GLboolean` as the codegen's `u32` ABI
/// (the low byte is the boolean a C caller reads).
#[no_mangle]
pub extern "C" fn glIsVertexArray(array: u32) -> u32 {
    with(|s| record::is_vertex_array(&s.ctx, array)) as u32
}

// ==================================================================================================
// GLES: framebuffer + renderbuffer objects (offscreen render targets)
//
// A guest drives offscreen rendering here: gen/bind a framebuffer, attach a color texture (or a
// texture-backed renderbuffer), check completeness, then a draw recorded while the FBO is bound renders
// into that attachment instead of the default window surface (resolved by `hl_gl::service::frame`). The
// bodies marshal the C ABI and call the shared `hl_gl::service::record` ops, which own the GL semantics +
// honest error register (the same deferred lowering the in-process render tests exercise).
// ==================================================================================================

#[no_mangle]
pub extern "C" fn glGenFramebuffers(n: i32, framebuffers: *mut u32) {
    if framebuffers.is_null() || n <= 0 {
        return;
    }
    with(|s| unsafe {
        for i in 0..n as isize {
            *framebuffers.offset(i) = record::gen_framebuffer(&mut s.ctx);
        }
    });
}

#[no_mangle]
pub extern "C" fn glBindFramebuffer(target: u32, framebuffer: u32) {
    with(|s| record::bind_framebuffer(&mut s.ctx, target, framebuffer));
}

#[no_mangle]
pub extern "C" fn glDeleteFramebuffers(n: i32, framebuffers: *const u32) {
    if framebuffers.is_null() || n <= 0 {
        return;
    }
    with(|s| unsafe {
        for i in 0..n as isize {
            record::delete_framebuffer(&mut s.ctx, *framebuffers.offset(i));
        }
    });
}

/// `glIsFramebuffer(framebuffer)` — `GL_TRUE`/`GL_FALSE`. Returns the `GLboolean` as the codegen's `u32`
/// ABI (the low byte is the boolean a C caller reads), matching `glIsVertexArray`.
#[no_mangle]
pub extern "C" fn glIsFramebuffer(framebuffer: u32) -> u32 {
    with(|s| record::is_framebuffer(&s.ctx, framebuffer)) as u32
}

/// `glCheckFramebufferStatus(target)` — the completeness enum of the bound draw/read framebuffer (see
/// [`record::check_framebuffer_status`]). A GLES app calls this before rendering to an FBO and bails on a
/// non-`GL_FRAMEBUFFER_COMPLETE` result.
#[no_mangle]
pub extern "C" fn glCheckFramebufferStatus(target: u32) -> u32 {
    with(|s| record::check_framebuffer_status(&mut s.ctx, target))
}

#[no_mangle]
pub extern "C" fn glFramebufferTexture2D(target: u32, attachment: u32, textarget: u32, texture: u32, level: i32) {
    with(|s| record::framebuffer_texture_2d(&mut s.ctx, target, attachment, textarget, texture, level));
}

#[no_mangle]
pub extern "C" fn glGenRenderbuffers(n: i32, renderbuffers: *mut u32) {
    if renderbuffers.is_null() || n <= 0 {
        return;
    }
    with(|s| unsafe {
        for i in 0..n as isize {
            *renderbuffers.offset(i) = record::gen_renderbuffer(&mut s.ctx);
        }
    });
}

#[no_mangle]
pub extern "C" fn glBindRenderbuffer(target: u32, renderbuffer: u32) {
    with(|s| record::bind_renderbuffer(&mut s.ctx, target, renderbuffer));
}

#[no_mangle]
pub extern "C" fn glDeleteRenderbuffers(n: i32, renderbuffers: *const u32) {
    if renderbuffers.is_null() || n <= 0 {
        return;
    }
    with(|s| unsafe {
        for i in 0..n as isize {
            record::delete_renderbuffer(&mut s.ctx, *renderbuffers.offset(i));
        }
    });
}

/// `glIsRenderbuffer(renderbuffer)` — `GL_TRUE`/`GL_FALSE` as the codegen's `u32` ABI (low byte is the
/// boolean), matching `glIsFramebuffer`/`glIsVertexArray`.
#[no_mangle]
pub extern "C" fn glIsRenderbuffer(renderbuffer: u32) -> u32 {
    with(|s| record::is_renderbuffer(&s.ctx, renderbuffer)) as u32
}

#[no_mangle]
pub extern "C" fn glRenderbufferStorage(target: u32, internalformat: u32, width: i32, height: i32) {
    with(|s| record::renderbuffer_storage(&mut s.ctx, target, internalformat, width, height));
}

#[no_mangle]
pub extern "C" fn glFramebufferRenderbuffer(target: u32, attachment: u32, renderbuffertarget: u32, renderbuffer: u32) {
    with(|s| record::framebuffer_renderbuffer(&mut s.ctx, target, attachment, renderbuffertarget, renderbuffer));
}

/// `glBlitFramebuffer(...)` — validate the read+draw framebuffers and record the blit. An honest partial:
/// this deferred model cannot materialize a cross-FBO pixel copy at record time (no rendered source plane
/// exists until swap), so the region/filter are validated but the copy is a documented no-op; see
/// [`record::blit_framebuffer`].
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glBlitFramebuffer(
    _src_x0: i32,
    _src_y0: i32,
    _src_x1: i32,
    _src_y1: i32,
    _dst_x0: i32,
    _dst_y0: i32,
    _dst_x1: i32,
    _dst_y1: i32,
    mask: u32,
    _filter: u32,
) {
    with(|s| record::blit_framebuffer(&mut s.ctx, mask));
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

// ==================================================================================================
// GLES3.1: compute dispatch (the GL analogue of cuLaunchKernel — lowers + submits immediately)
// ==================================================================================================

/// `glDispatchCompute(x, y, z)` — lower the bound compute program + its SSBO/UBO bindings into a
/// `CreateComputePipeline` + a `Dispatch` and submit (see [`compute::dispatch_compute`]). A sink error is
/// surfaced through `eglGetError` (the frame's `EGL_*`), never a false success.
#[no_mangle]
pub extern "C" fn glDispatchCompute(num_groups_x: u32, num_groups_y: u32, num_groups_z: u32) {
    with(|s| {
        if let Err(e) = compute::dispatch_compute(&mut s.ctx, &mut s.sink, num_groups_x, num_groups_y, num_groups_z) {
            s.set_egl_error(egl_error_from_gpu_error(&e));
        }
    });
}

/// `glDispatchComputeIndirect(indirect)` — dispatch with the group counts read from the buffer bound to
/// `GL_DISPATCH_INDIRECT_BUFFER` at byte offset `indirect` (see [`compute::dispatch_compute_indirect`]).
#[no_mangle]
pub extern "C" fn glDispatchComputeIndirect(indirect: isize) {
    with(|s| {
        if let Err(e) = compute::dispatch_compute_indirect(&mut s.ctx, &mut s.sink, indirect) {
            s.set_egl_error(egl_error_from_gpu_error(&e));
        }
    });
}

// ==================================================================================================
// GLES3.0: sync objects (GLsync) over the IR fence timeline
// ==================================================================================================

/// `glFenceSync(condition, flags)` — insert a fence into the command stream + return its `GLsync` token
/// (an opaque non-null pointer), or null on a bad `condition`/`flags`. See [`sync::fence_sync`].
#[no_mangle]
pub extern "C" fn glFenceSync(condition: u32, flags: u32) -> *mut c_void {
    with(|s| match sync::fence_sync(&mut s.ctx, &mut s.sink, condition, flags) {
        Some(token) => token as *mut c_void,
        None => core::ptr::null_mut(),
    })
}

/// `glClientWaitSync(sync, flags, timeout)` — client-side wait on the fence value `sync` marks. See
/// [`sync::client_wait_sync`].
#[no_mangle]
pub extern "C" fn glClientWaitSync(sync: *mut c_void, flags: u32, timeout: u64) -> u32 {
    with(|s| sync::client_wait_sync(&mut s.ctx, &mut s.sink, sync as usize, flags, timeout))
}

/// `glWaitSync(sync, flags, timeout)` — device-side (queue) wait; lowers to a `WaitFence`. See
/// [`sync::wait_sync`].
#[no_mangle]
pub extern "C" fn glWaitSync(sync: *mut c_void, flags: u32, timeout: u64) {
    with(|s| sync::wait_sync(&mut s.ctx, &mut s.sink, sync as usize, flags, timeout));
}

/// `glDeleteSync(sync)` — drop the sync object (an unknown non-null sync raises `GL_INVALID_VALUE`).
#[no_mangle]
pub extern "C" fn glDeleteSync(sync: *mut c_void) {
    with(|s| sync::delete_sync(&mut s.ctx, sync as usize));
}

/// `glIsSync(sync)` — `GL_TRUE`/`GL_FALSE` as the codegen's `u8` (`GLboolean`) ABI.
#[no_mangle]
pub extern "C" fn glIsSync(sync: *mut c_void) -> u8 {
    with(|s| sync::is_sync(&s.ctx, sync as usize)) as u8
}

/// `glGetSynciv(sync, pname, buf_size, length, values)` — write the single integer state value for
/// `pname` (see [`sync::get_synciv`]). Null-safe on both out-params.
#[no_mangle]
pub extern "C" fn glGetSynciv(sync: *mut c_void, pname: u32, buf_size: i32, length: *mut i32, values: *mut i32) {
    let v = with(|s| sync::get_synciv(&mut s.ctx, sync as usize, pname));
    if let Some(v) = v {
        unsafe {
            if !values.is_null() && buf_size >= 1 {
                *values = v;
                if !length.is_null() {
                    *length = 1;
                }
            } else if !length.is_null() {
                *length = 0;
            }
        }
    }
}

// ==================================================================================================
// GLES3.0: indexed buffer bindings (UBO/SSBO) — glBindBufferBase / glBindBufferRange
// ==================================================================================================

/// `glBindBufferBase(target, index, buffer)` — bind the whole `buffer` to indexed slot `index`. See
/// [`record::bind_buffer_base`].
#[no_mangle]
pub extern "C" fn glBindBufferBase(target: u32, index: u32, buffer: u32) {
    with(|s| record::bind_buffer_base(&mut s.ctx, target, index, buffer));
}

/// `glBindBufferRange(target, index, buffer, offset, size)` — bind `[offset, offset+size)` of `buffer` to
/// indexed slot `index`. See [`record::bind_buffer_range`].
#[no_mangle]
pub extern "C" fn glBindBufferRange(target: u32, index: u32, buffer: u32, offset: isize, size: isize) {
    with(|s| record::bind_buffer_range(&mut s.ctx, target, index, buffer, offset, size));
}

// ==================================================================================================
// GLES3.0: PBO-style buffer mapping — glMapBufferRange / glUnmapBuffer / glFlushMappedBufferRange
// ==================================================================================================

/// `glMapBufferRange(target, offset, length, access)` — map a range of the bound buffer and return a
/// pointer INTO its host storage (the app writes through it; `glUnmapBuffer` flushes). Null on error. The
/// pointer stays valid until the buffer's storage reallocates (the reference shim's fragile contract).
#[no_mangle]
pub extern "C" fn glMapBufferRange(target: u32, offset: isize, length: isize, access: u32) -> *mut c_void {
    with(|s| match map::map_buffer_range(&mut s.ctx, target, offset, length, access) {
        Some((name, off)) => match s.ctx.buffers.get_mut(name) {
            Some(b) => unsafe { b.data.as_mut_ptr().add(off) as *mut c_void },
            None => core::ptr::null_mut(),
        },
        None => core::ptr::null_mut(),
    })
}

/// `glUnmapBuffer(target)` — flush the mapped range as a `WriteBuffer` + clear the mapping. Returns the
/// `GLboolean` (`u8`) result; a sink error is surfaced via `eglGetError`. See [`map::unmap_buffer`].
#[no_mangle]
pub extern "C" fn glUnmapBuffer(target: u32) -> u8 {
    with(|s| match map::unmap_buffer(&mut s.ctx, &mut s.sink, target) {
        Ok(v) => v,
        Err(e) => {
            s.set_egl_error(egl_error_from_gpu_error(&e));
            0
        }
    })
}

/// `glFlushMappedBufferRange(target, offset, length)` — flush a sub-range of a still-mapped buffer as a
/// `WriteBuffer`. See [`map::flush_mapped_range`].
#[no_mangle]
pub extern "C" fn glFlushMappedBufferRange(target: u32, offset: isize, length: isize) {
    with(|s| {
        if let Err(e) = map::flush_mapped_range(&mut s.ctx, &mut s.sink, target, offset, length) {
            s.set_egl_error(egl_error_from_gpu_error(&e));
        }
    });
}

// ==================================================================================================
// GLES3.0: MRT draw/read buffer selection — glDrawBuffers / glReadBuffer
// ==================================================================================================

/// `glDrawBuffers(n, bufs)` — record the fragment-output color-buffer list. See [`record::draw_buffers`].
#[no_mangle]
pub extern "C" fn glDrawBuffers(n: i32, bufs: *const u32) {
    let list = if bufs.is_null() || n <= 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(bufs, n as usize) }.to_vec()
    };
    with(|s| record::draw_buffers(&mut s.ctx, &list));
}

/// `glReadBuffer(src)` — select the color buffer subsequent readbacks read from. See
/// [`record::read_buffer`].
#[no_mangle]
pub extern "C" fn glReadBuffer(src: u32) {
    with(|s| record::read_buffer(&mut s.ctx, src));
}

// ==================================================================================================
// ES3 sampler objects (client-side state; no GPU IR) — glGen/Bind/Delete/SamplerParameter*
// ==================================================================================================

#[no_mangle]
pub extern "C" fn glGenSamplers(count: i32, samplers: *mut u32) {
    if samplers.is_null() || count <= 0 {
        return;
    }
    with(|s| unsafe {
        for i in 0..count as isize {
            *samplers.offset(i) = es3::gen_sampler(&mut s.ctx);
        }
    });
}

#[no_mangle]
pub extern "C" fn glDeleteSamplers(count: i32, samplers: *const u32) {
    if count < 0 {
        with(|s| s.ctx.set_gl_error(GL_INVALID_VALUE));
        return;
    }
    if samplers.is_null() {
        return;
    }
    with(|s| unsafe {
        for i in 0..count as isize {
            es3::delete_sampler(&mut s.ctx, *samplers.offset(i));
        }
    });
}

#[no_mangle]
pub extern "C" fn glBindSampler(unit: u32, sampler: u32) {
    with(|s| es3::bind_sampler(&mut s.ctx, unit, sampler));
}

/// `glIsSampler(sampler)` — `GLboolean` in the codegen's `u8` ABI (low byte is the boolean).
#[no_mangle]
pub extern "C" fn glIsSampler(sampler: u32) -> u8 {
    with(|s| es3::is_sampler(&s.ctx, sampler)) as u8
}

#[no_mangle]
pub extern "C" fn glSamplerParameteri(sampler: u32, pname: u32, param: i32) {
    with(|s| es3::sampler_parameter(&mut s.ctx, sampler, pname, param, param as f32));
}

#[no_mangle]
pub extern "C" fn glSamplerParameterf(sampler: u32, pname: u32, param: f32) {
    with(|s| es3::sampler_parameter(&mut s.ctx, sampler, pname, param as i32, param));
}

#[no_mangle]
pub extern "C" fn glSamplerParameteriv(sampler: u32, pname: u32, param: *const i32) {
    if param.is_null() {
        return;
    }
    let v = unsafe { *param };
    with(|s| es3::sampler_parameter(&mut s.ctx, sampler, pname, v, v as f32));
}

#[no_mangle]
pub extern "C" fn glSamplerParameterfv(sampler: u32, pname: u32, param: *const f32) {
    if param.is_null() {
        return;
    }
    let v = unsafe { *param };
    with(|s| es3::sampler_parameter(&mut s.ctx, sampler, pname, v as i32, v));
}

/// `glSamplerParameterIiv` — the integer (non-normalized) vector form; reads `param[0]` (same setter path).
#[no_mangle]
pub extern "C" fn glSamplerParameterIiv(sampler: u32, pname: u32, param: *const i32) {
    if param.is_null() {
        return;
    }
    let v = unsafe { *param };
    with(|s| es3::sampler_parameter(&mut s.ctx, sampler, pname, v, v as f32));
}

/// `glSamplerParameterIuiv` — the unsigned integer vector form; reads `param[0]`.
#[no_mangle]
pub extern "C" fn glSamplerParameterIuiv(sampler: u32, pname: u32, param: *const u32) {
    if param.is_null() {
        return;
    }
    let v = unsafe { *param } as i32;
    with(|s| es3::sampler_parameter(&mut s.ctx, sampler, pname, v, v as f32));
}

#[no_mangle]
pub extern "C" fn glGetSamplerParameteriv(sampler: u32, pname: u32, params: *mut i32) {
    let v = with(|s| es3::get_sampler_parameter(&mut s.ctx, sampler, pname));
    if let Some(v) = v {
        if !params.is_null() {
            unsafe { *params = v.round() as i32 };
        }
    }
}

#[no_mangle]
pub extern "C" fn glGetSamplerParameterfv(sampler: u32, pname: u32, params: *mut f32) {
    let v = with(|s| es3::get_sampler_parameter(&mut s.ctx, sampler, pname));
    if let Some(v) = v {
        if !params.is_null() {
            unsafe { *params = v };
        }
    }
}

/// `glGetSamplerParameterIiv` — the integer view of `glGetSamplerParameteriv`.
#[no_mangle]
pub extern "C" fn glGetSamplerParameterIiv(sampler: u32, pname: u32, params: *mut i32) {
    glGetSamplerParameteriv(sampler, pname, params);
}

/// `glGetSamplerParameterIuiv` — the unsigned-integer view of `glGetSamplerParameteriv`.
#[no_mangle]
pub extern "C" fn glGetSamplerParameterIuiv(sampler: u32, pname: u32, params: *mut u32) {
    let v = with(|s| es3::get_sampler_parameter(&mut s.ctx, sampler, pname));
    if let Some(v) = v {
        if !params.is_null() {
            unsafe { *params = v.round() as u32 };
        }
    }
}

// ==================================================================================================
// ES3 query objects (occlusion / transform-feedback; client-side lifecycle, no GPU counter yet)
// ==================================================================================================

#[no_mangle]
pub extern "C" fn glGenQueries(n: i32, ids: *mut u32) {
    if ids.is_null() || n <= 0 {
        return;
    }
    with(|s| unsafe {
        for i in 0..n as isize {
            *ids.offset(i) = es3::gen_query(&mut s.ctx);
        }
    });
}

#[no_mangle]
pub extern "C" fn glDeleteQueries(n: i32, ids: *const u32) {
    if n < 0 {
        with(|s| s.ctx.set_gl_error(GL_INVALID_VALUE));
        return;
    }
    if ids.is_null() {
        return;
    }
    with(|s| unsafe {
        for i in 0..n as isize {
            es3::delete_query(&mut s.ctx, *ids.offset(i));
        }
    });
}

#[no_mangle]
pub extern "C" fn glBeginQuery(target: u32, id: u32) {
    with(|s| es3::begin_query(&mut s.ctx, target, id));
}

#[no_mangle]
pub extern "C" fn glEndQuery(target: u32) {
    with(|s| es3::end_query(&mut s.ctx, target));
}

/// `glIsQuery(id)` — `GLboolean` in the codegen's `u8` ABI.
#[no_mangle]
pub extern "C" fn glIsQuery(id: u32) -> u8 {
    with(|s| es3::is_query(&s.ctx, id)) as u8
}

#[no_mangle]
pub extern "C" fn glGetQueryiv(target: u32, pname: u32, params: *mut i32) {
    let v = with(|s| es3::get_queryiv(&mut s.ctx, target, pname));
    if let (Some(v), false) = (v, params.is_null()) {
        unsafe { *params = v };
    }
}

#[no_mangle]
pub extern "C" fn glGetQueryObjectuiv(id: u32, pname: u32, params: *mut u32) {
    let v = with(|s| es3::get_query_objectuiv(&mut s.ctx, id, pname));
    if let (Some(v), false) = (v, params.is_null()) {
        unsafe { *params = v };
    }
}

// ==================================================================================================
// ES3 transform-feedback objects (client-side lifecycle + per-program varying capture)
// ==================================================================================================

#[no_mangle]
pub extern "C" fn glGenTransformFeedbacks(n: i32, ids: *mut u32) {
    if ids.is_null() || n <= 0 {
        return;
    }
    with(|s| unsafe {
        for i in 0..n as isize {
            *ids.offset(i) = es3::gen_transform_feedback(&mut s.ctx);
        }
    });
}

#[no_mangle]
pub extern "C" fn glDeleteTransformFeedbacks(n: i32, ids: *const u32) {
    if n < 0 {
        with(|s| s.ctx.set_gl_error(GL_INVALID_VALUE));
        return;
    }
    if ids.is_null() {
        return;
    }
    with(|s| unsafe {
        for i in 0..n as isize {
            es3::delete_transform_feedback(&mut s.ctx, *ids.offset(i));
        }
    });
}

#[no_mangle]
pub extern "C" fn glBindTransformFeedback(target: u32, id: u32) {
    with(|s| es3::bind_transform_feedback(&mut s.ctx, target, id));
}

/// `glIsTransformFeedback(id)` — `GLboolean` in the codegen's `u8` ABI.
#[no_mangle]
pub extern "C" fn glIsTransformFeedback(id: u32) -> u8 {
    with(|s| es3::is_transform_feedback(&s.ctx, id)) as u8
}

#[no_mangle]
pub extern "C" fn glBeginTransformFeedback(primitive_mode: u32) {
    with(|s| es3::begin_transform_feedback(&mut s.ctx, primitive_mode));
}

#[no_mangle]
pub extern "C" fn glEndTransformFeedback() {
    with(|s| es3::end_transform_feedback(&mut s.ctx));
}

#[no_mangle]
pub extern "C" fn glPauseTransformFeedback() {
    with(|s| es3::pause_transform_feedback(&mut s.ctx));
}

#[no_mangle]
pub extern "C" fn glResumeTransformFeedback() {
    with(|s| es3::resume_transform_feedback(&mut s.ctx));
}

#[no_mangle]
pub extern "C" fn glTransformFeedbackVaryings(program: u32, count: i32, varyings: *const *const c_char, buffer_mode: u32) {
    // Marshal the NUL-terminated name array up front (a null entry with count>0 is GL_INVALID_VALUE).
    if count < 0 {
        with(|s| s.ctx.set_gl_error(GL_INVALID_VALUE));
        return;
    }
    let mut names = Vec::with_capacity(count as usize);
    if count > 0 {
        if varyings.is_null() {
            with(|s| s.ctx.set_gl_error(GL_INVALID_VALUE));
            return;
        }
        for i in 0..count as isize {
            match unsafe { cstr(*varyings.offset(i)) } {
                Some(name) => names.push(name),
                None => {
                    with(|s| s.ctx.set_gl_error(GL_INVALID_VALUE));
                    return;
                }
            }
        }
    }
    with(|s| es3::transform_feedback_varyings(&mut s.ctx, program, names, buffer_mode));
}

/// `glGetTransformFeedbackVarying(program, index, …)` — report the captured varying's name (real state)
/// plus a best-effort `size = 1`, `type = GL_FLOAT_VEC4` (no GLSL reflection). Out of range →
/// `GL_INVALID_VALUE` + empty name.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glGetTransformFeedbackVarying(
    program: u32,
    index: u32,
    buf_size: i32,
    length: *mut i32,
    size: *mut i32,
    type_: *mut u32,
    name: *mut c_char,
) {
    let varying = with(|s| es3::transform_feedback_varying(&s.ctx, program, index));
    match varying {
        Some(vname) => unsafe {
            if !size.is_null() {
                *size = 1;
            }
            if !type_.is_null() {
                *type_ = GL_FLOAT_VEC4;
            }
            write_c_name(vname.as_bytes(), buf_size, length, name);
        },
        None => {
            with(|s| s.ctx.set_gl_error(GL_INVALID_VALUE));
            unsafe {
                if !size.is_null() {
                    *size = 0;
                }
                if !type_.is_null() {
                    *type_ = 0;
                }
                write_c_name(&[], buf_size, length, name);
            }
        }
    }
}

/// Write a NUL-terminated name into `out` (capacity `buf_size`, incl. terminator) and report the char
/// count written (excl. NUL) in `length`. Null-safe on both out-params.
unsafe fn write_c_name(bytes: &[u8], buf_size: i32, length: *mut i32, out: *mut c_char) {
    let mut written = 0i32;
    if !out.is_null() && buf_size > 0 {
        let cap = (buf_size - 1) as usize;
        let n = bytes.len().min(cap);
        for (i, &b) in bytes.iter().take(n).enumerate() {
            *out.add(i) = b as c_char;
        }
        *out.add(n) = 0;
        written = n as i32;
    }
    if !length.is_null() {
        *length = written;
    }
}

// ==================================================================================================
// ES3 separate-shader program pipelines (client-side object state)
// ==================================================================================================

#[no_mangle]
pub extern "C" fn glGenProgramPipelines(n: i32, pipelines: *mut u32) {
    if pipelines.is_null() || n <= 0 {
        return;
    }
    with(|s| unsafe {
        for i in 0..n as isize {
            *pipelines.offset(i) = es3::gen_program_pipeline(&mut s.ctx);
        }
    });
}

#[no_mangle]
pub extern "C" fn glDeleteProgramPipelines(n: i32, pipelines: *const u32) {
    if n < 0 {
        with(|s| s.ctx.set_gl_error(GL_INVALID_VALUE));
        return;
    }
    if pipelines.is_null() {
        return;
    }
    with(|s| unsafe {
        for i in 0..n as isize {
            es3::delete_program_pipeline(&mut s.ctx, *pipelines.offset(i));
        }
    });
}

#[no_mangle]
pub extern "C" fn glBindProgramPipeline(pipeline: u32) {
    with(|s| es3::bind_program_pipeline(&mut s.ctx, pipeline));
}

/// `glIsProgramPipeline(pipeline)` — `GLboolean` in the codegen's `u8` ABI.
#[no_mangle]
pub extern "C" fn glIsProgramPipeline(pipeline: u32) -> u8 {
    with(|s| es3::is_program_pipeline(&s.ctx, pipeline)) as u8
}

#[no_mangle]
pub extern "C" fn glUseProgramStages(pipeline: u32, stages: u32, program: u32) {
    with(|s| es3::use_program_stages(&mut s.ctx, pipeline, stages, program));
}

#[no_mangle]
pub extern "C" fn glActiveShaderProgram(pipeline: u32, program: u32) {
    with(|s| es3::active_shader_program(&mut s.ctx, pipeline, program));
}

/// `glProgramParameteri(program, pname, value)` — only `GL_PROGRAM_SEPARABLE` is modeled (a linked
/// program is separable by construction here, so the flag is accepted). An unknown program →
/// `GL_INVALID_VALUE`; an unmodeled `pname` → `GL_INVALID_ENUM`.
#[no_mangle]
pub extern "C" fn glProgramParameteri(program: u32, pname: u32, value: i32) {
    let _ = value;
    with(|s| {
        if program == 0 || s.ctx.programs.program(program).is_none() {
            s.ctx.set_gl_error(GL_INVALID_VALUE);
        } else if pname != GL_PROGRAM_SEPARABLE {
            s.ctx.set_gl_error(GL_INVALID_ENUM);
        }
    });
}

#[no_mangle]
pub extern "C" fn glGetProgramPipelineiv(pipeline: u32, pname: u32, params: *mut i32) {
    let v = with(|s| es3::get_program_pipelineiv(&mut s.ctx, pipeline, pname));
    if let (Some(v), false) = (v, params.is_null()) {
        unsafe { *params = v };
    }
}

/// `glGetProgramPipelineInfoLog` — the pipeline validates clean, so the log is empty (length 0).
#[no_mangle]
pub extern "C" fn glGetProgramPipelineInfoLog(_pipeline: u32, buf_size: i32, length: *mut i32, info_log: *mut c_char) {
    unsafe { write_empty_info_log(buf_size, length, info_log) };
}

/// `glValidateProgramPipeline(pipeline)` — an unknown pipeline raises `GL_INVALID_OPERATION`; a known one
/// validates clean (the pipeline carries no cross-stage interface to reject in this model).
#[no_mangle]
pub extern "C" fn glValidateProgramPipeline(pipeline: u32) {
    // A known pipeline validates clean; an unknown one raises GL_INVALID_OPERATION (via the getter).
    with(|s| {
        let _ = es3::get_program_pipelineiv(&mut s.ctx, pipeline, GL_VALIDATE_STATUS);
    });
}

/// `glCreateShaderProgramv(type, count, strings)` — create + compile + link a single-stage separable
/// program from the joined source (a real body: the ES3 convenience constructor). Returns the new program
/// name, or `0` on a bad `type` / empty source.
#[no_mangle]
pub extern "C" fn glCreateShaderProgramv(type_: u32, count: i32, strings: *const *const c_char) -> u32 {
    if !matches!(type_, GL_VERTEX_SHADER | GL_FRAGMENT_SHADER | GL_COMPUTE_SHADER) {
        with(|s| s.ctx.set_gl_error(GL_INVALID_ENUM));
        return 0;
    }
    let src = unsafe { join_source(count, strings, core::ptr::null()) };
    with(|s| {
        let sh = record::create_shader(&mut s.ctx, type_);
        record::shader_source(&mut s.ctx, sh, &src);
        record::compile_shader(&mut s.ctx, sh);
        let prog = record::create_program(&mut s.ctx);
        record::attach_shader(&mut s.ctx, prog, sh);
        let _ = record::link_program(&mut s.ctx, prog);
        prog
    })
}

// ==================================================================================================
// ES3 texture storage / upload (glTexStorage*, glTexImage3D, glTexSubImage*, glCopyTexSubImage*)
// ==================================================================================================

#[no_mangle]
pub extern "C" fn glTexStorage2D(target: u32, levels: i32, internalformat: u32, width: i32, height: i32) {
    with(|s| record::tex_storage_2d(&mut s.ctx, target, levels, internalformat, width, height));
}

#[no_mangle]
pub extern "C" fn glTexStorage3D(target: u32, levels: i32, _internalformat: u32, width: i32, height: i32, depth: i32) {
    with(|s| record::tex_storage_3d(&mut s.ctx, target, levels, width, height, depth));
}

#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glTexImage3D(
    target: u32,
    level: i32,
    _internalformat: i32,
    width: i32,
    height: i32,
    depth: i32,
    _border: i32,
    format: u32,
    type_: u32,
    pixels: *const c_void,
) {
    let rgba = unsafe { to_rgba8(format, type_, width, height, pixels) };
    with(|s| record::tex_image_3d(&mut s.ctx, target, level, width, height, depth, &rgba));
}

#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glTexSubImage2D(
    target: u32,
    level: i32,
    xoffset: i32,
    yoffset: i32,
    width: i32,
    height: i32,
    format: u32,
    type_: u32,
    pixels: *const c_void,
) {
    let rgba = unsafe { to_rgba8(format, type_, width, height, pixels) };
    with(|s| record::tex_sub_image_2d(&mut s.ctx, target, level, xoffset, yoffset, width, height, &rgba));
}

#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glTexSubImage3D(
    target: u32,
    level: i32,
    xoffset: i32,
    yoffset: i32,
    zoffset: i32,
    width: i32,
    height: i32,
    depth: i32,
    format: u32,
    type_: u32,
    pixels: *const c_void,
) {
    let rgba = unsafe { to_rgba8(format, type_, width, height, pixels) };
    with(|s| record::tex_sub_image_3d(&mut s.ctx, target, level, xoffset, yoffset, zoffset, width, height, depth, &rgba));
}

#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glCopyTexSubImage2D(target: u32, level: i32, xoffset: i32, yoffset: i32, x: i32, y: i32, width: i32, height: i32) {
    with(|s| record::copy_tex_sub_image_2d(&mut s.ctx, target, level, xoffset, yoffset, x, y, width, height));
}

/// `glCopyTexSubImage3D` — the deferred model has no materialized source color plane per layer at record
/// time (see [`record::copy_tex_sub_image_2d`]); the layer copy is a documented no-op. Params validated
/// only insofar as a bad `target` is left to the bound-texture path — an honest no-op body.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glCopyTexSubImage3D(
    _target: u32,
    _level: i32,
    _xoffset: i32,
    _yoffset: i32,
    _zoffset: i32,
    _x: i32,
    _y: i32,
    _width: i32,
    _height: i32,
) {
}

/// `glCopyTexImage2D` — allocate the bound texture and copy from the read framebuffer. This deferred
/// model has no materialized default-framebuffer source plane at record time (only a color-attachment
/// texture carries pixels), so the allocation of the destination extent is honored and the pixel copy is
/// the documented no-op (mirrors `glCopyTexSubImage2D`/`glBlitFramebuffer`).
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glCopyTexImage2D(target: u32, level: i32, internalformat: u32, x: i32, y: i32, width: i32, height: i32, border: i32) {
    let _ = (x, y);
    if target != GL_TEXTURE_2D || level != 0 || border != 0 {
        with(|s| s.ctx.set_gl_error(GL_INVALID_VALUE));
        return;
    }
    // Allocate the destination extent so a later sample/subimage has storage (RGBA8 neutral plane).
    let ifmt = if matches!(internalformat, GL_RGB | GL_RGBA) { internalformat } else { GL_RGBA };
    let _ = ifmt;
    with(|s| {
        let name = s.ctx.tex_unit[s.ctx.active_texture];
        if name != 0 && width >= 0 && height >= 0 {
            s.ctx.textures.alloc_rgba(name, width, height);
        } else {
            s.ctx.set_gl_error(GL_INVALID_VALUE);
        }
    });
}

// ==================================================================================================
// ES3 compressed-texture uploads — no compressed codec is modeled, so these validate + no-op honestly
// (the RGBA8 render path samples uncompressed textures only; a compressed upload materializes no pixels).
// ==================================================================================================

/// `glCompressedTexImage2D` — a compressed upload the RGBA8 model cannot decode. We allocate the bound
/// texture's extent (so bookkeeping/bind proceeds) and truthfully do not materialize sampled pixels.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glCompressedTexImage2D(
    target: u32,
    level: i32,
    _internalformat: u32,
    width: i32,
    height: i32,
    _border: i32,
    _image_size: i32,
    _data: *const c_void,
) {
    if target != GL_TEXTURE_2D || level != 0 {
        return;
    }
    with(|s| {
        let name = s.ctx.tex_unit[s.ctx.active_texture];
        if name != 0 && width > 0 && height > 0 {
            s.ctx.textures.alloc_rgba(name, width, height);
        }
    });
}

/// `glCompressedTexImage3D` — the 2D-array / 3D compressed upload; layer-0 extent allocated, no decode.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glCompressedTexImage3D(
    target: u32,
    level: i32,
    _internalformat: u32,
    width: i32,
    height: i32,
    _depth: i32,
    _border: i32,
    _image_size: i32,
    _data: *const c_void,
) {
    if level != 0 || (target != GL_TEXTURE_2D_ARRAY && target != GL_TEXTURE_3D) {
        return;
    }
    with(|s| {
        let name = s.ctx.tex_unit[s.ctx.active_texture];
        if name != 0 && width > 0 && height > 0 {
            s.ctx.textures.alloc_rgba(name, width, height);
        }
    });
}

/// `glCompressedTexSubImage2D` — a compressed sub-image the model cannot decode: an honest no-op (the
/// texture's sampled pixels are unchanged).
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glCompressedTexSubImage2D(
    _target: u32,
    _level: i32,
    _xoffset: i32,
    _yoffset: i32,
    _width: i32,
    _height: i32,
    _format: u32,
    _image_size: i32,
    _data: *const c_void,
) {
}

/// `glCompressedTexSubImage3D` — the 2D-array / 3D compressed sub-image; an honest no-op.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glCompressedTexSubImage3D(
    _target: u32,
    _level: i32,
    _xoffset: i32,
    _yoffset: i32,
    _zoffset: i32,
    _width: i32,
    _height: i32,
    _depth: i32,
    _format: u32,
    _image_size: i32,
    _data: *const c_void,
) {
}

// ==================================================================================================
// ES3 buffer / texture / vertex-attribute state queries
// ==================================================================================================

#[no_mangle]
pub extern "C" fn glGetBufferParameteriv(target: u32, pname: u32, params: *mut i32) {
    if params.is_null() {
        return;
    }
    let v = with(|s| query::get_buffer_parameteriv(&s.ctx, target, pname));
    unsafe { *params = v };
}

/// `glGetBufferParameteri64v` — the 64-bit view of `glGetBufferParameteriv` (size/usage widened).
#[no_mangle]
pub extern "C" fn glGetBufferParameteri64v(target: u32, pname: u32, params: *mut i64) {
    if params.is_null() {
        return;
    }
    let v = with(|s| query::get_buffer_parameteriv(&s.ctx, target, pname));
    unsafe { *params = v as i64 };
}

#[no_mangle]
pub extern "C" fn glGetTexParameteriv(target: u32, pname: u32, params: *mut i32) {
    if params.is_null() {
        return;
    }
    let v = with(|s| query::get_tex_parameteriv(&s.ctx, target, pname));
    unsafe { *params = v };
}

#[no_mangle]
pub extern "C" fn glGetTexParameterfv(target: u32, pname: u32, params: *mut f32) {
    if params.is_null() {
        return;
    }
    let v = with(|s| query::get_tex_parameteriv(&s.ctx, target, pname));
    unsafe { *params = v as f32 };
}

/// `glGetTexParameterIiv` — the integer view of `glGetTexParameteriv`.
#[no_mangle]
pub extern "C" fn glGetTexParameterIiv(target: u32, pname: u32, params: *mut i32) {
    glGetTexParameteriv(target, pname, params);
}

/// `glGetTexParameterIuiv` — the unsigned-integer view.
#[no_mangle]
pub extern "C" fn glGetTexParameterIuiv(target: u32, pname: u32, params: *mut u32) {
    if params.is_null() {
        return;
    }
    let v = with(|s| query::get_tex_parameteriv(&s.ctx, target, pname));
    unsafe { *params = v as u32 };
}

/// `glGetVertexAttribfv`/`iv` — attribute readback. This model records the array pointer + enable state
/// but reports the safe default `0` for the queried parameters (no attribute reflected back), matching the
/// reference shim; the app's own `glVertexAttribPointer` state is authoritative.
#[no_mangle]
pub extern "C" fn glGetVertexAttribfv(_index: u32, _pname: u32, params: *mut f32) {
    if !params.is_null() {
        unsafe { *params = 0.0 };
    }
}

#[no_mangle]
pub extern "C" fn glGetVertexAttribiv(_index: u32, _pname: u32, params: *mut i32) {
    if !params.is_null() {
        unsafe { *params = 0 };
    }
}

/// `glGetVertexAttribIiv`/`Iuiv` — the integer forms; same `0` default.
#[no_mangle]
pub extern "C" fn glGetVertexAttribIiv(_index: u32, _pname: u32, params: *mut i32) {
    if !params.is_null() {
        unsafe { *params = 0 };
    }
}

#[no_mangle]
pub extern "C" fn glGetVertexAttribIuiv(_index: u32, _pname: u32, params: *mut u32) {
    if !params.is_null() {
        unsafe { *params = 0 };
    }
}

/// `glGetVertexAttribPointerv` — the attribute array pointer readback; reports null (the app's own bound
/// pointer is authoritative), matching the reference.
#[no_mangle]
pub extern "C" fn glGetVertexAttribPointerv(_index: u32, _pname: u32, pointer: *mut *mut c_void) {
    if !pointer.is_null() {
        unsafe { *pointer = core::ptr::null_mut() };
    }
}

// ==================================================================================================
// ES3 buffer copy (glCopyBufferSubData) + readback with a bound size (glReadnPixels)
// ==================================================================================================

#[no_mangle]
pub extern "C" fn glCopyBufferSubData(read_target: u32, write_target: u32, read_offset: isize, write_offset: isize, size: isize) {
    with(|s| record::copy_buffer_sub_data(&mut s.ctx, read_target, write_target, read_offset, write_offset, size));
}

/// `glReadnPixels(x, y, w, h, format, type, bufSize, data)` — the bounded-buffer form of `glReadPixels`:
/// identical readback, but never writes more than `bufSize` bytes into `data` (a `bufSize` too small for
/// the requested rect raises `GL_INVALID_OPERATION`).
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glReadnPixels(
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    format: u32,
    type_: u32,
    buf_size: i32,
    data: *mut c_void,
) {
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
    if width < 0 || height < 0 || buf_size < 0 {
        fail(GL_INVALID_VALUE);
        return;
    }
    if width == 0 || height == 0 {
        return;
    }
    if data.is_null() {
        fail(GL_INVALID_VALUE);
        return;
    }
    let need = width as usize * height as usize * bpp;
    if (buf_size as usize) < need {
        fail(GL_INVALID_OPERATION);
        return;
    }
    let packed = with(|s| readpixels::read_pixels(&mut s.ctx, &mut s.sink, x, y, width, height, format));
    match packed {
        Ok(bytes) => {
            let n = bytes.len().min(need).min(buf_size as usize);
            unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), data as *mut u8, n) };
        }
        Err(e) => with(|s| {
            s.ctx.set_gl_error(GL_OUT_OF_MEMORY);
            s.set_egl_error(egl_error_from_gpu_error(&e));
        }),
    }
}

// ==================================================================================================
// ES3 vertex-attribute constants + integer attribute pointers
// ==================================================================================================
//
// The shim sources vertex attributes from bound arrays (`glVertexAttribPointer`), so a CONSTANT generic
// attribute (`glVertexAttrib*f`) has no array slot to feed and is an honest no-op (matches the reference
// shim and `gl_shim.c`). The integer attribute pointer/format entry points record into the same attribute
// state the float pointer path uses.

#[no_mangle]
pub extern "C" fn glVertexAttrib1f(_index: u32, _x: f32) {}
#[no_mangle]
pub extern "C" fn glVertexAttrib2f(_index: u32, _x: f32, _y: f32) {}
#[no_mangle]
pub extern "C" fn glVertexAttrib3f(_index: u32, _x: f32, _y: f32, _z: f32) {}
#[no_mangle]
pub extern "C" fn glVertexAttrib4f(_index: u32, _x: f32, _y: f32, _z: f32, _w: f32) {}
#[no_mangle]
pub extern "C" fn glVertexAttrib1fv(_index: u32, _v: *const f32) {}
#[no_mangle]
pub extern "C" fn glVertexAttrib2fv(_index: u32, _v: *const f32) {}
#[no_mangle]
pub extern "C" fn glVertexAttrib3fv(_index: u32, _v: *const f32) {}
#[no_mangle]
pub extern "C" fn glVertexAttrib4fv(_index: u32, _v: *const f32) {}
#[no_mangle]
pub extern "C" fn glVertexAttribI4i(_index: u32, _x: i32, _y: i32, _z: i32, _w: i32) {}
#[no_mangle]
pub extern "C" fn glVertexAttribI4ui(_index: u32, _x: u32, _y: u32, _z: u32, _w: u32) {}
#[no_mangle]
pub extern "C" fn glVertexAttribI4iv(_index: u32, _v: *const i32) {}
#[no_mangle]
pub extern "C" fn glVertexAttribI4uiv(_index: u32, _v: *const u32) {}

/// `glVertexAttribIPointer(index, size, type, stride, pointer)` — an integer vertex-attribute array;
/// records into the same per-location attribute state as `glVertexAttribPointer` (marked integer, never
/// normalized).
#[no_mangle]
pub extern "C" fn glVertexAttribIPointer(index: u32, size: i32, type_: u32, stride: i32, pointer: *const c_void) {
    with(|s| record::vertex_attrib_pointer(&mut s.ctx, index as usize, size, type_, false, stride, pointer as usize));
}

/// `glVertexAttribIFormat(attribindex, size, type, relativeoffset)` — the separate-format (VAO) integer
/// attribute format; records size/type/offset into the attribute state (a no-op for the fields this model
/// does not separately track).
#[no_mangle]
pub extern "C" fn glVertexAttribIFormat(attribindex: u32, size: i32, type_: u32, relativeoffset: u32) {
    with(|s| record::vertex_attrib_pointer(&mut s.ctx, attribindex as usize, size, type_, false, 0, relativeoffset as usize));
}

// ==================================================================================================
// ES3 framebuffer-attachment invalidation (a hint) + separate-face stencil (IR-free) — honest no-ops
// ==================================================================================================

/// `glInvalidateFramebuffer(target, n, attachments)` — a discard HINT that the listed attachments'
/// contents are no longer needed. This deferred model rebuilds a fresh frame each swap (nothing is
/// preserved across frames), so the hint is already satisfied — an honest no-op.
#[no_mangle]
pub extern "C" fn glInvalidateFramebuffer(_target: u32, _num_attachments: i32, _attachments: *const u32) {}

/// `glInvalidateSubFramebuffer` — the sub-rectangle discard hint; same honest no-op.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glInvalidateSubFramebuffer(
    _target: u32,
    _num_attachments: i32,
    _attachments: *const u32,
    _x: i32,
    _y: i32,
    _width: i32,
    _height: i32,
) {
}

/// Separate-face stencil state — this model backs no stencil buffer (the default framebuffer has no
/// stencil attachment), so these carry no observable state: an honest no-op (matches the reference shim).
#[no_mangle]
pub extern "C" fn glStencilFuncSeparate(_face: u32, _func: u32, _ref: i32, _mask: u32) {}
#[no_mangle]
pub extern "C" fn glStencilMaskSeparate(_face: u32, _mask: u32) {}
#[no_mangle]
pub extern "C" fn glStencilOpSeparate(_face: u32, _sfail: u32, _dpfail: u32, _dppass: u32) {}
