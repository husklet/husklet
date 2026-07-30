use super::*;

mod external;
use external::*;
use std::sync::Arc;
// ==================================================================================================
// texture / renderbuffer extensions
// ==================================================================================================

/// `glTexStorage2DMultisample` — immutable multisample 2D storage. This model materializes single-sample
/// textures, so `samples` is ignored and the base RGBA8 plane is allocated (delegating to the 2D storage
/// path); the texture is usable as a single-sample attachment.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glTexStorage2DMultisample(
    target: u32,
    _samples: i32,
    internalformat: u32,
    width: i32,
    height: i32,
    _fixedsamplelocations: u8,
) {
    GlobalState::context(|s| {
        s.redefine_texture(|ctx| {
            record::tex_storage_2d(ctx, target, 1, internalformat, width, height)
        })
    });
}
/// `glTexStorage3DMultisample` — the 2D-array multisample form; `samples` ignored (single-sample plane).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glTexStorage3DMultisample(
    target: u32,
    _samples: i32,
    _internalformat: u32,
    width: i32,
    height: i32,
    depth: i32,
    _fixedsamplelocations: u8,
) {
    GlobalState::context(|s| record::tex_storage_3d(&mut s.gl, target, 1, width, height, depth));
}
/// `glRenderbufferStorageMultisample` — a multisample renderbuffer; single-sample in this model, so the
/// backing RGBA8 plane is sized (delegating to `glRenderbufferStorage`) and `samples` is ignored.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glRenderbufferStorageMultisample(
    target: u32,
    _samples: i32,
    internalformat: u32,
    width: i32,
    height: i32,
) {
    GlobalState::context(|s| {
        s.redefine_renderbuffer(|ctx| {
            record::renderbuffer_storage(ctx, target, internalformat, width, height)
        })
    });
}
/// `glTexBuffer` / `glTexBufferRange` — buffer textures (sampling a buffer object as a 1D texel array).
/// No buffer-texture sampling path is modeled (the render path samples 2D textures), so this is an honest
/// no-op; a shader that samples one reads the texture's neutral (dataless) content.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glTexBuffer(_target: u32, _internalformat: u32, _buffer: u32) {}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glTexBufferRange(
    _target: u32,
    _internalformat: u32,
    _buffer: u32,
    _offset: isize,
    _size: isize,
) {
}
/// `glTexParameterIiv` / `glTexParameterIuiv` — integer parameter vectors.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glTexParameterIiv(_target: u32, pname: u32, params: *const i32) {
    if params.is_null() {
        return;
    }
    let count = if pname == GL_TEXTURE_SWIZZLE_RGBA {
        4
    } else {
        1
    };
    let values = unsafe { std::slice::from_raw_parts(params, count) }
        .iter()
        .map(|value| *value as u32)
        .collect::<Vec<_>>();
    GlobalState::context(|s| record::tex_parameter_vector(&mut s.gl, pname, &values));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glTexParameterIuiv(_target: u32, pname: u32, params: *const u32) {
    if params.is_null() {
        return;
    }
    let count = if pname == GL_TEXTURE_SWIZZLE_RGBA {
        4
    } else {
        1
    };
    let values = unsafe { std::slice::from_raw_parts(params, count) };
    GlobalState::context(|s| record::tex_parameter_vector(&mut s.gl, pname, values));
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
    if src_target != GL_TEXTURE_2D
        || dst_target != GL_TEXTURE_2D
        || src_level != 0
        || dst_level != 0
    {
        return;
    }
    if src_width <= 0 || src_height <= 0 || src_x < 0 || src_y < 0 || dst_x < 0 || dst_y < 0 {
        return;
    }
    GlobalState::context(|s| {
        // Read the source rect out (immutable borrow) …
        let rows: Option<Vec<u8>> = s.gl.textures.get(src_name).cloned().and_then(|mut st| {
            st.resolve_shared();
            let (sw, sh) = (st.w, st.h);
            if st.data.is_empty() || src_x + src_width > sw || src_y + src_height > sh {
                return None;
            }
            let (sw, w, h, x, y) = (
                sw as usize,
                src_width as usize,
                src_height as usize,
                src_x as usize,
                src_y as usize,
            );
            let mut buf = Vec::with_capacity(w * h * 4);
            for row in 0..h {
                let base = ((y + row) * sw + x) * 4;
                buf.extend_from_slice(&st.data[base..base + w * 4]);
            }
            Some(buf)
        });
        // … then write it into the destination sub-rect (mutable borrow).
        if let Some(buf) = rows {
            if s.gl
                .textures
                .sub_image_2d(dst_name, dst_x, dst_y, src_width, src_height, &buf)
            {
                s.mark_linear_dirty(dst_name);
            }
        }
    });
}
/// `glBindImageTexture(...)` — bind a texture level as a shader image (image load/store). This model lowers
/// no image load/store, so the binding carries no observable state: an honest no-op.
#[cfg_attr(gles_client, no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glBindImageTexture(
    _unit: u32,
    _texture: u32,
    _level: i32,
    _layered: u8,
    _layer: i32,
    _access: u32,
    _format: u32,
) {
}

