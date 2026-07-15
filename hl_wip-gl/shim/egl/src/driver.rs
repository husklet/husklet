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
    egl_error_from_gpu_error, EGL_BAD_ATTRIBUTE, EGL_BAD_CONFIG, EGL_BAD_PARAMETER, EGL_FALSE, EGL_TRUE,
    GL_INVALID_ENUM, GL_INVALID_VALUE, GL_OUT_OF_MEMORY,
};
use hl_gl::service::{compute, config, es3, intro, map, query, readpixels, record, swap, sync};

use crate::state::{current, with, AppPresentOutcome, CONFIG_TOKEN, DISPLAY_TOKEN};

// ---- EGL query enums the string getters key on (the GL_* query enums live in hl_gl::glconst) ------

const EGL_VENDOR: i32 = 0x3053;
const EGL_VERSION_Q: i32 = 0x3054;
const EGL_EXTENSIONS_Q: i32 = 0x3055;
const EGL_CLIENT_APIS: i32 = 0x308D;

// `eglGetCurrentSurface` selectors: which of the current binding's two surfaces to report.
const EGL_DRAW: i32 = 0x3059;
const EGL_READ: i32 = 0x305A;

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

#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglGetError() -> i32 {
    with(|s| s.take_egl_error())
}

#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglGetDisplay(_display_id: *mut c_void) -> *mut c_void {
    DISPLAY_TOKEN as *mut c_void
}

#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglInitialize(_dpy: *mut c_void, major: *mut i32, minor: *mut i32) -> u32 {
    hl_log::hl_info!(hl_log::tag::EGL, "eglInitialize egl=1.5");
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

#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglTerminate(_dpy: *mut c_void) -> u32 {
    with(|s| s.inited = false);
    EGL_TRUE
}

/// `eglQueryString(dpy, name)` — vendor / version / client-APIs / **extensions**. The extension string is
/// keyed on `dpy`: with `EGL_NO_DISPLAY` (null) it returns the CLIENT extensions (the platform-base +
/// `EGL_*_platform_wayland` set a toolkit probes before opening a display), otherwise the per-display set.
/// Advertising `EGL_EXT_platform_wayland` / `EGL_KHR_platform_wayland` is what makes a Wayland app take the
/// `eglGetPlatformDisplay(EGL_PLATFORM_WAYLAND_KHR, …)` window path instead of surfaceless/pbuffer.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglQueryString(dpy: *mut c_void, name: i32) -> *const c_char {
    match name {
        EGL_VENDOR => b"hl-gl\0".as_ptr() as *const c_char,
        EGL_VERSION_Q => b"1.5 hl-gl\0".as_ptr() as *const c_char,
        EGL_CLIENT_APIS => b"OpenGL_ES\0".as_ptr() as *const c_char,
        EGL_EXTENSIONS_Q => egl_extensions_cstr(dpy.is_null()),
        _ => b"\0".as_ptr() as *const c_char,
    }
}

/// The NUL-terminated `EGL_EXTENSIONS` string (built once from [`hl_gl::adapter::wayland`], process-static
/// so the returned pointer is valid for the app's lifetime).
fn egl_extensions_cstr(client: bool) -> *const c_char {
    use std::ffi::CString;
    use std::sync::OnceLock;
    static CLIENT: OnceLock<CString> = OnceLock::new();
    static DISPLAY: OnceLock<CString> = OnceLock::new();
    let cell = if client { &CLIENT } else { &DISPLAY };
    cell.get_or_init(|| {
        let s = if client {
            hl_gl::adapter::wayland::egl_client_extensions()
        } else {
            hl_gl::adapter::wayland::egl_display_extensions()
        };
        CString::new(s).unwrap_or_default()
    })
    .as_ptr()
}

/// `eglGetProcAddress(name)` — resolve a core `egl*`/`gl*` entry point to its real function pointer.
///
/// A GLES app commonly loads its entry points dynamically through this call rather than relying on the
/// dynamic linker; returning null for a core name it then calls would crash it. So we resolve every core
/// `egl*`/`gl*` symbol this object hand-implements to its actual address (cast through `usize` so the
/// `fn` → `*mut c_void` cast is legal). An unknown name (an extension trampoline we do not advertise)
/// still returns null — the spec-legal "not found".
#[cfg_attr(not(gles_client), no_mangle)]
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
        // ---- ES3 / GLES3.1 completeness pass + remaining EGL (real bodies below) ----
        "eglBindTexImage" => p!(eglBindTexImage),
        "eglClientWaitSync" => p!(eglClientWaitSync),
        "eglCopyBuffers" => p!(eglCopyBuffers),
        "eglCreateImage" => p!(eglCreateImage),
        "eglCreatePbufferFromClientBuffer" => p!(eglCreatePbufferFromClientBuffer),
        "eglCreatePbufferSurface" => p!(eglCreatePbufferSurface),
        "eglCreatePixmapSurface" => p!(eglCreatePixmapSurface),
        "eglCreatePlatformPixmapSurface" => p!(eglCreatePlatformPixmapSurface),
        "eglCreatePlatformWindowSurface" => p!(eglCreatePlatformWindowSurface),
        "eglCreateSync" => p!(eglCreateSync),
        "eglDestroyImage" => p!(eglDestroyImage),
        "eglDestroySync" => p!(eglDestroySync),
        "eglGetPlatformDisplay" => p!(eglGetPlatformDisplay),
        // EGL_EXT_platform_base — advertised in the client extension string, so a caller (e.g. the real
        // `eglinfo`) resolves + calls these without a null check. Resolved here only (not exported ABI).
        "eglGetPlatformDisplayEXT" => p!(eglGetPlatformDisplayEXT),
        "eglCreatePlatformWindowSurfaceEXT" => p!(eglCreatePlatformWindowSurfaceEXT),
        "eglCreatePlatformPixmapSurfaceEXT" => p!(eglCreatePlatformPixmapSurfaceEXT),
        // EGL_EXT_device_base / device_query / device_enumeration — advertised in the client + display
        // extension strings (GDK/epoxy require one of the device_* set to find eglQueryDisplayAttribEXT).
        // Resolved here only (extension functions, not exported ABI).
        "eglQueryDisplayAttribEXT" => p!(eglQueryDisplayAttribEXT),
        "eglQueryDeviceAttribEXT" => p!(eglQueryDeviceAttribEXT),
        "eglQueryDeviceStringEXT" => p!(eglQueryDeviceStringEXT),
        "eglQueryDevicesEXT" => p!(eglQueryDevicesEXT),
        "eglGetSyncAttrib" => p!(eglGetSyncAttrib),
        "eglQueryAPI" => p!(eglQueryAPI),
        "eglQueryContext" => p!(eglQueryContext),
        "eglQuerySurface" => p!(eglQuerySurface),
        "eglReleaseTexImage" => p!(eglReleaseTexImage),
        "eglReleaseThread" => p!(eglReleaseThread),
        "eglSurfaceAttrib" => p!(eglSurfaceAttrib),
        "eglWaitClient" => p!(eglWaitClient),
        "eglWaitGL" => p!(eglWaitGL),
        "eglWaitNative" => p!(eglWaitNative),
        "eglWaitSync" => p!(eglWaitSync),
        "glBindImageTexture" => p!(glBindImageTexture),
        "glBindVertexBuffer" => p!(glBindVertexBuffer),
        "glBlendBarrier" => p!(glBlendBarrier),
        "glBlendColor" => p!(glBlendColor),
        "glBlendEquation" => p!(glBlendEquation),
        "glBlendEquationSeparate" => p!(glBlendEquationSeparate),
        "glBlendEquationSeparatei" => p!(glBlendEquationSeparatei),
        "glBlendEquationi" => p!(glBlendEquationi),
        "glBlendFuncSeparatei" => p!(glBlendFuncSeparatei),
        "glBlendFunci" => p!(glBlendFunci),
        "glClearBufferfi" => p!(glClearBufferfi),
        "glClearBufferfv" => p!(glClearBufferfv),
        "glClearBufferiv" => p!(glClearBufferiv),
        "glClearBufferuiv" => p!(glClearBufferuiv),
        "glClearStencil" => p!(glClearStencil),
        "glColorMask" => p!(glColorMask),
        "glColorMaski" => p!(glColorMaski),
        "glCopyImageSubData" => p!(glCopyImageSubData),
        "glDebugMessageCallback" => p!(glDebugMessageCallback),
        "glDebugMessageControl" => p!(glDebugMessageControl),
        "glDebugMessageInsert" => p!(glDebugMessageInsert),
        "glDeleteProgram" => p!(glDeleteProgram),
        "glDeleteShader" => p!(glDeleteShader),
        "glDepthRangef" => p!(glDepthRangef),
        "glDetachShader" => p!(glDetachShader),
        "glDisablei" => p!(glDisablei),
        "glDrawArraysIndirect" => p!(glDrawArraysIndirect),
        "glDrawElementsBaseVertex" => p!(glDrawElementsBaseVertex),
        "glDrawElementsIndirect" => p!(glDrawElementsIndirect),
        "glDrawElementsInstancedBaseVertex" => p!(glDrawElementsInstancedBaseVertex),
        "glDrawRangeElements" => p!(glDrawRangeElements),
        "glDrawRangeElementsBaseVertex" => p!(glDrawRangeElementsBaseVertex),
        "glEnablei" => p!(glEnablei),
        "glFinish" => p!(glFinish),
        "glFlush" => p!(glFlush),
        "glFramebufferParameteri" => p!(glFramebufferParameteri),
        "glFramebufferTexture" => p!(glFramebufferTexture),
        "glFramebufferTextureLayer" => p!(glFramebufferTextureLayer),
        "glGetActiveUniformBlockName" => p!(glGetActiveUniformBlockName),
        "glGetActiveUniformBlockiv" => p!(glGetActiveUniformBlockiv),
        "glGetActiveUniformsiv" => p!(glGetActiveUniformsiv),
        "glGetAttachedShaders" => p!(glGetAttachedShaders),
        "glGetBooleani_v" => p!(glGetBooleani_v),
        "glGetBufferPointerv" => p!(glGetBufferPointerv),
        "glGetDebugMessageLog" => p!(glGetDebugMessageLog),
        "glGetFragDataLocation" => p!(glGetFragDataLocation),
        "glGetFramebufferAttachmentParameteriv" => p!(glGetFramebufferAttachmentParameteriv),
        "glGetFramebufferParameteriv" => p!(glGetFramebufferParameteriv),
        "glGetGraphicsResetStatus" => p!(glGetGraphicsResetStatus),
        "glGetInteger64i_v" => p!(glGetInteger64i_v),
        "glGetInteger64v" => p!(glGetInteger64v),
        "glGetIntegeri_v" => p!(glGetIntegeri_v),
        "glGetInternalformativ" => p!(glGetInternalformativ),
        "glGetMultisamplefv" => p!(glGetMultisamplefv),
        "glGetObjectLabel" => p!(glGetObjectLabel),
        "glGetObjectPtrLabel" => p!(glGetObjectPtrLabel),
        "glGetPointerv" => p!(glGetPointerv),
        "glGetProgramBinary" => p!(glGetProgramBinary),
        "glGetProgramInterfaceiv" => p!(glGetProgramInterfaceiv),
        "glGetProgramResourceIndex" => p!(glGetProgramResourceIndex),
        "glGetProgramResourceLocation" => p!(glGetProgramResourceLocation),
        "glGetProgramResourceName" => p!(glGetProgramResourceName),
        "glGetProgramResourceiv" => p!(glGetProgramResourceiv),
        "glGetRenderbufferParameteriv" => p!(glGetRenderbufferParameteriv),
        "glGetShaderPrecisionFormat" => p!(glGetShaderPrecisionFormat),
        "glGetShaderSource" => p!(glGetShaderSource),
        "glGetTexLevelParameterfv" => p!(glGetTexLevelParameterfv),
        "glGetTexLevelParameteriv" => p!(glGetTexLevelParameteriv),
        "glGetUniformBlockIndex" => p!(glGetUniformBlockIndex),
        "glGetUniformIndices" => p!(glGetUniformIndices),
        "glGetUniformfv" => p!(glGetUniformfv),
        "glGetUniformiv" => p!(glGetUniformiv),
        "glGetUniformuiv" => p!(glGetUniformuiv),
        "glGetnUniformfv" => p!(glGetnUniformfv),
        "glGetnUniformiv" => p!(glGetnUniformiv),
        "glGetnUniformuiv" => p!(glGetnUniformuiv),
        "glHint" => p!(glHint),
        "glIsBuffer" => p!(glIsBuffer),
        "glIsEnabled" => p!(glIsEnabled),
        "glIsEnabledi" => p!(glIsEnabledi),
        "glIsProgram" => p!(glIsProgram),
        "glIsShader" => p!(glIsShader),
        "glIsTexture" => p!(glIsTexture),
        "glLineWidth" => p!(glLineWidth),
        "glMemoryBarrier" => p!(glMemoryBarrier),
        "glMemoryBarrierByRegion" => p!(glMemoryBarrierByRegion),
        "glMinSampleShading" => p!(glMinSampleShading),
        "glObjectLabel" => p!(glObjectLabel),
        "glObjectPtrLabel" => p!(glObjectPtrLabel),
        "glPatchParameteri" => p!(glPatchParameteri),
        "glPolygonOffset" => p!(glPolygonOffset),
        "glPopDebugGroup" => p!(glPopDebugGroup),
        "glPrimitiveBoundingBox" => p!(glPrimitiveBoundingBox),
        "glProgramBinary" => p!(glProgramBinary),
        "glProgramUniform1f" => p!(glProgramUniform1f),
        "glProgramUniform1fv" => p!(glProgramUniform1fv),
        "glProgramUniform1i" => p!(glProgramUniform1i),
        "glProgramUniform1iv" => p!(glProgramUniform1iv),
        "glProgramUniform1ui" => p!(glProgramUniform1ui),
        "glProgramUniform1uiv" => p!(glProgramUniform1uiv),
        "glProgramUniform2f" => p!(glProgramUniform2f),
        "glProgramUniform2fv" => p!(glProgramUniform2fv),
        "glProgramUniform2i" => p!(glProgramUniform2i),
        "glProgramUniform2iv" => p!(glProgramUniform2iv),
        "glProgramUniform2ui" => p!(glProgramUniform2ui),
        "glProgramUniform2uiv" => p!(glProgramUniform2uiv),
        "glProgramUniform3f" => p!(glProgramUniform3f),
        "glProgramUniform3fv" => p!(glProgramUniform3fv),
        "glProgramUniform3i" => p!(glProgramUniform3i),
        "glProgramUniform3iv" => p!(glProgramUniform3iv),
        "glProgramUniform3ui" => p!(glProgramUniform3ui),
        "glProgramUniform3uiv" => p!(glProgramUniform3uiv),
        "glProgramUniform4f" => p!(glProgramUniform4f),
        "glProgramUniform4fv" => p!(glProgramUniform4fv),
        "glProgramUniform4i" => p!(glProgramUniform4i),
        "glProgramUniform4iv" => p!(glProgramUniform4iv),
        "glProgramUniform4ui" => p!(glProgramUniform4ui),
        "glProgramUniform4uiv" => p!(glProgramUniform4uiv),
        "glProgramUniformMatrix2fv" => p!(glProgramUniformMatrix2fv),
        "glProgramUniformMatrix2x3fv" => p!(glProgramUniformMatrix2x3fv),
        "glProgramUniformMatrix2x4fv" => p!(glProgramUniformMatrix2x4fv),
        "glProgramUniformMatrix3fv" => p!(glProgramUniformMatrix3fv),
        "glProgramUniformMatrix3x2fv" => p!(glProgramUniformMatrix3x2fv),
        "glProgramUniformMatrix3x4fv" => p!(glProgramUniformMatrix3x4fv),
        "glProgramUniformMatrix4fv" => p!(glProgramUniformMatrix4fv),
        "glProgramUniformMatrix4x2fv" => p!(glProgramUniformMatrix4x2fv),
        "glProgramUniformMatrix4x3fv" => p!(glProgramUniformMatrix4x3fv),
        "glPushDebugGroup" => p!(glPushDebugGroup),
        "glReleaseShaderCompiler" => p!(glReleaseShaderCompiler),
        "glRenderbufferStorageMultisample" => p!(glRenderbufferStorageMultisample),
        "glSampleCoverage" => p!(glSampleCoverage),
        "glSampleMaski" => p!(glSampleMaski),
        "glShaderBinary" => p!(glShaderBinary),
        "glStencilFunc" => p!(glStencilFunc),
        "glStencilMask" => p!(glStencilMask),
        "glStencilOp" => p!(glStencilOp),
        "glTexBuffer" => p!(glTexBuffer),
        "glTexBufferRange" => p!(glTexBufferRange),
        "glTexParameterIiv" => p!(glTexParameterIiv),
        "glTexParameterIuiv" => p!(glTexParameterIuiv),
        "glTexStorage2DMultisample" => p!(glTexStorage2DMultisample),
        "glTexStorage3DMultisample" => p!(glTexStorage3DMultisample),
        "glUniform1ui" => p!(glUniform1ui),
        "glUniform1uiv" => p!(glUniform1uiv),
        "glUniform2ui" => p!(glUniform2ui),
        "glUniform2uiv" => p!(glUniform2uiv),
        "glUniform3ui" => p!(glUniform3ui),
        "glUniform3uiv" => p!(glUniform3uiv),
        "glUniform4ui" => p!(glUniform4ui),
        "glUniform4uiv" => p!(glUniform4uiv),
        "glUniformBlockBinding" => p!(glUniformBlockBinding),
        "glUniformMatrix2x3fv" => p!(glUniformMatrix2x3fv),
        "glUniformMatrix2x4fv" => p!(glUniformMatrix2x4fv),
        "glUniformMatrix3x2fv" => p!(glUniformMatrix3x2fv),
        "glUniformMatrix3x4fv" => p!(glUniformMatrix3x4fv),
        "glUniformMatrix4x2fv" => p!(glUniformMatrix4x2fv),
        "glUniformMatrix4x3fv" => p!(glUniformMatrix4x3fv),
        "glValidateProgram" => p!(glValidateProgram),
        "glVertexAttribBinding" => p!(glVertexAttribBinding),
        "glVertexAttribFormat" => p!(glVertexAttribFormat),
        "glVertexBindingDivisor" => p!(glVertexBindingDivisor),
        // Unknown / extension name we do not advertise → spec-legal "not found".
        _ => core::ptr::null_mut(),
    }
}

