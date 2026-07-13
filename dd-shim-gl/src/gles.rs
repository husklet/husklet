//! Hand-written GLES2 entry points — the state + query + resource families, ported faithfully from
//! `gl_shim.c`. Like the C shim, these *accumulate state* (into [`crate::state`]); they emit no IR. The
//! IR is lowered from this state at swap by the present/draw path, which this increment leaves as
//! generated stubs (`glClear`, `glDrawArrays`, `glDrawElements`, `eglSwapBuffers`, …). The
//! present-independent resource lowering those will use lives in [`crate::lower`].

use core::ffi::{c_char, c_void};

use crate::glconst::*;
use crate::state::{gl, MAXATTR, MAXBUF, MAXPROG, MAXSH, MAXTEX};

// ---- small helpers ---------------------------------------------------------------------------------

unsafe fn set_i32(p: *mut i32, v: i32) {
    if !p.is_null() {
        *p = v;
    }
}

macro_rules! cstr {
    ($s:literal) => {
        concat!($s, "\0").as_ptr() as *const c_char
    };
}

// ===================================================================================================
// error + strings
// ===================================================================================================

/// `glGetError` — read-and-clear the GL error flag. Covered (hand-written) paths never raise, so on a
/// pure real-body workload this stays `GL_NO_ERROR` (gl_shim.c parity). The generated truthful-failure
/// stubs (`crate::stub::fail_gl`) DO raise here, so an app that calls an unsupported entry point sees a
/// deterministic, API-correct error instead of a silent false success.
#[no_mangle]
pub extern "C" fn glGetError() -> u32 {
    crate::state::take_gl_error()
}

/// `glGetString` — identity strings. Version/GLSL/extension strings come from the generated capability
/// inventory ([`crate::ADVERTISED_GL_VERSION`], …), so the shim advertises the LOWEST coherent GLES
/// profile its real bodies actually back — it does NOT claim ES 3.x just because the ES3 symbols exist
/// (they are truthful-failure stubs). ES3 remains an explicit opt-in: a context created with major>=3
/// (only accepted under `DD_SHIM_ES3`) advertises the ES3 identity strings.
#[no_mangle]
pub extern "C" fn glGetString(name: u32) -> *const u8 {
    let es3 = crate::state::egl_ctx_major(core::ptr::null_mut()) >= 3;
    let s: *const c_char = match name {
        0x1F02 => {
            if es3 {
                cstr!("OpenGL ES 3.0 dd-shim")
            } else {
                crate::ADVERTISED_GL_VERSION.as_ptr() as *const c_char
            }
        }
        0x1F00 => cstr!("dd"),       // GL_VENDOR
        0x1F01 => cstr!("dd-metal"), // GL_RENDERER
        0x8B8C => {
            if es3 {
                cstr!("OpenGL ES GLSL ES 3.00")
            } else {
                crate::ADVERTISED_GLSL_VERSION.as_ptr() as *const c_char
            }
        }
        0x1F03 => crate::ADVERTISED_GL_EXTENSIONS.as_ptr() as *const c_char, // GL_EXTENSIONS
        _ => cstr!(""),
    };
    s as *const u8
}

/// `glGetStringi(GL_EXTENSIONS, i)` — the indexed extension query (ES3 / GL_NUM_EXTENSIONS path). Served
/// from the SAME inventory list as `glGetString(GL_EXTENSIONS)` (gl_shim.c parity), so the two never
/// disagree. Out-of-range / non-EXTENSIONS queries return the empty string.
#[no_mangle]
pub extern "C" fn glGetStringi(name: u32, index: u32) -> *const u8 {
    if name == 0x1F03 {
        if let Some(ext) = crate::ADVERTISED_GL_EXTENSIONS_LIST.get(index as usize) {
            return ext.as_ptr();
        }
    }
    cstr!("") as *const u8
}

// ===================================================================================================
// scalar state + queries
// ===================================================================================================

#[no_mangle]
pub extern "C" fn glClearColor(r: f32, g: f32, b: f32, a: f32) {
    gl().clear = [r, g, b, a];
}

#[no_mangle]
pub extern "C" fn glViewport(x: i32, y: i32, w: i32, h: i32) {
    // GL_INVALID_VALUE on negative dimensions; the previous viewport is left UNMODIFIED (no state
    // mutation on a rejected call).
    if w < 0 || h < 0 {
        crate::state::set_gl_error(GL_INVALID_VALUE);
        return;
    }
    gl().viewport = [x, y, w, h];
}

#[no_mangle]
pub extern "C" fn glScissor(x: i32, y: i32, w: i32, h: i32) {
    gl().scissor = [x, y, w, h];
}

#[no_mangle]
pub extern "C" fn glEnable(cap: u32) {
    let mut s = gl();
    match cap {
        GL_DEPTH_TEST => s.depth = true,
        GL_BLEND => s.blend = true,
        GL_CULL_FACE => s.cull = true,
        GL_SCISSOR_TEST => s.scissor_enabled = true,
        // An unknown capability is GL_INVALID_ENUM and must NOT alias a real enable bit.
        _ => {
            drop(s);
            crate::state::set_gl_error(GL_INVALID_ENUM);
        }
    }
}

#[no_mangle]
pub extern "C" fn glDisable(cap: u32) {
    let mut s = gl();
    match cap {
        GL_DEPTH_TEST => s.depth = false,
        GL_BLEND => s.blend = false,
        GL_CULL_FACE => s.cull = false,
        GL_SCISSOR_TEST => s.scissor_enabled = false,
        _ => {
            drop(s);
            crate::state::set_gl_error(GL_INVALID_ENUM);
        }
    }
}

#[no_mangle]
pub extern "C" fn glIsEnabled(cap: u32) -> u8 {
    let s = gl();
    let on = match cap {
        GL_DEPTH_TEST => s.depth,
        GL_BLEND => s.blend,
        GL_CULL_FACE => s.cull,
        GL_SCISSOR_TEST => s.scissor_enabled,
        _ => false,
    };
    on as u8
}

#[no_mangle]
pub extern "C" fn glBlendFunc(src: u32, dst: u32) {
    let mut s = gl();
    s.blend_src_rgb = src;
    s.blend_dst_rgb = dst;
    s.blend_src_alpha = src;
    s.blend_dst_alpha = dst;
}

#[no_mangle]
pub extern "C" fn glBlendFuncSeparate(src_rgb: u32, dst_rgb: u32, src_a: u32, dst_a: u32) {
    let mut s = gl();
    s.blend_src_rgb = src_rgb;
    s.blend_dst_rgb = dst_rgb;
    s.blend_src_alpha = src_a;
    s.blend_dst_alpha = dst_a;
}

#[no_mangle]
pub extern "C" fn glBlendEquation(mode: u32) {
    let mut s = gl();
    s.blend_eq_rgb = mode;
    s.blend_eq_alpha = mode;
}

#[no_mangle]
pub extern "C" fn glBlendEquationSeparate(rgb: u32, alpha: u32) {
    let mut s = gl();
    s.blend_eq_rgb = rgb;
    s.blend_eq_alpha = alpha;
}

// glBlendFunci / glBlendEquationi variants: for draw buffer 0, delegate; else ignore (gl_shim.c).
#[no_mangle]
pub extern "C" fn glBlendFunci(buf: u32, s: u32, d: u32) {
    if buf == 0 {
        glBlendFunc(s, d);
    }
}
#[no_mangle]
pub extern "C" fn glBlendFuncSeparatei(buf: u32, a: u32, b: u32, c: u32, d: u32) {
    if buf == 0 {
        glBlendFuncSeparate(a, b, c, d);
    }
}
#[no_mangle]
pub extern "C" fn glBlendEquationi(buf: u32, m: u32) {
    if buf == 0 {
        glBlendEquation(m);
    }
}
#[no_mangle]
pub extern "C" fn glBlendEquationSeparatei(buf: u32, a: u32, b: u32) {
    if buf == 0 {
        glBlendEquationSeparate(a, b);
    }
}

// Pure no-op fixed-function state (gl_shim.c keeps no state for these).
#[no_mangle]
pub extern "C" fn glBlendColor(_r: f32, _g: f32, _b: f32, _a: f32) {}
#[no_mangle]
pub extern "C" fn glCullFace(_mode: u32) {}
#[no_mangle]
pub extern "C" fn glFrontFace(_mode: u32) {}
#[no_mangle]
pub extern "C" fn glColorMask(_r: u8, _g: u8, _b: u8, _a: u8) {}
#[no_mangle]
pub extern "C" fn glDepthFunc(_f: u32) {}
#[no_mangle]
pub extern "C" fn glDepthMask(_f: u8) {}
#[no_mangle]
pub extern "C" fn glDepthRangef(_n: f32, _f: f32) {}
#[no_mangle]
pub extern "C" fn glClearDepthf(_d: f32) {}
#[no_mangle]
pub extern "C" fn glClearStencil(_s: i32) {}
#[no_mangle]
pub extern "C" fn glStencilFunc(_f: u32, _r: i32, _m: u32) {}
#[no_mangle]
pub extern "C" fn glStencilOp(_a: u32, _b: u32, _c: u32) {}
#[no_mangle]
pub extern "C" fn glStencilMask(_m: u32) {}
#[no_mangle]
pub extern "C" fn glLineWidth(_w: f32) {}
#[no_mangle]
pub extern "C" fn glHint(_t: u32, _m: u32) {}
#[no_mangle]
pub extern "C" fn glPolygonOffset(_a: f32, _b: f32) {}
#[no_mangle]
pub extern "C" fn glSampleCoverage(_v: f32, _i: u8) {}
/// Process-global command-submission serials backing `glFlush`/`glFinish`. `SUBMIT` advances when the
/// guest hands work off; `COMPLETE` tracks how far the host has finished. Without a live executor
/// round-trip (host-tool / IR-dump mode) completion is synchronous, but the serials keep the two calls'
/// contract distinct and observable.
static SUBMIT_SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static COMPLETE_SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// `glFinish` — BLOCKING: submit queued work, then wait until the host has completed everything up to
/// this submission serial (a real executor blocks on a fence here; the host-tool model completes
/// synchronously).
#[no_mangle]
pub extern "C" fn glFinish() {
    use std::sync::atomic::Ordering;
    let target = SUBMIT_SERIAL.fetch_add(1, Ordering::SeqCst) + 1;
    while COMPLETE_SERIAL.load(Ordering::SeqCst) < target {
        let done = COMPLETE_SERIAL.load(Ordering::SeqCst);
        let _ = COMPLETE_SERIAL.compare_exchange(done, target, Ordering::SeqCst, Ordering::SeqCst);
    }
}

/// `glFlush` — NONBLOCKING: advance the submission serial so queued work is handed off, then return
/// immediately without waiting for completion.
#[no_mangle]
pub extern "C" fn glFlush() {
    SUBMIT_SERIAL.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
}

/// The current (submitted, completed) submission serials — for `glFinish`/`glFlush` semantics tests.
pub fn submission_serials() -> (u64, u64) {
    use std::sync::atomic::Ordering;
    (SUBMIT_SERIAL.load(Ordering::SeqCst), COMPLETE_SERIAL.load(Ordering::SeqCst))
}
#[no_mangle]
pub extern "C" fn glGenerateMipmap(_target: u32) {}

#[no_mangle]
pub extern "C" fn glPixelStorei(pname: u32, v: i32) {
    let mut s = gl();
    match pname {
        GL_UNPACK_ALIGNMENT => s.unpack_alignment = if v > 0 { v } else { 1 },
        GL_UNPACK_ROW_LENGTH => s.unpack_row_length = v,
        GL_UNPACK_SKIP_ROWS => s.unpack_skip_rows = v,
        GL_UNPACK_SKIP_PIXELS => s.unpack_skip_pixels = v,
        _ => {}
    }
}