// ==================================================================================================
// separate-format vertex arrays + framebuffer attach extensions
// ==================================================================================================

/// `glVertexAttribFormat(attribindex, size, type, normalized, relativeoffset)` — the separate-format
/// vertex-attribute description; records size/type/normalized into the same per-location attribute state
/// the pointer path uses (the relative offset is folded into the attribute offset).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glVertexAttribFormat(
    attribindex: u32,
    size: i32,
    type_: u32,
    normalized: u8,
    relativeoffset: u32,
) {
    crate::stub::trace(
        "glVertexAttribFormat",
        &format!(
            "attribute={attribindex} size={size} type={type_:#x} relative_offset={relativeoffset}"
        ),
    );
    GlobalState::context(|s| {
        record::vertex_attrib_format(
            &mut s.gl,
            attribindex as usize,
            size,
            type_,
            normalized != 0,
            false,
            relativeoffset as usize,
        )
    });
}
/// `glVertexAttribBinding(attribindex, bindingindex)` — associate an attribute with a vertex-buffer binding
/// slot. This model keys attributes to a single array-buffer binding, so the binding index is not
/// separately tracked: an honest no-op.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glVertexAttribBinding(attribindex: u32, bindingindex: u32) {
    crate::stub::trace(
        "glVertexAttribBinding",
        &format!("attribute={attribindex} binding={bindingindex}"),
    );
    GlobalState::context(|s| {
        record::vertex_attrib_binding(&mut s.gl, attribindex as usize, bindingindex)
    });
}
/// `glBindVertexBuffer(bindingindex, buffer, offset, stride)` — bind a buffer to a vertex-buffer slot. The
/// separate binding slots are not modeled; binding slot 0 updates the array-buffer binding so a following
/// `glVertexAttribFormat`-based draw still sources vertices, other slots are an honest no-op.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBindVertexBuffer(bindingindex: u32, buffer: u32, offset: isize, stride: i32) {
    crate::stub::trace(
        "glBindVertexBuffer",
        &format!("binding={bindingindex} buffer={buffer} offset={offset} stride={stride}"),
    );
    GlobalState::context(|s| {
        record::bind_vertex_buffer(&mut s.gl, bindingindex as usize, buffer, offset, stride)
    });
}
/// `glVertexBindingDivisor(bindingindex, divisor)` — the instance-step divisor for a binding slot; applied
/// to the attribute at that index (single-slot model).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glVertexBindingDivisor(bindingindex: u32, divisor: u32) {
    GlobalState::context(|s| {
        record::vertex_binding_divisor(&mut s.gl, bindingindex as usize, divisor)
    });
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
    GlobalState::context(|s| {
        record::framebuffer_texture_2d(&mut s.gl, target, attachment, GL_TEXTURE_2D, texture, level)
    });
}
/// `glFramebufferTextureLayer(target, attachment, texture, level, layer)` — attach one layer of an array/3D
/// texture as the color target. This model materializes the layer-0 plane, so the layer is folded into the
/// color attachment via the same 2D color-attach path.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glFramebufferTextureLayer(
    target: u32,
    attachment: u32,
    texture: u32,
    level: i32,
    _layer: i32,
) {
    GlobalState::context(|s| {
        record::framebuffer_texture_2d(&mut s.gl, target, attachment, GL_TEXTURE_2D, texture, level)
    });
}