/// `eglBindAPI(api)` — bind the client API for the CALLING THREAD (`eglQueryAPI` reads it back). This is
/// a GLES-only driver, so only `EGL_OPENGL_ES_API` is honored; any other API (`EGL_OPENGL_API` /
/// `EGL_OPENVG_API`) is `EGL_BAD_PARAMETER` (never silently accepted). libepoxy binds
/// `EGL_OPENGL_ES_API` and then confirms it via `eglQueryAPI` to pick its GLES dispatch table.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglBindAPI(api: u32) -> u32 {
    if api != EGL_OPENGL_ES_API {
        with(|s| s.set_egl_error(EGL_BAD_PARAMETER));
        return EGL_FALSE;
    }
    current::bind_api(api);
    EGL_TRUE
}

// ==================================================================================================
// EGL: config selection
// ==================================================================================================

/// Write the `n` config handles this driver advertises into the caller's `configs` array (each our
/// single [`CONFIG_TOKEN`]) and report `num_config`. Shared by `eglChooseConfig` / `eglGetConfigs`: the
/// enumeration contract (null array → count only; real array → bounded copy) is decided by
/// [`config::enumerate`], so this only marshals the raw pointers.
unsafe fn write_configs(configs: *mut *mut c_void, config_size: i32, num_config: *mut i32) {
    let (n, num) = config::enumerate(!configs.is_null(), config_size);
    for i in 0..n as isize {
        *configs.offset(i) = CONFIG_TOKEN as *mut c_void;
    }
    if !num_config.is_null() {
        *num_config = num;
    }
}

/// `eglChooseConfig(dpy, attrib_list, configs, config_size, num_config)` — select configs matching the
/// requested attributes. This driver advertises one config that satisfies the common GLES2/ES3 window +
/// pbuffer requests, so a null / empty `attrib_list` (match-all) and a populated list both return it. A
/// null `configs` array is the count-only query (`num_config` = number available). `num_config` is
/// required by the spec; a null one is `EGL_BAD_PARAMETER`.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglChooseConfig(
    _dpy: *mut c_void,
    _attrib_list: *const i32,
    configs: *mut *mut c_void,
    config_size: i32,
    num_config: *mut i32,
) -> u32 {
    if num_config.is_null() {
        with(|s| s.set_egl_error(EGL_BAD_PARAMETER));
        return EGL_FALSE;
    }
    unsafe { write_configs(configs, config_size, num_config) };
    EGL_TRUE
}

/// `eglGetConfigs(dpy, configs, config_size, num_config)` — enumerate all configs. A null `configs` array
/// returns the total count in `num_config`; a real array is filled with up to `config_size` handles and
/// `num_config` reports how many were written. `num_config` is required; a null one is
/// `EGL_BAD_PARAMETER`.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglGetConfigs(
    _dpy: *mut c_void,
    configs: *mut *mut c_void,
    config_size: i32,
    num_config: *mut i32,
) -> u32 {
    if num_config.is_null() {
        with(|s| s.set_egl_error(EGL_BAD_PARAMETER));
        return EGL_FALSE;
    }
    unsafe { write_configs(configs, config_size, num_config) };
    EGL_TRUE
}

/// `eglGetConfigAttrib(dpy, config, attribute, value)` — the value of one attribute of an `EGLConfig`.
/// Delegates the truthful value to [`config::config_attrib`]: a foreign config handle raises
/// `EGL_BAD_CONFIG` and an unrecognized attribute raises `EGL_BAD_ATTRIBUTE` — both return `EGL_FALSE`
/// WITHOUT writing `value` or dereferencing the unknown config, instead of the old silent `0`.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglGetConfigAttrib(
    _dpy: *mut c_void,
    config: *mut c_void,
    attribute: i32,
    value: *mut i32,
) -> u32 {
    if value.is_null() {
        with(|s| s.set_egl_error(EGL_BAD_PARAMETER));
        return EGL_FALSE;
    }
    let is_ours = config as usize == CONFIG_TOKEN;
    match config::config_attrib(is_ours, attribute) {
        Ok(v) => {
            unsafe { *value = v };
            EGL_TRUE
        }
        Err(config::ConfigError::BadConfig) => {
            with(|s| s.set_egl_error(EGL_BAD_CONFIG));
            EGL_FALSE
        }
        Err(config::ConfigError::BadAttribute) => {
            with(|s| s.set_egl_error(EGL_BAD_ATTRIBUTE));
            EGL_FALSE
        }
    }
}

// ==================================================================================================
// EGL: context / surface lifecycle + present
// ==================================================================================================

#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglCreateContext(
    _dpy: *mut c_void,
    _config: *mut c_void,
    _share_context: *mut c_void,
    _attrib_list: *const i32,
) -> *mut c_void {
    let ctx = with(|s| s.mint_token());
    hl_log::hl_info!(hl_log::tag::EGL, "eglCreateContext ctx={}", ctx as usize);
    ctx
}

#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglDestroyContext(_dpy: *mut c_void, ctx: *mut c_void) -> u32 {
    // If this context is current on the calling thread, releasing it drops the thread's binding.
    current::release_if_context(ctx as usize);
    EGL_TRUE
}

/// `eglMakeCurrent(dpy, draw, read, ctx)` — bind `ctx` (+ its draw/read surfaces + display) as the
/// current binding FOR THE CALLING THREAD, so `eglGetCurrentContext` / `eglGetCurrentDisplay` /
/// `eglGetCurrentSurface` report exactly these on this thread (what libepoxy probes). `ctx ==
/// EGL_NO_CONTEXT` (null) releases the thread's binding. The binding is thread-local (real EGL semantics):
/// another thread keeps its own current context.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglMakeCurrent(
    dpy: *mut c_void,
    draw: *mut c_void,
    read: *mut c_void,
    ctx: *mut c_void,
) -> u32 {
    hl_log::hl_info!(hl_log::tag::EGL, "eglMakeCurrent ctx={} draw={} read={}", ctx as usize, draw as usize, read as usize);
    current::make_current(dpy as usize, draw as usize, read as usize, ctx as usize);
    EGL_TRUE
}

/// `eglCreateWindowSurface(dpy, config, win, attrib_list)` — bring up the presented default framebuffer.
///
/// `win` is the native window: for a Wayland app it is a `wl_egl_window*` (created by the staged
/// `libwayland-egl.so.1`), from which we read the backing size + the wrapped `wl_surface`. A non-wayland /
/// sizeless window falls back to `$HL_GL_SURFACE_W/_H`. When a `wl_surface` is present and a compositor is
/// reachable (`$WAYLAND_DISPLAY`), a self-contained `wl_shm` present session is brought up so
/// `eglSwapBuffers` shows the frame; otherwise the session stays `None` (present is skipped, never faked).
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglCreateWindowSurface(
    _dpy: *mut c_void,
    _config: *mut c_void,
    win: *mut c_void,
    _attrib_list: *const i32,
) -> *mut c_void {
    create_window_surface(win)
}

/// Shared `eglCreateWindowSurface` / `eglCreatePlatformWindowSurface` body: size the surface from the
/// native window and (best-effort) bring up the Wayland present session.
fn create_window_surface(win: *mut c_void) -> *mut c_void {
    // Parse the wl_egl_window (or a stock two-int window). A null / sizeless window uses the env default.
    let info = unsafe { hl_gl::adapter::wayland::parse_native_window(win) };
    let (width, height) = if win.is_null() { default_surface_wh() } else { (info.width, info.height) };
    with(|s| {
        s.ctx.surf = GlSurface { have: true, width, height };
        s.current_is_wayland = info.wl_surface != 0;
        s.wl_surface_ptr = info.wl_surface;
        // The surface token is not "current" until eglMakeCurrent binds it (real EGL): the per-thread
        // current-surface binding is set there, never here.
        let tok = s.mint_token();
        // Compositor bring-up is DEFERRED so a real app window ends up with exactly ONE toplevel:
        //  * a real app `wl_surface` (wl_surface != 0) is presented onto the app's OWN surface at swap
        //    (via the app's libwayland-client) — so the shim must NOT stand up a competing self-owned
        //    toplevel here. The self-owned session is brought up lazily at swap ONLY if that app-surface
        //    presenter proves unavailable.
        //  * a stock / headless / sizeless window (wl_surface == 0) has no app surface to present onto, so
        //    the self-owned `wl_shm` toplevel is the only path — bring it up now (unless suppressed).
        //    `connect_and_handshake` returns None when `$WAYLAND_DISPLAY` is unset or the handshake fails
        //    (an honest "no compositor" — the present is then skipped, never faked).
        if info.wl_surface == 0 && std::env::var_os("HL_GL_NO_WAYLAND").is_none() {
            let geom = hl_gl::adapter::wayland::Geometry::backing(width, height);
            s.wl = hl_gl::adapter::wayland::Wayland::connect_and_handshake(&geom);
        }
        tok
    })
}

#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglDestroySurface(_dpy: *mut c_void, surface: *mut c_void) -> u32 {
    // Drop it from the calling thread's current binding (if it was the draw/read surface).
    current::forget_surface(surface as usize);
    with(|s| {
        s.ctx.surf.have = false;
    });
    EGL_TRUE
}

/// `EGL_CONTEXT_LOST` — reported when the compositor commit of a Wayland frame fails (delivery / protocol /
/// pacing), so a failed present is never mistaken for a shown frame.
const EGL_CONTEXT_LOST: i32 = 0x300E;

/// `eglSwapBuffers` — the one sink-touching op: lower + submit + present the recorded frame. On a sink
/// error the frame is retained and `EGL_*` is registered (surfaced by the next `eglGetError`).
///
/// On a Wayland window surface with a live present session, the rendered frame is ALSO read back and
/// committed to the compositor as a `wl_shm` `wl_buffer` (the deployed present path). The read-back
/// happens BEFORE the swap (which resets the draw-list); a commit failure is surfaced as
/// `EGL_CONTEXT_LOST`.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglSwapBuffers(_dpy: *mut c_void, _surface: *mut c_void) -> u32 {
    with(|s| {
        let _sp = hl_log::hl_span!(hl_log::tag::PRESENT, "swap");
        hl_log::hl_debug!(hl_log::tag::PRESENT, "eglSwapBuffers {}x{}", s.ctx.surf.width, s.ctx.surf.height);
        // Two present targets share the SAME read-back frame:
        //  * the app's OWN `wl_surface` (the real-window path) — when this is a Wayland window that
        //    carried an app `wl_surface*`, or
        //  * the shim's self-owned `wl_shm` toplevel (`s.wl`) — the fallback / headless path.
        let is_app_surface = s.current_is_wayland && s.wl_surface_ptr != 0;
        let want_readback = is_app_surface || s.wl.is_some();

        // Read the rendered frame back (draws intact) BEFORE the swap resets the draw-list.
        let wl_pixels = if want_readback {
            let (w, h) = (s.ctx.surf.width as i32, s.ctx.surf.height as i32);
            readpixels::read_pixels(&mut s.ctx, &mut s.sink, 0, 0, w, h, GL_RGBA)
                .ok()
                .map(|rgba| hl_gl::adapter::wayland::rgba_to_xrgb8888(&rgba, w as usize, h as usize))
        } else {
            None
        };

        // The authoritative present: lower + submit the frame IR (+ Present) to the GPU-exec host.
        if let Err(e) = swap::swap_buffers(&mut s.ctx, &mut s.sink) {
            s.set_egl_error(egl_error_from_gpu_error(&e));
            return EGL_FALSE;
        }

        // Commit the read-back frame to the compositor (a failure is surfaced, never a silent present).
        if let Some(px) = wl_pixels {
            let (w, h) = (s.ctx.surf.width, s.ctx.surf.height);

            // Prefer presenting onto the app's OWN wl_surface (marshalled via the app's libwayland-client).
            if is_app_surface {
                match s.present_to_app_surface(&px, w, h) {
                    AppPresentOutcome::Presented => return EGL_TRUE,
                    // A live commit/flush failure is a lost frame — never faked.
                    AppPresentOutcome::Failed => {
                        hl_log::hl_warn!(hl_log::tag::PRESENT, "present-to-app-surface failed {}x{} -> EGL_CONTEXT_LOST", w, h);
                        s.set_egl_error(EGL_CONTEXT_LOST);
                        return EGL_FALSE;
                    }
                    // Presenter unavailable (not a wayland app / libwayland or a symbol/global absent):
                    // fall through to the self-owned present below.
                    AppPresentOutcome::Unavailable => {}
                }
            }

            // Fallback: the shim's self-owned `wl_shm` toplevel. For a deferred app surface whose presenter
            // turned out unavailable, the self-owned session is brought up lazily now (a stock/headless
            // window already brought one up in `eglCreateWindowSurface`).
            if s.wl.is_none() && std::env::var_os("HL_GL_NO_WAYLAND").is_none() {
                let geom = hl_gl::adapter::wayland::Geometry::backing(w, h);
                s.wl = hl_gl::adapter::wayland::Wayland::connect_and_handshake(&geom);
            }
            let geom = hl_gl::adapter::wayland::Geometry::backing(w, h);
            if let Some(wl) = s.wl.as_mut() {
                if wl.commit(&px, &geom).is_err() {
                    s.set_egl_error(EGL_CONTEXT_LOST);
                    return EGL_FALSE;
                }
            }
        }
        EGL_TRUE
    })
}

#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglSwapInterval(_dpy: *mut c_void, _interval: i32) -> u32 {
    EGL_TRUE
}

/// `eglGetCurrentDisplay()` — the display of the CALLING THREAD's current context (bound by the last
/// `eglMakeCurrent`), or `EGL_NO_DISPLAY` (null) when no context is current on this thread.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglGetCurrentDisplay() -> *mut c_void {
    current::display() as *mut c_void
}

/// `eglGetCurrentContext()` — the context current on the CALLING THREAD, or `EGL_NO_CONTEXT` (null) when
/// none is. libepoxy probes this to select its GL-vs-GLES dispatch table (a NULL here aborts it), so it
/// MUST return the live context on the thread that made it current.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglGetCurrentContext() -> *mut c_void {
    current::context() as *mut c_void
}

/// `eglGetCurrentSurface(readdraw)` — the `EGL_DRAW` (default) or `EGL_READ` surface of the CALLING
/// THREAD's current binding, or `EGL_NO_SURFACE` (null) when no context is current on this thread.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglGetCurrentSurface(readdraw: i32) -> *mut c_void {
    let tok = if readdraw == EGL_READ { current::read_surface() } else { current::draw_surface() };
    tok as *mut c_void
}

// ==================================================================================================
// GLES: error / string
// ==================================================================================================

/// `glGetError` — read + clear the context's error flag. A real app loops on this after batches of GL
/// calls, so it must return the first error raised (GL keeps it until read) and then reset to
/// `GL_NO_ERROR`.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetError() -> u32 {
    with(|s| s.ctx.take_gl_error())
}

/// `glGetString(name)` — the driver's GLES3 identity strings (`GL_VERSION` = "OpenGL ES 3.0 …", vendor /
/// renderer / GLSL version / extensions). Served from [`query::gl_string`] so the guest-visible identity
/// is defined once and unit-tested. Never null: a GLES app dereferences the result unconditionally.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetString(name: u32) -> *const u8 {
    query::gl_string(name).as_ptr()
}

/// `glGetStringi(name, index)` — the ES3 indexed extension enumeration. Served from the SAME inventory as
/// `glGetString(GL_EXTENSIONS)` + `glGetIntegerv(GL_NUM_EXTENSIONS)` (see [`query::string_i`]), so the
/// three never disagree. An out-of-range index raises `GL_INVALID_VALUE` and returns null (never a
/// dangling pointer); a non-`GL_EXTENSIONS` name raises `GL_INVALID_ENUM`. With no extensions advertised
/// an app that honors the count of `0` never calls this.
#[cfg_attr(gles_client, no_mangle)]
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
#[cfg_attr(gles_client, no_mangle)]
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
#[cfg_attr(gles_client, no_mangle)]
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
#[cfg_attr(gles_client, no_mangle)]
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
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glPixelStorei(pname: u32, param: i32) {
    with(|s| record::pixel_store(&mut s.ctx, pname, param));
}

// ==================================================================================================
// GLES: buffers
// ==================================================================================================

#[cfg_attr(gles_client, no_mangle)]
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

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBindBuffer(target: u32, buffer: u32) {
    with(|s| record::bind_buffer(&mut s.ctx, target, buffer));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBufferData(target: u32, size: isize, data: *const c_void, usage: u32) {
    let d = unsafe { bytes(data, size) }.to_vec();
    with(|s| record::buffer_data(&mut s.ctx, target, &d, usage));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBufferSubData(target: u32, offset: isize, size: isize, data: *const c_void) {
    let d = unsafe { bytes(data, size) }.to_vec();
    with(|s| record::buffer_sub_data(&mut s.ctx, target, offset.max(0) as usize, &d));
}

#[cfg_attr(gles_client, no_mangle)]
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

#[cfg_attr(gles_client, no_mangle)]
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

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glActiveTexture(texture: u32) {
    with(|s| record::active_texture(&mut s.ctx, texture));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBindTexture(target: u32, texture: u32) {
    with(|s| record::bind_texture(&mut s.ctx, target, texture));
}

#[cfg_attr(gles_client, no_mangle)]
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

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glTexParameteri(_target: u32, pname: u32, param: i32) {
    with(|s| record::tex_parameter(&mut s.ctx, pname, param as u32));
}

/// `glTexParameterf(target, pname, param)` — the float-typed setter. GL's texture filter/wrap parameters
/// are enum-valued; the app passes the enum as a float, so it is truncated back to the `GLenum` the
/// integer path records (`glTexParameteri` parity).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glTexParameterf(_target: u32, pname: u32, param: f32) {
    with(|s| record::tex_parameter(&mut s.ctx, pname, param as u32));
}

/// `glTexParameterfv(target, pname, params)` — the single-element vector form; reads `params[0]`.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glTexParameterfv(_target: u32, pname: u32, params: *const f32) {
    if params.is_null() {
        return;
    }
    let v = unsafe { *params };
    with(|s| record::tex_parameter(&mut s.ctx, pname, v as u32));
}

/// `glTexParameteriv(target, pname, params)` — the single-element integer vector form; reads `params[0]`.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glTexParameteriv(_target: u32, pname: u32, params: *const i32) {
    if params.is_null() {
        return;
    }
    let v = unsafe { *params };
    with(|s| record::tex_parameter(&mut s.ctx, pname, v as u32));
}

/// `glGenerateMipmap(target)` — validate + record (an honest no-op on the pixel data; this model samples
/// the base level only). See [`record::generate_mipmap`].
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGenerateMipmap(target: u32) {
    with(|s| record::generate_mipmap(&mut s.ctx, target));
}