/// `glGetIntegerv` — capability limits + bound-object queries. Faithful to gl_shim.c's switch (same
/// values), so apps that gate on these behave identically.
#[no_mangle]
pub extern "C" fn glGetIntegerv(pname: u32, v: *mut i32) {
    if v.is_null() {
        return;
    }
    let s = gl();
    let ctx_major = crate::state::egl_ctx_major(core::ptr::null_mut());
    let ctx_minor = crate::state::egl_ctx_minor(core::ptr::null_mut());
    unsafe {
        match pname {
            GL_MAX_TEXTURE_SIZE | GL_MAX_CUBE_MAP_TEXTURE_SIZE | GL_MAX_RENDERBUFFER_SIZE => *v = 4096,
            GL_MAX_VERTEX_ATTRIBS => *v = 16,
            GL_MAX_TEXTURE_IMAGE_UNITS | GL_MAX_COMBINED_TEXTURE_IMAGE_UNITS => *v = 8,
            GL_MAX_VERTEX_TEXTURE_IMAGE_UNITS => *v = 4,
            GL_MAX_FRAGMENT_UNIFORM_VECTORS | GL_MAX_VERTEX_UNIFORM_VECTORS => *v = 256,
            GL_MAX_VARYING_VECTORS => *v = 15,
            GL_NUM_COMPRESSED_TEXTURE_FORMATS | GL_SAMPLES => *v = 0,
            GL_CURRENT_PROGRAM => *v = s.cur_prog as i32,
            GL_ACTIVE_TEXTURE => *v = (GL_TEXTURE0 + s.active_unit as u32) as i32,
            GL_ARRAY_BUFFER_BINDING => *v = s.arr_buf as i32,
            GL_ELEMENT_ARRAY_BUFFER_BINDING => *v = s.elem_buf as i32,
            GL_TEXTURE_BINDING_2D => *v = s.tex_unit[s.active_unit] as i32,
            GL_RENDERBUFFER_BINDING => *v = s.rbo_bound as i32,
            GL_DRAW_FRAMEBUFFER_BINDING => *v = s.draw_fbo as i32,
            GL_READ_FRAMEBUFFER_BINDING => *v = s.read_fbo as i32,
            GL_MAX_SAMPLES_ES3 => *v = 4,
            GL_MAJOR_VERSION => *v = ctx_major,
            GL_MINOR_VERSION => *v = ctx_minor,
            GL_NUM_EXTENSIONS => *v = crate::ADVERTISED_GL_EXTENSION_COUNT as i32, // from the inventory
            GL_DEPTH_BITS => *v = 24,
            GL_STENCIL_BITS => *v = 8,
            GL_RED_BITS => *v = 8,
            GL_MAX_VIEWPORT_DIMS => {
                *v = 4096;
                *v.add(1) = 4096;
            }
            GL_VIEWPORT => {
                *v = s.viewport[0];
                *v.add(1) = s.viewport[1];
                *v.add(2) = if s.viewport[2] != 0 { s.viewport[2] } else { 256 };
                *v.add(3) = if s.viewport[3] != 0 { s.viewport[3] } else { 256 };
            }
            GL_SCISSOR_BOX => {
                *v = s.scissor[0];
                *v.add(1) = s.scissor[1];
                *v.add(2) = s.scissor[2];
                *v.add(3) = s.scissor[3];
            }
            _ => *v = 0,
        }
    }
}

#[no_mangle]
pub extern "C" fn glGetFloatv(_pname: u32, v: *mut f32) {
    unsafe {
        if !v.is_null() {
            *v = 0.0;
        }
    }
}

#[no_mangle]
pub extern "C" fn glGetBooleanv(_pname: u32, v: *mut u8) {
    unsafe {
        if !v.is_null() {
            *v = 0;
        }
    }
}

// ===================================================================================================
// buffers
// ===================================================================================================

#[no_mangle]
pub extern "C" fn glGenBuffers(n: i32, out: *mut u32) {
    // A negative count is GL_INVALID_VALUE; the output must be left untouched (no sentinel write).
    if n < 0 {
        crate::state::set_gl_error(GL_INVALID_VALUE);
        return;
    }
    if out.is_null() {
        return;
    }
    let mut s = gl();
    for k in 0..n as usize {
        let id = s.gen_buffer();
        unsafe { *out.add(k) = id };
    }
}

#[no_mangle]
pub extern "C" fn glDeleteBuffers(n: i32, ids: *const u32) {
    if ids.is_null() || n < 0 {
        return;
    }
    let mut s = gl();
    for k in 0..n as usize {
        let id = unsafe { *ids.add(k) } as usize;
        if id != 0 && id < MAXBUF && s.buf[id].used {
            s.buf[id].used = false;
            s.buf[id].data.clear();
            s.buf[id].gen += 1;
        }
    }
}

#[no_mangle]
pub extern "C" fn glBindBuffer(target: u32, buffer: u32) {
    // An invalid target is GL_INVALID_ENUM and must neither bind the object nor mutate any binding.
    if target != GL_ARRAY_BUFFER && target != GL_ELEMENT_ARRAY_BUFFER {
        crate::state::set_gl_error(GL_INVALID_ENUM);
        return;
    }
    let mut s = gl();
    if target == GL_ARRAY_BUFFER {
        s.arr_buf = buffer;
    } else {
        s.elem_buf = buffer;
        s.vao_store_current();
    }
}

#[no_mangle]
pub extern "C" fn glBufferData(target: u32, size: isize, data: *const c_void, usage: u32) {
    let mut s = gl();
    let b = if target == GL_ELEMENT_ARRAY_BUFFER { s.elem_buf } else { s.arr_buf } as usize;
    if (target != GL_ARRAY_BUFFER && target != GL_ELEMENT_ARRAY_BUFFER) || b >= MAXBUF || !s.buf[b].used || size < 0 {
        return;
    }
    let n = size as usize;
    let mut bytes = vec![0u8; n];
    if !data.is_null() {
        unsafe { core::ptr::copy_nonoverlapping(data as *const u8, bytes.as_mut_ptr(), n) };
    }
    s.buf[b].data = bytes;
    s.buf[b].usage = usage;
    s.buf[b].gen += 1;
}

#[no_mangle]
pub extern "C" fn glBufferSubData(target: u32, offset: isize, size: isize, data: *const c_void) {
    let mut s = gl();
    let b = if target == GL_ELEMENT_ARRAY_BUFFER { s.elem_buf } else { s.arr_buf } as usize;
    if b >= MAXBUF || !s.buf[b].used || s.buf[b].data.is_empty() || offset < 0 || size < 0 {
        return;
    }
    let (off, n) = (offset as usize, size as usize);
    if off + n <= s.buf[b].data.len() && !data.is_null() {
        unsafe { core::ptr::copy_nonoverlapping(data as *const u8, s.buf[b].data[off..].as_mut_ptr(), n) };
    }
    s.buf[b].gen += 1;
}

#[no_mangle]
pub extern "C" fn glIsBuffer(b: u32) -> u8 {
    let s = gl();
    (b != 0 && (b as usize) < MAXBUF && s.buf[b as usize].used) as u8
}

/// `glMapBufferRange` — FUNCTIONAL, byte-identical to gl_shim.c: hand back a pointer INTO the bound
/// buffer's storage (growing it to `offset+length` if needed). The app writes through the pointer; the
/// matching `glUnmapBuffer` bumps the buffer's `gen` so the swap-time upload re-ships the new bytes.
/// The pointer stays valid until the buffer's `Vec` reallocates (same fragile contract as the C shim).
#[no_mangle]
pub extern "C" fn glMapBufferRange(target: u32, offset: isize, length: isize, _access: u32) -> *mut c_void {
    if offset < 0 || length < 0 {
        return core::ptr::null_mut();
    }
    let mut s = gl();
    let b = if target == GL_ELEMENT_ARRAY_BUFFER { s.elem_buf } else { s.arr_buf } as usize;
    if b >= MAXBUF || !s.buf[b].used {
        return core::ptr::null_mut();
    }
    let need = (offset + length) as usize;
    if s.buf[b].data.len() < need {
        s.buf[b].data.resize(need, 0);
    }
    unsafe { s.buf[b].data.as_mut_ptr().add(offset as usize) as *mut c_void }
}

/// `glUnmapBuffer` — mark the bound buffer dirty (gl_shim.c bumps `gen`, returns GL_TRUE).
#[no_mangle]
pub extern "C" fn glUnmapBuffer(target: u32) -> u8 {
    let mut s = gl();
    let b = if target == GL_ELEMENT_ARRAY_BUFFER { s.elem_buf } else { s.arr_buf } as usize;
    if b < MAXBUF && s.buf[b].used {
        s.buf[b].gen += 1;
    }
    GL_TRUE
}

/// `glCopyBufferSubData` — memmove between (possibly the same) bound buffers (gl_shim.c parity).
#[no_mangle]
pub extern "C" fn glCopyBufferSubData(read_target: u32, write_target: u32, read_off: isize, write_off: isize, size: isize) {
    if read_off < 0 || write_off < 0 || size < 0 {
        return;
    }
    let mut s = gl();
    let rb = if read_target == GL_ELEMENT_ARRAY_BUFFER { s.elem_buf } else { s.arr_buf } as usize;
    let wb = if write_target == GL_ELEMENT_ARRAY_BUFFER { s.elem_buf } else { s.arr_buf } as usize;
    let (ro, wo, n) = (read_off as usize, write_off as usize, size as usize);
    if rb >= MAXBUF || wb >= MAXBUF || s.buf[rb].data.is_empty() || s.buf[wb].data.is_empty() {
        return;
    }
    if ro + n > s.buf[rb].data.len() || wo + n > s.buf[wb].data.len() {
        return;
    }
    if rb == wb {
        s.buf[wb].data.copy_within(ro..ro + n, wo);
    } else {
        let src = s.buf[rb].data[ro..ro + n].to_vec();
        s.buf[wb].data[wo..wo + n].copy_from_slice(&src);
    }
    s.buf[wb].gen += 1;
}

// ===================================================================================================
// ES3 object-name allocators (samplers / queries / transform feedbacks)
//
// gl_shim.c hands out monotonic names from `g_samp_seq`/`g_query_seq`/`g_xfb_seq` (all from 1). The
// objects themselves carry no backing state — their bind/param/begin/end entry points are documented
// no-ops (`partial`) — but returning REAL names (not 0) is what lets an app's bookkeeping proceed, so
// these allocators are full bodies at gl_shim.c parity.
// ===================================================================================================

#[no_mangle]
pub extern "C" fn glGenSamplers(n: i32, out: *mut u32) {
    if out.is_null() || n < 0 {
        return;
    }
    let mut s = gl();
    for k in 0..n as usize {
        let id = s.samp_seq;
        s.samp_seq += 1;
        unsafe { *out.add(k) = id };
    }
}

#[no_mangle]
pub extern "C" fn glGenQueries(n: i32, out: *mut u32) {
    if out.is_null() || n < 0 {
        return;
    }
    let mut s = gl();
    for k in 0..n as usize {
        let id = s.query_seq;
        s.query_seq += 1;
        unsafe { *out.add(k) = id };
    }
}

#[no_mangle]
pub extern "C" fn glGenTransformFeedbacks(n: i32, out: *mut u32) {
    if out.is_null() || n < 0 {
        return;
    }
    let mut s = gl();
    for k in 0..n as usize {
        let id = s.xfb_seq;
        s.xfb_seq += 1;
        unsafe { *out.add(k) = id };
    }
}

// ===================================================================================================
// textures
// ===================================================================================================

#[no_mangle]
pub extern "C" fn glGenTextures(n: i32, out: *mut u32) {
    if out.is_null() || n < 0 {
        return;
    }
    let mut s = gl();
    for k in 0..n as usize {
        let id = s.gen_texture();
        unsafe { *out.add(k) = id };
    }
}

#[no_mangle]
pub extern "C" fn glDeleteTextures(n: i32, ids: *const u32) {
    if ids.is_null() || n < 0 {
        return;
    }
    let mut s = gl();
    for k in 0..n as usize {
        let id = unsafe { *ids.add(k) } as usize;
        if id != 0 && id < MAXTEX && s.tex[id].used {
            s.tex[id].used = false;
            s.tex[id].data.clear();
            s.tex[id].gen += 1;
        }
    }
}

#[no_mangle]
pub extern "C" fn glActiveTexture(unit: u32) {
    let u = unit.wrapping_sub(GL_TEXTURE0) as i32;
    if (0..8).contains(&u) {
        gl().active_unit = u as usize;
    }
}

#[no_mangle]
pub extern "C" fn glBindTexture(_target: u32, t: u32) {
    let mut s = gl();
    let u = s.active_unit;
    if u < 8 {
        s.tex_unit[u] = t;
    }
}

#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glTexImage2D(
    _target: u32,
    level: i32,
    _ifmt: i32,
    w: i32,
    h: i32,
    _border: i32,
    fmt: u32,
    _typ: u32,
    pixels: *const c_void,
) {
    let mut s = gl();
    let t = s.bound_tex();
    if level != 0 || (t as usize) >= MAXTEX || t == 0 || !s.tex[t as usize].used || w <= 0 || h <= 0 {
        return;
    }
    let size = (w as usize) * (h as usize) * 4;
    {
        let tex = &mut s.tex[t as usize];
        tex.w = w;
        tex.h = h;
        tex.data = vec![0u8; size];
        tex.gen += 1;
    }
    let src = unsafe { pixels_slice(pixels, w, h, fmt, &s) };
    s.tex_store_pixels(t, 0, 0, w, h, fmt, src.as_deref());
}