pub(super) fn flush_pending(
    group: &mut crate::state::GroupData,
    sink: &mut dyn hl_gpu::CommandSink,
    max_buffer_bytes: u64,
) -> hl_gpu::Result<bool> {
    match flush_imported_images(group, sink, max_buffer_bytes)? {
        Some(captures) => Ok(captures > 0),
        None => swap::flush(&mut group.gl, sink),
    }
}

/// `glFlush` submits all pending draw work (see [`swap::flush`]) so a
/// multi-context app (Chrome's gpu-raster workers render into FBOs and `glFlush` but never
/// `eglSwapBuffers`) does not accumulate an unbounded draw-list into the eventual swap frame. Window
/// (default-framebuffer) draws are retained for `eglSwapBuffers`.
///
/// The command sink accepts the work synchronously. There is no separate process-local submission serial:
/// one would neither make the sink asynchronous nor expose additional ordering to a GL caller.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glFlush() {
    crate::stub::trace("glFlush", "submitting pending offscreen work");
    let max_buffer_bytes = GlobalState::access(|state| state.max_buffer_bytes);
    let result = GlobalState::gpu_submit(move |group, sink| {
        crate::stub::trace("glFlush.flush", "flushing offscreen work");
        let debug = std::env::var_os("HL_SHIM_DEBUG").is_some();
        let diagnostics = debug.then(|| {
            let draws = group.gl.recording_counts().0;
            let current_fbo = group.gl.bound_framebuffer();
            let mut first_fbo = None;
            let mut last_fbo = None;
            let mut unique_fbos = Vec::new();
            for fbo in group.gl.recorded_framebuffers() {
                first_fbo.get_or_insert(fbo);
                last_fbo = Some(fbo);
                if !unique_fbos.contains(&fbo) {
                    unique_fbos.push(fbo);
                }
            }
            let imported_matches = unique_fbos
                .iter()
                .filter(|&&fbo| {
                    fbo != 0
                        && group
                            .images
                            .contains_key(&group.gl.framebuffer_color_attachment(fbo))
                })
                .count();
            (
                draws,
                current_fbo,
                first_fbo,
                last_fbo,
                unique_fbos.len(),
                imported_matches,
            )
        });
        let result = flush_pending(group, sink, max_buffer_bytes);
        if let Some((draws, current_fbo, first_fbo, last_fbo, unique_fbos, imported_matches)) =
            diagnostics
        {
            eprintln!(
                "[hl-gl-shim] flush draws={draws} fbo=current:{current_fbo} first:{first_fbo:?} \
                 last:{last_fbo:?} unique={unique_fbos} imported_matches={imported_matches} result={result:?}"
            );
        }
        crate::stub::trace("glFlush.flushed", "offscreen flush returned");
        result
    });
    if let Err(error) = result {
        GlobalState::access(|state| state.set_egl_error(egl_error_from_gpu_error(&error)));
    }
    crate::stub::trace("glFlush.release", "driver state released");
}
/// `glFinish` incrementally submits pending OFFSCREEN work (same as `glFlush`) and returns after the
/// synchronous command sink has accepted it. This deferred driver has no in-flight executor between
/// swaps, so returning from the sink is the completion boundary.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glFinish() {
    crate::stub::trace("glFinish", "waiting for pending offscreen work");
    let max_buffer_bytes = GlobalState::access(|state| state.max_buffer_bytes);
    let result = GlobalState::gpu_submit(move |group, sink| {
        crate::stub::trace("glFinish.flush", "flushing offscreen work");
        let result = flush_pending(group, sink, max_buffer_bytes);
        crate::stub::trace("glFinish.flushed", "offscreen flush returned");
        result
    });
    if let Err(error) = result {
        GlobalState::access(|state| state.set_egl_error(egl_error_from_gpu_error(&error)));
    }
    crate::stub::trace("glFinish.release", "driver state released");
    crate::stub::trace("glFinish.complete", "completed");
}

#[cfg(test)]
#[path = "storage/tests.rs"]
mod capture_tests;
