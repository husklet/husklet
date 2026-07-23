use super::*;
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
pub extern "C" fn glVertexAttribIPointer(
    index: u32,
    size: i32,
    type_: u32,
    stride: i32,
    pointer: *const c_void,
) {
    GlobalState::access(|s| {
        record::vertex_attrib_pointer(
            &mut s.ctx,
            index as usize,
            size,
            type_,
            false,
            stride,
            pointer as usize,
        )
    });
}

/// `glVertexAttribIFormat(attribindex, size, type, relativeoffset)` — the separate-format (VAO) integer
/// attribute format; records size/type/offset into the attribute state (a no-op for the fields this model
/// does not separately track).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glVertexAttribIFormat(
    attribindex: u32,
    size: i32,
    type_: u32,
    relativeoffset: u32,
) {
    GlobalState::access(|s| {
        record::vertex_attrib_pointer(
            &mut s.ctx,
            attribindex as usize,
            size,
            type_,
            false,
            0,
            relativeoffset as usize,
        )
    });
}

// ==================================================================================================
// ES3 framebuffer-attachment invalidation (a hint) + separate-face stencil (IR-free) — honest no-ops
// ==================================================================================================

/// `glInvalidateFramebuffer(target, n, attachments)` — a discard HINT that the listed attachments'
/// contents are no longer needed. This deferred model rebuilds a fresh frame each swap (nothing is
/// preserved across frames), so the hint is already satisfied — an honest no-op.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glInvalidateFramebuffer(
    _target: u32,
    _num_attachments: i32,
    _attachments: *const u32,
) {
}

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