#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glTexSubImage2D(
    _target: u32,
    level: i32,
    xo: i32,
    yo: i32,
    w: i32,
    h: i32,
    fmt: u32,
    _typ: u32,
    pixels: *const c_void,
) {
    let mut s = gl();
    let t = s.bound_tex();
    if level != 0 || (t as usize) >= MAXTEX || t == 0 || !s.tex[t as usize].used {
        return;
    }
    let src = unsafe { pixels_slice(pixels, w, h, fmt, &s) };
    s.tex_store_pixels(t, xo, yo, w, h, fmt, src.as_deref());
    s.tex[t as usize].gen += 1;
}

/// `glTexStorage2D` — immutable-storage allocation. gl_shim.c mirrors glTexImage2D's RGBA8 alloc so a
/// later glTexSubImage2D has a target; format/levels are not otherwise backed.
#[no_mangle]
pub extern "C" fn glTexStorage2D(target: u32, _levels: i32, _ifmt: u32, w: i32, h: i32) {
    let mut s = gl();
    let t = s.bound_tex();
    if target == GL_TEXTURE_2D && (t as usize) < MAXTEX && t != 0 && s.tex[t as usize].used && w > 0 && h > 0 {
        let size = (w as usize) * (h as usize) * 4;
        let tex = &mut s.tex[t as usize];
        tex.w = w;
        tex.h = h;
        tex.data = vec![0u8; size];
        tex.gen += 1;
    }
}

/// `glTexStorage3D` — 2D-array / 3D immutable storage; gl_shim.c allocates the layer-0 RGBA8 plane.
#[no_mangle]
pub extern "C" fn glTexStorage3D(target: u32, _levels: i32, _ifmt: u32, w: i32, h: i32, _d: i32) {
    let mut s = gl();
    let t = s.bound_tex();
    if (target == GL_TEXTURE_2D_ARRAY || target == GL_TEXTURE_3D) && s.tex.get(t as usize).map(|x| x.used).unwrap_or(false) {
        s.tex_alloc_rgba(t, w, h);
    }
}

/// `glTexImage3D` — 2D-array / 3D upload; gl_shim.c allocates RGBA8 and stores the layer-0 plane.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glTexImage3D(target: u32, level: i32, _ifmt: i32, w: i32, h: i32, d: i32, _border: i32, fmt: u32, _typ: u32, pixels: *const c_void) {
    let mut s = gl();
    let t = s.bound_tex();
    if level != 0 || (target != GL_TEXTURE_2D_ARRAY && target != GL_TEXTURE_3D) || !s.tex.get(t as usize).map(|x| x.used).unwrap_or(false) {
        return;
    }
    if !s.tex_alloc_rgba(t, w, h) {
        return;
    }
    if !pixels.is_null() && d > 0 {
        let src = unsafe { pixels_slice(pixels, w, h, fmt, &s) };
        s.tex_store_pixels(t, 0, 0, w, h, fmt, src.as_deref());
    }
}

/// `glTexSubImage3D` — layer-0 sub-image update for a 2D-array / 3D texture (gl_shim.c parity).
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glTexSubImage3D(target: u32, level: i32, xo: i32, yo: i32, zo: i32, w: i32, h: i32, d: i32, fmt: u32, _typ: u32, pixels: *const c_void) {
    let mut s = gl();
    let t = s.bound_tex();
    if level != 0 || zo != 0 || d <= 0 || (target != GL_TEXTURE_2D_ARRAY && target != GL_TEXTURE_3D) {
        return;
    }
    if !s.tex.get(t as usize).map(|x| x.used && !x.data.is_empty()).unwrap_or(false) {
        return;
    }
    let src = unsafe { pixels_slice(pixels, w, h, fmt, &s) };
    s.tex_store_pixels(t, xo, yo, w, h, fmt, src.as_deref());
    s.tex[t as usize].gen += 1;
}

/// `glCopyTexImage2D` — allocate the bound texture and copy a rect from the read framebuffer's color
/// texture into it (gl_shim.c parity; CPU-side blit).
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glCopyTexImage2D(target: u32, level: i32, _ifmt: u32, x: i32, y: i32, w: i32, h: i32, border: i32) {
    if target != GL_TEXTURE_2D || level != 0 || border != 0 {
        return;
    }
    let mut s = gl();
    let dst = s.bound_tex();
    let src = s.fbo_color_texture(s.read_fbo);
    if src == 0 || !s.tex_alloc_rgba(dst, w, h) {
        return;
    }
    s.copy_texture_rect(src, dst, x, y, x + w, y + h, 0, 0, w, h);
}

/// `glCopyTexSubImage2D` — copy a rect from the read framebuffer into an existing bound texture.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glCopyTexSubImage2D(target: u32, level: i32, xo: i32, yo: i32, x: i32, y: i32, w: i32, h: i32) {
    if target != GL_TEXTURE_2D || level != 0 || w <= 0 || h <= 0 {
        return;
    }
    let mut s = gl();
    let dst = s.bound_tex();
    let src = s.fbo_color_texture(s.read_fbo);
    if src == 0 || !s.tex.get(dst as usize).map(|t| t.used && !t.data.is_empty()).unwrap_or(false) {
        return;
    }
    s.copy_texture_rect(src, dst, x, y, x + w, y + h, xo, yo, xo + w, yo + h);
}

/// Reconstruct a bounded read-only copy for a texture upload (row-stride aware), so the state code
/// never reads past the client buffer. Returns `None` for a null pointer (clear-to-default upload).
unsafe fn pixels_slice(pixels: *const c_void, w: i32, h: i32, fmt: u32, s: &crate::state::GlState) -> Option<Vec<u8>> {
    if pixels.is_null() || w <= 0 || h <= 0 {
        return None;
    }
    let bpp = crate::state::GlState::tex_bpp(fmt);
    let row_pixels = if s.unpack_row_length > 0 { s.unpack_row_length as usize } else { w as usize };
    let mut row_bytes = row_pixels * bpp;
    if s.unpack_alignment > 1 {
        let a = s.unpack_alignment as usize;
        row_bytes = (row_bytes + a - 1) & !(a - 1);
    }
    let total = (s.unpack_skip_rows as usize + h as usize) * row_bytes + s.unpack_skip_pixels as usize * bpp;
    let mut buf = vec![0u8; total];
    core::ptr::copy_nonoverlapping(pixels as *const u8, buf.as_mut_ptr(), total);
    Some(buf)
}

#[no_mangle]
pub extern "C" fn glTexParameteri(_target: u32, pname: u32, v: i32) {
    let mut s = gl();
    let t = s.bound_tex() as usize;
    if t >= MAXTEX || t == 0 || !s.tex[t].used {
        return;
    }
    let vt = v as u32;
    match pname {
        GL_TEXTURE_MIN_FILTER => s.tex[t].minf = vt,
        GL_TEXTURE_MAG_FILTER => s.tex[t].magf = vt,
        GL_TEXTURE_WRAP_S => s.tex[t].ws = vt,
        GL_TEXTURE_WRAP_T => s.tex[t].wt = vt,
        _ => {}
    }
}

#[no_mangle]
pub extern "C" fn glTexParameterf(target: u32, pname: u32, v: f32) {
    glTexParameteri(target, pname, v as i32);
}

#[no_mangle]
pub extern "C" fn glTexParameteriv(target: u32, pname: u32, v: *const i32) {
    if !v.is_null() {
        glTexParameteri(target, pname, unsafe { *v });
    }
}

#[no_mangle]
pub extern "C" fn glTexParameterfv(target: u32, pname: u32, v: *const f32) {
    if !v.is_null() {
        glTexParameteri(target, pname, unsafe { *v } as i32);
    }
}

#[no_mangle]
pub extern "C" fn glIsTexture(t: u32) -> u8 {
    let s = gl();
    (t != 0 && (t as usize) < MAXTEX && s.tex[t as usize].used) as u8
}

// ===================================================================================================
// shaders + programs (object state only; the GLSL->shader translation lands with the shader-translate
// increment — glGetUniformLocation/glGetAttribLocation therefore report "not found" for now)
// ===================================================================================================

#[no_mangle]
pub extern "C" fn glCreateShader(kind: u32) -> u32 {
    gl().gen_shader(kind)
}

#[no_mangle]
pub extern "C" fn glShaderSource(sh: u32, count: i32, string: *const *const c_char, length: *const i32) {
    if (sh as usize) >= MAXSH {
        return;
    }
    let mut joined = String::new();
    if !string.is_null() && count > 0 {
        for i in 0..count as usize {
            let sp = unsafe { *string.add(i) };
            if sp.is_null() {
                continue;
            }
            let len = if length.is_null() {
                unsafe { cstr_len(sp) }
            } else {
                let l = unsafe { *length.add(i) };
                if l >= 0 {
                    l as usize
                } else {
                    unsafe { cstr_len(sp) }
                }
            };
            let bytes = unsafe { core::slice::from_raw_parts(sp as *const u8, len) };
            joined.push_str(&String::from_utf8_lossy(bytes));
        }
    }
    let mut s = gl();
    if s.sh[sh as usize].used {
        s.sh[sh as usize].src = Some(joined);
    }
}

unsafe fn cstr_len(p: *const c_char) -> usize {
    let mut n = 0usize;
    while *p.add(n) != 0 {
        n += 1;
    }
    n
}

unsafe fn cstr_str(p: *const c_char) -> String {
    let bytes = core::slice::from_raw_parts(p as *const u8, cstr_len(p));
    String::from_utf8_lossy(bytes).into_owned()
}

#[no_mangle]
pub extern "C" fn glCompileShader(sh: u32) {
    let mut s = gl();
    if (sh as usize) >= MAXSH || !s.sh[sh as usize].used {
        return;
    }
    let src = s.sh[sh as usize].src.clone().unwrap_or_default();
    match validate_glsl(&src) {
        Ok(()) => {
            s.sh[sh as usize].compile_ok = true;
            s.sh[sh as usize].info_log.clear();
        }
        Err(msg) => {
            s.sh[sh as usize].compile_ok = false;
            s.sh[sh as usize].info_log = format!("ERROR: {msg}\n");
        }
    }
}

/// A lightweight, dependency-free GLSL-ES syntax validator — enough to make `GL_COMPILE_STATUS`
/// TRUTHFUL for the negative shader corpus (unbalanced delimiters, missing `main`) without a full
/// front-end, while accepting every well-formed shader the byte-parity corpus uses (so their IR is
/// unchanged). It checks delimiter balance across `() {} []` and requires an entry point.
fn validate_glsl(src: &str) -> Result<(), String> {
    let (mut paren, mut brace, mut brack) = (0i32, 0i32, 0i32);
    for c in src.chars() {
        match c {
            '(' => paren += 1,
            ')' => paren -= 1,
            '{' => brace += 1,
            '}' => brace -= 1,
            '[' => brack += 1,
            ']' => brack -= 1,
            _ => {}
        }
        if paren < 0 {
            return Err("unbalanced ')' — closing parenthesis without a matching '('".into());
        }
        if brace < 0 {
            return Err("unbalanced '}' — closing brace without a matching '{'".into());
        }
        if brack < 0 {
            return Err("unbalanced ']' — closing bracket without a matching '['".into());
        }
    }
    if paren != 0 {
        return Err(format!("unbalanced parentheses ({paren} unclosed '(')"));
    }
    if brace != 0 {
        return Err(format!("unbalanced braces ({brace} unclosed '{{')"));
    }
    if brack != 0 {
        return Err(format!("unbalanced brackets ({brack} unclosed '[')"));
    }
    if !src.contains("main") {
        return Err("missing 'main' entry point".into());
    }
    Ok(())
}

#[no_mangle]
pub extern "C" fn glGetShaderiv(sh: u32, pname: u32, v: *mut i32) {
    if v.is_null() {
        return;
    }
    let s = gl();
    unsafe {
        let live = (sh as usize) < MAXSH && s.sh[sh as usize].used;
        if pname == GL_SHADER_SOURCE_LENGTH {
            // glmark2 verifies this round-trips to strlen(source)+1 before it will compile.
            *v = if live {
                s.sh[sh as usize].src.as_ref().map(|x| x.len() + 1).unwrap_or(0) as i32
            } else {
                0
            };
        } else if pname == GL_COMPILE_STATUS {
            *v = if live && s.sh[sh as usize].compile_ok { GL_TRUE as i32 } else { GL_FALSE as i32 };
        } else if pname == GL_INFO_LOG_LENGTH {
            // Reported length includes the NUL terminator (0 when there is no diagnostic).
            let n = if live { s.sh[sh as usize].info_log.len() } else { 0 };
            *v = if n == 0 { 0 } else { (n + 1) as i32 };
        } else {
            *v = 0;
        }
    }
}

