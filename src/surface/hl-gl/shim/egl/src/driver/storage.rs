use super::*;
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
    GlobalState::access(|s| {
        record::tex_storage_2d(&mut s.ctx, target, 1, internalformat, width, height)
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
    GlobalState::access(|s| record::tex_storage_3d(&mut s.ctx, target, 1, width, height, depth));
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
    GlobalState::access(|s| {
        record::renderbuffer_storage(&mut s.ctx, target, internalformat, width, height)
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
/// `glTexParameterIiv` / `glTexParameterIuiv` — the integer (non-normalized) parameter vectors; reads
/// `params[0]` into the same filter/wrap setter the scalar path uses.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glTexParameterIiv(_target: u32, pname: u32, params: *const i32) {
    if params.is_null() {
        return;
    }
    let v = unsafe { *params };
    GlobalState::access(|s| record::tex_parameter(&mut s.ctx, pname, v as u32));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glTexParameterIuiv(_target: u32, pname: u32, params: *const u32) {
    if params.is_null() {
        return;
    }
    let v = unsafe { *params };
    GlobalState::access(|s| record::tex_parameter(&mut s.ctx, pname, v));
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
    GlobalState::access(|s| {
        // Read the source rect out (immutable borrow) …
        let rows: Option<Vec<u8>> = s.ctx.textures.get(src_name).and_then(|st| {
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
            s.ctx
                .textures
                .sub_image_2d(dst_name, dst_x, dst_y, src_width, src_height, &buf);
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
    GlobalState::access(|s| {
        record::vertex_attrib_pointer(
            &mut s.ctx,
            attribindex as usize,
            size,
            type_,
            normalized != 0,
            0,
            relativeoffset as usize,
        )
    });
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
        GlobalState::access(|s| record::bind_buffer(&mut s.ctx, GL_ARRAY_BUFFER, buffer));
    }
}
/// `glVertexBindingDivisor(bindingindex, divisor)` — the instance-step divisor for a binding slot; applied
/// to the attribute at that index (single-slot model).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glVertexBindingDivisor(bindingindex: u32, divisor: u32) {
    GlobalState::access(|s| {
        record::vertex_attrib_divisor(&mut s.ctx, bindingindex as usize, divisor)
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
    GlobalState::access(|s| {
        record::framebuffer_texture_2d(
            &mut s.ctx,
            target,
            attachment,
            GL_TEXTURE_2D,
            texture,
            level,
        )
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
    GlobalState::access(|s| {
        record::framebuffer_texture_2d(
            &mut s.ctx,
            target,
            attachment,
            GL_TEXTURE_2D,
            texture,
            level,
        )
    });
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

/// `glFlush` — NONBLOCKING: advance the submission serial, and incrementally flush any pending OFFSCREEN
/// draw work (see [`swap::flush_offscreen`]) so a multi-context app (Chrome's gpu-raster workers render into
/// FBOs and `glFlush` but never `eglSwapBuffers`) does not accumulate an unbounded draw-list into the
/// eventual swap frame. Window (default-framebuffer) draws are retained for `eglSwapBuffers`.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glFlush() {
    SUBMIT_SERIAL.fetch_add(1, Ordering::SeqCst);
    GlobalState::access(|s| {
        if let Err(e) = swap::flush_offscreen(&mut s.ctx, &mut s.sink) {
            // A flush is best-effort ordering, not a frame boundary: register the GL error but do not abort
            // (the retained draws surface again at the next flush/swap).
            s.set_egl_error(egl_error_from_gpu_error(&e));
        }
    });
}
/// `glFinish` — BLOCKING: advance the submission serial, incrementally flush pending OFFSCREEN work (same as
/// `glFlush` — bounds a multi-context app's draw-list), then catch completion up to the target (this deferred
/// model completes synchronously — there is no in-flight host executor to wait on between swaps).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glFinish() {
    let target = SUBMIT_SERIAL.fetch_add(1, Ordering::SeqCst) + 1;
    GlobalState::access(|s| {
        if let Err(e) = swap::flush_offscreen(&mut s.ctx, &mut s.sink) {
            s.set_egl_error(egl_error_from_gpu_error(&e));
        }
    });
    let mut done = COMPLETE_SERIAL.load(Ordering::SeqCst);
    while done < target {
        match COMPLETE_SERIAL.compare_exchange(done, target, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => break,
            Err(cur) => done = cur,
        }
    }
}