/// Separate-face stencil state — record the per-face compare/reference/masks + ops. A pass whose draw
/// enables `GL_STENCIL_TEST` materializes a `Depth24PlusStencil8` attachment and lowers this into the
/// pipeline's `DepthState` stencil faces + `Enc::SetStencilReference` (see `service::frame`).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glStencilFuncSeparate(face: u32, func: u32, ref_: i32, mask: u32) {
    GlobalState::access(|s| record::stencil_func_separate(&mut s.ctx, face, func, ref_, mask));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glStencilMaskSeparate(face: u32, mask: u32) {
    GlobalState::access(|s| record::stencil_mask_separate(&mut s.ctx, face, mask));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glStencilOpSeparate(face: u32, sfail: u32, dpfail: u32, dppass: u32) {
    GlobalState::access(|s| record::stencil_op_separate(&mut s.ctx, face, sfail, dpfail, dppass));
}

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
pub(super) const EGL_OPENGL_ES_API: u32 = 0x30A0;
pub(super) const EGL_CONDITION_SATISFIED: i32 = 0x30F6;
pub(super) const EGL_WIDTH: i32 = 0x3057;
pub(super) const EGL_HEIGHT: i32 = 0x3056;
/// `eglQueryContext` attributes: the client API type (`EGL_OPENGL_ES_API` for this driver), the requested
/// client version (3 — this is an ES3 driver), and the render buffer of the bound surface (back-buffered).
pub(super) const EGL_CONTEXT_CLIENT_TYPE: i32 = 0x3097;
pub(super) const EGL_CONTEXT_CLIENT_VERSION: i32 = 0x3098;
pub(super) const EGL_RENDER_BUFFER: i32 = 0x3086;
pub(super) const EGL_BACK_BUFFER: i32 = 0x3084;
/// A fixed non-null opaque token for the EGL sync / image objects this driver hands back (their lifecycle
/// is accepted but not separately tracked — one shared token keeps a `!= EGL_NO_SYNC` contract).
pub(super) const EGL_OBJECT_TOKEN: usize = 0x5171;

// ---- EGL_EXT_device_base / device_query / device_enumeration enums + the single software device ----
/// `EGL_DEVICE_EXT` — the `eglQueryDisplayAttribEXT` attribute GDK asks for to learn the display's backing
/// `EGLDeviceEXT` (and the `eglCreatePlatformDisplay(EGL_PLATFORM_DEVICE_EXT, …)` platform enum).
pub(super) const EGL_DEVICE_EXT: i32 = 0x322C;
/// `EGL_BAD_DEVICE_EXT` — the error for an `EGLDeviceEXT` handle we did not hand out.
pub(super) const EGL_BAD_DEVICE_EXT: i32 = 0x322B;
/// The single, truthful `EGLDeviceEXT` handle this driver reports: our software (hl-gl) renderer. Non-null
/// and distinct from the display/config/object tokens so `eglQueryDeviceStringEXT` et al. can validate it.
pub(super) const DEVICE_TOKEN: usize = 0xDE71;

// ---- little-endian marshalling helpers (unsigned + non-square matrices) --------------------------

/// Borrow a `count`×`n` `u32` array (`glUniform{N}uiv` value), empty if null / non-positive count.
pub(super) unsafe fn slice_u32<'a>(value: *const u32, count: i32, n: usize) -> &'a [u32] {
    if value.is_null() || count <= 0 {
        &[]
    } else {
        std::slice::from_raw_parts(value, count as usize * n)
    }
}

/// Marshal a `cols`×`rows` GL matrix array into MSL `floatCxR` struct layout: `count` matrices, each
/// `cols` columns of `rows` floats; every column is padded to 4 floats when `rows == 3` (MSL's 16-byte
/// column stride). GL's source is column-major unless `transpose` (then row-major).
pub(super) unsafe fn mat_bytes_cr(
    cols: usize,
    rows: usize,
    count: i32,
    transpose: bool,
    value: *const f32,
) -> Vec<u8> {
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
                let v = if transpose {
                    src[base + row * cols + col]
                } else {
                    src[base + col * rows + row]
                };
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
    Uniform::set(location, &LittleEndian::encode(&[v0]));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniform2ui(location: i32, v0: u32, v1: u32) {
    Uniform::set(location, &LittleEndian::encode(&[v0, v1]));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniform3ui(location: i32, v0: u32, v1: u32, v2: u32) {
    Uniform::set(location, &LittleEndian::encode(&[v0, v1, v2]));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniform4ui(location: i32, v0: u32, v1: u32, v2: u32, v3: u32) {
    Uniform::set(location, &LittleEndian::encode(&[v0, v1, v2, v3]));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniform1uiv(location: i32, count: i32, value: *const u32) {
    Uniform::set(
        location,
        &LittleEndian::encode(unsafe { slice_u32(value, count, 1) }),
    );
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniform2uiv(location: i32, count: i32, value: *const u32) {
    Uniform::set(
        location,
        &LittleEndian::encode(unsafe { slice_u32(value, count, 2) }),
    );
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniform3uiv(location: i32, count: i32, value: *const u32) {
    Uniform::set(
        location,
        &LittleEndian::encode(unsafe { slice_u32(value, count, 3) }),
    );
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniform4uiv(location: i32, count: i32, value: *const u32) {
    Uniform::set(
        location,
        &LittleEndian::encode(unsafe { slice_u32(value, count, 4) }),
    );
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniformMatrix2x3fv(
    location: i32,
    count: i32,
    transpose: u8,
    value: *const f32,
) {
    Uniform::set(location, &unsafe {
        mat_bytes_cr(2, 3, count, transpose != 0, value)
    });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniformMatrix3x2fv(
    location: i32,
    count: i32,
    transpose: u8,
    value: *const f32,
) {
    Uniform::set(location, &unsafe {
        mat_bytes_cr(3, 2, count, transpose != 0, value)
    });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniformMatrix2x4fv(
    location: i32,
    count: i32,
    transpose: u8,
    value: *const f32,
) {
    Uniform::set(location, &unsafe {
        mat_bytes_cr(2, 4, count, transpose != 0, value)
    });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniformMatrix4x2fv(
    location: i32,
    count: i32,
    transpose: u8,
    value: *const f32,
) {
    Uniform::set(location, &unsafe {
        mat_bytes_cr(4, 2, count, transpose != 0, value)
    });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniformMatrix3x4fv(
    location: i32,
    count: i32,
    transpose: u8,
    value: *const f32,
) {
    Uniform::set(location, &unsafe {
        mat_bytes_cr(3, 4, count, transpose != 0, value)
    });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniformMatrix4x3fv(
    location: i32,
    count: i32,
    transpose: u8,
    value: *const f32,
) {
    Uniform::set(location, &unsafe {
        mat_bytes_cr(4, 3, count, transpose != 0, value)
    });
}
