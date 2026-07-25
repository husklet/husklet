use super::*;
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glCreateShader(type_: u32) -> u32 {
    GlobalState::access(|s| record::create_shader(&mut s.ctx, type_))
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glShaderSource(
    shader: u32,
    count: i32,
    string: *const *const c_char,
    length: *const i32,
) {
    let src = unsafe { join_source(count, string, length) };
    GlobalState::access(|s| record::shader_source(&mut s.ctx, shader, &src));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glCompileShader(shader: u32) {
    GlobalState::access(|s| record::compile_shader(&mut s.ctx, shader));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glCreateProgram() -> u32 {
    GlobalState::access(|s| record::create_program(&mut s.ctx))
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glAttachShader(program: u32, shader: u32) {
    GlobalState::access(|s| record::attach_shader(&mut s.ctx, program, shader));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glLinkProgram(program: u32) {
    GlobalState::access(|s| {
        let _ = record::link_program(&mut s.ctx, program);
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUseProgram(program: u32) {
    GlobalState::access(|s| record::use_program(&mut s.ctx, program));
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
    let v = GlobalState::access(|s| query::get_shaderiv(&s.ctx, shader, pname));
    unsafe { *params = v };
}

/// `glGetProgramiv(program, pname, params)` — `GL_LINK_STATUS`/`GL_VALIDATE_STATUS` (TRUE once linked),
/// `GL_INFO_LOG_LENGTH` (0), `GL_ATTACHED_SHADERS`, `GL_ACTIVE_UNIFORMS`, `GL_ACTIVE_ATTRIBUTES`.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetProgramiv(program: u32, pname: u32, params: *mut i32) {
    if params.is_null() {
        return;
    }
    let v = GlobalState::access(|s| query::get_programiv(&s.ctx, program, pname));
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
pub(super) unsafe fn write_empty_info_log(buf_size: i32, length: *mut i32, info_log: *mut c_char) {
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
    let want = match unsafe { Text::read(name) } {
        Some(s) => s,
        None => return -1,
    };
    GlobalState::access(|s| query::uniform_location(&s.ctx, program, &want))
}

/// `glGetAttribLocation(program, name)` — the vertex attribute's declaration-order slot in the linked
/// program, or `-1` if `name` is not an active attribute. Null-safe.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetAttribLocation(program: u32, name: *const c_char) -> i32 {
    let want = match unsafe { Text::read(name) } {
        Some(s) => s,
        None => return -1,
    };
    GlobalState::access(|s| query::attrib_location(&s.ctx, program, &want))
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
    let var = GlobalState::access(|s| query::active_uniform(&s.ctx, program, index));
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
    let var = GlobalState::access(|s| query::active_attrib(&s.ctx, program, index));
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
            GlobalState::access(|s| s.ctx.set_gl_error(GL_INVALID_VALUE));
            let empty = query::ActiveVar {
                name: String::new(),
                gl_type: 0,
                size: 0,
            };
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
    GlobalState::access(|s| record::uniform_sampler(&mut s.ctx, location as usize, v0));
}

// ---- data uniforms (record into the bound program's uniform block; shipped at binding 1 at draw) -----
//
// `location` is the uniform's declaration index — the same convention `glUniform1i`/`uniform_sampler`
// use for samplers. `glUniform1i` stays sampler-only (above); the integer variants below and every float
// variant write the value's little-endian bytes into the uniform block at the member's reflected offset.

/// Write `bytes` into data uniform `location` of the bound program (no-op for a negative location).
pub(super) struct Uniform;

impl Uniform {
    pub(super) fn set(location: i32, bytes: &[u8]) {
        if location < 0 {
            return;
        }
        GlobalState::access(|state| record::uniform_at(&mut state.ctx, location as usize, bytes));
    }
}

pub(super) trait UniformScalar {
    fn append(&self, bytes: &mut Vec<u8>);
}

macro_rules! uniform_scalar {
    ($type:ty) => {
        impl UniformScalar for $type {
            fn append(&self, bytes: &mut Vec<u8>) {
                bytes.extend_from_slice(&self.to_le_bytes());
            }
        }
    };
}

uniform_scalar!(f32);
uniform_scalar!(i32);
uniform_scalar!(u32);

pub(super) struct LittleEndian;

impl LittleEndian {
    pub(super) fn encode<T: UniformScalar>(values: &[T]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(values.len() * 4);
        for value in values {
            value.append(&mut bytes);
        }
        bytes
    }
}

/// Borrow a `count`×`n` scalar array (`glUniform{N}{f,i}v` value), empty if null / non-positive count.
pub(super) unsafe fn slice_f32<'a>(value: *const f32, count: i32, n: usize) -> &'a [f32] {
    if value.is_null() || count <= 0 {
        &[]
    } else {
        std::slice::from_raw_parts(value, count as usize * n)
    }
}
pub(super) unsafe fn slice_i32<'a>(value: *const i32, count: i32, n: usize) -> &'a [i32] {
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
                let v = if transpose {
                    src[base + row * n + col]
                } else {
                    src[base + col * n + row]
                };
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
    Uniform::set(location, &LittleEndian::encode(&[v0]));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniform2f(location: i32, v0: f32, v1: f32) {
    Uniform::set(location, &LittleEndian::encode(&[v0, v1]));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniform3f(location: i32, v0: f32, v1: f32, v2: f32) {
    Uniform::set(location, &LittleEndian::encode(&[v0, v1, v2]));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniform4f(location: i32, v0: f32, v1: f32, v2: f32, v3: f32) {
    Uniform::set(location, &LittleEndian::encode(&[v0, v1, v2, v3]));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniform2i(location: i32, v0: i32, v1: i32) {
    Uniform::set(location, &LittleEndian::encode(&[v0, v1]));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniform3i(location: i32, v0: i32, v1: i32, v2: i32) {
    Uniform::set(location, &LittleEndian::encode(&[v0, v1, v2]));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniform4i(location: i32, v0: i32, v1: i32, v2: i32, v3: i32) {
    Uniform::set(location, &LittleEndian::encode(&[v0, v1, v2, v3]));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniform1fv(location: i32, count: i32, value: *const f32) {
    Uniform::set(
        location,
        &LittleEndian::encode(unsafe { slice_f32(value, count, 1) }),
    );
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniform2fv(location: i32, count: i32, value: *const f32) {
    Uniform::set(
        location,
        &LittleEndian::encode(unsafe { slice_f32(value, count, 2) }),
    );
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniform3fv(location: i32, count: i32, value: *const f32) {
    Uniform::set(
        location,
        &LittleEndian::encode(unsafe { slice_f32(value, count, 3) }),
    );
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniform4fv(location: i32, count: i32, value: *const f32) {
    Uniform::set(
        location,
        &LittleEndian::encode(unsafe { slice_f32(value, count, 4) }),
    );
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniform1iv(location: i32, count: i32, value: *const i32) {
    Uniform::set(
        location,
        &LittleEndian::encode(unsafe { slice_i32(value, count, 1) }),
    );
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniform2iv(location: i32, count: i32, value: *const i32) {
    Uniform::set(
        location,
        &LittleEndian::encode(unsafe { slice_i32(value, count, 2) }),
    );
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniform3iv(location: i32, count: i32, value: *const i32) {
    Uniform::set(
        location,
        &LittleEndian::encode(unsafe { slice_i32(value, count, 3) }),
    );
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniform4iv(location: i32, count: i32, value: *const i32) {
    Uniform::set(
        location,
        &LittleEndian::encode(unsafe { slice_i32(value, count, 4) }),
    );
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniformMatrix2fv(location: i32, count: i32, transpose: u8, value: *const f32) {
    Uniform::set(location, &unsafe {
        mat_bytes(2, count, transpose != 0, value)
    });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniformMatrix3fv(location: i32, count: i32, transpose: u8, value: *const f32) {
    Uniform::set(location, &unsafe {
        mat_bytes(3, count, transpose != 0, value)
    });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniformMatrix4fv(location: i32, count: i32, transpose: u8, value: *const f32) {
    Uniform::set(location, &unsafe {
        mat_bytes(4, count, transpose != 0, value)
    });
}
