use super::*;
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glVertexAttribPointer(
    index: u32,
    size: i32,
    type_: u32,
    normalized: u8,
    stride: i32,
    pointer: *const c_void,
) {
    crate::stub::trace(
        "glVertexAttribPointer",
        &format!(
            "index={index} size={size} type={type_:#x} normalized={normalized} stride={stride} pointer={pointer:p}"
        ),
    );
    GlobalState::context(|s| {
        record::vertex_attrib_pointer(
            &mut s.gl,
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
    crate::stub::trace("glEnableVertexAttribArray", &format!("attribute={index}"));
    GlobalState::context(|s| record::enable_vertex_attrib(&mut s.gl, index as usize));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDisableVertexAttribArray(index: u32) {
    GlobalState::context(|s| record::disable_vertex_attrib(&mut s.gl, index as usize));
}

/// `glVertexAttribDivisor(index, divisor)` — the instance-step rate for attribute `index` (`0` =
/// per-vertex, `>0` = per-instance). See [`record::vertex_attrib_divisor`].
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glVertexAttribDivisor(index: u32, divisor: u32) {
    GlobalState::context(|s| record::vertex_attrib_divisor(&mut s.gl, index as usize, divisor));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glClearColor(red: f32, green: f32, blue: f32, alpha: f32) {
    GlobalState::context(|s| record::clear_color(&mut s.gl, [red, green, blue, alpha]));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glViewport(x: i32, y: i32, width: i32, height: i32) {
    GlobalState::context(|s| record::viewport(&mut s.gl, [x, y, width, height]));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glScissor(x: i32, y: i32, width: i32, height: i32) {
    GlobalState::context(|s| record::scissor(&mut s.gl, [x, y, width, height]));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glEnable(cap: u32) {
    GlobalState::context(|s| record::enable(&mut s.gl, cap));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDisable(cap: u32) {
    GlobalState::context(|s| record::disable(&mut s.gl, cap));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glClearDepthf(d: f32) {
    GlobalState::context(|s| record::clear_depth(&mut s.gl, d));
}

/// Desktop OpenGL spelling used by Chromium's ANGLE/OpenGL dispatch.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glClearDepth(depth: f64) {
    glClearDepthf(depth as f32);
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBlendFunc(sfactor: u32, dfactor: u32) {
    GlobalState::context(|s| record::blend_func(&mut s.gl, sfactor, dfactor));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBlendFuncSeparate(src_rgb: u32, dst_rgb: u32, src_alpha: u32, dst_alpha: u32) {
    GlobalState::context(|s| {
        record::blend_func_separate(&mut s.gl, src_rgb, dst_rgb, src_alpha, dst_alpha)
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDepthFunc(func: u32) {
    GlobalState::context(|s| record::depth_func(&mut s.gl, func));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDepthMask(flag: u8) {
    GlobalState::context(|s| record::depth_mask(&mut s.gl, flag != 0));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glCullFace(mode: u32) {
    GlobalState::context(|s| record::cull_face(&mut s.gl, mode));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glFrontFace(mode: u32) {
    GlobalState::context(|s| record::front_face(&mut s.gl, mode));
}

// ==================================================================================================
// GLES: draw recording (frame draw-list; IR lowered at eglSwapBuffers)
// ==================================================================================================

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glClear(mask: u32) {
    crate::stub::trace("glClear", "recording a clear");
    GlobalState::context(|s| record::clear_buffers(&mut s.gl, mask));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDrawArrays(mode: u32, first: i32, count: i32) {
    crate::stub::trace("glDrawArrays", "recording an array draw");
    GlobalState::context(|s| record::draw_arrays(&mut s.gl, mode, first, count));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDrawElements(mode: u32, count: i32, type_: u32, indices: *const c_void) {
    crate::stub::trace("glDrawElements", "recording an indexed draw");
    let started = std::time::Instant::now();
    GlobalState::context(|s| {
        crate::stub::trace(
            "glDrawElements.state",
            &format!(
                "count={count} type={type_:#x} indices={indices:p} element_buffer={}",
                s.gl.buffer_for_target(GL_ELEMENT_ARRAY_BUFFER)
            ),
        );
        record::draw_elements(&mut s.gl, mode, count, type_, indices as usize)
    });
    crate::stub::trace(
        "glDrawElements.complete",
        &format!(
            "count={count} type={type_:#x} indices={indices:p} elapsed_us={}",
            started.elapsed().as_micros()
        ),
    );
}

/// `glDrawArraysInstanced(mode, first, count, instancecount)` — record an instanced array draw; the
/// frame builder lowers the recorded instance count into `Draw { instance_count }`.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDrawArraysInstanced(mode: u32, first: i32, count: i32, instancecount: i32) {
    GlobalState::context(|s| {
        record::draw_arrays_instanced(&mut s.gl, mode, first, count, instancecount)
    });
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
    GlobalState::context(|s| {
        record::draw_elements_instanced(
            &mut s.gl,
            mode,
            count,
            type_,
            indices as usize,
            instancecount,
        )
    });
}

// ==================================================================================================
// GLES: vertex array objects (GLES3 requires a bound VAO to draw)
// ==================================================================================================

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGenVertexArrays(n: i32, arrays: *mut u32) {
    if arrays.is_null() || n <= 0 {
        return;
    }
    GlobalState::context(|s| unsafe {
        for i in 0..n as isize {
            *arrays.offset(i) = record::gen_vertex_array(&mut s.gl);
        }
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBindVertexArray(array: u32) {
    crate::stub::trace("glBindVertexArray", &format!("array={array}"));
    GlobalState::context(|s| record::bind_vertex_array(&mut s.gl, array));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDeleteVertexArrays(n: i32, arrays: *const u32) {
    if arrays.is_null() || n <= 0 {
        return;
    }
    GlobalState::context(|s| unsafe {
        for i in 0..n as isize {
            record::delete_vertex_array(&mut s.gl, *arrays.offset(i));
        }
    });
}

/// `glIsVertexArray(array)` — `GL_TRUE`/`GL_FALSE`. Returns the `GLboolean` as the codegen's `u32` ABI
/// (the low byte is the boolean a C caller reads).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glIsVertexArray(array: u32) -> u32 {
    GlobalState::context(|s| record::is_vertex_array(&s.gl, array)) as u32
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
    crate::stub::trace("glGenFramebuffers", "allocating framebuffer names");
    if framebuffers.is_null() || n <= 0 {
        return;
    }
    GlobalState::context(|s| unsafe {
        for i in 0..n as isize {
            *framebuffers.offset(i) = s.gl.gen_framebuffer();
        }
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBindFramebuffer(target: u32, framebuffer: u32) {
    crate::stub::trace("glBindFramebuffer", "binding a framebuffer");
    GlobalState::context(|s| record::bind_framebuffer(&mut s.gl, target, framebuffer));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDeleteFramebuffers(n: i32, framebuffers: *const u32) {
    crate::stub::trace("glDeleteFramebuffers", "deleting framebuffer names");
    if framebuffers.is_null() || n <= 0 {
        return;
    }
    GlobalState::context(|s| unsafe {
        for i in 0..n as isize {
            record::delete_framebuffer(&mut s.gl, *framebuffers.offset(i));
        }
    });
}

/// `glIsFramebuffer(framebuffer)` — `GL_TRUE`/`GL_FALSE`. Returns the `GLboolean` as the codegen's `u32`
/// ABI (the low byte is the boolean a C caller reads), matching `glIsVertexArray`.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glIsFramebuffer(framebuffer: u32) -> u32 {
    GlobalState::context(|s| record::is_framebuffer(&s.gl, framebuffer)) as u32
}

/// `glCheckFramebufferStatus(target)` — the completeness enum of the bound draw/read framebuffer (see
/// [`record::check_framebuffer_status`]). A GLES app calls this before rendering to an FBO and bails on a
/// non-`GL_FRAMEBUFFER_COMPLETE` result.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glCheckFramebufferStatus(target: u32) -> u32 {
    let status = GlobalState::context(|s| record::check_framebuffer_status(&mut s.gl, target));
    crate::stub::trace(
        "glCheckFramebufferStatus",
        &format!("returning 0x{status:x}"),
    );
    status
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glFramebufferTexture2D(
    target: u32,
    attachment: u32,
    textarget: u32,
    texture: u32,
    level: i32,
) {
    crate::stub::trace(
        "glFramebufferTexture2D",
        "attaching a texture to a framebuffer",
    );
    GlobalState::context(|s| {
        record::framebuffer_texture_2d(&mut s.gl, target, attachment, textarget, texture, level)
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGenRenderbuffers(n: i32, renderbuffers: *mut u32) {
    if renderbuffers.is_null() || n <= 0 {
        return;
    }
    GlobalState::context(|s| unsafe {
        for i in 0..n as isize {
            *renderbuffers.offset(i) = record::gen_renderbuffer(&mut s.gl);
        }
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBindRenderbuffer(target: u32, renderbuffer: u32) {
    GlobalState::context(|s| record::bind_renderbuffer(&mut s.gl, target, renderbuffer));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDeleteRenderbuffers(n: i32, renderbuffers: *const u32) {
    if renderbuffers.is_null() || n <= 0 {
        return;
    }
    GlobalState::context(|s| unsafe {
        for i in 0..n as isize {
            s.delete_renderbuffer(*renderbuffers.offset(i));
        }
    });
}

/// `glIsRenderbuffer(renderbuffer)` — `GL_TRUE`/`GL_FALSE` as the codegen's `u32` ABI (low byte is the
/// boolean), matching `glIsFramebuffer`/`glIsVertexArray`.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glIsRenderbuffer(renderbuffer: u32) -> u32 {
    GlobalState::context(|s| record::is_renderbuffer(&s.gl, renderbuffer)) as u32
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glRenderbufferStorage(target: u32, internalformat: u32, width: i32, height: i32) {
    GlobalState::context(|s| {
        s.redefine_renderbuffer(|ctx| {
            record::renderbuffer_storage(ctx, target, internalformat, width, height)
        })
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glFramebufferRenderbuffer(
    target: u32,
    attachment: u32,
    renderbuffertarget: u32,
    renderbuffer: u32,
) {
    GlobalState::context(|s| {
        record::framebuffer_renderbuffer(
            &mut s.gl,
            target,
            attachment,
            renderbuffertarget,
            renderbuffer,
        )
    });
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
    GlobalState::context(|s| {
        record::blit_framebuffer(
            &mut s.gl, src_x0, src_y0, src_x1, src_y1, dst_x0, dst_y0, dst_x1, dst_y1, mask, filter,
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
    crate::stub::trace("glReadPixels", "reading framebuffer pixels");
    // Record the first GL error and bail (GL keeps the first error until glGetError clears it).
    let fail = |e: u32| GlobalState::context(|s| s.gl.set_gl_error(e));
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
    let pbo = GlobalState::context(|s| s.gl.buffer_for_target(GL_PIXEL_PACK_BUFFER));
    if pbo != 0 {
        let byte_off = pixels as usize; // GL: the offset is the `pixels` argument treated as an integer
        let packed = gpu_read_pixels(x, y, width, height, format);
        match packed {
            Ok(bytes) => GlobalState::context(|s| s.gl.buffers.set_sub_data(pbo, byte_off, &bytes)),
            Err(e) => {
                GlobalState::context(|s| s.gl.set_gl_error(GL_OUT_OF_MEMORY));
                GlobalState::access(|s| s.set_egl_error(egl_error_from_gpu_error(&e)));
            }
        }
        return;
    }
    if pixels.is_null() {
        fail(GL_INVALID_VALUE);
        return;
    }
    let packed = gpu_read_pixels(x, y, width, height, format);
    match packed {
        Ok(bytes) => {
            let n = bytes.len().min(width as usize * height as usize * bpp);
            unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), pixels as *mut u8, n) };
        }
        Err(e) => {
            GlobalState::context(|s| s.gl.set_gl_error(GL_OUT_OF_MEMORY));
            GlobalState::access(|s| s.set_egl_error(egl_error_from_gpu_error(&e)));
        }
    }
}