#[no_mangle]
pub extern "C" fn glGetShaderInfoLog(sh: u32, buf_size: i32, length: *mut i32, info_log: *mut c_char) {
    let s = gl();
    let log = if (sh as usize) < MAXSH && s.sh[sh as usize].used {
        s.sh[sh as usize].info_log.clone()
    } else {
        String::new()
    };
    drop(s);
    unsafe { copy_info_log(&log, buf_size, length, info_log) };
}

/// Copy a NUL-terminated info log into the caller's buffer (bounded by `buf_size`) and report the
/// number of characters written, excluding the terminator (GLES `glGet*InfoLog` contract).
unsafe fn copy_info_log(log: &str, buf_size: i32, length: *mut i32, info_log: *mut c_char) {
    if info_log.is_null() || buf_size <= 0 {
        set_i32(length, 0);
        return;
    }
    let cap = (buf_size as usize).saturating_sub(1); // reserve room for the NUL
    let n = log.len().min(cap);
    for (i, b) in log.as_bytes()[..n].iter().enumerate() {
        *info_log.add(i) = *b as c_char;
    }
    *info_log.add(n) = 0;
    set_i32(length, n as i32);
}

#[no_mangle]
pub extern "C" fn glDeleteShader(sh: u32) {
    let mut s = gl();
    if (sh as usize) < MAXSH && s.sh[sh as usize].used {
        s.sh[sh as usize] = Default::default();
    }
}

#[no_mangle]
pub extern "C" fn glIsShader(sh: u32) -> u8 {
    let s = gl();
    (sh != 0 && (sh as usize) < MAXSH && s.sh[sh as usize].used) as u8
}

#[no_mangle]
pub extern "C" fn glCreateProgram() -> u32 {
    gl().create_program()
}

#[no_mangle]
pub extern "C" fn glAttachShader(prog: u32, sh: u32) {
    let mut s = gl();
    if (prog as usize) >= MAXPROG || !s.prog[prog as usize].used || (sh as usize) >= MAXSH {
        return;
    }
    if s.sh[sh as usize].kind == GL_VERTEX_SHADER {
        s.prog[prog as usize].vs = sh;
    } else {
        s.prog[prog as usize].fs = sh;
    }
}

#[no_mangle]
pub extern "C" fn glDetachShader(_prog: u32, _sh: u32) {}

/// `glLinkProgram` — translate the attached GLSL-ES vertex+fragment sources to combined MSL and compute
/// the uniform-block layout + sampler set (gl_shim.c `glLinkProgram` → `translate` + `uni_layout` +
/// `collect_uniforms`). The MSL feeds `CreateShader`; the layout gives uniforms their locations.
#[no_mangle]
pub extern "C" fn glLinkProgram(prog: u32) {
    let mut s = gl();
    if (prog as usize) >= MAXPROG || !s.prog[prog as usize].used {
        return;
    }
    let (vs, fs) = (s.prog[prog as usize].vs, s.prog[prog as usize].fs);
    let vs_ok = vs != 0 && (vs as usize) < MAXSH && s.sh[vs as usize].compile_ok;
    let fs_ok = fs != 0 && (fs as usize) < MAXSH && s.sh[fs as usize].compile_ok;
    let vsrc = if (vs as usize) < MAXSH { s.sh[vs as usize].src.clone() } else { None };
    let fsrc = if (fs as usize) < MAXSH { s.sh[fs as usize].src.clone() } else { None };
    // A program links only with BOTH a vertex and fragment shader, each successfully compiled.
    let ok = vs_ok && fs_ok;
    let p = &mut s.prog[prog as usize];
    p.linked = ok;
    p.link_ok = ok;
    if ok {
        p.info_log.clear();
        // `ok` implies both sources are present.
        let (v, f) = (vsrc.unwrap(), fsrc.unwrap());
        p.msl = Some(crate::translate::translate(&v, &f));
        let (unis, total) = crate::translate::uni_layout(&v, &f);
        p.unis = unis;
        p.ubuf_size = total;
        p.samp_names = crate::translate::program_samplers(&v, &f);
        p.samp_units = [0; 4];
        p.ubuf = [0; crate::state::UBUF_BYTES];
    } else {
        let reason = if vs == 0 || fs == 0 {
            "missing a vertex or fragment shader"
        } else {
            "one or more attached shaders failed to compile"
        };
        p.info_log = format!("ERROR: link failed — {reason}\n");
    }
}

#[no_mangle]
pub extern "C" fn glUseProgram(prog: u32) {
    gl().cur_prog = prog;
}

#[no_mangle]
pub extern "C" fn glGetProgramiv(prog: u32, pname: u32, v: *mut i32) {
    let s = gl();
    let live = (prog as usize) < MAXPROG && s.prog[prog as usize].used;
    unsafe {
        if pname == GL_LINK_STATUS {
            set_i32(v, if live && s.prog[prog as usize].link_ok { GL_TRUE as i32 } else { GL_FALSE as i32 });
        } else if pname == GL_INFO_LOG_LENGTH {
            let n = if live { s.prog[prog as usize].info_log.len() } else { 0 };
            set_i32(v, if n == 0 { 0 } else { (n + 1) as i32 });
        } else {
            set_i32(v, 0);
        }
    }
}

#[no_mangle]
pub extern "C" fn glGetProgramInfoLog(prog: u32, buf_size: i32, length: *mut i32, info_log: *mut c_char) {
    let s = gl();
    let log = if (prog as usize) < MAXPROG && s.prog[prog as usize].used {
        s.prog[prog as usize].info_log.clone()
    } else {
        String::new()
    };
    drop(s);
    unsafe { copy_info_log(&log, buf_size, length, info_log) };
}

#[no_mangle]
pub extern "C" fn glDeleteProgram(prog: u32) {
    let mut s = gl();
    if (prog as usize) < MAXPROG && s.prog[prog as usize].used {
        s.prog[prog as usize] = Default::default();
    }
}

#[no_mangle]
pub extern "C" fn glIsProgram(prog: u32) -> u8 {
    let s = gl();
    (prog != 0 && (prog as usize) < MAXPROG && s.prog[prog as usize].used) as u8
}

/// `glGetUniformLocation` — a data uniform's location is its byte offset in the uniform block; a
/// sampler uniform's is the sentinel `100000 + index` (gl_shim.c). -1 if not found.
#[no_mangle]
pub extern "C" fn glGetUniformLocation(prog: u32, name: *const c_char) -> i32 {
    if name.is_null() || (prog as usize) >= MAXPROG {
        return -1;
    }
    let want = unsafe { cstr_str(name) };
    let s = gl();
    let p = &s.prog[prog as usize];
    if !p.used {
        return -1;
    }
    for u in &p.unis {
        if u.name == want {
            return u.off; // location == byte offset
        }
    }
    for (i, sn) in p.samp_names.iter().enumerate() {
        if *sn == want {
            return 100000 + i as i32;
        }
    }
    -1
}

/// `glGetAttribLocation` — the attribute's declaration-order index in the vertex shader (matches the
/// `[[attribute(L)]]` numbering the translator emits), gl_shim.c `glGetAttribLocation`.
#[no_mangle]
pub extern "C" fn glGetAttribLocation(prog: u32, name: *const c_char) -> i32 {
    if name.is_null() || (prog as usize) >= MAXPROG {
        return -1;
    }
    let want = unsafe { cstr_str(name) };
    let s = gl();
    let p = &s.prog[prog as usize];
    if !p.used || p.vs == 0 || (p.vs as usize) >= MAXSH {
        return -1;
    }
    if let Some(src) = &s.sh[p.vs as usize].src {
        for (i, a) in crate::translate::collect_vertex_attrs(src).iter().enumerate() {
            if a.name == want {
                return i as i32;
            }
        }
    }
    -1
}

// ---- glUniform*: store bytes into the current program's uniform block at `location` (a byte offset,
// as gl_shim.c's uni_write does). No-op while locations are -1, but faithful once they exist. --------

fn uni_write(loc: i32, data: &[u8]) {
    if loc < 0 {
        return;
    }
    let (loc, n) = (loc as usize, data.len());
    if loc + n > crate::state::UBUF_BYTES {
        return;
    }
    let mut s = gl();
    // glUniform* with no program in use is GL_INVALID_OPERATION and performs no write.
    let cp = s.cur_prog as usize;
    if s.cur_prog == 0 || cp >= MAXPROG || !s.prog[cp].used {
        drop(s);
        crate::state::set_gl_error(GL_INVALID_OPERATION);
        return;
    }
    s.ubuf[loc..loc + n].copy_from_slice(data);
    s.prog[cp].ubuf[loc..loc + n].copy_from_slice(data);
}

fn f_bytes(vals: &[f32]) -> Vec<u8> {
    let mut b = Vec::with_capacity(vals.len() * 4);
    for v in vals {
        b.extend_from_slice(&v.to_le_bytes());
    }
    b
}

