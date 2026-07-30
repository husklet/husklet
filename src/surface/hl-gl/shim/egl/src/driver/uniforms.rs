use super::*;
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glVertexAttrib1f(index: u32, x: f32) {
    GlobalState::context(|state| {
        record::vertex_attrib(&mut state.gl, index as usize, [x, 0.0, 0.0, 1.0])
    });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glVertexAttrib2f(index: u32, x: f32, y: f32) {
    GlobalState::context(|state| {
        record::vertex_attrib(&mut state.gl, index as usize, [x, y, 0.0, 1.0])
    });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glVertexAttrib3f(index: u32, x: f32, y: f32, z: f32) {
    GlobalState::context(|state| {
        record::vertex_attrib(&mut state.gl, index as usize, [x, y, z, 1.0])
    });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glVertexAttrib4f(index: u32, x: f32, y: f32, z: f32, w: f32) {
    GlobalState::context(|state| {
        record::vertex_attrib(&mut state.gl, index as usize, [x, y, z, w])
    });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glVertexAttrib1fv(index: u32, value: *const f32) {
    if !value.is_null() {
        glVertexAttrib1f(index, unsafe { *value });
    }
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glVertexAttrib2fv(index: u32, value: *const f32) {
    if !value.is_null() {
        glVertexAttrib2f(index, unsafe { *value }, unsafe { *value.add(1) });
    }
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glVertexAttrib3fv(index: u32, value: *const f32) {
    if !value.is_null() {
        glVertexAttrib3f(index, unsafe { *value }, unsafe { *value.add(1) }, unsafe {
            *value.add(2)
        });
    }
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glVertexAttrib4fv(index: u32, value: *const f32) {
    if !value.is_null() {
        glVertexAttrib4f(
            index,
            unsafe { *value },
            unsafe { *value.add(1) },
            unsafe { *value.add(2) },
            unsafe { *value.add(3) },
        );
    }
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glVertexAttribI4i(index: u32, x: i32, y: i32, z: i32, w: i32) {
    GlobalState::context(|state| {
        record::vertex_attrib_i(&mut state.gl, index as usize, [x, y, z, w])
    });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glVertexAttribI4ui(index: u32, x: u32, y: u32, z: u32, w: u32) {
    GlobalState::context(|state| {
        record::vertex_attrib_ui(&mut state.gl, index as usize, [x, y, z, w])
    });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glVertexAttribI4iv(index: u32, v: *const i32) {
    if !v.is_null() {
        glVertexAttribI4i(
            index,
            unsafe { *v },
            unsafe { *v.add(1) },
            unsafe { *v.add(2) },
            unsafe { *v.add(3) },
        );
    }
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glVertexAttribI4uiv(index: u32, v: *const u32) {
    if !v.is_null() {
        glVertexAttribI4ui(
            index,
            unsafe { *v },
            unsafe { *v.add(1) },
            unsafe { *v.add(2) },
            unsafe { *v.add(3) },
        );
    }
}

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
    GlobalState::context(|s| {
        record::vertex_attrib_pointer(
            &mut s.gl,
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
/// attribute format; buffer/stride/divisor come from the selected separate vertex binding.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glVertexAttribIFormat(
    attribindex: u32,
    size: i32,
    type_: u32,
    relativeoffset: u32,
) {
    GlobalState::context(|s| {
        record::vertex_attrib_format(
            &mut s.gl,
            attribindex as usize,
            size,
            type_,
            false,
            true,
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
    GlobalState::context(|s| record::stencil_func_separate(&mut s.gl, face, func, ref_, mask));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glStencilMaskSeparate(face: u32, mask: u32) {
    GlobalState::context(|s| record::stencil_mask_separate(&mut s.gl, face, mask));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glStencilOpSeparate(face: u32, sfail: u32, dpfail: u32, dppass: u32) {
    GlobalState::context(|s| record::stencil_op_separate(&mut s.gl, face, sfail, dpfail, dppass));
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
// ---- EGL_EXT_device_base / device_query / device_enumeration enums + the hl-gl device ----------------
/// `EGL_DEVICE_EXT` — the `eglQueryDisplayAttribEXT` attribute GDK asks for to learn the display's backing
/// `EGLDeviceEXT` (and the `eglCreatePlatformDisplay(EGL_PLATFORM_DEVICE_EXT, …)` platform enum).
pub(super) const EGL_DEVICE_EXT: i32 = 0x322C;
/// `EGL_BAD_DEVICE_EXT` — the error for an `EGLDeviceEXT` handle we did not hand out.
pub(super) const EGL_BAD_DEVICE_EXT: i32 = 0x322B;
/// `EGL_DRM_RENDER_NODE_FILE_EXT` — the render-node pathname exposed by
/// `EGL_EXT_device_drm_render_node`.
pub(super) const EGL_DRM_RENDER_NODE_FILE_EXT: i32 = 0x3377;
/// The single, truthful `EGLDeviceEXT` handle this driver reports: the hl-gl renderer backed by Husklet's
/// projected DRM render node. Non-null
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

/// Marshal a `cols`×`rows` GL matrix array into the tightly packed representation consumed by
/// `hl_gl::adapter::glsl::Uni`. That model layer is the single owner of std140 column padding. Padding here
/// too would make a three-row matrix's second column start with the first column's padding.
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
    let mut out = Vec::with_capacity(count as usize * cols * rows * 4);
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
