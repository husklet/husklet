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

/// `glGetError` — no error on the covered paths (gl_shim.c returns GL_NO_ERROR unconditionally).
#[no_mangle]
pub extern "C" fn glGetError() -> u32 {
    GL_NO_ERROR
}

/// `glGetString` — identity strings. The version/GLSL strings track the created context version
/// (gl_shim.c keys them on `g_ctx_major`).
#[no_mangle]
pub extern "C" fn glGetString(name: u32) -> *const u8 {
    let es3 = crate::state::egl().ctx_major >= 3;
    let s: *const c_char = match name {
        0x1F02 => {
            if es3 {
                cstr!("OpenGL ES 3.0 dd-shim")
            } else {
                cstr!("OpenGL ES 2.0 dd-shim")
            }
        }
        0x1F00 => cstr!("dd"),       // GL_VENDOR
        0x1F01 => cstr!("dd-metal"), // GL_RENDERER
        0x8B8C => {
            if es3 {
                cstr!("OpenGL ES GLSL ES 3.00")
            } else {
                cstr!("OpenGL ES GLSL ES 1.00")
            }
        }
        0x1F03 => cstr!("GL_OES_element_index_uint GL_OES_texture_npot"), // GL_EXTENSIONS
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
            GL_NUM_EXTENSIONS => *v = 2, // matches the two-extension GL_EXTENSIONS string
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

/// `glLinkProgram` — mark the program linked. The GLSL→shader-IR translation (and the uniform-block
/// layout that gives uniforms their locations) lands with the shader-translation increment; until
/// then linkage is tracked but no MSL/SPIR-V is produced.
#[no_mangle]
pub extern "C" fn glLinkProgram(prog: u32) {
    let mut s = gl();
    if (prog as usize) < MAXPROG && s.prog[prog as usize].used {
        let (vs, fs) = (s.prog[prog as usize].vs, s.prog[prog as usize].fs);
        s.prog[prog as usize].linked = vs != 0 && fs != 0;
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

/// `glGetUniformLocation` — returns -1 (not found) until the uniform-block layout parser lands with the
/// shader-translation increment. gl_shim.c derives the location as a byte offset from that parse.
#[no_mangle]
pub extern "C" fn glGetUniformLocation(_prog: u32, _name: *const c_char) -> i32 {
    -1
}

/// `glGetAttribLocation` — returns -1 (not found) until the GLSL attribute collector lands with the
/// shader-translation increment.
#[no_mangle]
pub extern "C" fn glGetAttribLocation(_prog: u32, _name: *const c_char) -> i32 {
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
    // gl_shim.c uses loc in [100000,100004) as a sampler-unit sentinel; those locations only exist once
    // the layout parser lands, so here glUniform1i is a plain int store.
    uni_write(loc, &a.to_le_bytes());
}
#[no_mangle]
pub extern "C" fn glUniformMatrix4fv(loc: i32, _count: i32, _transpose: u8, v: *const f32) {
    if !v.is_null() {
        uni_write(loc, &f_bytes(unsafe { core::slice::from_raw_parts(v, 16) }));
    }
}

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
            s.default_full_clear_since_swap = true; // draw_fbo == 0 (default) in the ported subset
            s.record_clear_call(sx, sy, sw, sh);
        }
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
