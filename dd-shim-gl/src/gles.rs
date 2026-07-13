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
    let es3 = crate::state::egl().ctx_major >= 3;
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

// ===================================================================================================
// scalar state + queries
// ===================================================================================================

#[no_mangle]
pub extern "C" fn glClearColor(r: f32, g: f32, b: f32, a: f32) {
    gl().clear = [r, g, b, a];
}

#[no_mangle]
pub extern "C" fn glViewport(x: i32, y: i32, w: i32, h: i32) {
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
        _ => {}
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
        _ => {}
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
#[no_mangle]
pub extern "C" fn glFinish() {}
#[no_mangle]
pub extern "C" fn glFlush() {}
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
    let e = crate::state::egl();
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
            GL_MAJOR_VERSION => *v = e.ctx_major,
            GL_MINOR_VERSION => *v = e.ctx_minor,
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
    if out.is_null() || n < 0 {
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
    let mut s = gl();
    if target == GL_ARRAY_BUFFER {
        s.arr_buf = buffer;
    } else if target == GL_ELEMENT_ARRAY_BUFFER {
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
pub extern "C" fn glCompileShader(_sh: u32) {}

#[no_mangle]
pub extern "C" fn glGetShaderiv(sh: u32, pname: u32, v: *mut i32) {
    if v.is_null() {
        return;
    }
    let s = gl();
    unsafe {
        if pname == GL_SHADER_SOURCE_LENGTH {
            // glmark2 verifies this round-trips to strlen(source)+1 before it will compile.
            *v = if (sh as usize) < MAXSH && s.sh[sh as usize].used {
                s.sh[sh as usize].src.as_ref().map(|x| x.len() + 1).unwrap_or(0) as i32
            } else {
                0
            };
        } else if pname == GL_COMPILE_STATUS {
            *v = GL_TRUE as i32;
        } else {
            *v = 0;
        }
    }
}

#[no_mangle]
pub extern "C" fn glGetShaderInfoLog(_sh: u32, buf_size: i32, length: *mut i32, info_log: *mut c_char) {
    unsafe {
        set_i32(length, 0);
        if !info_log.is_null() && buf_size > 0 {
            *info_log = 0;
        }
    }
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
    let vsrc = if (vs as usize) < MAXSH { s.sh[vs as usize].src.clone() } else { None };
    let fsrc = if (fs as usize) < MAXSH { s.sh[fs as usize].src.clone() } else { None };
    let p = &mut s.prog[prog as usize];
    p.linked = vs != 0 && fs != 0;
    if let (Some(v), Some(f)) = (vsrc, fsrc) {
        p.msl = Some(crate::translate::translate(&v, &f));
        let (unis, total) = crate::translate::uni_layout(&v, &f);
        p.unis = unis;
        p.ubuf_size = total;
        p.samp_names = crate::translate::program_samplers(&v, &f);
        p.samp_units = [0; 4];
        p.ubuf = [0; crate::state::UBUF_BYTES];
    }
}

#[no_mangle]
pub extern "C" fn glUseProgram(prog: u32) {
    gl().cur_prog = prog;
}

#[no_mangle]
pub extern "C" fn glGetProgramiv(_prog: u32, pname: u32, v: *mut i32) {
    unsafe {
        if pname == GL_LINK_STATUS {
            set_i32(v, GL_TRUE as i32);
        } else {
            set_i32(v, 0);
        }
    }
}

#[no_mangle]
pub extern "C" fn glGetProgramInfoLog(_prog: u32, buf_size: i32, length: *mut i32, info_log: *mut c_char) {
    unsafe {
        set_i32(length, 0);
        if !info_log.is_null() && buf_size > 0 {
            *info_log = 0;
        }
    }
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
    s.ubuf[loc..loc + n].copy_from_slice(data);
    let cp = s.cur_prog as usize;
    if cp < MAXPROG && s.prog[cp].used {
        s.prog[cp].ubuf[loc..loc + n].copy_from_slice(data);
    }
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