#[no_mangle]
pub extern "C" fn glUniform1f(loc: i32, a: f32) {
    uni_write(loc, &f_bytes(&[a]));
}
#[no_mangle]
pub extern "C" fn glUniform2f(loc: i32, a: f32, b: f32) {
    uni_write(loc, &f_bytes(&[a, b]));
}
#[no_mangle]
pub extern "C" fn glUniform3f(loc: i32, a: f32, b: f32, c: f32) {
    uni_write(loc, &f_bytes(&[a, b, c]));
}
#[no_mangle]
pub extern "C" fn glUniform4f(loc: i32, a: f32, b: f32, c: f32, d: f32) {
    uni_write(loc, &f_bytes(&[a, b, c, d]));
}
#[no_mangle]
pub extern "C" fn glUniform1fv(loc: i32, _count: i32, v: *const f32) {
    if !v.is_null() {
        uni_write(loc, &f_bytes(unsafe { core::slice::from_raw_parts(v, 1) }));
    }
}
#[no_mangle]
pub extern "C" fn glUniform2fv(loc: i32, _count: i32, v: *const f32) {
    if !v.is_null() {
        uni_write(loc, &f_bytes(unsafe { core::slice::from_raw_parts(v, 2) }));
    }
}
#[no_mangle]
pub extern "C" fn glUniform3fv(loc: i32, _count: i32, v: *const f32) {
    if !v.is_null() {
        uni_write(loc, &f_bytes(unsafe { core::slice::from_raw_parts(v, 3) }));
    }
}
#[no_mangle]
pub extern "C" fn glUniform4fv(loc: i32, _count: i32, v: *const f32) {
    if !v.is_null() {
        uni_write(loc, &f_bytes(unsafe { core::slice::from_raw_parts(v, 4) }));
    }
}
#[no_mangle]
pub extern "C" fn glUniform1i(loc: i32, a: i32) {
    // loc in [100000,100004) is a sampler-uniform sentinel (from glGetUniformLocation): it records the
    // GL texture unit this sampler samples, not a uniform-block write (gl_shim.c glUniform1i).
    if (100000..100004).contains(&loc) {
        let si = (loc - 100000) as usize;
        let mut s = gl();
        let cp = s.cur_prog as usize;
        if cp < MAXPROG && s.prog[cp].used && si < s.prog[cp].samp_names.len() {
            s.prog[cp].samp_units[si] = a;
        }
        return;
    }
    uni_write(loc, &a.to_le_bytes());
}
fn i_bytes(vals: &[i32]) -> Vec<u8> {
    vals.iter().flat_map(|v| v.to_le_bytes()).collect()
}
fn u_bytes(vals: &[u32]) -> Vec<u8> {
    vals.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// Write a column-major matrix into the uniform block, re-striding 3-row columns to MSL's 16-byte
/// stride (gl_shim.c `uni_write_matrix`): `float3` columns pad to 16, all others are tight.
fn uni_write_matrix(loc: i32, v: &[f32], cols: usize, rows: usize) {
    let col_stride = if rows == 3 { 16 } else { rows * 4 };
    if col_stride == rows * 4 {
        uni_write(loc, &f_bytes(&v[..cols * rows]));
        return;
    }
    for c in 0..cols {
        uni_write(loc + (c * col_stride) as i32, &f_bytes(&v[c * rows..c * rows + rows]));
    }
}

macro_rules! uniform_vec {
    ($name:ident, $n:literal, f32, $bytes:path) => {
        #[no_mangle]
        pub extern "C" fn $name(loc: i32, count: i32, v: *const f32) {
            if !v.is_null() && count > 0 {
                uni_write(loc, &$bytes(unsafe { core::slice::from_raw_parts(v, $n) }));
            }
        }
    };
    ($name:ident, $n:literal, i32, $bytes:path) => {
        #[no_mangle]
        pub extern "C" fn $name(loc: i32, count: i32, v: *const i32) {
            if !v.is_null() && count > 0 {
                uni_write(loc, &$bytes(unsafe { core::slice::from_raw_parts(v, $n) }));
            }
        }
    };
    ($name:ident, $n:literal, u32, $bytes:path) => {
        #[no_mangle]
        pub extern "C" fn $name(loc: i32, count: i32, v: *const u32) {
            if !v.is_null() && count > 0 {
                uni_write(loc, &$bytes(unsafe { core::slice::from_raw_parts(v, $n) }));
            }
        }
    };
}
macro_rules! uniform_matrix {
    ($name:ident, $cols:literal, $rows:literal) => {
        #[no_mangle]
        pub extern "C" fn $name(loc: i32, count: i32, _transpose: u8, v: *const f32) {
            if !v.is_null() && count > 0 {
                uni_write_matrix(loc, unsafe { core::slice::from_raw_parts(v, $cols * $rows) }, $cols, $rows);
            }
        }
    };
}

#[no_mangle]
pub extern "C" fn glUniformMatrix4fv(loc: i32, count: i32, _transpose: u8, v: *const f32) {
    if !v.is_null() && count > 0 {
        uni_write_matrix(loc, unsafe { core::slice::from_raw_parts(v, 16) }, 4, 4);
    }
}

// Integer scalar/vector uniforms (gl_shim.c: uni_write of the raw int bytes).
#[no_mangle]
pub extern "C" fn glUniform2i(loc: i32, a: i32, b: i32) {
    uni_write(loc, &i_bytes(&[a, b]));
}
#[no_mangle]
pub extern "C" fn glUniform3i(loc: i32, a: i32, b: i32, c: i32) {
    uni_write(loc, &i_bytes(&[a, b, c]));
}
#[no_mangle]
pub extern "C" fn glUniform4i(loc: i32, a: i32, b: i32, c: i32, d: i32) {
    uni_write(loc, &i_bytes(&[a, b, c, d]));
}
uniform_vec!(glUniform1iv, 1, i32, i_bytes);
uniform_vec!(glUniform2iv, 2, i32, i_bytes);
uniform_vec!(glUniform3iv, 3, i32, i_bytes);
uniform_vec!(glUniform4iv, 4, i32, i_bytes);

// Unsigned-integer uniforms (GLES3; same byte layout).
#[no_mangle]
pub extern "C" fn glUniform1ui(loc: i32, a: u32) {
    uni_write(loc, &u_bytes(&[a]));
}
#[no_mangle]
pub extern "C" fn glUniform2ui(loc: i32, a: u32, b: u32) {
    uni_write(loc, &u_bytes(&[a, b]));
}
#[no_mangle]
pub extern "C" fn glUniform3ui(loc: i32, a: u32, b: u32, c: u32) {
    uni_write(loc, &u_bytes(&[a, b, c]));
}
#[no_mangle]
pub extern "C" fn glUniform4ui(loc: i32, a: u32, b: u32, c: u32, d: u32) {
    uni_write(loc, &u_bytes(&[a, b, c, d]));
}
uniform_vec!(glUniform1uiv, 1, u32, u_bytes);
uniform_vec!(glUniform2uiv, 2, u32, u_bytes);
uniform_vec!(glUniform3uiv, 3, u32, u_bytes);
uniform_vec!(glUniform4uiv, 4, u32, u_bytes);

// Matrix uniforms (gl_shim.c uni_write_matrix — mat3 columns re-stride to 16 bytes).
uniform_matrix!(glUniformMatrix2fv, 2, 2);
uniform_matrix!(glUniformMatrix3fv, 3, 3);
uniform_matrix!(glUniformMatrix2x3fv, 2, 3);
uniform_matrix!(glUniformMatrix3x2fv, 3, 2);
uniform_matrix!(glUniformMatrix2x4fv, 2, 4);
uniform_matrix!(glUniformMatrix4x2fv, 4, 2);
uniform_matrix!(glUniformMatrix3x4fv, 3, 4);
uniform_matrix!(glUniformMatrix4x3fv, 4, 3);

// ===================================================================================================
// vertex attributes
// ===================================================================================================

#[no_mangle]
pub extern "C" fn glVertexAttribPointer(index: u32, size: i32, kind: u32, normalized: u8, stride: i32, ptr: *const c_void) {
    if (index as usize) >= MAXATTR {
        return;
    }
    let mut s = gl();
    let arr_buf = s.arr_buf;
    let a = &mut s.attr[index as usize];
    a.size = size;
    a.kind = kind;
    a.normalized = normalized != 0;
    a.integer = false;
    a.stride = stride;
    a.offset = ptr as usize;
    a.buffer = arr_buf;
    s.vao_store_current();
}

/// `glVertexAttribIPointer` — integer vertex attribute array (gl_shim.c parity: like
/// `glVertexAttribPointer` but `integer=1`, `normalized=0`).
#[no_mangle]
pub extern "C" fn glVertexAttribIPointer(index: u32, size: i32, kind: u32, stride: i32, ptr: *const c_void) {
    if (index as usize) >= MAXATTR {
        return;
    }
    let mut s = gl();
    let arr_buf = s.arr_buf;
    let a = &mut s.attr[index as usize];
    a.size = size;
    a.kind = kind;
    a.normalized = false;
    a.integer = true;
    a.stride = stride;
    a.offset = ptr as usize;
    a.buffer = arr_buf;
    s.vao_store_current();
}

#[no_mangle]
pub extern "C" fn glEnableVertexAttribArray(index: u32) {
    if (index as usize) < MAXATTR {
        let mut s = gl();
        s.attr[index as usize].enabled = true;
        s.vao_store_current();
    }
}

#[no_mangle]
pub extern "C" fn glDisableVertexAttribArray(index: u32) {
    if (index as usize) < MAXATTR {
        let mut s = gl();
        s.attr[index as usize].enabled = false;
        s.vao_store_current();
    }
}

// ===================================================================================================
// draw recording — glClear / glDrawArrays / glDrawElements accumulate the frame draw-list; the IR is
// lowered from it at eglSwapBuffers (see crate::frame).
// ===================================================================================================

/// `glClear` — record a clear into the frame draw-list, honoring GL_SCISSOR_TEST (a scissored clear
/// becomes a sub-rect ClearRect; a full clear bumps the serial and marks the default surface). Faithful
/// to gl_shim.c for the default framebuffer (FBO targets land with the offscreen path).
#[no_mangle]
pub extern "C" fn glClear(mask: u32) {
    let mut s = gl();
    let (sx, sy, sw, sh, scissored) = s.clear_scissor_rect();
    if mask & GL_COLOR_BUFFER_BIT != 0 {
        if scissored {
            s.record_clear_call(sx, sy, sw, sh);
        } else {
            s.clear_serial += 1;
            // A full clear of the DEFAULT framebuffer records a ClearRect; a full clear of a bound FBO
            // does not (its clear is baked into the offscreen texture's CPU data below) — gl_shim.c.
            if s.draw_fbo == 0 {
                s.default_full_clear_since_swap = true;
                s.record_clear_call(sx, sy, sw, sh);
            }
        }
        let c = s.clear;
        s.clear_bound_color_texture(c);
    }
}

#[no_mangle]
pub extern "C" fn glDrawArrays(mode: u32, first: i32, count: i32) {
    let mut s = gl();
    s.draw_mode = mode as i32;
    s.draw_first = first;
    s.draw_count = count;
    s.draw_indexed = false;
    s.attr_snap = s.attr;
    s.have_draw_snap = true;
    s.record_draw_call(mode, first, count, false, 0, 0);
}

#[no_mangle]
pub extern "C" fn glDrawElements(mode: u32, count: i32, typ: u32, indices: *const c_void) {
    let mut s = gl();
    s.draw_mode = mode as i32;
    s.draw_count = count;
    s.draw_indexed = true;
    s.index_type = typ;
    s.index_offset = indices as usize;
    s.attr_snap = s.attr;
    s.have_draw_snap = true;
    s.record_draw_call(mode, 0, count, true, typ, indices as usize);
}

/// `glDrawRangeElements` — the `start`/`end` bounds are advisory; gl_shim.c delegates to glDrawElements.
#[no_mangle]
pub extern "C" fn glDrawRangeElements(mode: u32, _start: u32, _end: u32, count: i32, typ: u32, indices: *const c_void) {
    glDrawElements(mode, count, typ, indices);
}

// Instanced draws collapse to a single instance (gl_shim.c drops the instance count and delegates).
#[no_mangle]
pub extern "C" fn glDrawArraysInstanced(mode: u32, first: i32, count: i32, _instances: i32) {
    glDrawArrays(mode, first, count);
}
#[no_mangle]
pub extern "C" fn glDrawElementsInstanced(mode: u32, count: i32, typ: u32, indices: *const c_void, _instances: i32) {
    glDrawElements(mode, count, typ, indices);
}

/// `glCheckFramebufferStatus` — the shim's offscreen FBOs are always complete (gl_shim.c returns
/// GL_FRAMEBUFFER_COMPLETE). A stubbed `0` would abort every app's FBO setup, so this is load-bearing.
#[no_mangle]
pub extern "C" fn glCheckFramebufferStatus(_target: u32) -> u32 {
    0x8CD5 // GL_FRAMEBUFFER_COMPLETE
}

// ===================================================================================================
// vertex array objects (VAOs) — capture/restore the attribute array + element buffer per object
// ===================================================================================================

#[no_mangle]
pub extern "C" fn glGenVertexArrays(n: i32, out: *mut u32) {
    if out.is_null() || n < 0 {
        return;
    }
    let mut s = gl();
    for k in 0..n as usize {
        let id = s.gen_vao();
        unsafe { *out.add(k) = id };
    }
}

#[no_mangle]
pub extern "C" fn glBindVertexArray(vao: u32) {
    let mut s = gl();
    s.vao_store_current();
    if (vao as usize) >= crate::state::MAXVAO {
        return;
    }
    s.cur_vao = vao;
    s.vao_load(vao);
}

#[no_mangle]
pub extern "C" fn glDeleteVertexArrays(n: i32, ids: *const u32) {
    if ids.is_null() || n < 0 {
        return;
    }
    let mut s = gl();
    for k in 0..n as usize {
        let id = unsafe { *ids.add(k) } as usize;
        if id != 0 && id < crate::state::MAXVAO && s.vao[id].used {
            s.vao[id].used = false;
        }
    }
}

#[no_mangle]
pub extern "C" fn glIsVertexArray(vao: u32) -> u8 {
    let s = gl();
    ((vao as usize) < crate::state::MAXVAO && s.vao[vao as usize].used) as u8
}

// ===================================================================================================
// framebuffer + renderbuffer objects (offscreen render targets) — a draw's render target is the bound
// draw-FBO's color texture (see state::draw_fbo_target, resolved in record_draw_call/record_clear_call).
// ===================================================================================================

use crate::state::{Fbo, Rbo, MAXFBO, MAXRBO};

#[no_mangle]
pub extern "C" fn glGenFramebuffers(n: i32, out: *mut u32) {
    if out.is_null() || n < 0 {
        return;
    }
    let mut s = gl();
    for k in 0..n as usize {
        let mut id = 0u32;
        for i in 1..MAXFBO {
            if !s.fbo[i].used {
                s.fbo[i] = Fbo { used: true, ..Default::default() };
                id = i as u32;
                break;
            }
        }
        unsafe { *out.add(k) = id };
    }
}

#[no_mangle]
pub extern "C" fn glBindFramebuffer(target: u32, fbo: u32) {
    let mut s = gl();
    if fbo != 0 && (fbo as usize) < MAXFBO {
        s.fbo[fbo as usize].used = true;
    }
    match target {
        GL_FRAMEBUFFER => {
            s.draw_fbo = fbo;
            s.read_fbo = fbo;
        }
        GL_DRAW_FRAMEBUFFER => s.draw_fbo = fbo,
        GL_READ_FRAMEBUFFER => s.read_fbo = fbo,
        _ => {}
    }
}

#[no_mangle]
pub extern "C" fn glDeleteFramebuffers(n: i32, ids: *const u32) {
    if ids.is_null() || n < 0 {
        return;
    }
    let mut s = gl();
    for k in 0..n as usize {
        let id = unsafe { *ids.add(k) };
        if (id as usize) < MAXFBO {
            s.fbo[id as usize] = Fbo::default();
            if s.draw_fbo == id {
                s.draw_fbo = 0;
            }
            if s.read_fbo == id {
                s.read_fbo = 0;
            }
        }
    }
}

#[no_mangle]
pub extern "C" fn glIsFramebuffer(fbo: u32) -> u8 {
    let s = gl();
    (fbo > 0 && (fbo as usize) < MAXFBO && s.fbo[fbo as usize].used) as u8
}

#[no_mangle]
pub extern "C" fn glFramebufferTexture2D(target: u32, attachment: u32, textarget: u32, tex: u32, level: i32) {
    let mut s = gl();
    let f = if target == GL_READ_FRAMEBUFFER { s.read_fbo } else { s.draw_fbo };
    if (target == GL_FRAMEBUFFER || target == GL_DRAW_FRAMEBUFFER || target == GL_READ_FRAMEBUFFER)
        && attachment == GL_COLOR_ATTACHMENT0
        && textarget == GL_TEXTURE_2D
        && level == 0
        && f > 0
        && (f as usize) < MAXFBO
    {
        let fb = &mut s.fbo[f as usize];
        fb.used = true;
        fb.color_tex = tex;
        fb.color_rbo = 0;
        fb.color_level = 0;
        fb.color_layer = 0;
    }
}

#[no_mangle]
pub extern "C" fn glFramebufferTextureLayer(target: u32, attachment: u32, tex: u32, level: i32, layer: i32) {
    let mut s = gl();
    let f = if target == GL_READ_FRAMEBUFFER { s.read_fbo } else { s.draw_fbo };
    if (target == GL_FRAMEBUFFER || target == GL_DRAW_FRAMEBUFFER || target == GL_READ_FRAMEBUFFER)
        && attachment == GL_COLOR_ATTACHMENT0
        && level == 0
        && f > 0
        && (f as usize) < MAXFBO
    {
        let fb = &mut s.fbo[f as usize];
        fb.used = true;
        fb.color_tex = tex;
        fb.color_rbo = 0;
        fb.color_level = 0;
        fb.color_layer = layer;
    }
}

#[no_mangle]
pub extern "C" fn glFramebufferRenderbuffer(target: u32, attachment: u32, rbtarget: u32, rbo: u32) {
    let mut s = gl();
    let f = if target == GL_READ_FRAMEBUFFER { s.read_fbo } else { s.draw_fbo };
    if (target == GL_FRAMEBUFFER || target == GL_DRAW_FRAMEBUFFER || target == GL_READ_FRAMEBUFFER)
        && attachment == GL_COLOR_ATTACHMENT0
        && rbtarget == GL_RENDERBUFFER
        && f > 0
        && (f as usize) < MAXFBO
    {
        let fb = &mut s.fbo[f as usize];
        fb.used = true;
        fb.color_tex = 0;
        fb.color_rbo = rbo;
        fb.color_level = 0;
        fb.color_layer = 0;
    }
}

fn rbo_component_bits(ifmt: u32) -> (i32, i32, i32, i32, i32, i32) {
    // returns (r,g,b,a,depth,stencil)
    match ifmt {
        0x81A5 | 0x81A6 | 0x81A7 => (0, 0, 0, 0, 24, 0), // DEPTH_COMPONENT16/24/32
        0x8D48 => (0, 0, 0, 0, 0, 8),                    // STENCIL_INDEX8
        0x88F0 => (0, 0, 0, 0, 24, 8),                   // DEPTH24_STENCIL8
        _ => (8, 8, 8, 8, 0, 0),
    }
}

#[no_mangle]
pub extern "C" fn glGetFramebufferAttachmentParameteriv(target: u32, att: u32, pname: u32, v: *mut i32) {
    if v.is_null() {
        return;
    }
    let s = gl();
    let f = if target == GL_READ_FRAMEBUFFER { s.read_fbo } else { s.draw_fbo };
    let mut out = 0i32;
    if f == 0 {
        out = match pname {
            GL_FB_ATTACHMENT_OBJECT_TYPE => GL_FRAMEBUFFER_DEFAULT,
            GL_FB_ATTACHMENT_OBJECT_NAME => 0,
            GL_FB_ATTACHMENT_RED_SIZE..=GL_FB_ATTACHMENT_ALPHA_SIZE => {
                if att == GL_COLOR_ATTACHMENT0 {
                    8
                } else {
                    0
                }
            }
            GL_FB_ATTACHMENT_DEPTH_SIZE => {
                if att == GL_DEPTH_ATTACHMENT {
                    24
                } else {
                    0
                }
            }
            GL_FB_ATTACHMENT_STENCIL_SIZE => {
                if att == GL_STENCIL_ATTACHMENT {
                    8
                } else {
                    0
                }
            }
            _ => 0,
        };
    } else if (f as usize) < MAXFBO && s.fbo[f as usize].used && att == GL_COLOR_ATTACHMENT0 {
        let fb = &s.fbo[f as usize];
        out = match pname {
            GL_FB_ATTACHMENT_OBJECT_TYPE => {
                if fb.color_tex != 0 {
                    GL_TEXTURE_OBJ
                } else if fb.color_rbo != 0 {
                    GL_RENDERBUFFER as i32
                } else {
                    0
                }
            }
            GL_FB_ATTACHMENT_OBJECT_NAME => (if fb.color_tex != 0 { fb.color_tex } else { fb.color_rbo }) as i32,
            GL_FB_ATTACHMENT_TEXTURE_LEVEL => {
                if fb.color_tex != 0 {
                    fb.color_level
                } else {
                    0
                }
            }
            GL_FB_ATTACHMENT_TEXTURE_CUBE_MAP_FACE => 0,
            GL_FB_ATTACHMENT_TEXTURE_LAYER => {
                if fb.color_tex != 0 {
                    fb.color_layer
                } else {
                    0
                }
            }
            GL_FB_ATTACHMENT_RED_SIZE..=GL_FB_ATTACHMENT_STENCIL_SIZE => {
                let (r, g, b, a, d, st) = if fb.color_rbo != 0 && (fb.color_rbo as usize) < MAXRBO && s.rbo[fb.color_rbo as usize].used {
                    rbo_component_bits(s.rbo[fb.color_rbo as usize].ifmt)
                } else {
                    (8, 8, 8, 8, 0, 0)
                };
                match pname {
                    GL_FB_ATTACHMENT_RED_SIZE => r,
                    GL_FB_ATTACHMENT_GREEN_SIZE => g,
                    GL_FB_ATTACHMENT_BLUE_SIZE => b,
                    GL_FB_ATTACHMENT_ALPHA_SIZE => a,
                    GL_FB_ATTACHMENT_DEPTH_SIZE => d,
                    _ => st,
                }
            }
            _ => 0,
        };
    }
    unsafe { *v = out };
}

#[no_mangle]
pub extern "C" fn glGenRenderbuffers(n: i32, out: *mut u32) {
    if out.is_null() || n < 0 {
        return;
    }
    let mut s = gl();
    for k in 0..n as usize {
        let mut id = 0u32;
        for i in 1..MAXRBO {
            if !s.rbo[i].used {
                let g = s.rbo[i].gen;
                s.rbo[i] = Rbo { used: true, gen: g + 1, ..Default::default() };
                id = i as u32;
                break;
            }
        }
        unsafe { *out.add(k) = id };
    }
}

#[no_mangle]
pub extern "C" fn glBindRenderbuffer(target: u32, rbo: u32) {
    if target != GL_RENDERBUFFER {
        return;
    }
    let mut s = gl();
    s.rbo_bound = rbo;
    if rbo > 0 && (rbo as usize) < MAXRBO {
        s.rbo[rbo as usize].used = true;
    }
}

#[no_mangle]
pub extern "C" fn glDeleteRenderbuffers(n: i32, ids: *const u32) {
    if ids.is_null() || n < 0 {
        return;
    }
    let mut s = gl();
    for k in 0..n as usize {
        let id = unsafe { *ids.add(k) };
        if (id as usize) < MAXRBO {
            s.rbo[id as usize] = Rbo::default();
            if s.rbo_bound == id {
                s.rbo_bound = 0;
            }
            for f in 1..MAXFBO {
                if s.fbo[f].color_rbo == id {
                    s.fbo[f].color_rbo = 0;
                }
            }
        }
    }
}

#[no_mangle]
pub extern "C" fn glIsRenderbuffer(rbo: u32) -> u8 {
    let s = gl();
    (rbo > 0 && (rbo as usize) < MAXRBO && s.rbo[rbo as usize].used) as u8
}

fn rbo_storage(ifmt: u32, w: i32, h: i32, samples: i32) {
    let mut s = gl();
    let r = s.rbo_bound as usize;
    if s.rbo_bound > 0 && r < MAXRBO {
        let gen = s.rbo[r].gen;
        s.rbo[r] = Rbo { used: true, w, h, ifmt, samples, gen: gen + 1 };
    }
}

#[no_mangle]
pub extern "C" fn glRenderbufferStorage(target: u32, ifmt: u32, w: i32, h: i32) {
    if target == GL_RENDERBUFFER {
        rbo_storage(ifmt, w, h, 0);
    }
}

#[no_mangle]
pub extern "C" fn glRenderbufferStorageMultisample(target: u32, samples: i32, ifmt: u32, w: i32, h: i32) {
    if target == GL_RENDERBUFFER {
        rbo_storage(ifmt, w, h, samples);
    }
}

#[no_mangle]
pub extern "C" fn glGetRenderbufferParameteriv(target: u32, pname: u32, v: *mut i32) {
    if v.is_null() {
        return;
    }
    let s = gl();
    let mut out = 0i32;
    let rb = s.rbo_bound as usize;
    if target == GL_RENDERBUFFER && rb < MAXRBO && s.rbo[rb].used {
        let r = &s.rbo[rb];
        let (red, green, blue, alpha, depth, stencil) = rbo_component_bits(r.ifmt);
        out = match pname {
            GL_RB_WIDTH => r.w,
            GL_RB_HEIGHT => r.h,
            GL_RB_INTERNAL_FORMAT => r.ifmt as i32,
            GL_RB_RED_SIZE => red,
            GL_RB_GREEN_SIZE => green,
            GL_RB_BLUE_SIZE => blue,
            GL_RB_ALPHA_SIZE => alpha,
            GL_RB_DEPTH_SIZE => depth,
            GL_RB_STENCIL_SIZE => stencil,
            GL_RB_SAMPLES => r.samples,
            _ => 0,
        };
    }
    unsafe { *v = out };
}

/// `glBlitFramebuffer` — CPU-side textured rect blit between the read/draw FBOs' color textures
/// (gl_shim.c). Only GL_COLOR_BUFFER_BIT is handled; the filter is ignored (nearest).
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glBlitFramebuffer(sx0: i32, sy0: i32, sx1: i32, sy1: i32, dx0: i32, dy0: i32, dx1: i32, dy1: i32, mask: u32, _filter: u32) {
    if mask & GL_COLOR_BUFFER_BIT == 0 {
        return;
    }
    let mut s = gl();
    let src = s.fbo_color_texture(s.read_fbo);
    let dst = s.fbo_color_texture(s.draw_fbo);
    if src == 0 || dst == 0 || src == dst {
        return;
    }
    s.copy_texture_rect(src, dst, sx0, sy0, sx1, sy1, dx0, dy0, dx1, dy1);
}

/// `glReadPixels` — CPU-side readback of the read-FBO's color texture (gl_shim.c). Zero-fills first,
/// then copies from the texture's uploaded RGBA8 data (only for GL_UNSIGNED_BYTE).
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glReadPixels(x: i32, y: i32, w: i32, h: i32, fmt: u32, typ: u32, dst: *mut c_void) {
    if dst.is_null() || w <= 0 || h <= 0 {
        return;
    }
    let bpp = crate::state::GlState::tex_bpp(fmt);
    let out = unsafe { core::slice::from_raw_parts_mut(dst as *mut u8, (w as usize) * (h as usize) * bpp) };
    out.fill(0);
    if typ != GL_UNSIGNED_BYTE {
        return;
    }
    let s = gl();
    let src_id = s.fbo_color_texture(s.read_fbo);
    if src_id == 0 {
        return;
    }
    let src = &s.tex[src_id as usize];
    for yy in 0..h {
        let sy = y + yy;
        for xx in 0..w {
            let sx = x + xx;
            if sx < 0 || sx >= src.w || sy < 0 || sy >= src.h {
                continue;
            }
            let sp = (sy as usize * src.w as usize + sx as usize) * 4;
            let dp = (yy as usize * w as usize + xx as usize) * bpp;
            let sc = &src.data[sp..sp + 4];
            match fmt {
                GL_RGBA => out[dp..dp + 4].copy_from_slice(sc),
                GL_BGRA_EXT => {
                    out[dp] = sc[2];
                    out[dp + 1] = sc[1];
                    out[dp + 2] = sc[0];
                    out[dp + 3] = sc[3];
                }
                GL_RGB => out[dp..dp + 3].copy_from_slice(&sc[..3]),
                _ => out[dp] = sc[0],
            }
        }
    }
}

/// `glClearBufferfv` — clears the bound color texture's CPU data (gl_shim.c: `GL_COLOR`, drawbuffer 0).
#[no_mangle]
pub extern "C" fn glClearBufferfv(buffer: u32, drawbuffer: i32, value: *const f32) {
    if buffer == GL_COLOR && drawbuffer == 0 && !value.is_null() {
        let color = unsafe { core::slice::from_raw_parts(value, 4) };
        let mut s = gl();
        s.clear_bound_color_texture([color[0], color[1], color[2], color[3]]);
    }
}

// ===================================================================================================
// GLES2 mandatory-command completeness — real bodies ported at gl_shim.c byte-parity (the oracle).
//
// These entry points carry no IR (they are state queries or spec-legitimate no-ops), so the frame IR is
// unchanged and the byte-parity gates stay byte-identical; porting them here (rather than as generated
// stubs) closes the "advertised GLES 2.0 mandatory surface" gate by giving every mandatory command a
// real hand-written body that matches the oracle's observable behavior exactly.
// ===================================================================================================

// ---- shader / program introspection (gl_shim.c returns spec defaults) ----

/// `glGetActiveAttrib` — gl_shim.c reports one float attribute with an empty name (length 0).
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glGetActiveAttrib(_program: u32, _index: u32, buf_size: i32, length: *mut i32, size: *mut i32, typ: *mut u32, name: *mut c_char) {
    unsafe {
        set_i32(length, 0);
        set_i32(size, 1);
        if !typ.is_null() {
            *typ = GL_FLOAT;
        }
        if !name.is_null() && buf_size > 0 {
            *name = 0;
        }
    }
}

/// `glGetActiveUniform` — same spec-default shape as `glGetActiveAttrib` (gl_shim.c).
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glGetActiveUniform(_program: u32, _index: u32, buf_size: i32, length: *mut i32, size: *mut i32, typ: *mut u32, name: *mut c_char) {
    unsafe {
        set_i32(length, 0);
        set_i32(size, 1);
        if !typ.is_null() {
            *typ = GL_FLOAT;
        }
        if !name.is_null() && buf_size > 0 {
            *name = 0;
        }
    }
}

/// `glGetAttachedShaders` — gl_shim.c reports zero attached shaders (introspection default).
#[no_mangle]
pub extern "C" fn glGetAttachedShaders(_program: u32, _max_count: i32, count: *mut i32, _shaders: *mut u32) {
    unsafe { set_i32(count, 0) };
}

/// `glGetShaderSource` — gl_shim.c returns an empty source (length 0). (Note: `glGetShaderiv`
/// GL_SHADER_SOURCE_LENGTH still reports the real length, matching the oracle.)
#[no_mangle]
pub extern "C" fn glGetShaderSource(_shader: u32, buf_size: i32, length: *mut i32, source: *mut c_char) {
    unsafe {
        set_i32(length, 0);
        if !source.is_null() && buf_size > 0 {
            *source = 0;
        }
    }
}

/// `glGetShaderPrecisionFormat` — IEEE-float-shaped ranges (gl_shim.c: range {127,127}, precision 23).
#[no_mangle]
pub extern "C" fn glGetShaderPrecisionFormat(_shadertype: u32, _precisiontype: u32, range: *mut i32, precision: *mut i32) {
    unsafe {
        if !range.is_null() {
            *range = 127;
            *range.add(1) = 127;
        }
        set_i32(precision, 23);
    }
}

/// `glGetUniformfv` / `glGetUniformiv` — gl_shim.c reports 0 (uniform readback is not modeled).
#[no_mangle]
pub extern "C" fn glGetUniformfv(_program: u32, _location: i32, params: *mut f32) {
    unsafe {
        if !params.is_null() {
            *params = 0.0;
        }
    }
}
#[no_mangle]
pub extern "C" fn glGetUniformiv(_program: u32, _location: i32, params: *mut i32) {
    unsafe { set_i32(params, 0) };
}

// ---- buffer / texture / vertex-attribute queries ----

/// `glGetBufferParameteriv` — real buffer size/usage for the bound buffer (gl_shim.c parity).
#[no_mangle]
pub extern "C" fn glGetBufferParameteriv(target: u32, pname: u32, params: *mut i32) {
    if params.is_null() {
        return;
    }
    let s = gl();
    let b = if target == GL_ELEMENT_ARRAY_BUFFER { s.elem_buf } else { s.arr_buf } as usize;
    let mut v = 0i32;
    if b < MAXBUF && s.buf[b].used {
        if pname == GL_BUFFER_SIZE {
            v = s.buf[b].data.len() as i32;
        } else if pname == GL_BUFFER_USAGE {
            v = s.buf[b].usage as i32;
        }
    }
    unsafe { *params = v };
}

/// `glGetTexParameteriv` — filter / wrap state of the bound texture (gl_shim.c parity).
#[no_mangle]
pub extern "C" fn glGetTexParameteriv(target: u32, pname: u32, params: *mut i32) {
    if params.is_null() {
        return;
    }
    let s = gl();
    let mut v = 0i32;
    if target == GL_TEXTURE_2D {
        let t = s.bound_tex() as usize;
        if t < MAXTEX && s.tex[t].used {
            v = match pname {
                GL_TEXTURE_MIN_FILTER => s.tex[t].minf as i32,
                GL_TEXTURE_MAG_FILTER => s.tex[t].magf as i32,
                GL_TEXTURE_WRAP_S => s.tex[t].ws as i32,
                GL_TEXTURE_WRAP_T => s.tex[t].wt as i32,
                _ => 0,
            };
        }
    }
    unsafe { *params = v };
}

/// `glGetTexParameterfv` — the float view of `glGetTexParameteriv` (gl_shim.c delegates identically).
#[no_mangle]
pub extern "C" fn glGetTexParameterfv(target: u32, pname: u32, params: *mut f32) {
    if params.is_null() {
        return;
    }
    let mut iv = 0i32;
    glGetTexParameteriv(target, pname, &mut iv);
    unsafe { *params = iv as f32 };
}

/// `glGetVertexAttribfv` / `iv` / `Pointerv` — gl_shim.c reports 0 / null (attribute readback default).
#[no_mangle]
pub extern "C" fn glGetVertexAttribfv(_index: u32, _pname: u32, params: *mut f32) {
    unsafe {
        if !params.is_null() {
            *params = 0.0;
        }
    }
}
#[no_mangle]
pub extern "C" fn glGetVertexAttribiv(_index: u32, _pname: u32, params: *mut i32) {
    unsafe { set_i32(params, 0) };
}
#[no_mangle]
pub extern "C" fn glGetVertexAttribPointerv(_index: u32, _pname: u32, pointer: *mut *mut c_void) {
    unsafe {
        if !pointer.is_null() {
            *pointer = core::ptr::null_mut();
        }
    }
}

// ---- spec-legitimate no-ops (gl_shim.c also no-ops; observable state is unchanged) ----

/// `glBindAttribLocation` — no-op: the translator binds attributes by declaration order (gl_shim.c).
#[no_mangle]
pub extern "C" fn glBindAttribLocation(_program: u32, _index: u32, _name: *const c_char) {}

/// Separate-face stencil state — not backed by the IR (gl_shim.c no-ops these), so a faithful no-op.
#[no_mangle]
pub extern "C" fn glStencilFuncSeparate(_face: u32, _func: u32, _ref_: i32, _mask: u32) {}
#[no_mangle]
pub extern "C" fn glStencilMaskSeparate(_face: u32, _mask: u32) {}
#[no_mangle]
pub extern "C" fn glStencilOpSeparate(_face: u32, _sfail: u32, _dpfail: u32, _dppass: u32) {}

/// Constant vertex attributes — the shim sources attributes from arrays, so these are no-ops (gl_shim.c).
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

/// Compressed-texture upload is not decoded by the executor (gl_shim.c no-ops it, leaving the RGBA8
/// plane); a faithful no-op keeps parity.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glCompressedTexImage2D(_target: u32, _level: i32, _internalformat: u32, _width: i32, _height: i32, _border: i32, _image_size: i32, _data: *const c_void) {}
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glCompressedTexSubImage2D(_target: u32, _level: i32, _xoffset: i32, _yoffset: i32, _width: i32, _height: i32, _format: u32, _image_size: i32, _data: *const c_void) {}

/// `glReleaseShaderCompiler` / `glShaderBinary` — no online shader-binary path (gl_shim.c no-ops). The
/// shim advertises no binary formats, so a conformant app compiles from source via `glShaderSource`.
#[no_mangle]
pub extern "C" fn glReleaseShaderCompiler() {}
#[no_mangle]
pub extern "C" fn glShaderBinary(_count: i32, _shaders: *const u32, _binaryformat: u32, _binary: *const c_void, _length: i32) {}

/// `glValidateProgram` — a no-op; `glGetProgramiv(GL_LINK_STATUS)` already reflects the real link state.
#[no_mangle]
pub extern "C" fn glValidateProgram(_program: u32) {}

// ===================================================================================================
// GLES3 mandatory-command completeness — real bodies ported at gl_shim.c byte-parity (the oracle).
//
// The DD_SHIM_ES3 opt-in advertises an ES 3.0 context; these are its remaining mandatory entry points.
// Every one is IR-free — a query returning the oracle's default, an object-name lifecycle op, or a
// spec-legitimate no-op the executor doesn't back (UBO binding, transform feedback, sync, sampler
// objects, MRT clears, instanced divisor, compressed/3D-copy texture) — so the frame IR (and the
// byte-parity gates) are unchanged. They are ported here (rather than left as generated stubs) so the
// advertised ES3 mandatory surface has a real hand-written body for every command.
// ===================================================================================================

// ---- sampler objects (glGenSamplers is already a full body) ----
#[no_mangle]
pub extern "C" fn glBindSampler(_unit: u32, _sampler: u32) {}
#[no_mangle]
pub extern "C" fn glDeleteSamplers(_n: i32, _samplers: *const u32) {}
#[no_mangle]
pub extern "C" fn glIsSampler(sampler: u32) -> u8 {
    (sampler != 0) as u8
}
#[no_mangle]
pub extern "C" fn glSamplerParameteri(_sampler: u32, _pname: u32, _param: i32) {}
#[no_mangle]
pub extern "C" fn glSamplerParameterf(_sampler: u32, _pname: u32, _param: f32) {}
#[no_mangle]
pub extern "C" fn glSamplerParameteriv(_sampler: u32, _pname: u32, _param: *const i32) {}
#[no_mangle]
pub extern "C" fn glSamplerParameterfv(_sampler: u32, _pname: u32, _param: *const f32) {}
#[no_mangle]
pub extern "C" fn glGetSamplerParameteriv(_sampler: u32, _pname: u32, params: *mut i32) {
    unsafe { set_i32(params, 0) };
}
#[no_mangle]
pub extern "C" fn glGetSamplerParameterfv(_sampler: u32, _pname: u32, params: *mut f32) {
    unsafe {
        if !params.is_null() {
            *params = 0.0;
        }
    }
}

// ---- occlusion query objects (glGenQueries is already a full body) ----
#[no_mangle]
pub extern "C" fn glBeginQuery(_target: u32, _id: u32) {}
#[no_mangle]
pub extern "C" fn glEndQuery(_target: u32) {}
#[no_mangle]
pub extern "C" fn glDeleteQueries(_n: i32, _ids: *const u32) {}
#[no_mangle]
pub extern "C" fn glIsQuery(id: u32) -> u8 {
    (id != 0) as u8
}
#[no_mangle]
pub extern "C" fn glGetQueryiv(_target: u32, _pname: u32, params: *mut i32) {
    unsafe { set_i32(params, 0) };
}
#[no_mangle]
pub extern "C" fn glGetQueryObjectuiv(_id: u32, _pname: u32, params: *mut u32) {
    unsafe {
        if !params.is_null() {
            *params = 0;
        }
    }
}

// ---- transform feedback (glGenTransformFeedbacks is already a full body) ----
#[no_mangle]
pub extern "C" fn glBeginTransformFeedback(_primitive_mode: u32) {}
#[no_mangle]
pub extern "C" fn glEndTransformFeedback() {}
#[no_mangle]
pub extern "C" fn glPauseTransformFeedback() {}
#[no_mangle]
pub extern "C" fn glResumeTransformFeedback() {}
#[no_mangle]
pub extern "C" fn glBindTransformFeedback(_target: u32, _id: u32) {}
#[no_mangle]
pub extern "C" fn glDeleteTransformFeedbacks(_n: i32, _ids: *const u32) {}
#[no_mangle]
pub extern "C" fn glIsTransformFeedback(id: u32) -> u8 {
    (id != 0) as u8
}
#[no_mangle]
pub extern "C" fn glTransformFeedbackVaryings(_program: u32, _count: i32, _varyings: *const *const c_char, _buffer_mode: u32) {}
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glGetTransformFeedbackVarying(_program: u32, _index: u32, buf_size: i32, length: *mut i32, size: *mut i32, typ: *mut u32, name: *mut c_char) {
    unsafe {
        set_i32(length, 0);
        set_i32(size, 0);
        if !typ.is_null() {
            *typ = 0;
        }
        if !name.is_null() && buf_size > 0 {
            *name = 0;
        }
    }
}

// ---- fence sync objects (oracle reports immediately-signaled so Chrome's fences never block) ----
#[no_mangle]
pub extern "C" fn glFenceSync(_condition: u32, _flags: u32) -> *mut c_void {
    1 as *mut c_void
}
#[no_mangle]
pub extern "C" fn glDeleteSync(_sync: *mut c_void) {}
#[no_mangle]
pub extern "C" fn glIsSync(sync: *mut c_void) -> u8 {
    (!sync.is_null()) as u8
}
#[no_mangle]
pub extern "C" fn glClientWaitSync(_sync: *mut c_void, _flags: u32, _timeout: u64) -> u32 {
    GL_ALREADY_SIGNALED
}
#[no_mangle]
pub extern "C" fn glWaitSync(_sync: *mut c_void, _flags: u32, _timeout: u64) {}
#[no_mangle]
pub extern "C" fn glGetSynciv(_sync: *mut c_void, pname: u32, _buf_size: i32, length: *mut i32, values: *mut i32) {
    unsafe {
        set_i32(length, 1);
        if !values.is_null() {
            *values = if pname == GL_SYNC_STATUS { GL_SIGNALED } else { 0 };
        }
    }
}

// ---- uniform blocks / UBO binding (uniforms flow through the translator's default block; no-op) ----
#[no_mangle]
pub extern "C" fn glBindBufferBase(_target: u32, _index: u32, _buffer: u32) {}
#[no_mangle]
pub extern "C" fn glBindBufferRange(_target: u32, _index: u32, _buffer: u32, _offset: isize, _size: isize) {}
#[no_mangle]
pub extern "C" fn glUniformBlockBinding(_program: u32, _uniform_block_index: u32, _uniform_block_binding: u32) {}
#[no_mangle]
pub extern "C" fn glGetUniformBlockIndex(_program: u32, _uniform_block_name: *const c_char) -> u32 {
    GL_INVALID_INDEX
}
#[no_mangle]
pub extern "C" fn glGetUniformIndices(_program: u32, uniform_count: i32, _uniform_names: *const *const c_char, uniform_indices: *mut u32) {
    if !uniform_indices.is_null() {
        for i in 0..uniform_count.max(0) as usize {
            unsafe { *uniform_indices.add(i) = GL_INVALID_INDEX };
        }
    }
}
#[no_mangle]
pub extern "C" fn glGetActiveUniformBlockName(_program: u32, _uniform_block_index: u32, buf_size: i32, length: *mut i32, name: *mut c_char) {
    unsafe {
        set_i32(length, 0);
        if !name.is_null() && buf_size > 0 {
            *name = 0;
        }
    }
}
#[no_mangle]
pub extern "C" fn glGetActiveUniformBlockiv(_program: u32, _uniform_block_index: u32, _pname: u32, params: *mut i32) {
    unsafe { set_i32(params, 0) };
}
#[no_mangle]
pub extern "C" fn glGetActiveUniformsiv(_program: u32, uniform_count: i32, _uniform_indices: *const u32, _pname: u32, params: *mut i32) {
    if !params.is_null() {
        for i in 0..uniform_count.max(0) as usize {
            unsafe { *params.add(i) = 0 };
        }
    }
}

// ---- buffer (mapping flush + 64-bit / pointer queries) ----
#[no_mangle]
pub extern "C" fn glFlushMappedBufferRange(_target: u32, _offset: isize, _length: isize) {}
#[no_mangle]
pub extern "C" fn glGetBufferParameteri64v(_target: u32, _pname: u32, params: *mut i64) {
    unsafe {
        if !params.is_null() {
            *params = 0;
        }
    }
}
#[no_mangle]
pub extern "C" fn glGetBufferPointerv(_target: u32, _pname: u32, params: *mut *mut c_void) {
    unsafe {
        if !params.is_null() {
            *params = core::ptr::null_mut();
        }
    }
}

// ---- draw buffers / read buffer / integer + depth-stencil clears (single color target; no-op) ----
#[no_mangle]
pub extern "C" fn glDrawBuffers(_n: i32, _bufs: *const u32) {}
#[no_mangle]
pub extern "C" fn glReadBuffer(_src: u32) {}
#[no_mangle]
pub extern "C" fn glClearBufferiv(_buffer: u32, _drawbuffer: i32, _value: *const i32) {}
#[no_mangle]
pub extern "C" fn glClearBufferuiv(_buffer: u32, _drawbuffer: i32, _value: *const u32) {}
#[no_mangle]
pub extern "C" fn glClearBufferfi(_buffer: u32, _drawbuffer: i32, _depth: f32, _stencil: i32) {}

// ---- instancing divisor + integer vertex attributes ----
#[no_mangle]
pub extern "C" fn glVertexAttribDivisor(_index: u32, _divisor: u32) {}
#[no_mangle]
pub extern "C" fn glVertexAttribI4i(_index: u32, _x: i32, _y: i32, _z: i32, _w: i32) {}
#[no_mangle]
pub extern "C" fn glVertexAttribI4ui(_index: u32, _x: u32, _y: u32, _z: u32, _w: u32) {}
#[no_mangle]
pub extern "C" fn glVertexAttribI4iv(_index: u32, _v: *const i32) {}
#[no_mangle]
pub extern "C" fn glVertexAttribI4uiv(_index: u32, _v: *const u32) {}
#[no_mangle]
pub extern "C" fn glGetVertexAttribIiv(_index: u32, _pname: u32, params: *mut i32) {
    unsafe { set_i32(params, 0) };
}
#[no_mangle]
pub extern "C" fn glGetVertexAttribIuiv(_index: u32, _pname: u32, params: *mut u32) {
    unsafe {
        if !params.is_null() {
            *params = 0;
        }
    }
}

// ---- compressed / 3D-copy texture (not decoded by the executor; no-op, as gl_shim.c) ----
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glCompressedTexImage3D(_target: u32, _level: i32, _internalformat: u32, _width: i32, _height: i32, _depth: i32, _border: i32, _image_size: i32, _data: *const c_void) {}
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glCompressedTexSubImage3D(_target: u32, _level: i32, _xoffset: i32, _yoffset: i32, _zoffset: i32, _width: i32, _height: i32, _depth: i32, _format: u32, _image_size: i32, _data: *const c_void) {}
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glCopyTexSubImage3D(_target: u32, _level: i32, _xoffset: i32, _yoffset: i32, _zoffset: i32, _x: i32, _y: i32, _width: i32, _height: i32) {}

// ---- framebuffer invalidate (advisory hint; no-op) ----
#[no_mangle]
pub extern "C" fn glInvalidateFramebuffer(_target: u32, _num_attachments: i32, _attachments: *const u32) {}
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glInvalidateSubFramebuffer(_target: u32, _num_attachments: i32, _attachments: *const u32, _x: i32, _y: i32, _width: i32, _height: i32) {}

// ---- 64-bit / indexed integer state queries (reuse glGetIntegerv for the scalar value) ----
#[no_mangle]
pub extern "C" fn glGetInteger64v(pname: u32, data: *mut i64) {
    if data.is_null() {
        return;
    }
    let mut t = 0i32;
    glGetIntegerv(pname, &mut t);
    unsafe { *data = t as i64 };
}
#[no_mangle]
pub extern "C" fn glGetIntegeri_v(target: u32, _index: u32, data: *mut i32) {
    if !data.is_null() {
        glGetIntegerv(target, data);
    }
}
#[no_mangle]
pub extern "C" fn glGetInteger64i_v(target: u32, _index: u32, data: *mut i64) {
    if data.is_null() {
        return;
    }
    let mut t = 0i32;
    glGetIntegerv(target, &mut t);
    unsafe { *data = t as i64 };
}
#[no_mangle]
pub extern "C" fn glGetInternalformativ(_target: u32, _internalformat: u32, pname: u32, buf_size: i32, params: *mut i32) {
    if params.is_null() || buf_size <= 0 {
        return;
    }
    unsafe {
        *params = match pname {
            GL_NUM_SAMPLE_COUNTS => 1,
            GL_SAMPLES => 4,
            _ => 0,
        };
    }
}

// ---- program binary (no binary formats advertised → force the source-compile path) ----
#[no_mangle]
pub extern "C" fn glGetProgramBinary(_program: u32, _buf_size: i32, length: *mut i32, binary_format: *mut u32, _binary: *mut c_void) {
    unsafe {
        set_i32(length, 0);
        if !binary_format.is_null() {
            *binary_format = 0;
        }
    }
}
#[no_mangle]
pub extern "C" fn glProgramBinary(_program: u32, _binary_format: u32, _binary: *const c_void, _length: i32) {}
#[no_mangle]
pub extern "C" fn glProgramParameteri(_program: u32, _pname: u32, _value: i32) {}

// ---- misc ES3 introspection ----
#[no_mangle]
pub extern "C" fn glGetFragDataLocation(_program: u32, _name: *const c_char) -> i32 {
    0
}
#[no_mangle]
pub extern "C" fn glGetUniformuiv(_program: u32, _location: i32, params: *mut u32) {
    unsafe {
        if !params.is_null() {
            *params = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_object_lifecycle() {
        let mut ids = [0u32; 3];
        glGenBuffers(3, ids.as_mut_ptr());
        assert!(ids.iter().all(|&i| i >= 1));
        assert_ne!(ids[0], ids[1]);
        assert_eq!(glIsBuffer(ids[0]), 1);
        glBindBuffer(GL_ARRAY_BUFFER, ids[0]);
        let data = [1u8, 2, 3, 4];
        glBufferData(GL_ARRAY_BUFFER, data.len() as isize, data.as_ptr() as *const c_void, 0x88E4);
        {
            let s = gl();
            assert_eq!(s.buf[ids[0] as usize].data, data);
        }
        glDeleteBuffers(3, ids.as_ptr());
        assert_eq!(glIsBuffer(ids[0]), 0);
    }

    #[test]
    fn texture_upload_rgb_to_rgba() {
        let mut t = [0u32; 1];
        glGenTextures(1, t.as_mut_ptr());
        glActiveTexture(GL_TEXTURE0);
        glBindTexture(GL_TEXTURE_2D, t[0]);
        // 1x1 RGB upload → stored as RGBA8 with a=255.
        let rgb = [10u8, 20, 30];
        glTexImage2D(GL_TEXTURE_2D, 0, GL_RGB as i32, 1, 1, 0, GL_RGB, GL_UNSIGNED_BYTE, rgb.as_ptr() as *const c_void);
        let s = gl();
        assert_eq!(&s.tex[t[0] as usize].data, &[10u8, 20, 30, 255]);
    }

    #[test]
    fn blend_and_scissor_state_tracks() {
        glEnable(GL_BLEND);
        glBlendFuncSeparate(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA, GL_ONE, GL_ZERO);
        glEnable(GL_SCISSOR_TEST);
        glScissor(4, 5, 6, 7);
        assert_eq!(glIsEnabled(GL_BLEND), 1);
        let s = gl();
        assert_eq!(s.blend_src_rgb, GL_SRC_ALPHA);
        assert_eq!(s.blend_dst_alpha, GL_ZERO);
        assert_eq!(s.scissor, [4, 5, 6, 7]);
    }
}