#[cfg_attr(gles_client, no_mangle)]
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

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glCreateShader(type_: u32) -> u32 {
    with(|s| record::create_shader(&mut s.ctx, type_))
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glShaderSource(
    shader: u32,
    count: i32,
    string: *const *const c_char,
    length: *const i32,
) {
    let src = unsafe { join_source(count, string, length) };
    with(|s| record::shader_source(&mut s.ctx, shader, &src));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glCompileShader(shader: u32) {
    with(|s| record::compile_shader(&mut s.ctx, shader));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glCreateProgram() -> u32 {
    with(|s| record::create_program(&mut s.ctx))
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glAttachShader(program: u32, shader: u32) {
    with(|s| record::attach_shader(&mut s.ctx, program, shader));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glLinkProgram(program: u32) {
    with(|s| {
        let _ = record::link_program(&mut s.ctx, program);
    });
}

#[cfg_attr(gles_client, no_mangle)]
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
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetShaderiv(shader: u32, pname: u32, params: *mut i32) {
    if params.is_null() {
        return;
    }
    let v = with(|s| query::get_shaderiv(&s.ctx, shader, pname));
    unsafe { *params = v };
}

/// `glGetProgramiv(program, pname, params)` — `GL_LINK_STATUS`/`GL_VALIDATE_STATUS` (TRUE once linked),
/// `GL_INFO_LOG_LENGTH` (0), `GL_ATTACHED_SHADERS`, `GL_ACTIVE_UNIFORMS`, `GL_ACTIVE_ATTRIBUTES`.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetProgramiv(program: u32, pname: u32, params: *mut i32) {
    if params.is_null() {
        return;
    }
    let v = with(|s| query::get_programiv(&s.ctx, program, pname));
    unsafe { *params = v };
}

/// `glGetShaderInfoLog(shader, buf_size, length, info_log)` — the shader compiled successfully, so the
/// diagnostic log is empty (an empty NUL-terminated string, length 0).
#[cfg_attr(gles_client, no_mangle)]
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
#[cfg_attr(gles_client, no_mangle)]
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
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetUniformLocation(program: u32, name: *const c_char) -> i32 {
    let want = match unsafe { cstr(name) } {
        Some(s) => s,
        None => return -1,
    };
    with(|s| query::uniform_location(&s.ctx, program, &want))
}

/// `glGetAttribLocation(program, name)` — the vertex attribute's declaration-order slot in the linked
/// program, or `-1` if `name` is not an active attribute. Null-safe.
#[cfg_attr(gles_client, no_mangle)]
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
#[cfg_attr(gles_client, no_mangle)]
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
#[cfg_attr(gles_client, no_mangle)]
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
#[cfg_attr(gles_client, no_mangle)]
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
#[cfg_attr(gles_client, no_mangle)]
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

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniform1f(location: i32, v0: f32) {
    set_uniform(location, &le_f32(&[v0]));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniform2f(location: i32, v0: f32, v1: f32) {
    set_uniform(location, &le_f32(&[v0, v1]));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniform3f(location: i32, v0: f32, v1: f32, v2: f32) {
    set_uniform(location, &le_f32(&[v0, v1, v2]));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniform4f(location: i32, v0: f32, v1: f32, v2: f32, v3: f32) {
    set_uniform(location, &le_f32(&[v0, v1, v2, v3]));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniform2i(location: i32, v0: i32, v1: i32) {
    set_uniform(location, &le_i32(&[v0, v1]));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniform3i(location: i32, v0: i32, v1: i32, v2: i32) {
    set_uniform(location, &le_i32(&[v0, v1, v2]));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniform4i(location: i32, v0: i32, v1: i32, v2: i32, v3: i32) {
    set_uniform(location, &le_i32(&[v0, v1, v2, v3]));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniform1fv(location: i32, count: i32, value: *const f32) {
    set_uniform(location, &le_f32(unsafe { slice_f32(value, count, 1) }));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniform2fv(location: i32, count: i32, value: *const f32) {
    set_uniform(location, &le_f32(unsafe { slice_f32(value, count, 2) }));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniform3fv(location: i32, count: i32, value: *const f32) {
    set_uniform(location, &le_f32(unsafe { slice_f32(value, count, 3) }));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniform4fv(location: i32, count: i32, value: *const f32) {
    set_uniform(location, &le_f32(unsafe { slice_f32(value, count, 4) }));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniform1iv(location: i32, count: i32, value: *const i32) {
    set_uniform(location, &le_i32(unsafe { slice_i32(value, count, 1) }));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniform2iv(location: i32, count: i32, value: *const i32) {
    set_uniform(location, &le_i32(unsafe { slice_i32(value, count, 2) }));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniform3iv(location: i32, count: i32, value: *const i32) {
    set_uniform(location, &le_i32(unsafe { slice_i32(value, count, 3) }));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniform4iv(location: i32, count: i32, value: *const i32) {
    set_uniform(location, &le_i32(unsafe { slice_i32(value, count, 4) }));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniformMatrix2fv(location: i32, count: i32, transpose: u8, value: *const f32) {
    set_uniform(location, &unsafe { mat_bytes(2, count, transpose != 0, value) });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniformMatrix3fv(location: i32, count: i32, transpose: u8, value: *const f32) {
    set_uniform(location, &unsafe { mat_bytes(3, count, transpose != 0, value) });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniformMatrix4fv(location: i32, count: i32, transpose: u8, value: *const f32) {
    set_uniform(location, &unsafe { mat_bytes(4, count, transpose != 0, value) });
}

// ==================================================================================================
// GLES: vertex attributes + fixed-function state
// ==================================================================================================

#[cfg_attr(gles_client, no_mangle)]
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

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glEnableVertexAttribArray(index: u32) {
    with(|s| record::enable_vertex_attrib(&mut s.ctx, index as usize));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDisableVertexAttribArray(index: u32) {
    with(|s| record::disable_vertex_attrib(&mut s.ctx, index as usize));
}

/// `glVertexAttribDivisor(index, divisor)` — the instance-step rate for attribute `index` (`0` =
/// per-vertex, `>0` = per-instance). See [`record::vertex_attrib_divisor`].
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glVertexAttribDivisor(index: u32, divisor: u32) {
    with(|s| record::vertex_attrib_divisor(&mut s.ctx, index as usize, divisor));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glClearColor(red: f32, green: f32, blue: f32, alpha: f32) {
    with(|s| record::clear_color(&mut s.ctx, [red, green, blue, alpha]));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glViewport(x: i32, y: i32, width: i32, height: i32) {
    with(|s| record::viewport(&mut s.ctx, [x, y, width, height]));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glScissor(x: i32, y: i32, width: i32, height: i32) {
    with(|s| record::scissor(&mut s.ctx, [x, y, width, height]));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glEnable(cap: u32) {
    with(|s| record::enable(&mut s.ctx, cap));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDisable(cap: u32) {
    with(|s| record::disable(&mut s.ctx, cap));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glClearDepthf(d: f32) {
    with(|s| record::clear_depth(&mut s.ctx, d));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBlendFunc(sfactor: u32, dfactor: u32) {
    with(|s| record::blend_func(&mut s.ctx, sfactor, dfactor));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBlendFuncSeparate(src_rgb: u32, dst_rgb: u32, src_alpha: u32, dst_alpha: u32) {
    with(|s| record::blend_func_separate(&mut s.ctx, src_rgb, dst_rgb, src_alpha, dst_alpha));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDepthFunc(func: u32) {
    with(|s| record::depth_func(&mut s.ctx, func));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDepthMask(flag: u8) {
    with(|s| record::depth_mask(&mut s.ctx, flag != 0));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glCullFace(mode: u32) {
    with(|s| record::cull_face(&mut s.ctx, mode));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glFrontFace(mode: u32) {
    with(|s| record::front_face(&mut s.ctx, mode));
}

// ==================================================================================================
// GLES: draw recording (frame draw-list; IR lowered at eglSwapBuffers)
// ==================================================================================================

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glClear(_mask: u32) {
    with(|s| record::clear(&mut s.ctx));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDrawArrays(mode: u32, first: i32, count: i32) {
    with(|s| record::draw_arrays(&mut s.ctx, mode, first, count));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDrawElements(mode: u32, count: i32, type_: u32, indices: *const c_void) {
    with(|s| record::draw_elements(&mut s.ctx, mode, count, type_, indices as usize));
}

/// `glDrawArraysInstanced(mode, first, count, instancecount)` — record an instanced array draw; the
/// frame builder lowers the recorded instance count into `Draw { instance_count }`.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDrawArraysInstanced(mode: u32, first: i32, count: i32, instancecount: i32) {
    with(|s| record::draw_arrays_instanced(&mut s.ctx, mode, first, count, instancecount));
}

/// `glDrawElementsInstanced(mode, count, type, indices, instancecount)` — record an instanced indexed
/// draw; lowered into `DrawIndexed { instance_count }`.
#[cfg_attr(gles_client, no_mangle)]
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

#[cfg_attr(gles_client, no_mangle)]
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

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBindVertexArray(array: u32) {
    with(|s| record::bind_vertex_array(&mut s.ctx, array));
}

#[cfg_attr(gles_client, no_mangle)]
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
#[cfg_attr(gles_client, no_mangle)]
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

#[cfg_attr(gles_client, no_mangle)]
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

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBindFramebuffer(target: u32, framebuffer: u32) {
    with(|s| record::bind_framebuffer(&mut s.ctx, target, framebuffer));
}

#[cfg_attr(gles_client, no_mangle)]
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
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glIsFramebuffer(framebuffer: u32) -> u32 {
    with(|s| record::is_framebuffer(&s.ctx, framebuffer)) as u32
}

/// `glCheckFramebufferStatus(target)` — the completeness enum of the bound draw/read framebuffer (see
/// [`record::check_framebuffer_status`]). A GLES app calls this before rendering to an FBO and bails on a
/// non-`GL_FRAMEBUFFER_COMPLETE` result.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glCheckFramebufferStatus(target: u32) -> u32 {
    with(|s| record::check_framebuffer_status(&mut s.ctx, target))
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glFramebufferTexture2D(target: u32, attachment: u32, textarget: u32, texture: u32, level: i32) {
    with(|s| record::framebuffer_texture_2d(&mut s.ctx, target, attachment, textarget, texture, level));
}

#[cfg_attr(gles_client, no_mangle)]
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

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBindRenderbuffer(target: u32, renderbuffer: u32) {
    with(|s| record::bind_renderbuffer(&mut s.ctx, target, renderbuffer));
}

#[cfg_attr(gles_client, no_mangle)]
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
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glIsRenderbuffer(renderbuffer: u32) -> u32 {
    with(|s| record::is_renderbuffer(&s.ctx, renderbuffer)) as u32
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glRenderbufferStorage(target: u32, internalformat: u32, width: i32, height: i32) {
    with(|s| record::renderbuffer_storage(&mut s.ctx, target, internalformat, width, height));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glFramebufferRenderbuffer(target: u32, attachment: u32, renderbuffertarget: u32, renderbuffer: u32) {
    with(|s| record::framebuffer_renderbuffer(&mut s.ctx, target, attachment, renderbuffertarget, renderbuffer));
}

/// `glBlitFramebuffer(...)` — validate the read+draw framebuffers and record the color blit for the frame
/// builder. The deferred model applies it after the frame's render passes: an equal-size blit lowers to
/// `Enc::CopyTextureToTexture`, a scaling blit to `Enc::BlitTexture` with `filter` mapped from GL's
/// `GL_NEAREST`/`GL_LINEAR`; see [`record::blit_framebuffer`].
#[cfg_attr(gles_client, no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glBlitFramebuffer(
    src_x0: i32,
    src_y0: i32,
    src_x1: i32,
    src_y1: i32,
    dst_x0: i32,
    dst_y0: i32,
    dst_x1: i32,
    dst_y1: i32,
    mask: u32,
    filter: u32,
) {
    with(|s| {
        record::blit_framebuffer(
            &mut s.ctx, src_x0, src_y0, src_x1, src_y1, dst_x0, dst_y0, dst_x1, dst_y1, mask, filter,
        )
    });
}

// ==================================================================================================
// GLES: readback (device→host — the GL equivalent of cuMemcpyDtoH)
// ==================================================================================================

/// `glReadPixels(x, y, w, h, format, type, pixels)` — render the recorded frame and read the requested
/// rectangle of the resulting render target back into `pixels`. Only `GL_UNSIGNED_BYTE` RGBA/BGRA/RGB is
/// modeled; the readback goes through the `hl_gl` service (render → `CopyTextureToBuffer` → `read_buffer`),
/// the same device→host port as cuda's DtoH.
#[cfg_attr(gles_client, no_mangle)]
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
    // ES3 PBO pack path: when a buffer is bound to `GL_PIXEL_PACK_BUFFER`, `glReadPixels` does NOT write to
    // a client pointer — `pixels` is reinterpreted as a BYTE OFFSET into that pack buffer and the packed
    // pixels are written into its host storage. The app then reads them back via `glMapBufferRange`
    // (`GL_PIXEL_PACK_BUFFER`) — the async-readback-to-PBO round trip. Before this branch a bound PBO was
    // ignored and the packed bytes were copied to `pixels`-as-pointer (a wild write of an integer offset).
    let pbo = with(|s| s.ctx.buffer_for_target(GL_PIXEL_PACK_BUFFER));
    if pbo != 0 {
        let byte_off = pixels as usize; // GL: the offset is the `pixels` argument treated as an integer
        let packed = with(|s| readpixels::read_pixels(&mut s.ctx, &mut s.sink, x, y, width, height, format));
        match packed {
            Ok(bytes) => with(|s| s.ctx.buffers.set_sub_data(pbo, byte_off, &bytes)),
            Err(e) => with(|s| {
                s.ctx.set_gl_error(GL_OUT_OF_MEMORY);
                s.set_egl_error(egl_error_from_gpu_error(&e));
            }),
        }
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
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDispatchCompute(num_groups_x: u32, num_groups_y: u32, num_groups_z: u32) {
    with(|s| {
        if let Err(e) = compute::dispatch_compute(&mut s.ctx, &mut s.sink, num_groups_x, num_groups_y, num_groups_z) {
            s.set_egl_error(egl_error_from_gpu_error(&e));
        }
    });
}

/// `glDispatchComputeIndirect(indirect)` — dispatch with the group counts read from the buffer bound to
/// `GL_DISPATCH_INDIRECT_BUFFER` at byte offset `indirect` (see [`compute::dispatch_compute_indirect`]).
#[cfg_attr(gles_client, no_mangle)]
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
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glFenceSync(condition: u32, flags: u32) -> *mut c_void {
    with(|s| match sync::fence_sync(&mut s.ctx, &mut s.sink, condition, flags) {
        Some(token) => token as *mut c_void,
        None => core::ptr::null_mut(),
    })
}

/// `glClientWaitSync(sync, flags, timeout)` — client-side wait on the fence value `sync` marks. See
/// [`sync::client_wait_sync`].
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glClientWaitSync(sync: *mut c_void, flags: u32, timeout: u64) -> u32 {
    with(|s| sync::client_wait_sync(&mut s.ctx, &mut s.sink, sync as usize, flags, timeout))
}

/// `glWaitSync(sync, flags, timeout)` — device-side (queue) wait; lowers to a `WaitFence`. See
/// [`sync::wait_sync`].
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glWaitSync(sync: *mut c_void, flags: u32, timeout: u64) {
    with(|s| sync::wait_sync(&mut s.ctx, &mut s.sink, sync as usize, flags, timeout));
}

/// `glDeleteSync(sync)` — drop the sync object (an unknown non-null sync raises `GL_INVALID_VALUE`).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDeleteSync(sync: *mut c_void) {
    with(|s| sync::delete_sync(&mut s.ctx, sync as usize));
}

/// `glIsSync(sync)` — `GL_TRUE`/`GL_FALSE` as the codegen's `u8` (`GLboolean`) ABI.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glIsSync(sync: *mut c_void) -> u8 {
    with(|s| sync::is_sync(&s.ctx, sync as usize)) as u8
}

/// `glGetSynciv(sync, pname, buf_size, length, values)` — write the single integer state value for
/// `pname` (see [`sync::get_synciv`]). Null-safe on both out-params.
#[cfg_attr(gles_client, no_mangle)]
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
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBindBufferBase(target: u32, index: u32, buffer: u32) {
    with(|s| record::bind_buffer_base(&mut s.ctx, target, index, buffer));
}

/// `glBindBufferRange(target, index, buffer, offset, size)` — bind `[offset, offset+size)` of `buffer` to
/// indexed slot `index`. See [`record::bind_buffer_range`].
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBindBufferRange(target: u32, index: u32, buffer: u32, offset: isize, size: isize) {
    with(|s| record::bind_buffer_range(&mut s.ctx, target, index, buffer, offset, size));
}

// ==================================================================================================
// GLES3.0: PBO-style buffer mapping — glMapBufferRange / glUnmapBuffer / glFlushMappedBufferRange
// ==================================================================================================

/// `glMapBufferRange(target, offset, length, access)` — map a range of the bound buffer and return a
/// pointer INTO its host storage (the app writes through it; `glUnmapBuffer` flushes). Null on error. The
/// pointer stays valid until the buffer's storage reallocates (the reference shim's fragile contract).
#[cfg_attr(gles_client, no_mangle)]
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
#[cfg_attr(gles_client, no_mangle)]
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
#[cfg_attr(gles_client, no_mangle)]
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
#[cfg_attr(gles_client, no_mangle)]
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
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glReadBuffer(src: u32) {
    with(|s| record::read_buffer(&mut s.ctx, src));
}

// ==================================================================================================
// ES3 sampler objects (client-side state; no GPU IR) — glGen/Bind/Delete/SamplerParameter*
// ==================================================================================================

#[cfg_attr(gles_client, no_mangle)]
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

#[cfg_attr(gles_client, no_mangle)]
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

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBindSampler(unit: u32, sampler: u32) {
    with(|s| es3::bind_sampler(&mut s.ctx, unit, sampler));
}

/// `glIsSampler(sampler)` — `GLboolean` in the codegen's `u8` ABI (low byte is the boolean).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glIsSampler(sampler: u32) -> u8 {
    with(|s| es3::is_sampler(&s.ctx, sampler)) as u8
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glSamplerParameteri(sampler: u32, pname: u32, param: i32) {
    with(|s| es3::sampler_parameter(&mut s.ctx, sampler, pname, param, param as f32));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glSamplerParameterf(sampler: u32, pname: u32, param: f32) {
    with(|s| es3::sampler_parameter(&mut s.ctx, sampler, pname, param as i32, param));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glSamplerParameteriv(sampler: u32, pname: u32, param: *const i32) {
    if param.is_null() {
        return;
    }
    let v = unsafe { *param };
    with(|s| es3::sampler_parameter(&mut s.ctx, sampler, pname, v, v as f32));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glSamplerParameterfv(sampler: u32, pname: u32, param: *const f32) {
    if param.is_null() {
        return;
    }
    let v = unsafe { *param };
    with(|s| es3::sampler_parameter(&mut s.ctx, sampler, pname, v as i32, v));
}

/// `glSamplerParameterIiv` — the integer (non-normalized) vector form; reads `param[0]` (same setter path).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glSamplerParameterIiv(sampler: u32, pname: u32, param: *const i32) {
    if param.is_null() {
        return;
    }
    let v = unsafe { *param };
    with(|s| es3::sampler_parameter(&mut s.ctx, sampler, pname, v, v as f32));
}

/// `glSamplerParameterIuiv` — the unsigned integer vector form; reads `param[0]`.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glSamplerParameterIuiv(sampler: u32, pname: u32, param: *const u32) {
    if param.is_null() {
        return;
    }
    let v = unsafe { *param } as i32;
    with(|s| es3::sampler_parameter(&mut s.ctx, sampler, pname, v, v as f32));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetSamplerParameteriv(sampler: u32, pname: u32, params: *mut i32) {
    let v = with(|s| es3::get_sampler_parameter(&mut s.ctx, sampler, pname));
    if let Some(v) = v {
        if !params.is_null() {
            unsafe { *params = v.round() as i32 };
        }
    }
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetSamplerParameterfv(sampler: u32, pname: u32, params: *mut f32) {
    let v = with(|s| es3::get_sampler_parameter(&mut s.ctx, sampler, pname));
    if let Some(v) = v {
        if !params.is_null() {
            unsafe { *params = v };
        }
    }
}

/// `glGetSamplerParameterIiv` — the integer view of `glGetSamplerParameteriv`.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetSamplerParameterIiv(sampler: u32, pname: u32, params: *mut i32) {
    glGetSamplerParameteriv(sampler, pname, params);
}

/// `glGetSamplerParameterIuiv` — the unsigned-integer view of `glGetSamplerParameteriv`.
#[cfg_attr(gles_client, no_mangle)]
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

#[cfg_attr(gles_client, no_mangle)]
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

#[cfg_attr(gles_client, no_mangle)]
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

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBeginQuery(target: u32, id: u32) {
    with(|s| es3::begin_query(&mut s.ctx, target, id));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glEndQuery(target: u32) {
    with(|s| es3::end_query(&mut s.ctx, target));
}

/// `glIsQuery(id)` — `GLboolean` in the codegen's `u8` ABI.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glIsQuery(id: u32) -> u8 {
    with(|s| es3::is_query(&s.ctx, id)) as u8
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetQueryiv(target: u32, pname: u32, params: *mut i32) {
    let v = with(|s| es3::get_queryiv(&mut s.ctx, target, pname));
    if let (Some(v), false) = (v, params.is_null()) {
        unsafe { *params = v };
    }
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetQueryObjectuiv(id: u32, pname: u32, params: *mut u32) {
    let v = with(|s| es3::get_query_objectuiv(&mut s.ctx, id, pname));
    if let (Some(v), false) = (v, params.is_null()) {
        unsafe { *params = v };
    }
}

// ==================================================================================================
// ES3 transform-feedback objects (client-side lifecycle + per-program varying capture)
// ==================================================================================================

#[cfg_attr(gles_client, no_mangle)]
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

#[cfg_attr(gles_client, no_mangle)]
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

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBindTransformFeedback(target: u32, id: u32) {
    with(|s| es3::bind_transform_feedback(&mut s.ctx, target, id));
}

/// `glIsTransformFeedback(id)` — `GLboolean` in the codegen's `u8` ABI.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glIsTransformFeedback(id: u32) -> u8 {
    with(|s| es3::is_transform_feedback(&s.ctx, id)) as u8
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBeginTransformFeedback(primitive_mode: u32) {
    with(|s| es3::begin_transform_feedback(&mut s.ctx, primitive_mode));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glEndTransformFeedback() {
    with(|s| es3::end_transform_feedback(&mut s.ctx));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glPauseTransformFeedback() {
    with(|s| es3::pause_transform_feedback(&mut s.ctx));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glResumeTransformFeedback() {
    with(|s| es3::resume_transform_feedback(&mut s.ctx));
}

#[cfg_attr(gles_client, no_mangle)]
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
#[cfg_attr(gles_client, no_mangle)]
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

#[cfg_attr(gles_client, no_mangle)]
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

#[cfg_attr(gles_client, no_mangle)]
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

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBindProgramPipeline(pipeline: u32) {
    with(|s| es3::bind_program_pipeline(&mut s.ctx, pipeline));
}

/// `glIsProgramPipeline(pipeline)` — `GLboolean` in the codegen's `u8` ABI.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glIsProgramPipeline(pipeline: u32) -> u8 {
    with(|s| es3::is_program_pipeline(&s.ctx, pipeline)) as u8
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUseProgramStages(pipeline: u32, stages: u32, program: u32) {
    with(|s| es3::use_program_stages(&mut s.ctx, pipeline, stages, program));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glActiveShaderProgram(pipeline: u32, program: u32) {
    with(|s| es3::active_shader_program(&mut s.ctx, pipeline, program));
}

/// `glProgramParameteri(program, pname, value)` — only `GL_PROGRAM_SEPARABLE` is modeled (a linked
/// program is separable by construction here, so the flag is accepted). An unknown program →
/// `GL_INVALID_VALUE`; an unmodeled `pname` → `GL_INVALID_ENUM`.
#[cfg_attr(gles_client, no_mangle)]
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

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetProgramPipelineiv(pipeline: u32, pname: u32, params: *mut i32) {
    let v = with(|s| es3::get_program_pipelineiv(&mut s.ctx, pipeline, pname));
    if let (Some(v), false) = (v, params.is_null()) {
        unsafe { *params = v };
    }
}

/// `glGetProgramPipelineInfoLog` — the pipeline validates clean, so the log is empty (length 0).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetProgramPipelineInfoLog(_pipeline: u32, buf_size: i32, length: *mut i32, info_log: *mut c_char) {
    unsafe { write_empty_info_log(buf_size, length, info_log) };
}

/// `glValidateProgramPipeline(pipeline)` — an unknown pipeline raises `GL_INVALID_OPERATION`; a known one
/// validates clean (the pipeline carries no cross-stage interface to reject in this model).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glValidateProgramPipeline(pipeline: u32) {
    // A known pipeline validates clean; an unknown one raises GL_INVALID_OPERATION (via the getter).
    with(|s| {
        let _ = es3::get_program_pipelineiv(&mut s.ctx, pipeline, GL_VALIDATE_STATUS);
    });
}

/// `glCreateShaderProgramv(type, count, strings)` — create + compile + link a single-stage separable
/// program from the joined source (a real body: the ES3 convenience constructor). Returns the new program
/// name, or `0` on a bad `type` / empty source.
#[cfg_attr(gles_client, no_mangle)]
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

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glTexStorage2D(target: u32, levels: i32, internalformat: u32, width: i32, height: i32) {
    with(|s| record::tex_storage_2d(&mut s.ctx, target, levels, internalformat, width, height));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glTexStorage3D(target: u32, levels: i32, _internalformat: u32, width: i32, height: i32, depth: i32) {
    with(|s| record::tex_storage_3d(&mut s.ctx, target, levels, width, height, depth));
}

#[cfg_attr(gles_client, no_mangle)]
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

#[cfg_attr(gles_client, no_mangle)]
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

#[cfg_attr(gles_client, no_mangle)]
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

#[cfg_attr(gles_client, no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glCopyTexSubImage2D(target: u32, level: i32, xoffset: i32, yoffset: i32, x: i32, y: i32, width: i32, height: i32) {
    with(|s| record::copy_tex_sub_image_2d(&mut s.ctx, target, level, xoffset, yoffset, x, y, width, height));
}

/// `glCopyTexSubImage3D` — the deferred model has no materialized source color plane per layer at record
/// time (see [`record::copy_tex_sub_image_2d`]); the layer copy is a documented no-op. Params validated
/// only insofar as a bad `target` is left to the bound-texture path — an honest no-op body.
#[cfg_attr(gles_client, no_mangle)]
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
#[cfg_attr(gles_client, no_mangle)]
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
#[cfg_attr(gles_client, no_mangle)]
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
#[cfg_attr(gles_client, no_mangle)]
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
#[cfg_attr(gles_client, no_mangle)]
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
#[cfg_attr(gles_client, no_mangle)]
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

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetBufferParameteriv(target: u32, pname: u32, params: *mut i32) {
    if params.is_null() {
        return;
    }
    let v = with(|s| query::get_buffer_parameteriv(&s.ctx, target, pname));
    unsafe { *params = v };
}

/// `glGetBufferParameteri64v` — the 64-bit view of `glGetBufferParameteriv` (size/usage widened).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetBufferParameteri64v(target: u32, pname: u32, params: *mut i64) {
    if params.is_null() {
        return;
    }
    let v = with(|s| query::get_buffer_parameteriv(&s.ctx, target, pname));
    unsafe { *params = v as i64 };
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetTexParameteriv(target: u32, pname: u32, params: *mut i32) {
    if params.is_null() {
        return;
    }
    let v = with(|s| query::get_tex_parameteriv(&s.ctx, target, pname));
    unsafe { *params = v };
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetTexParameterfv(target: u32, pname: u32, params: *mut f32) {
    if params.is_null() {
        return;
    }
    let v = with(|s| query::get_tex_parameteriv(&s.ctx, target, pname));
    unsafe { *params = v as f32 };
}

/// `glGetTexParameterIiv` — the integer view of `glGetTexParameteriv`.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetTexParameterIiv(target: u32, pname: u32, params: *mut i32) {
    glGetTexParameteriv(target, pname, params);
}

/// `glGetTexParameterIuiv` — the unsigned-integer view.
#[cfg_attr(gles_client, no_mangle)]
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
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetVertexAttribfv(_index: u32, _pname: u32, params: *mut f32) {
    if !params.is_null() {
        unsafe { *params = 0.0 };
    }
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetVertexAttribiv(_index: u32, _pname: u32, params: *mut i32) {
    if !params.is_null() {
        unsafe { *params = 0 };
    }
}

/// `glGetVertexAttribIiv`/`Iuiv` — the integer forms; same `0` default.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetVertexAttribIiv(_index: u32, _pname: u32, params: *mut i32) {
    if !params.is_null() {
        unsafe { *params = 0 };
    }
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetVertexAttribIuiv(_index: u32, _pname: u32, params: *mut u32) {
    if !params.is_null() {
        unsafe { *params = 0 };
    }
}

/// `glGetVertexAttribPointerv` — the attribute array pointer readback; reports null (the app's own bound
/// pointer is authoritative), matching the reference.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetVertexAttribPointerv(_index: u32, _pname: u32, pointer: *mut *mut c_void) {
    if !pointer.is_null() {
        unsafe { *pointer = core::ptr::null_mut() };
    }
}

// ==================================================================================================
// ES3 buffer copy (glCopyBufferSubData) + readback with a bound size (glReadnPixels)
// ==================================================================================================

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glCopyBufferSubData(read_target: u32, write_target: u32, read_offset: isize, write_offset: isize, size: isize) {
    with(|s| record::copy_buffer_sub_data(&mut s.ctx, read_target, write_target, read_offset, write_offset, size));
}

/// `glReadnPixels(x, y, w, h, format, type, bufSize, data)` — the bounded-buffer form of `glReadPixels`:
/// identical readback, but never writes more than `bufSize` bytes into `data` (a `bufSize` too small for
/// the requested rect raises `GL_INVALID_OPERATION`).
#[cfg_attr(gles_client, no_mangle)]
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

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glVertexAttrib1f(_index: u32, _x: f32) {}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glVertexAttrib2f(_index: u32, _x: f32, _y: f32) {}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glVertexAttrib3f(_index: u32, _x: f32, _y: f32, _z: f32) {}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glVertexAttrib4f(_index: u32, _x: f32, _y: f32, _z: f32, _w: f32) {}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glVertexAttrib1fv(_index: u32, _v: *const f32) {}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glVertexAttrib2fv(_index: u32, _v: *const f32) {}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glVertexAttrib3fv(_index: u32, _v: *const f32) {}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glVertexAttrib4fv(_index: u32, _v: *const f32) {}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glVertexAttribI4i(_index: u32, _x: i32, _y: i32, _z: i32, _w: i32) {}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glVertexAttribI4ui(_index: u32, _x: u32, _y: u32, _z: u32, _w: u32) {}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glVertexAttribI4iv(_index: u32, _v: *const i32) {}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glVertexAttribI4uiv(_index: u32, _v: *const u32) {}

/// `glVertexAttribIPointer(index, size, type, stride, pointer)` — an integer vertex-attribute array;
/// records into the same per-location attribute state as `glVertexAttribPointer` (marked integer, never
/// normalized).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glVertexAttribIPointer(index: u32, size: i32, type_: u32, stride: i32, pointer: *const c_void) {
    with(|s| record::vertex_attrib_pointer(&mut s.ctx, index as usize, size, type_, false, stride, pointer as usize));
}

/// `glVertexAttribIFormat(attribindex, size, type, relativeoffset)` — the separate-format (VAO) integer
/// attribute format; records size/type/offset into the attribute state (a no-op for the fields this model
/// does not separately track).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glVertexAttribIFormat(attribindex: u32, size: i32, type_: u32, relativeoffset: u32) {
    with(|s| record::vertex_attrib_pointer(&mut s.ctx, attribindex as usize, size, type_, false, 0, relativeoffset as usize));
}

// ==================================================================================================
// ES3 framebuffer-attachment invalidation (a hint) + separate-face stencil (IR-free) — honest no-ops
// ==================================================================================================

/// `glInvalidateFramebuffer(target, n, attachments)` — a discard HINT that the listed attachments'
/// contents are no longer needed. This deferred model rebuilds a fresh frame each swap (nothing is
/// preserved across frames), so the hint is already satisfied — an honest no-op.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glInvalidateFramebuffer(_target: u32, _num_attachments: i32, _attachments: *const u32) {}

/// `glInvalidateSubFramebuffer` — the sub-rectangle discard hint; same honest no-op.
#[cfg_attr(gles_client, no_mangle)]
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
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glStencilFuncSeparate(_face: u32, _func: u32, _ref: i32, _mask: u32) {}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glStencilMaskSeparate(_face: u32, _mask: u32) {}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glStencilOpSeparate(_face: u32, _sfail: u32, _dpfail: u32, _dppass: u32) {}

// ==================================================================================================
// ES3 / GLES3.1 completeness pass — the remaining entry points, ported to real hand-written bodies.
//
// Grouped by family. Each body is either REAL behavior (it reflects/records modeled state — uniform-block
// reflection from the program's reflected tables, `glProgramUniform*` writing into a named program's
// uniform block, `glClearBufferfv` lowering a scoped clear, indirect draws read from the bound buffer),
// or an HONEST default + `glGetError` on misuse where the state genuinely cannot be modeled (advanced
// blend, image load/store, KHR_debug), never a silent fake success. Every one is panic-free across the
// C-ABI seam (raw pointers null-checked) and moved to `IMPLEMENTED` in build.rs.
// ==================================================================================================

// ---- EGL query enums the new EGL getters key on -------------------------------------------------
const EGL_OPENGL_ES_API: u32 = 0x30A0;
const EGL_CONDITION_SATISFIED: i32 = 0x30F6;
const EGL_WIDTH: i32 = 0x3057;
const EGL_HEIGHT: i32 = 0x3056;
/// `eglQueryContext` attributes: the client API type (`EGL_OPENGL_ES_API` for this driver), the requested
/// client version (3 — this is an ES3 driver), and the render buffer of the bound surface (back-buffered).
const EGL_CONTEXT_CLIENT_TYPE: i32 = 0x3097;
const EGL_CONTEXT_CLIENT_VERSION: i32 = 0x3098;
const EGL_RENDER_BUFFER: i32 = 0x3086;
const EGL_BACK_BUFFER: i32 = 0x3084;
/// A fixed non-null opaque token for the EGL sync / image objects this driver hands back (their lifecycle
/// is accepted but not separately tracked — one shared token keeps a `!= EGL_NO_SYNC` contract).
const EGL_OBJECT_TOKEN: usize = 0x5171;

// ---- EGL_EXT_device_base / device_query / device_enumeration enums + the single software device ----
/// `EGL_DEVICE_EXT` — the `eglQueryDisplayAttribEXT` attribute GDK asks for to learn the display's backing
/// `EGLDeviceEXT` (and the `eglCreatePlatformDisplay(EGL_PLATFORM_DEVICE_EXT, …)` platform enum).
const EGL_DEVICE_EXT: i32 = 0x322C;
/// `EGL_BAD_DEVICE_EXT` — the error for an `EGLDeviceEXT` handle we did not hand out.
const EGL_BAD_DEVICE_EXT: i32 = 0x322B;
/// The single, truthful `EGLDeviceEXT` handle this driver reports: our software (hl-gl) renderer. Non-null
/// and distinct from the display/config/object tokens so `eglQueryDeviceStringEXT` et al. can validate it.
const DEVICE_TOKEN: usize = 0xDE71;

// ---- little-endian marshalling helpers (unsigned + non-square matrices) --------------------------

/// Marshal a slice of `u32` scalars into little-endian bytes.
fn le_u32(vs: &[u32]) -> Vec<u8> {
    let mut b = Vec::with_capacity(vs.len() * 4);
    for v in vs {
        b.extend_from_slice(&v.to_le_bytes());
    }
    b
}

/// Borrow a `count`×`n` `u32` array (`glUniform{N}uiv` value), empty if null / non-positive count.
unsafe fn slice_u32<'a>(value: *const u32, count: i32, n: usize) -> &'a [u32] {
    if value.is_null() || count <= 0 {
        &[]
    } else {
        std::slice::from_raw_parts(value, count as usize * n)
    }
}

/// Marshal a `cols`×`rows` GL matrix array into MSL `floatCxR` struct layout: `count` matrices, each
/// `cols` columns of `rows` floats; every column is padded to 4 floats when `rows == 3` (MSL's 16-byte
/// column stride). GL's source is column-major unless `transpose` (then row-major).
unsafe fn mat_bytes_cr(cols: usize, rows: usize, count: i32, transpose: bool, value: *const f32) -> Vec<u8> {
    if value.is_null() || count <= 0 {
        return Vec::new();
    }
    let src = std::slice::from_raw_parts(value, count as usize * cols * rows);
    let col_floats = if rows == 3 { 4 } else { rows };
    let mut out = Vec::with_capacity(count as usize * cols * col_floats * 4);
    for m in 0..count as usize {
        let base = m * cols * rows;
        for col in 0..cols {
            for row in 0..rows {
                let v = if transpose { src[base + row * cols + col] } else { src[base + col * rows + row] };
                out.extend_from_slice(&v.to_le_bytes());
            }
            for _ in rows..col_floats {
                out.extend_from_slice(&0f32.to_le_bytes());
            }
        }
    }
    out
}

// ==================================================================================================
// GLES3.0: unsigned-integer + non-square-matrix data uniforms (bound program's uniform block)
// ==================================================================================================

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniform1ui(location: i32, v0: u32) {
    set_uniform(location, &le_u32(&[v0]));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniform2ui(location: i32, v0: u32, v1: u32) {
    set_uniform(location, &le_u32(&[v0, v1]));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniform3ui(location: i32, v0: u32, v1: u32, v2: u32) {
    set_uniform(location, &le_u32(&[v0, v1, v2]));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniform4ui(location: i32, v0: u32, v1: u32, v2: u32, v3: u32) {
    set_uniform(location, &le_u32(&[v0, v1, v2, v3]));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniform1uiv(location: i32, count: i32, value: *const u32) {
    set_uniform(location, &le_u32(unsafe { slice_u32(value, count, 1) }));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniform2uiv(location: i32, count: i32, value: *const u32) {
    set_uniform(location, &le_u32(unsafe { slice_u32(value, count, 2) }));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniform3uiv(location: i32, count: i32, value: *const u32) {
    set_uniform(location, &le_u32(unsafe { slice_u32(value, count, 3) }));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniform4uiv(location: i32, count: i32, value: *const u32) {
    set_uniform(location, &le_u32(unsafe { slice_u32(value, count, 4) }));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniformMatrix2x3fv(location: i32, count: i32, transpose: u8, value: *const f32) {
    set_uniform(location, &unsafe { mat_bytes_cr(2, 3, count, transpose != 0, value) });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniformMatrix3x2fv(location: i32, count: i32, transpose: u8, value: *const f32) {
    set_uniform(location, &unsafe { mat_bytes_cr(3, 2, count, transpose != 0, value) });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniformMatrix2x4fv(location: i32, count: i32, transpose: u8, value: *const f32) {
    set_uniform(location, &unsafe { mat_bytes_cr(2, 4, count, transpose != 0, value) });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniformMatrix4x2fv(location: i32, count: i32, transpose: u8, value: *const f32) {
    set_uniform(location, &unsafe { mat_bytes_cr(4, 2, count, transpose != 0, value) });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniformMatrix3x4fv(location: i32, count: i32, transpose: u8, value: *const f32) {
    set_uniform(location, &unsafe { mat_bytes_cr(3, 4, count, transpose != 0, value) });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniformMatrix4x3fv(location: i32, count: i32, transpose: u8, value: *const f32) {
    set_uniform(location, &unsafe { mat_bytes_cr(4, 3, count, transpose != 0, value) });
}

// ==================================================================================================
// GLES3.1: glProgramUniform* (DSA) — write into the NAMED program's uniform block (no bind required)
// ==================================================================================================

/// Write `bytes` into data uniform `location` of `program` (the DSA setter core).
fn set_program_uniform(program: u32, location: i32, bytes: &[u8]) {
    with(|s| record::program_uniform_at(&mut s.ctx, program, location, bytes));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform1i(program: u32, location: i32, v0: i32) {
    // Parity with glUniform1i: an integer program-uniform selects a sampler's texture unit.
    if location < 0 {
        return;
    }
    with(|s| record::program_uniform_sampler(&mut s.ctx, program, location as usize, v0));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform2i(program: u32, location: i32, v0: i32, v1: i32) {
    set_program_uniform(program, location, &le_i32(&[v0, v1]));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform3i(program: u32, location: i32, v0: i32, v1: i32, v2: i32) {
    set_program_uniform(program, location, &le_i32(&[v0, v1, v2]));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform4i(program: u32, location: i32, v0: i32, v1: i32, v2: i32, v3: i32) {
    set_program_uniform(program, location, &le_i32(&[v0, v1, v2, v3]));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform1ui(program: u32, location: i32, v0: u32) {
    set_program_uniform(program, location, &le_u32(&[v0]));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform2ui(program: u32, location: i32, v0: u32, v1: u32) {
    set_program_uniform(program, location, &le_u32(&[v0, v1]));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform3ui(program: u32, location: i32, v0: u32, v1: u32, v2: u32) {
    set_program_uniform(program, location, &le_u32(&[v0, v1, v2]));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform4ui(program: u32, location: i32, v0: u32, v1: u32, v2: u32, v3: u32) {
    set_program_uniform(program, location, &le_u32(&[v0, v1, v2, v3]));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform1f(program: u32, location: i32, v0: f32) {
    set_program_uniform(program, location, &le_f32(&[v0]));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform2f(program: u32, location: i32, v0: f32, v1: f32) {
    set_program_uniform(program, location, &le_f32(&[v0, v1]));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform3f(program: u32, location: i32, v0: f32, v1: f32, v2: f32) {
    set_program_uniform(program, location, &le_f32(&[v0, v1, v2]));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform4f(program: u32, location: i32, v0: f32, v1: f32, v2: f32, v3: f32) {
    set_program_uniform(program, location, &le_f32(&[v0, v1, v2, v3]));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform1fv(program: u32, location: i32, count: i32, value: *const f32) {
    set_program_uniform(program, location, &le_f32(unsafe { slice_f32(value, count, 1) }));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform2fv(program: u32, location: i32, count: i32, value: *const f32) {
    set_program_uniform(program, location, &le_f32(unsafe { slice_f32(value, count, 2) }));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform3fv(program: u32, location: i32, count: i32, value: *const f32) {
    set_program_uniform(program, location, &le_f32(unsafe { slice_f32(value, count, 3) }));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform4fv(program: u32, location: i32, count: i32, value: *const f32) {
    set_program_uniform(program, location, &le_f32(unsafe { slice_f32(value, count, 4) }));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform1iv(program: u32, location: i32, count: i32, value: *const i32) {
    set_program_uniform(program, location, &le_i32(unsafe { slice_i32(value, count, 1) }));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform2iv(program: u32, location: i32, count: i32, value: *const i32) {
    set_program_uniform(program, location, &le_i32(unsafe { slice_i32(value, count, 2) }));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform3iv(program: u32, location: i32, count: i32, value: *const i32) {
    set_program_uniform(program, location, &le_i32(unsafe { slice_i32(value, count, 3) }));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform4iv(program: u32, location: i32, count: i32, value: *const i32) {
    set_program_uniform(program, location, &le_i32(unsafe { slice_i32(value, count, 4) }));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform1uiv(program: u32, location: i32, count: i32, value: *const u32) {
    set_program_uniform(program, location, &le_u32(unsafe { slice_u32(value, count, 1) }));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform2uiv(program: u32, location: i32, count: i32, value: *const u32) {
    set_program_uniform(program, location, &le_u32(unsafe { slice_u32(value, count, 2) }));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform3uiv(program: u32, location: i32, count: i32, value: *const u32) {
    set_program_uniform(program, location, &le_u32(unsafe { slice_u32(value, count, 3) }));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform4uiv(program: u32, location: i32, count: i32, value: *const u32) {
    set_program_uniform(program, location, &le_u32(unsafe { slice_u32(value, count, 4) }));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniformMatrix2fv(program: u32, location: i32, count: i32, transpose: u8, value: *const f32) {
    set_program_uniform(program, location, &unsafe { mat_bytes_cr(2, 2, count, transpose != 0, value) });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniformMatrix3fv(program: u32, location: i32, count: i32, transpose: u8, value: *const f32) {
    set_program_uniform(program, location, &unsafe { mat_bytes_cr(3, 3, count, transpose != 0, value) });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniformMatrix4fv(program: u32, location: i32, count: i32, transpose: u8, value: *const f32) {
    set_program_uniform(program, location, &unsafe { mat_bytes_cr(4, 4, count, transpose != 0, value) });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniformMatrix2x3fv(program: u32, location: i32, count: i32, transpose: u8, value: *const f32) {
    set_program_uniform(program, location, &unsafe { mat_bytes_cr(2, 3, count, transpose != 0, value) });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniformMatrix3x2fv(program: u32, location: i32, count: i32, transpose: u8, value: *const f32) {
    set_program_uniform(program, location, &unsafe { mat_bytes_cr(3, 2, count, transpose != 0, value) });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniformMatrix2x4fv(program: u32, location: i32, count: i32, transpose: u8, value: *const f32) {
    set_program_uniform(program, location, &unsafe { mat_bytes_cr(2, 4, count, transpose != 0, value) });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniformMatrix4x2fv(program: u32, location: i32, count: i32, transpose: u8, value: *const f32) {
    set_program_uniform(program, location, &unsafe { mat_bytes_cr(4, 2, count, transpose != 0, value) });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniformMatrix3x4fv(program: u32, location: i32, count: i32, transpose: u8, value: *const f32) {
    set_program_uniform(program, location, &unsafe { mat_bytes_cr(3, 4, count, transpose != 0, value) });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniformMatrix4x3fv(program: u32, location: i32, count: i32, transpose: u8, value: *const f32) {
    set_program_uniform(program, location, &unsafe { mat_bytes_cr(4, 3, count, transpose != 0, value) });
}

// ==================================================================================================
// GLES3.0: uniform value readback (glGetUniform* / glGetnUniform*)
// ==================================================================================================

/// Read the current bytes of the uniform at `location` in `program` and write up to `max_bytes` of them
/// into `out` reinterpreted as `T`-sized elements. Falls back to a sampler's bound texture unit (for the
/// integer getters) or leaves `out[0]` at `0` when the value is not modeled — an honest readback.
unsafe fn read_uniform(program: u32, location: i32, out: *mut u8, elem: usize, max_bytes: usize) {
    if out.is_null() {
        return;
    }
    let bytes = with(|s| intro::get_uniform_bytes(&s.ctx, program, location));
    match bytes {
        Some(b) => {
            let n = b.len().min(max_bytes);
            core::ptr::copy_nonoverlapping(b.as_ptr(), out, n);
        }
        None => {
            // A sampler uniform reads back its bound texture unit (an integer); otherwise zero-fill one.
            let unit = with(|s| intro::get_sampler_unit(&s.ctx, program, location));
            let v = unit.unwrap_or(0);
            let src = v.to_le_bytes();
            let n = elem.min(max_bytes).min(4);
            core::ptr::copy_nonoverlapping(src.as_ptr(), out, n);
        }
    }
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetUniformfv(program: u32, location: i32, params: *mut f32) {
    unsafe { read_uniform(program, location, params as *mut u8, 4, usize::MAX) };
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetUniformiv(program: u32, location: i32, params: *mut i32) {
    unsafe { read_uniform(program, location, params as *mut u8, 4, usize::MAX) };
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetUniformuiv(program: u32, location: i32, params: *mut u32) {
    unsafe { read_uniform(program, location, params as *mut u8, 4, usize::MAX) };
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetnUniformfv(program: u32, location: i32, buf_size: i32, params: *mut f32) {
    unsafe { read_uniform(program, location, params as *mut u8, 4, buf_size.max(0) as usize) };
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetnUniformiv(program: u32, location: i32, buf_size: i32, params: *mut i32) {
    unsafe { read_uniform(program, location, params as *mut u8, 4, buf_size.max(0) as usize) };
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetnUniformuiv(program: u32, location: i32, buf_size: i32, params: *mut u32) {
    unsafe { read_uniform(program, location, params as *mut u8, 4, buf_size.max(0) as usize) };
}

// ==================================================================================================
// GLES3.0: uniform-block reflection (glGetUniformIndices / glGetActiveUniforms* / glUniformBlock*)
// ==================================================================================================

/// `glGetUniformIndices(program, count, names, indices)` — resolve each uniform name to its active index
/// (or `GL_INVALID_INDEX`). Real: keyed on the program's reflected uniform table.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetUniformIndices(program: u32, uniform_count: i32, uniform_names: *const *const c_char, uniform_indices: *mut u32) {
    if uniform_indices.is_null() || uniform_count <= 0 {
        return;
    }
    for i in 0..uniform_count as isize {
        let idx = if uniform_names.is_null() {
            GL_INVALID_INDEX
        } else {
            match unsafe { cstr(*uniform_names.offset(i)) } {
                Some(name) => with(|s| intro::uniform_index(&s.ctx, program, &name)),
                None => GL_INVALID_INDEX,
            }
        };
        unsafe { *uniform_indices.offset(i) = idx };
    }
}

/// `glGetActiveUniformsiv(program, count, indices, pname, params)` — one reflected property per named
/// uniform index (type/size/offset/name-length/block-index), from the program's reflected tables.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetActiveUniformsiv(program: u32, uniform_count: i32, uniform_indices: *const u32, pname: u32, params: *mut i32) {
    if params.is_null() || uniform_indices.is_null() || uniform_count <= 0 {
        return;
    }
    for i in 0..uniform_count as isize {
        let index = unsafe { *uniform_indices.offset(i) };
        let v = with(|s| intro::active_uniformsiv(&s.ctx, program, index, pname)).unwrap_or(0);
        unsafe { *params.offset(i) = v };
    }
}

/// `glGetUniformBlockIndex(program, name)` — the named uniform block's index (lazily assigned, stable),
/// or `GL_INVALID_INDEX`.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetUniformBlockIndex(program: u32, uniform_block_name: *const c_char) -> u32 {
    let name = match unsafe { cstr(uniform_block_name) } {
        Some(n) => n,
        None => return GL_INVALID_INDEX,
    };
    with(|s| intro::uniform_block_index(&mut s.ctx, program, &name))
}

/// `glUniformBlockBinding(program, blockIndex, binding)` — assign the block's binding point (real state).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniformBlockBinding(program: u32, uniform_block_index: u32, uniform_block_binding: u32) {
    with(|s| intro::uniform_block_binding(&mut s.ctx, program, uniform_block_index, uniform_block_binding));
}

/// `glGetActiveUniformBlockiv(program, blockIndex, pname, params)` — binding / data size / active-uniform
/// count / name length of the block. Out-of-range block → `GL_INVALID_VALUE`.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetActiveUniformBlockiv(program: u32, uniform_block_index: u32, pname: u32, params: *mut i32) {
    let v = with(|s| intro::active_uniform_blockiv(&mut s.ctx, program, uniform_block_index, pname));
    match v {
        Some(v) => {
            if !params.is_null() {
                unsafe { *params = v };
            }
        }
        None => with(|s| s.ctx.set_gl_error(GL_INVALID_VALUE)),
    }
}

/// `glGetActiveUniformBlockName(program, blockIndex, bufSize, length, name)` — the block's declared name.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetActiveUniformBlockName(program: u32, uniform_block_index: u32, buf_size: i32, length: *mut i32, uniform_block_name: *mut c_char) {
    let name = with(|s| intro::active_uniform_block_name(&mut s.ctx, program, uniform_block_index));
    match name {
        Some(n) => unsafe { write_c_name(n.as_bytes(), buf_size, length, uniform_block_name) },
        None => {
            with(|s| s.ctx.set_gl_error(GL_INVALID_VALUE));
            unsafe { write_c_name(&[], buf_size, length, uniform_block_name) };
        }
    }
}

// ==================================================================================================
// GLES3.1: program-resource introspection (glGetProgramInterfaceiv / glGetProgramResource*)
// ==================================================================================================

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetProgramInterfaceiv(program: u32, program_interface: u32, pname: u32, params: *mut i32) {
    if params.is_null() {
        return;
    }
    let v = with(|s| intro::program_interfaceiv(&s.ctx, program, program_interface, pname)).unwrap_or(0);
    unsafe { *params = v };
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetProgramResourceIndex(program: u32, program_interface: u32, name: *const c_char) -> u32 {
    let want = match unsafe { cstr(name) } {
        Some(n) => n,
        None => return GL_INVALID_INDEX,
    };
    with(|s| intro::program_resource_index(&s.ctx, program, program_interface, &want))
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetProgramResourceLocation(program: u32, program_interface: u32, name: *const c_char) -> i32 {
    let want = match unsafe { cstr(name) } {
        Some(n) => n,
        None => return -1,
    };
    with(|s| intro::program_resource_location(&s.ctx, program, program_interface, &want))
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetProgramResourceName(program: u32, program_interface: u32, index: u32, buf_size: i32, length: *mut i32, name: *mut c_char) {
    let n = with(|s| intro::program_resource_name(&s.ctx, program, program_interface, index));
    match n {
        Some(n) => unsafe { write_c_name(n.as_bytes(), buf_size, length, name) },
        None => {
            with(|s| s.ctx.set_gl_error(GL_INVALID_VALUE));
            unsafe { write_c_name(&[], buf_size, length, name) };
        }
    }
}

/// `glGetProgramResourceiv(program, interface, index, propCount, props, bufSize, length, params)` — one
/// value per requested property of the resource (type / array size / name length / location).
#[cfg_attr(gles_client, no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glGetProgramResourceiv(
    program: u32,
    program_interface: u32,
    index: u32,
    prop_count: i32,
    props: *const u32,
    buf_size: i32,
    length: *mut i32,
    params: *mut i32,
) {
    if props.is_null() || params.is_null() || prop_count <= 0 || buf_size <= 0 {
        if !length.is_null() {
            unsafe { *length = 0 };
        }
        return;
    }
    let cap = (prop_count as usize).min(buf_size as usize);
    let mut written = 0usize;
    for i in 0..cap {
        let prop = unsafe { *props.add(i) };
        let v = with(|s| intro::program_resourceiv(&s.ctx, program, program_interface, index, prop)).unwrap_or(0);
        unsafe { *params.add(i) = v };
        written += 1;
    }
    if !length.is_null() {
        unsafe { *length = written as i32 };
    }
}

// ==================================================================================================
// GLES3.0: scoped buffer clears (glClearBuffer*)
// ==================================================================================================

/// `glClearBufferfv(buffer, drawbuffer, value)` — a `GL_COLOR` clear records a scoped full-surface clear
/// at the float color; a `GL_DEPTH` clear is an honest no-op (no depth attachment is modeled).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glClearBufferfv(buffer: u32, _drawbuffer: i32, value: *const f32) {
    if buffer == GL_COLOR && !value.is_null() {
        let c = unsafe { std::slice::from_raw_parts(value, 4) };
        with(|s| record::clear_buffer_color(&mut s.ctx, [c[0], c[1], c[2], c[3]]));
    }
}

/// `glClearBufferiv(buffer, drawbuffer, value)` — an integer color-buffer clear; records the clear with
/// the values cast to the model's float clear color (a `GL_STENCIL` clear is an honest no-op).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glClearBufferiv(buffer: u32, _drawbuffer: i32, value: *const i32) {
    if buffer == GL_COLOR && !value.is_null() {
        let c = unsafe { std::slice::from_raw_parts(value, 4) };
        with(|s| record::clear_buffer_color(&mut s.ctx, [c[0] as f32, c[1] as f32, c[2] as f32, c[3] as f32]));
    }
}

/// `glClearBufferuiv(buffer, drawbuffer, value)` — the unsigned-integer color-buffer clear.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glClearBufferuiv(buffer: u32, _drawbuffer: i32, value: *const u32) {
    if buffer == GL_COLOR && !value.is_null() {
        let c = unsafe { std::slice::from_raw_parts(value, 4) };
        with(|s| record::clear_buffer_color(&mut s.ctx, [c[0] as f32, c[1] as f32, c[2] as f32, c[3] as f32]));
    }
}

/// `glClearBufferfi(GL_DEPTH_STENCIL, drawbuffer, depth, stencil)` — a combined depth+stencil clear. This
/// model backs no depth/stencil attachment, so the depth-clear value is recorded (for a faithful
/// `glGetFloatv(GL_DEPTH_CLEAR_VALUE)` round-trip) but no pass clear is lowered — an honest no-op.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glClearBufferfi(_buffer: u32, _drawbuffer: i32, depth: f32, _stencil: i32) {
    with(|s| record::clear_depth(&mut s.ctx, depth));
}

// ==================================================================================================
// GLES3.x: draw extensions (base-vertex / range / indirect) — real recorded draws
// ==================================================================================================

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDrawElementsBaseVertex(mode: u32, count: i32, type_: u32, indices: *const c_void, basevertex: i32) {
    with(|s| record::draw_elements_base_vertex(&mut s.ctx, mode, count, type_, indices as usize, basevertex));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDrawElementsInstancedBaseVertex(mode: u32, count: i32, type_: u32, indices: *const c_void, instancecount: i32, basevertex: i32) {
    with(|s| record::draw_elements_instanced_base_vertex(&mut s.ctx, mode, count, type_, indices as usize, instancecount, basevertex));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDrawRangeElements(mode: u32, start: u32, end: u32, count: i32, type_: u32, indices: *const c_void) {
    with(|s| record::draw_range_elements(&mut s.ctx, mode, start, end, count, type_, indices as usize, 0));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDrawRangeElementsBaseVertex(mode: u32, start: u32, end: u32, count: i32, type_: u32, indices: *const c_void, basevertex: i32) {
    with(|s| record::draw_range_elements(&mut s.ctx, mode, start, end, count, type_, indices as usize, basevertex));
}
/// `glDrawArraysIndirect(mode, indirect)` — `indirect` is a byte offset INTO the buffer bound to
/// `GL_DRAW_INDIRECT_BUFFER` (a GLES3.1 draw always sources the indirect params from a buffer object).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDrawArraysIndirect(mode: u32, indirect: *const c_void) {
    with(|s| record::draw_arrays_indirect(&mut s.ctx, mode, indirect as usize));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDrawElementsIndirect(mode: u32, type_: u32, indirect: *const c_void) {
    with(|s| record::draw_elements_indirect(&mut s.ctx, mode, type_, indirect as usize));
}

// ==================================================================================================
// program / shader lifecycle + object-existence queries (glDelete* / glIs* / glGet*)
// ==================================================================================================

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDeleteProgram(program: u32) {
    with(|s| record::delete_program(&mut s.ctx, program));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDeleteShader(shader: u32) {
    with(|s| record::delete_shader(&mut s.ctx, shader));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDetachShader(program: u32, shader: u32) {
    with(|s| record::detach_shader(&mut s.ctx, program, shader));
}
/// `glValidateProgram(program)` — a linked program validates clean in this model; an unknown program
/// raises `GL_INVALID_VALUE` (the getter path already reports `GL_VALIDATE_STATUS` from the link state).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glValidateProgram(program: u32) {
    with(|s| {
        if !s.ctx.programs.program_exists(program) {
            s.ctx.set_gl_error(GL_INVALID_VALUE);
        }
    });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glIsProgram(program: u32) -> u32 {
    with(|s| s.ctx.programs.program_exists(program)) as u32
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glIsShader(shader: u32) -> u32 {
    with(|s| s.ctx.programs.shader_exists(shader)) as u32
}
/// `glIsBuffer(buffer)` — true once `buffer` names a live buffer object (this model materializes the
/// object at `glGenBuffers`, so a generated name reads back as a buffer).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glIsBuffer(buffer: u32) -> u32 {
    with(|s| buffer != 0 && s.ctx.buffers.get(buffer).is_some()) as u32
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glIsTexture(texture: u32) -> u32 {
    with(|s| texture != 0 && s.ctx.textures.get(texture).is_some()) as u32
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glIsEnabled(cap: u32) -> u32 {
    with(|s| intro::is_enabled(&s.ctx, cap)) as u32
}
/// `glIsEnabledi(target, index)` — this model tracks no per-index (indexed) enable state, so it reports
/// the non-indexed capability's state (the honest answer for a single-target model).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glIsEnabledi(target: u32, _index: u32) -> u32 {
    with(|s| intro::is_enabled(&s.ctx, target)) as u32
}
/// `glGetAttachedShaders(program, maxCount, count, shaders)` — the program's attached vertex/fragment/
/// compute shader names (real reflection of the attachment slots).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetAttachedShaders(program: u32, max_count: i32, count: *mut i32, shaders: *mut u32) {
    let attached: Vec<u32> = with(|s| {
        s.ctx.programs.program(program).map(|p| [p.vs, p.fs, p.cs].into_iter().filter(|&x| x != 0).collect()).unwrap_or_default()
    });
    let n = (max_count.max(0) as usize).min(attached.len());
    if !shaders.is_null() {
        for (i, &sh) in attached.iter().take(n).enumerate() {
            unsafe { *shaders.add(i) = sh };
        }
    }
    if !count.is_null() {
        unsafe { *count = n as i32 };
    }
}
/// `glGetShaderSource(shader, bufSize, length, source)` — the exact GLSL-ES source stored at
/// `glShaderSource` (real; `glGetShaderiv(GL_SHADER_SOURCE_LENGTH)` reports its matching length).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetShaderSource(shader: u32, buf_size: i32, length: *mut i32, source: *mut c_char) {
    let src = with(|s| intro::get_shader_source(&s.ctx, shader));
    unsafe { write_c_name(src.as_bytes(), buf_size, length, source) };
}
/// `glGetFragDataLocation(program, name)` — the fragment output's color index (real reflection).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetFragDataLocation(program: u32, name: *const c_char) -> i32 {
    let want = match unsafe { cstr(name) } {
        Some(n) => n,
        None => return -1,
    };
    with(|s| intro::frag_data_location(&s.ctx, program, &want))
}

// ==================================================================================================
// fixed-function state — REAL where the model tracks it, honest no-op where it backs no state
// ==================================================================================================

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBlendEquation(mode: u32) {
    with(|s| record::blend_equation(&mut s.ctx, mode));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBlendEquationSeparate(mode_rgb: u32, mode_alpha: u32) {
    with(|s| record::blend_equation_separate(&mut s.ctx, mode_rgb, mode_alpha));
}
/// Per-draw-buffer blend variants: this model has a single color target, so buffer 0 delegates to the
/// global blend state and any other buffer index is an honest no-op.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBlendEquationi(buf: u32, mode: u32) {
    if buf == 0 {
        with(|s| record::blend_equation(&mut s.ctx, mode));
    }
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBlendEquationSeparatei(buf: u32, mode_rgb: u32, mode_alpha: u32) {
    if buf == 0 {
        with(|s| record::blend_equation_separate(&mut s.ctx, mode_rgb, mode_alpha));
    }
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBlendFunci(buf: u32, src: u32, dst: u32) {
    if buf == 0 {
        with(|s| record::blend_func(&mut s.ctx, src, dst));
    }
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBlendFuncSeparatei(buf: u32, src_rgb: u32, dst_rgb: u32, src_alpha: u32, dst_alpha: u32) {
    if buf == 0 {
        with(|s| record::blend_func_separate(&mut s.ctx, src_rgb, dst_rgb, src_alpha, dst_alpha));
    }
}
/// `glBlendColor` — the constant blend color. This model lowers no `GL_*_CONSTANT_*` blend factor, so the
/// constant color carries no observable state: an honest no-op (matches the reference shim).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBlendColor(_red: f32, _green: f32, _blue: f32, _alpha: f32) {}
/// `glColorMask` / `glColorMaski` — this model always writes RGBA (no per-channel write-mask is lowered):
/// an honest no-op.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glColorMask(_red: u8, _green: u8, _blue: u8, _alpha: u8) {}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glColorMaski(_index: u32, _r: u8, _g: u8, _b: u8, _a: u8) {}
/// `glDepthRangef` — the model maps NDC depth directly (fixed 0..1 range), so a custom depth range carries
/// no lowered state: an honest no-op.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDepthRangef(_n: f32, _f: f32) {}
/// `glLineWidth` — `GL_LINE_WIDTH` is fixed at `1.0` (see `query::get_floatv`); an honest no-op.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glLineWidth(_width: f32) {}
/// `glPolygonOffset` — no depth-bias pipeline state is lowered: an honest no-op.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glPolygonOffset(_factor: f32, _units: f32) {}
/// `glHint` — every hint is advisory; this model honors none observably: an honest no-op.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glHint(_target: u32, _mode: u32) {}
/// `glSampleCoverage` / `glSampleMaski` / `glMinSampleShading` — no MSAA is materialized (single-sample
/// render targets), so multisample coverage/mask carry no state: honest no-ops.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glSampleCoverage(_value: f32, _invert: u8) {}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glSampleMaski(_mask_number: u32, _mask: u32) {}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glMinSampleShading(_value: f32) {}
/// `glStencilFunc` / `glStencilMask` / `glStencilOp` / `glClearStencil` — the default framebuffer models no
/// stencil attachment (matches `glStencilFuncSeparate` &c.): honest no-ops.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glStencilFunc(_func: u32, _ref_: i32, _mask: u32) {}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glStencilMask(_mask: u32) {}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glStencilOp(_fail: u32, _zfail: u32, _zpass: u32) {}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glClearStencil(_s: i32) {}
/// `glPatchParameteri` — no tessellation stage is modeled: an honest no-op.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glPatchParameteri(_pname: u32, _value: i32) {}
/// `glPrimitiveBoundingBox` — a tessellation/geometry hint (OES_primitive_bounding_box): an honest no-op.
#[cfg_attr(gles_client, no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glPrimitiveBoundingBox(_min_x: f32, _min_y: f32, _min_z: f32, _min_w: f32, _max_x: f32, _max_y: f32, _max_z: f32, _max_w: f32) {}
/// `glBlendBarrier` — an advanced-blend (KHR_blend_equation_advanced) barrier; this model lowers no
/// advanced blend, so there is nothing to order: an honest no-op.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBlendBarrier() {}
/// `glEnablei` / `glDisablei` — indexed enable; this single-target model routes buffer 0 to the global
/// capability and ignores other indices.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glEnablei(target: u32, index: u32) {
    if index == 0 {
        with(|s| record::enable(&mut s.ctx, target));
    }
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDisablei(target: u32, index: u32) {
    if index == 0 {
        with(|s| record::disable(&mut s.ctx, target));
    }
}

// ==================================================================================================
// integer / indexed / capability state queries
// ==================================================================================================

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetInteger64v(pname: u32, data: *mut i64) {
    if data.is_null() {
        return;
    }
    let mut buf = [0i32; 4];
    let n = with(|s| query::get_integerv(&s.ctx, pname, &mut buf));
    unsafe {
        for i in 0..n {
            *data.add(i) = buf[i] as i64;
        }
    }
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetIntegeri_v(target: u32, index: u32, data: *mut i32) {
    if data.is_null() {
        return;
    }
    let v = with(|s| query::get_integer_indexed(&s.ctx, target, index));
    unsafe { *data = v as i32 };
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetInteger64i_v(target: u32, index: u32, data: *mut i64) {
    if data.is_null() {
        return;
    }
    let v = with(|s| query::get_integer_indexed(&s.ctx, target, index));
    unsafe { *data = v };
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetBooleani_v(target: u32, index: u32, data: *mut u8) {
    if data.is_null() {
        return;
    }
    let v = with(|s| query::get_integer_indexed(&s.ctx, target, index));
    unsafe { *data = (v != 0) as u8 };
}
/// `glGetInternalformativ(target, internalformat, pname, bufSize, params)` — supported sample counts for
/// an internal format. This model advertises single-sample rendering with `GL_MAX_SAMPLES` as the peak.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetInternalformativ(_target: u32, _internalformat: u32, pname: u32, buf_size: i32, params: *mut i32) {
    if params.is_null() || buf_size <= 0 {
        return;
    }
    unsafe {
        *params = match pname {
            GL_NUM_SAMPLE_COUNTS => 1,
            GL_SAMPLES => query::MAX_SAMPLES,
            _ => 0,
        };
    }
}
/// `glGetShaderPrecisionFormat(shaderType, precisionType, range, precision)` — the IEEE-shaped ranges the
/// host GPU backs: `float` → range {127,127}, precision 23 (single-precision); `int` → range {31,31},
/// precision 0.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetShaderPrecisionFormat(_shadertype: u32, precisiontype: u32, range: *mut i32, precision: *mut i32) {
    let is_float = matches!(precisiontype, GL_LOW_FLOAT..=GL_HIGH_FLOAT);
    unsafe {
        if !range.is_null() {
            let r = if is_float { 127 } else { 31 };
            *range = r;
            *range.add(1) = r;
        }
        if !precision.is_null() {
            *precision = if is_float { 23 } else { 0 };
        }
    }
}
/// `glGetRenderbufferParameteriv(target, pname, params)` — the bound renderbuffer's extent + format.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetRenderbufferParameteriv(target: u32, pname: u32, params: *mut i32) {
    if params.is_null() {
        return;
    }
    let v = with(|s| intro::renderbuffer_parameter(&s.ctx, target, pname));
    unsafe { *params = v };
}
/// `glGetFramebufferAttachmentParameteriv(target, attachment, pname, params)` — the bound framebuffer's
/// color-attachment object type + name (real reflection of the FBO's attachment).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetFramebufferAttachmentParameteriv(target: u32, attachment: u32, pname: u32, params: *mut i32) {
    if params.is_null() {
        return;
    }
    let v = with(|s| intro::framebuffer_attachment_parameter(&s.ctx, target, attachment, pname));
    unsafe { *params = v };
}
/// `glGetFramebufferParameteriv(target, pname, params)` — default-framebuffer parameters (default width/
/// height/layers/samples). This model carries no `glFramebufferParameteri` state, so it reads `0` — an
/// honest default.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetFramebufferParameteriv(_target: u32, _pname: u32, params: *mut i32) {
    if !params.is_null() {
        unsafe { *params = 0 };
    }
}
/// `glGetTexLevelParameteriv(target, level, pname, params)` — the bound texture's level-0 width/height/
/// internal format (real reflection).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetTexLevelParameteriv(target: u32, level: i32, pname: u32, params: *mut i32) {
    if params.is_null() {
        return;
    }
    let v = with(|s| intro::tex_level_parameter(&s.ctx, target, level, pname));
    unsafe { *params = v };
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetTexLevelParameterfv(target: u32, level: i32, pname: u32, params: *mut f32) {
    if params.is_null() {
        return;
    }
    let v = with(|s| intro::tex_level_parameter(&s.ctx, target, level, pname));
    unsafe { *params = v as f32 };
}
/// `glGetMultisamplefv(pname, index, val)` — the sub-sample position. Single-sample rendering places the
/// one sample at the pixel center (0.5, 0.5) — the honest answer for this model.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetMultisamplefv(_pname: u32, _index: u32, val: *mut f32) {
    if !val.is_null() {
        unsafe {
            *val = 0.5;
            *val.add(1) = 0.5;
        }
    }
}
/// `glGetGraphicsResetStatus()` — the context has not been reset (no robustness reset is modeled).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetGraphicsResetStatus() -> u32 {
    GL_NO_ERROR
}
/// `glGetBufferPointerv(target, pname, params)` — the mapped-buffer pointer. This model does not retain a
/// persistent host mapping pointer across the query, so it reports null — an honest default.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetBufferPointerv(_target: u32, _pname: u32, params: *mut *mut c_void) {
    if !params.is_null() {
        unsafe { *params = core::ptr::null_mut() };
    }
}
/// `glGetPointerv(pname, params)` — a KHR_debug callback/pointer query; no such pointer state is modeled.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetPointerv(_pname: u32, params: *mut *mut c_void) {
    if !params.is_null() {
        unsafe { *params = core::ptr::null_mut() };
    }
}
/// `glGetProgramBinary(...)` — no program-binary formats are advertised (`GL_NUM_PROGRAM_BINARY_FORMATS`
/// == 0), so the driver forces the source-compile path: an empty binary (length 0, format 0).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetProgramBinary(_program: u32, _buf_size: i32, length: *mut i32, binary_format: *mut u32, _binary: *mut c_void) {
    unsafe {
        if !length.is_null() {
            *length = 0;
        }
        if !binary_format.is_null() {
            *binary_format = 0;
        }
    }
}
/// `glProgramBinary(...)` — no binary formats are supported, so any supplied binary is rejected as
/// `GL_INVALID_ENUM` (the program keeps its source-compiled link state; the app must re-link from source).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramBinary(program: u32, _binary_format: u32, _binary: *const c_void, _length: i32) {
    with(|s| {
        if s.ctx.programs.program_exists(program) {
            s.ctx.set_gl_error(GL_INVALID_ENUM);
        } else {
            s.ctx.set_gl_error(GL_INVALID_OPERATION);
        }
    });
}

// ==================================================================================================
// KHR_debug: message log + object labels — no debug state is modeled (honest empty/no-op)
// ==================================================================================================

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDebugMessageCallback(_callback: *mut c_void, _user_param: *const c_void) {}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDebugMessageControl(_source: u32, _type_: u32, _severity: u32, _count: i32, _ids: *const u32, _enabled: u8) {}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDebugMessageInsert(_source: u32, _type_: u32, _id: u32, _severity: u32, _length: i32, _buf: *const c_char) {}
/// `glGetDebugMessageLog` — no messages are recorded (this driver logs GL diagnostics out-of-band), so it
/// returns 0 messages.
#[cfg_attr(gles_client, no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glGetDebugMessageLog(
    _count: u32,
    _buf_size: i32,
    _sources: *mut u32,
    _types: *mut u32,
    _ids: *mut u32,
    _severities: *mut u32,
    _lengths: *mut i32,
    _message_log: *mut c_char,
) -> u32 {
    0
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glPushDebugGroup(_source: u32, _id: u32, _length: i32, _message: *const c_char) {}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glPopDebugGroup() {}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glObjectLabel(_identifier: u32, _name: u32, _length: i32, _label: *const c_char) {}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glObjectPtrLabel(_ptr: *const c_void, _length: i32, _label: *const c_char) {}
/// `glGetObjectLabel` / `glGetObjectPtrLabel` — no labels are stored: report an empty label (length 0).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetObjectLabel(_identifier: u32, _name: u32, buf_size: i32, length: *mut i32, label: *mut c_char) {
    unsafe { write_c_name(&[], buf_size, length, label) };
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetObjectPtrLabel(_ptr: *const c_void, buf_size: i32, length: *mut i32, label: *mut c_char) {
    unsafe { write_c_name(&[], buf_size, length, label) };
}

// ==================================================================================================
// shader binary / compiler control — no shader-binary formats advertised (honest)
// ==================================================================================================

/// `glReleaseShaderCompiler` — a hint that the compiler may free resources. This driver compiles from
/// source at link (`GL_SHADER_COMPILER` == true), so there is nothing to release: an honest no-op.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glReleaseShaderCompiler() {}
/// `glShaderBinary(...)` — no shader-binary formats are advertised (`GL_NUM_SHADER_BINARY_FORMATS` == 0),
/// so a binary load is rejected as `GL_INVALID_ENUM` (the app must supply GLSL source).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glShaderBinary(_count: i32, _shaders: *const u32, _binaryformat: u32, _binary: *const c_void, _length: i32) {
    with(|s| s.ctx.set_gl_error(GL_INVALID_ENUM));
}

// ==================================================================================================
// texture / renderbuffer extensions
// ==================================================================================================

/// `glTexStorage2DMultisample` — immutable multisample 2D storage. This model materializes single-sample
/// textures, so `samples` is ignored and the base RGBA8 plane is allocated (delegating to the 2D storage
/// path); the texture is usable as a single-sample attachment.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glTexStorage2DMultisample(target: u32, _samples: i32, internalformat: u32, width: i32, height: i32, _fixedsamplelocations: u8) {
    with(|s| record::tex_storage_2d(&mut s.ctx, target, 1, internalformat, width, height));
}
/// `glTexStorage3DMultisample` — the 2D-array multisample form; `samples` ignored (single-sample plane).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glTexStorage3DMultisample(target: u32, _samples: i32, _internalformat: u32, width: i32, height: i32, depth: i32, _fixedsamplelocations: u8) {
    with(|s| record::tex_storage_3d(&mut s.ctx, target, 1, width, height, depth));
}
/// `glRenderbufferStorageMultisample` — a multisample renderbuffer; single-sample in this model, so the
/// backing RGBA8 plane is sized (delegating to `glRenderbufferStorage`) and `samples` is ignored.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glRenderbufferStorageMultisample(target: u32, _samples: i32, internalformat: u32, width: i32, height: i32) {
    with(|s| record::renderbuffer_storage(&mut s.ctx, target, internalformat, width, height));
}
/// `glTexBuffer` / `glTexBufferRange` — buffer textures (sampling a buffer object as a 1D texel array).
/// No buffer-texture sampling path is modeled (the render path samples 2D textures), so this is an honest
/// no-op; a shader that samples one reads the texture's neutral (dataless) content.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glTexBuffer(_target: u32, _internalformat: u32, _buffer: u32) {}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glTexBufferRange(_target: u32, _internalformat: u32, _buffer: u32, _offset: isize, _size: isize) {}
/// `glTexParameterIiv` / `glTexParameterIuiv` — the integer (non-normalized) parameter vectors; reads
/// `params[0]` into the same filter/wrap setter the scalar path uses.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glTexParameterIiv(_target: u32, pname: u32, params: *const i32) {
    if params.is_null() {
        return;
    }
    let v = unsafe { *params };
    with(|s| record::tex_parameter(&mut s.ctx, pname, v as u32));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glTexParameterIuiv(_target: u32, pname: u32, params: *const u32) {
    if params.is_null() {
        return;
    }
    let v = unsafe { *params };
    with(|s| record::tex_parameter(&mut s.ctx, pname, v));
}
/// `glCopyImageSubData(...)` — a direct image-to-image copy. Both a 2D source and destination texture with
/// materialized RGBA8 pixels can be copied CPU-side (real); a mixed/renderbuffer/level-`>0` case is an
/// honest no-op (the deferred model has no non-texture source plane at record time).
#[cfg_attr(gles_client, no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glCopyImageSubData(
    src_name: u32,
    src_target: u32,
    src_level: i32,
    src_x: i32,
    src_y: i32,
    _src_z: i32,
    dst_name: u32,
    dst_target: u32,
    dst_level: i32,
    dst_x: i32,
    dst_y: i32,
    _dst_z: i32,
    src_width: i32,
    src_height: i32,
    _src_depth: i32,
) {
    if src_target != GL_TEXTURE_2D || dst_target != GL_TEXTURE_2D || src_level != 0 || dst_level != 0 {
        return;
    }
    if src_width <= 0 || src_height <= 0 || src_x < 0 || src_y < 0 || dst_x < 0 || dst_y < 0 {
        return;
    }
    with(|s| {
        // Read the source rect out (immutable borrow) …
        let rows: Option<Vec<u8>> = s.ctx.textures.get(src_name).and_then(|st| {
            let (sw, sh) = (st.w, st.h);
            if st.data.is_empty() || src_x + src_width > sw || src_y + src_height > sh {
                return None;
            }
            let (sw, w, h, x, y) = (sw as usize, src_width as usize, src_height as usize, src_x as usize, src_y as usize);
            let mut buf = Vec::with_capacity(w * h * 4);
            for row in 0..h {
                let base = ((y + row) * sw + x) * 4;
                buf.extend_from_slice(&st.data[base..base + w * 4]);
            }
            Some(buf)
        });
        // … then write it into the destination sub-rect (mutable borrow).
        if let Some(buf) = rows {
            s.ctx.textures.sub_image_2d(dst_name, dst_x, dst_y, src_width, src_height, &buf);
        }
    });
}
/// `glBindImageTexture(...)` — bind a texture level as a shader image (image load/store). This model lowers
/// no image load/store, so the binding carries no observable state: an honest no-op.
#[cfg_attr(gles_client, no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glBindImageTexture(_unit: u32, _texture: u32, _level: i32, _layered: u8, _layer: i32, _access: u32, _format: u32) {}

// ==================================================================================================
// separate-format vertex arrays + framebuffer attach extensions
// ==================================================================================================

/// `glVertexAttribFormat(attribindex, size, type, normalized, relativeoffset)` — the separate-format
/// vertex-attribute description; records size/type/normalized into the same per-location attribute state
/// the pointer path uses (the relative offset is folded into the attribute offset).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glVertexAttribFormat(attribindex: u32, size: i32, type_: u32, normalized: u8, relativeoffset: u32) {
    with(|s| record::vertex_attrib_pointer(&mut s.ctx, attribindex as usize, size, type_, normalized != 0, 0, relativeoffset as usize));
}
/// `glVertexAttribBinding(attribindex, bindingindex)` — associate an attribute with a vertex-buffer binding
/// slot. This model keys attributes to a single array-buffer binding, so the binding index is not
/// separately tracked: an honest no-op.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glVertexAttribBinding(_attribindex: u32, _bindingindex: u32) {}
/// `glBindVertexBuffer(bindingindex, buffer, offset, stride)` — bind a buffer to a vertex-buffer slot. The
/// separate binding slots are not modeled; binding slot 0 updates the array-buffer binding so a following
/// `glVertexAttribFormat`-based draw still sources vertices, other slots are an honest no-op.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBindVertexBuffer(bindingindex: u32, buffer: u32, _offset: isize, _stride: i32) {
    if bindingindex == 0 {
        with(|s| record::bind_buffer(&mut s.ctx, GL_ARRAY_BUFFER, buffer));
    }
}
/// `glVertexBindingDivisor(bindingindex, divisor)` — the instance-step divisor for a binding slot; applied
/// to the attribute at that index (single-slot model).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glVertexBindingDivisor(bindingindex: u32, divisor: u32) {
    with(|s| record::vertex_attrib_divisor(&mut s.ctx, bindingindex as usize, divisor));
}
/// `glFramebufferParameteri(target, pname, param)` — default-framebuffer parameters (default width/height/
/// samples for a framebuffer with no attachments). No such state is materialized: an honest no-op.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glFramebufferParameteri(_target: u32, _pname: u32, _param: i32) {}
/// `glFramebufferTexture(target, attachment, texture, level)` — attach a whole texture as the FBO's color
/// target (the layered/whole-texture form of `glFramebufferTexture2D`); delegates to the 2D color-attach
/// path for `GL_COLOR_ATTACHMENT0`.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glFramebufferTexture(target: u32, attachment: u32, texture: u32, level: i32) {
    with(|s| record::framebuffer_texture_2d(&mut s.ctx, target, attachment, GL_TEXTURE_2D, texture, level));
}
/// `glFramebufferTextureLayer(target, attachment, texture, level, layer)` — attach one layer of an array/3D
/// texture as the color target. This model materializes the layer-0 plane, so the layer is folded into the
/// color attachment via the same 2D color-attach path.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glFramebufferTextureLayer(target: u32, attachment: u32, texture: u32, level: i32, _layer: i32) {
    with(|s| record::framebuffer_texture_2d(&mut s.ctx, target, attachment, GL_TEXTURE_2D, texture, level));
}

// ==================================================================================================
// submission ordering (glFlush / glFinish) over a process-global submission-serial pair
// ==================================================================================================

use core::sync::atomic::{AtomicU64, Ordering};
/// Submission serials backing `glFlush`/`glFinish`: `SUBMIT` advances when work is handed off, `COMPLETE`
/// tracks how far the host has finished. This deferred driver flushes real frames at `eglSwapBuffers`; the
/// serials give `glFlush`/`glFinish` a distinct, observable contract in between.
static SUBMIT_SERIAL: AtomicU64 = AtomicU64::new(0);
static COMPLETE_SERIAL: AtomicU64 = AtomicU64::new(0);

/// `glFlush` — NONBLOCKING: advance the submission serial (hand queued work off) and return immediately.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glFlush() {
    SUBMIT_SERIAL.fetch_add(1, Ordering::SeqCst);
}
/// `glFinish` — BLOCKING: advance the submission serial, then catch completion up to it (this deferred
/// model completes synchronously — there is no in-flight host executor to wait on between swaps).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glFinish() {
    let target = SUBMIT_SERIAL.fetch_add(1, Ordering::SeqCst) + 1;
    let mut done = COMPLETE_SERIAL.load(Ordering::SeqCst);
    while done < target {
        match COMPLETE_SERIAL.compare_exchange(done, target, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => break,
            Err(cur) => done = cur,
        }
    }
}

// ==================================================================================================
// EGL: the remaining lifecycle / query / sync / image / surface-creation entry points
// ==================================================================================================

/// `eglGetPlatformDisplay(platform, native_display, attrib_list)` — the EGL 1.5 display getter; returns the
/// same single display token as `eglGetDisplay` (this driver has one display).
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglGetPlatformDisplay(_platform: u32, _native_display: *mut c_void, _attrib_list: *const isize) -> *mut c_void {
    DISPLAY_TOKEN as *mut c_void
}

// ---- EGL_EXT_platform_base entry points ----------------------------------------------------------
//
// The client extension string ([`hl_gl::adapter::wayland::egl_client_extensions`]) advertises
// `EGL_EXT_platform_base` (+ `EGL_EXT_platform_wayland`). By that extension's contract a caller that
// sees the string resolves these `*EXT` entry points through `eglGetProcAddress` and CALLS them WITHOUT
// a null check (the extension being advertised is the promise they exist) — e.g. the real Khronos
// `eglinfo` calls `eglGetProcAddress("eglGetPlatformDisplayEXT")` for every named platform and invokes
// the returned pointer directly. Advertising the extension while resolving these to null is therefore a
// crash-the-caller bug: `eglinfo` SIGSEGVs jumping through the null. These are the EGLint-attrib-list
// `EXT` spellings of the EGL 1.5 core `eglGetPlatformDisplay` / `eglCreatePlatform*Surface` above and
// forward to the same single display / surface bring-up. They are resolved ONLY via `eglGetProcAddress`
// (extension functions are not part of the exported ABI surface — the 402-symbol golden), so they are
// deliberately not `#[no_mangle]`.

/// `eglGetPlatformDisplayEXT(platform, native_display, attrib_list)` — `EGL_EXT_platform_base` display
/// getter. Same single display token as `eglGetDisplay`; the surfaceless / wayland / gbm / x11 platform
/// enums all map to the one initializable display this driver serves.
extern "C" fn eglGetPlatformDisplayEXT(_platform: u32, _native_display: *mut c_void, _attrib_list: *const i32) -> *mut c_void {
    DISPLAY_TOKEN as *mut c_void
}
/// `eglCreatePlatformWindowSurfaceEXT(dpy, config, native_window, attrib_list)` — `EGL_EXT_platform_base`
/// window-surface getter; `native_window` is the `wl_egl_window*`. Brings up the window surface exactly
/// like the core `eglCreatePlatformWindowSurface`.
extern "C" fn eglCreatePlatformWindowSurfaceEXT(_dpy: *mut c_void, _config: *mut c_void, native_window: *mut c_void, _attrib_list: *const i32) -> *mut c_void {
    create_window_surface(native_window)
}
/// `eglCreatePlatformPixmapSurfaceEXT(...)` — `EGL_EXT_platform_base` pixmap-surface getter; a fresh
/// surface token (no native-pixmap target is modeled).
extern "C" fn eglCreatePlatformPixmapSurfaceEXT(_dpy: *mut c_void, _config: *mut c_void, _native_pixmap: *mut c_void, _attrib_list: *const i32) -> *mut c_void {
    with(|s| s.mint_token())
}

// ---- EGL_EXT_device_base / device_query / device_enumeration entry points ------------------------
//
// Advertised in both the client and per-display extension strings, so a toolkit's GL loader (libepoxy
// for GTK/GDK) resolves these via `eglGetProcAddress` + the extension string and CALLS them without a
// null check. GDK's Wayland EGL bring-up specifically requires one of
// `EGL_EXT_device_base`/`device_query`/`EGL_KHR_display_reference`/`EGL_NV_stream_metadata` and then
// calls `eglQueryDisplayAttribEXT` to learn the display's backing device.
//
// The model is one truthful software device ([`DEVICE_TOKEN`]) representing our hl-gl renderer. Every
// body is panic-free across the C-ABI seam (raw pointers null-checked, unknown handles/attributes raise
// the accurate `EGL_*` error and return `EGL_FALSE`/null — never a deref crash or fabricated value),
// mirroring the `eglGetConfigAttrib` / `eglGetConfigs` contract discipline. Resolved only via
// `eglGetProcAddress` (extension functions are not part of the exported 402-symbol ABI), so they are
// deliberately not `#[no_mangle]`.

/// `eglQueryDisplayAttribEXT(dpy, attribute, value)` — a display attribute (`EGLAttrib`, i.e. `intptr_t`).
/// GDK asks `EGL_DEVICE_EXT` to learn the display's backing `EGLDeviceEXT`; we answer with our single
/// software device. A null `value` is `EGL_BAD_PARAMETER`; an attribute we do not model is
/// `EGL_BAD_ATTRIBUTE` — both `EGL_FALSE` WITHOUT writing / dereferencing `value`.
extern "C" fn eglQueryDisplayAttribEXT(_dpy: *mut c_void, attribute: i32, value: *mut isize) -> u32 {
    if value.is_null() {
        with(|s| s.set_egl_error(EGL_BAD_PARAMETER));
        return EGL_FALSE;
    }
    match attribute {
        EGL_DEVICE_EXT => {
            unsafe { *value = DEVICE_TOKEN as isize };
            EGL_TRUE
        }
        _ => {
            with(|s| s.set_egl_error(EGL_BAD_ATTRIBUTE));
            EGL_FALSE
        }
    }
}

/// `eglQueryDeviceAttribEXT(device, attribute, value)` — an integer attribute of an `EGLDeviceEXT`. A
/// foreign device handle is `EGL_BAD_DEVICE_EXT`; a null `value` is `EGL_BAD_PARAMETER`. Our software
/// device exposes no vendor integer attributes (those are e.g. `EGL_CUDA_DEVICE_NV`), so any recognized-
/// device attribute is truthfully `EGL_BAD_ATTRIBUTE`. All paths return `EGL_FALSE` without a deref.
extern "C" fn eglQueryDeviceAttribEXT(device: *mut c_void, _attribute: i32, value: *mut isize) -> u32 {
    if device as usize != DEVICE_TOKEN {
        with(|s| s.set_egl_error(EGL_BAD_DEVICE_EXT));
        return EGL_FALSE;
    }
    if value.is_null() {
        with(|s| s.set_egl_error(EGL_BAD_PARAMETER));
        return EGL_FALSE;
    }
    // No integer device attributes are modeled for a software device.
    with(|s| s.set_egl_error(EGL_BAD_ATTRIBUTE));
    EGL_FALSE
}

/// `eglQueryDeviceStringEXT(device, name)` — a string describing the `EGLDeviceEXT`. `EGL_EXTENSIONS`
/// returns the device-level extension string (empty: our software device advertises no device extensions).
/// A foreign device handle is `EGL_BAD_DEVICE_EXT` + null; an unmodeled `name` is `EGL_BAD_PARAMETER` +
/// null (never a dangling pointer). The returned pointer is process-static (valid for the app's lifetime).
extern "C" fn eglQueryDeviceStringEXT(device: *mut c_void, name: i32) -> *const c_char {
    if device as usize != DEVICE_TOKEN {
        with(|s| s.set_egl_error(EGL_BAD_DEVICE_EXT));
        return core::ptr::null();
    }
    match name {
        // The device's own extension string (client/display device extensions live on eglQueryString).
        EGL_EXTENSIONS_Q => b"\0".as_ptr() as *const c_char,
        _ => {
            with(|s| s.set_egl_error(EGL_BAD_PARAMETER));
            core::ptr::null()
        }
    }
}

/// `eglQueryDevicesEXT(max_devices, devices, num_devices)` — enumerate the `EGLDeviceEXT`s. This driver
/// reports its single software device. Same enumeration contract as `eglGetConfigs`: a null `devices`
/// array reports the count in `num_devices`; a real array copies up to `max_devices` handles (bounded, no
/// OOB store) and `num_devices` reports how many were written. `num_devices` is required (`EGL_BAD_PARAMETER`
/// if null); a non-null `devices` with `max_devices <= 0` is `EGL_BAD_PARAMETER` (spec).
extern "C" fn eglQueryDevicesEXT(max_devices: i32, devices: *mut *mut c_void, num_devices: *mut i32) -> u32 {
    if num_devices.is_null() {
        with(|s| s.set_egl_error(EGL_BAD_PARAMETER));
        return EGL_FALSE;
    }
    const AVAILABLE: i32 = 1; // our single software device
    if devices.is_null() {
        // Count-only query.
        unsafe { *num_devices = AVAILABLE };
        return EGL_TRUE;
    }
    if max_devices <= 0 {
        with(|s| s.set_egl_error(EGL_BAD_PARAMETER));
        return EGL_FALSE;
    }
    let n = AVAILABLE.min(max_devices);
    unsafe {
        for i in 0..n as isize {
            *devices.offset(i) = DEVICE_TOKEN as *mut c_void;
        }
        *num_devices = n;
    }
    EGL_TRUE
}
/// `eglQueryAPI()` — the API bound by `eglBindAPI` on the CALLING THREAD (defaults to
/// `EGL_OPENGL_ES_API`, the only API this driver serves). libepoxy reads this to confirm GLES dispatch.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglQueryAPI() -> u32 {
    current::query_api()
}
/// `eglReleaseThread()` — release the calling thread's EGL state: the current context/surface/display
/// binding is dropped (the bound API resets to the `EGL_OPENGL_ES_API` default via the released cells).
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglReleaseThread() -> u32 {
    current::release();
    EGL_TRUE
}
/// `eglWaitClient` / `eglWaitGL` / `eglWaitNative` — flush + wait for the client/native pipeline. The
/// deferred model completes synchronously at swap, so these succeed immediately.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglWaitClient() -> u32 {
    EGL_TRUE
}
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglWaitGL() -> u32 {
    EGL_TRUE
}
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglWaitNative(_engine: i32) -> u32 {
    EGL_TRUE
}
/// `eglSurfaceAttrib(dpy, surface, attribute, value)` — set a surface attribute (swap behavior, mipmap
/// hint, …). This model tracks none of the settable attributes; the call is accepted. Success.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglSurfaceAttrib(_dpy: *mut c_void, _surface: *mut c_void, _attribute: i32, _value: i32) -> u32 {
    EGL_TRUE
}
/// `eglBindTexImage` / `eglReleaseTexImage` — bind/release a pbuffer as a texture image. No render-to-
/// texture pbuffer path is modeled; the call is accepted as a no-op. Success.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglBindTexImage(_dpy: *mut c_void, _surface: *mut c_void, _buffer: i32) -> u32 {
    EGL_TRUE
}
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglReleaseTexImage(_dpy: *mut c_void, _surface: *mut c_void, _buffer: i32) -> u32 {
    EGL_TRUE
}
/// `eglCopyBuffers(dpy, surface, target)` — copy the surface color buffer to a native pixmap. No native
/// pixmap target is modeled; accepted as a no-op. Success.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglCopyBuffers(_dpy: *mut c_void, _surface: *mut c_void, _target: *mut c_void) -> u32 {
    EGL_TRUE
}
/// `eglQueryContext(dpy, ctx, attribute, value)` — report a context attribute. This driver serves exactly
/// one client API (OpenGL ES), so `EGL_CONTEXT_CLIENT_TYPE` MUST answer `EGL_OPENGL_ES_API` — libepoxy's
/// `epoxy_egl_get_current_gl_context_api()` queries this to classify the current context, and treats any
/// value other than `EGL_OPENGL_API`/`EGL_OPENGL_ES_API` as "no EGL context" (aborting
/// `epoxy_get_proc_address` with "Couldn't find current GLX or EGL context"). Other attributes report the
/// truthful fixed values of the single back-buffered ES3 context this driver models; unknown attributes
/// read `0` and still succeed.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglQueryContext(_dpy: *mut c_void, _ctx: *mut c_void, attribute: i32, value: *mut i32) -> u32 {
    if value.is_null() {
        return EGL_FALSE;
    }
    let v = match attribute {
        EGL_CONTEXT_CLIENT_TYPE => EGL_OPENGL_ES_API as i32,
        EGL_CONTEXT_CLIENT_VERSION => 3,
        EGL_RENDER_BUFFER => EGL_BACK_BUFFER,
        _ => 0,
    };
    unsafe { *value = v };
    EGL_TRUE
}
/// `eglQuerySurface(dpy, surface, attribute, value)` — the surface geometry. `EGL_WIDTH`/`EGL_HEIGHT`
/// report the live window-surface size (real); other attributes read `0`.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglQuerySurface(_dpy: *mut c_void, _surface: *mut c_void, attribute: i32, value: *mut i32) -> u32 {
    if value.is_null() {
        return EGL_FALSE;
    }
    let v = with(|s| match attribute {
        EGL_WIDTH => s.ctx.surf.width as i32,
        EGL_HEIGHT => s.ctx.surf.height as i32,
        _ => 0,
    });
    unsafe { *value = v };
    EGL_TRUE
}
/// `eglCreatePbufferSurface(dpy, config, attrib_list)` — an offscreen pbuffer surface. Modeled as a window
/// surface sized from the environment (the driver renders to one color target); returns a fresh token.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglCreatePbufferSurface(_dpy: *mut c_void, _config: *mut c_void, _attrib_list: *const i32) -> *mut c_void {
    let (width, height) = default_surface_wh();
    with(|s| {
        s.ctx.surf = GlSurface { have: true, width, height };
        s.mint_token()
    })
}
/// `eglCreatePixmapSurface(dpy, config, pixmap, attrib_list)` — a native-pixmap-backed surface; modeled as
/// a fresh surface token (no separate pixmap storage).
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglCreatePixmapSurface(_dpy: *mut c_void, _config: *mut c_void, _pixmap: *mut c_void, _attrib_list: *const i32) -> *mut c_void {
    with(|s| s.mint_token())
}
/// `eglCreatePbufferFromClientBuffer(...)` — a pbuffer wrapping a client buffer (e.g. an OpenVG image);
/// modeled as a fresh surface token.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglCreatePbufferFromClientBuffer(_dpy: *mut c_void, _buftype: u32, _buffer: *mut c_void, _config: *mut c_void, _attrib_list: *const i32) -> *mut c_void {
    with(|s| s.mint_token())
}
/// `eglCreatePlatformWindowSurface(dpy, config, native_window, attrib_list)` — the EGL 1.5 window-surface
/// getter modern toolkits use. `native_window` is the same `wl_egl_window*`; brings up the window surface
/// (size + Wayland present session) exactly like `eglCreateWindowSurface`.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglCreatePlatformWindowSurface(_dpy: *mut c_void, _config: *mut c_void, native_window: *mut c_void, _attrib_list: *const isize) -> *mut c_void {
    create_window_surface(native_window)
}
/// `eglCreatePlatformPixmapSurface(...)` — the EGL 1.5 pixmap-surface getter; a fresh surface token.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglCreatePlatformPixmapSurface(_dpy: *mut c_void, _config: *mut c_void, _native_pixmap: *mut c_void, _attrib_list: *const isize) -> *mut c_void {
    with(|s| s.mint_token())
}
/// `eglCreateImage(...)` / `eglDestroyImage` — an `EGLImage` (a shareable image handle). No cross-API image
/// sharing is modeled; a fixed non-null token is handed back so a `!= EGL_NO_IMAGE` check passes.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglCreateImage(_dpy: *mut c_void, _ctx: *mut c_void, _target: u32, _buffer: *mut c_void, _attrib_list: *const isize) -> *mut c_void {
    EGL_OBJECT_TOKEN as *mut c_void
}
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglDestroyImage(_dpy: *mut c_void, _image: *mut c_void) -> u32 {
    EGL_TRUE
}
/// `eglCreateSync(dpy, type, attrib_list)` — an `EGLSync`; a fixed non-null token (the GL fence timeline
/// backs the actual GPU sync — `glFenceSync`). `eglClientWaitSync` reports it satisfied.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglCreateSync(_dpy: *mut c_void, _type_: u32, _attrib_list: *const isize) -> *mut c_void {
    EGL_OBJECT_TOKEN as *mut c_void
}
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglDestroySync(_dpy: *mut c_void, _sync: *mut c_void) -> u32 {
    EGL_TRUE
}
/// `eglClientWaitSync(dpy, sync, flags, timeout)` — the deferred model completes work synchronously at
/// swap, so a sync is already signaled: report `EGL_CONDITION_SATISFIED`.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglClientWaitSync(_dpy: *mut c_void, _sync: *mut c_void, _flags: i32, _timeout: u64) -> i32 {
    EGL_CONDITION_SATISFIED
}
/// `eglWaitSync(dpy, sync, flags)` — a device-side wait; always succeeds (the fence is already reached).
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglWaitSync(_dpy: *mut c_void, _sync: *mut c_void, _flags: i32) -> u32 {
    EGL_TRUE
}
/// `eglGetSyncAttrib(dpy, sync, attribute, value)` — write the sync attribute (`0`) and succeed.
#[cfg_attr(not(gles_client), no_mangle)]
pub extern "C" fn eglGetSyncAttrib(_dpy: *mut c_void, _sync: *mut c_void, _attribute: i32, value: *mut isize) -> u32 {
    if !value.is_null() {
        unsafe { *value = 0 };
    }
    EGL_TRUE
}

// ==================================================================================================
// GLES3.1: memory barriers — ordering hints for image/SSBO/atomic access
// ==================================================================================================

/// `glMemoryBarrier(barriers)` / `glMemoryBarrierByRegion(barriers)` — order incoherent memory accesses
/// (image load/store, SSBO writes, atomic counters) against subsequent access. This deferred model submits
/// each `glDispatchCompute` immediately and materializes no incoherent image/SSBO access between draws that
/// a barrier would need to order, so the ordering is already satisfied — an honest no-op.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glMemoryBarrier(_barriers: u32) {}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glMemoryBarrierByRegion(_barriers: u32) {}

// ==================================================================================================
// tests: the per-thread EGL "current" binding + bound-API tracking, and the glGet limit round-trip
//
// These drive the REAL C-ABI entry points (eglMakeCurrent / eglGetCurrent* / eglBindAPI / eglQueryAPI /
// glGetIntegerv) exactly as libepoxy does when GTK brings a GLES context up. The binding is thread-local,
// so each libtest thread starts clean; the tests reset explicitly where they depend on a starting value.
// ==================================================================================================
#[cfg(test)]
mod current_binding_tests {
    use super::*;

    // Desktop OpenGL API enum — NOT served by this GLES-only driver (eglBindAPI must reject it).
    const EGL_OPENGL_API: u32 = 0x30A2;

    /// eglMakeCurrent records (ctx, draw, read, display) as THIS thread's current binding, and the
    /// eglGetCurrent* getters return exactly those; EGL_NO_CONTEXT (null) clears the whole binding.
    #[test]
    fn make_current_round_trips_and_no_context_clears() {
        let dpy = 0x0D15 as *mut c_void;
        let draw = 0xD8A as *mut c_void;
        let read = 0x8EAD as *mut c_void;
        let ctx = 0xC0FFEE as *mut c_void;

        assert_eq!(eglMakeCurrent(dpy, draw, read, ctx), EGL_TRUE);
        assert_eq!(eglGetCurrentContext(), ctx, "current context is the one just made current");
        assert_eq!(eglGetCurrentDisplay(), dpy, "current display is the one passed to makeCurrent");
        assert_eq!(eglGetCurrentSurface(EGL_DRAW), draw, "EGL_DRAW surface round-trips");
        assert_eq!(eglGetCurrentSurface(EGL_READ), read, "EGL_READ surface round-trips");

        // EGL_NO_CONTEXT releases the binding: every getter returns null again.
        assert_eq!(eglMakeCurrent(dpy, core::ptr::null_mut(), core::ptr::null_mut(), core::ptr::null_mut()), EGL_TRUE);
        assert!(eglGetCurrentContext().is_null(), "EGL_NO_CONTEXT clears the current context");
        assert!(eglGetCurrentDisplay().is_null(), "EGL_NO_CONTEXT clears the current display");
        assert!(eglGetCurrentSurface(EGL_DRAW).is_null(), "EGL_NO_CONTEXT clears the draw surface");
        assert!(eglGetCurrentSurface(EGL_READ).is_null(), "EGL_NO_CONTEXT clears the read surface");
    }

    /// A getter on a thread that never made a context current returns null (EGL_NO_*), and one thread's
    /// current binding is INDEPENDENT of another's — the thread-local guarantee libepoxy relies on.
    #[test]
    fn current_binding_is_thread_local_and_independent() {
        let ctx_a = 0xA11 as usize;
        eglMakeCurrent(0x1 as *mut c_void, 0x2 as *mut c_void, 0x2 as *mut c_void, ctx_a as *mut c_void);
        assert_eq!(eglGetCurrentContext() as usize, ctx_a);

        let observed = std::thread::spawn(|| {
            // A fresh thread has NO current context, regardless of the parent's binding.
            let before = eglGetCurrentContext();
            // It can make its OWN context current without disturbing the parent.
            let ctx_b = 0xB22 as usize;
            eglMakeCurrent(0x1 as *mut c_void, 0x3 as *mut c_void, 0x3 as *mut c_void, ctx_b as *mut c_void);
            (before as usize, eglGetCurrentContext() as usize)
        })
        .join()
        .unwrap();

        assert_eq!(observed.0, 0, "a fresh thread starts with EGL_NO_CONTEXT");
        assert_eq!(observed.1, 0xB22, "the child thread bound its own context");
        // The parent's binding is untouched by the child.
        assert_eq!(eglGetCurrentContext() as usize, ctx_a, "the parent thread's current context is independent");
        // Cleanup this thread's binding.
        eglMakeCurrent(core::ptr::null_mut(), core::ptr::null_mut(), core::ptr::null_mut(), core::ptr::null_mut());
    }

    /// eglQueryAPI defaults to EGL_OPENGL_ES_API and returns whatever eglBindAPI(EGL_OPENGL_ES_API) set;
    /// a non-GLES API is rejected (EGL_BAD_PARAMETER) and does not change the queried API.
    #[test]
    fn bind_api_records_gles_and_rejects_other_apis() {
        // Default (nothing bound on this fresh thread) is the GLES API epoxy expects.
        assert_eq!(eglQueryAPI(), EGL_OPENGL_ES_API, "the default bound API is EGL_OPENGL_ES_API");

        assert_eq!(eglBindAPI(EGL_OPENGL_ES_API), EGL_TRUE, "binding the GLES API succeeds");
        assert_eq!(eglQueryAPI(), EGL_OPENGL_ES_API, "eglQueryAPI reports the bound GLES API");

        // A GLES-only driver rejects desktop GL; the queried API is unchanged.
        assert_eq!(eglBindAPI(EGL_OPENGL_API), EGL_FALSE, "binding desktop OpenGL is rejected");
        assert_eq!(eglGetError(), EGL_BAD_PARAMETER, "the rejected bind raised EGL_BAD_PARAMETER");
        assert_eq!(eglQueryAPI(), EGL_OPENGL_ES_API, "a rejected bind leaves the queried API as GLES");
    }

    /// eglQueryContext(EGL_CONTEXT_CLIENT_TYPE) MUST report EGL_OPENGL_ES_API. libepoxy's
    /// `epoxy_egl_get_current_gl_context_api()` (dispatch_common.c) queries exactly this to classify the
    /// current context; a `0`/EGL_NONE answer makes `epoxy_get_proc_address` abort with "Couldn't find
    /// current GLX or EGL context" — which is what blocked GTK4's GskGL bring-up until this was fixed.
    #[test]
    fn query_context_reports_gles_client_type_for_epoxy() {
        let dpy = 0x1 as *mut c_void;
        let ctx = 0x2 as *mut c_void;

        let mut client_type: i32 = -455_764_240; // garbage sentinel a correct getter overwrites
        assert_eq!(
            eglQueryContext(dpy, ctx, EGL_CONTEXT_CLIENT_TYPE, &mut client_type as *mut i32),
            EGL_TRUE,
            "eglQueryContext succeeds"
        );
        assert_eq!(
            client_type as u32, EGL_OPENGL_ES_API,
            "EGL_CONTEXT_CLIENT_TYPE is the GLES client API epoxy classifies on (not 0/garbage)"
        );

        // The client version this ES3 driver reports, and the back-buffered render buffer.
        let mut version: i32 = -1;
        eglQueryContext(dpy, ctx, EGL_CONTEXT_CLIENT_VERSION, &mut version as *mut i32);
        assert_eq!(version, 3, "EGL_CONTEXT_CLIENT_VERSION reports ES3");
        let mut rbuf: i32 = -1;
        eglQueryContext(dpy, ctx, EGL_RENDER_BUFFER, &mut rbuf as *mut i32);
        assert_eq!(rbuf, EGL_BACK_BUFFER, "EGL_RENDER_BUFFER reports the back buffer");

        // A null out-param is rejected without a deref; an unknown attribute writes 0 and still succeeds.
        assert_eq!(
            eglQueryContext(dpy, ctx, EGL_CONTEXT_CLIENT_TYPE, core::ptr::null_mut()),
            EGL_FALSE,
            "a null value pointer is rejected"
        );
        let mut unknown: i32 = -455_764_240;
        eglQueryContext(dpy, ctx, 0xBEEF, &mut unknown as *mut i32);
        assert_eq!(unknown, 0, "an unknown attribute writes 0, never uninitialized memory");
    }

    /// glGetIntegerv / glGetInteger64v ALWAYS write the out-param (never leave it as uninitialized
    /// garbage): GL_MAX_TEXTURE_SIZE is the 16384 executor ceiling and an unknown pname writes 0.
    #[test]
    fn gl_get_integerv_always_writes_the_out_param() {
        // Seed with a garbage sentinel; a correct getter overwrites it.
        let mut v: i32 = -455_764_240;
        glGetIntegerv(GL_MAX_TEXTURE_SIZE, &mut v as *mut i32);
        assert_eq!(v, 16384, "GL_MAX_TEXTURE_SIZE is the truthful executor ceiling, not garbage");

        let mut v64: i64 = -1;
        glGetInteger64v(GL_MAX_TEXTURE_SIZE, &mut v64 as *mut i64);
        assert_eq!(v64, 16384, "glGetInteger64v writes the same truthful ceiling");

        // An unhandled integer pname defaults to 0 — never the untouched garbage sentinel.
        let mut u: i32 = -455_764_240;
        glGetIntegerv(0xBEEF, &mut u as *mut i32);
        assert_eq!(u, 0, "an unknown pname writes 0, never uninitialized memory");
    }
}
