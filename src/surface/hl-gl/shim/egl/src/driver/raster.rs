use super::*;

mod debug;
pub use debug::*;
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBlendEquation(mode: u32) {
    GlobalState::context(|s| record::blend_equation(&mut s.gl, mode));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBlendEquationSeparate(mode_rgb: u32, mode_alpha: u32) {
    GlobalState::context(|s| record::blend_equation_separate(&mut s.gl, mode_rgb, mode_alpha));
}
/// Per-draw-buffer blend variants: this model has a single color target, so buffer 0 delegates to the
/// global blend state and any other buffer index is an honest no-op.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBlendEquationi(buf: u32, mode: u32) {
    if buf == 0 {
        GlobalState::context(|s| record::blend_equation(&mut s.gl, mode));
    }
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBlendEquationSeparatei(buf: u32, mode_rgb: u32, mode_alpha: u32) {
    if buf == 0 {
        GlobalState::context(|s| record::blend_equation_separate(&mut s.gl, mode_rgb, mode_alpha));
    }
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBlendFunci(buf: u32, src: u32, dst: u32) {
    if buf == 0 {
        GlobalState::context(|s| record::blend_func(&mut s.gl, src, dst));
    }
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBlendFuncSeparatei(
    buf: u32,
    src_rgb: u32,
    dst_rgb: u32,
    src_alpha: u32,
    dst_alpha: u32,
) {
    if buf == 0 {
        GlobalState::context(|s| {
            record::blend_func_separate(&mut s.gl, src_rgb, dst_rgb, src_alpha, dst_alpha)
        });
    }
}
/// `glBlendColor` — the constant blend color used by `GL_*_CONSTANT_*` factors.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBlendColor(red: f32, green: f32, blue: f32, alpha: f32) {
    GlobalState::context(|s| record::blend_color(&mut s.gl, [red, green, blue, alpha]));
}
/// `glColorMask` / `glColorMaski` — record the per-channel framebuffer write mask; it lowers into every
/// color target's `ColorTargetState::write_mask`, so a masked channel (e.g. `glColorMask(1,1,1,0)` to
/// preserve framebuffer alpha, or an all-false mask for a depth-only pass) is honored rather than dropped.
/// `glColorMaski` targets one draw buffer; this single-target model routes buffer 0 to the global mask and
/// ignores other indices.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glColorMask(red: u8, green: u8, blue: u8, alpha: u8) {
    GlobalState::context(|s| {
        record::color_mask(&mut s.gl, red != 0, green != 0, blue != 0, alpha != 0)
    });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glColorMaski(index: u32, r: u8, g: u8, b: u8, a: u8) {
    if index == 0 {
        GlobalState::context(|s| record::color_mask(&mut s.gl, r != 0, g != 0, b != 0, a != 0));
    }
}
/// `glDepthRangef` — the model maps NDC depth directly (fixed 0..1 range), so a custom depth range carries
/// no lowered state: an honest no-op.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDepthRangef(_n: f32, _f: f32) {}
/// Desktop OpenGL spelling used by Chromium's ANGLE/OpenGL dispatch.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDepthRange(near: f64, far: f64) {
    glDepthRangef(near as f32, far as f32);
}
/// Point primitives are not emitted by the current IR, so point raster state has no observable effect.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glPointParameteri(_name: u32, _value: i32) {}
/// The current draw IR supports filled polygons. Reject desktop line/point modes explicitly.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glPolygonMode(face: u32, mode: u32) {
    const GL_FRONT_AND_BACK: u32 = 0x0408;
    const GL_FILL: u32 = 0x1B02;
    if face != GL_FRONT_AND_BACK || mode != GL_FILL {
        GlobalState::context(|state| state.gl.set_gl_error(GL_INVALID_ENUM));
    }
}
/// Indexed draws currently carry expanded index ranges, so no separate restart state is retained.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glPrimitiveRestartIndex(_index: u32) {}
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
/// `glStencilFunc` / `glStencilMask` / `glStencilOp` / `glClearStencil` — record the stencil test state.
/// A draw that enables `GL_STENCIL_TEST` lowers these into the pipeline's `DepthState` stencil faces +
/// masks + `Enc::SetStencilReference`, and the pass materializes a `Depth24PlusStencil8` attachment (whose
/// stencil plane clears to `glClearStencil`'s value) — see `service::frame`.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glStencilFunc(func: u32, ref_: i32, mask: u32) {
    GlobalState::context(|s| record::stencil_func(&mut s.gl, func, ref_, mask));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glStencilMask(mask: u32) {
    GlobalState::context(|s| record::stencil_mask(&mut s.gl, mask));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glStencilOp(fail: u32, zfail: u32, zpass: u32) {
    GlobalState::context(|s| record::stencil_op(&mut s.gl, fail, zfail, zpass));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glClearStencil(s: i32) {
    GlobalState::context(|state| record::clear_stencil(&mut state.gl, s));
}
/// `glPatchParameteri` — no tessellation stage is modeled: an honest no-op.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glPatchParameteri(_pname: u32, _value: i32) {}
/// `glPrimitiveBoundingBox` — a tessellation/geometry hint (OES_primitive_bounding_box): an honest no-op.
#[cfg_attr(gles_client, no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glPrimitiveBoundingBox(
    _min_x: f32,
    _min_y: f32,
    _min_z: f32,
    _min_w: f32,
    _max_x: f32,
    _max_y: f32,
    _max_z: f32,
    _max_w: f32,
) {
}
/// `glBlendBarrier` — an advanced-blend (KHR_blend_equation_advanced) barrier; this model lowers no
/// advanced blend, so there is nothing to order: an honest no-op.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBlendBarrier() {}
/// `glEnablei` / `glDisablei` — indexed enable; this single-target model routes buffer 0 to the global
/// capability and ignores other indices.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glEnablei(target: u32, index: u32) {
    if index == 0 {
        GlobalState::context(|s| record::enable(&mut s.gl, target));
    }
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDisablei(target: u32, index: u32) {
    if index == 0 {
        GlobalState::context(|s| record::disable(&mut s.gl, target));
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
    let n = GlobalState::context(|s| query::get_integerv(&s.gl, pname, &mut buf));
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
    let v = GlobalState::context(|s| query::get_integer_indexed(&s.gl, target, index));
    unsafe { *data = v as i32 };
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetInteger64i_v(target: u32, index: u32, data: *mut i64) {
    if data.is_null() {
        return;
    }
    let v = GlobalState::context(|s| query::get_integer_indexed(&s.gl, target, index));
    unsafe { *data = v };
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetBooleani_v(target: u32, index: u32, data: *mut u8) {
    if data.is_null() {
        return;
    }
    let v = GlobalState::context(|s| query::get_integer_indexed(&s.gl, target, index));
    unsafe { *data = (v != 0) as u8 };
}
/// `glGetInternalformativ(target, internalformat, pname, bufSize, params)` — supported sample counts for
/// an internal format. This model advertises single-sample rendering with `GL_MAX_SAMPLES` as the peak.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetInternalformativ(
    _target: u32,
    _internalformat: u32,
    pname: u32,
    buf_size: i32,
    params: *mut i32,
) {
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
pub extern "C" fn glGetShaderPrecisionFormat(
    _shadertype: u32,
    precisiontype: u32,
    range: *mut i32,
    precision: *mut i32,
) {
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
    let v = GlobalState::context(|s| intro::renderbuffer_parameter(&s.gl, target, pname));
    unsafe { *params = v };
}
/// `glGetFramebufferAttachmentParameteriv(target, attachment, pname, params)` — the bound framebuffer's
/// color-attachment object type + name (real reflection of the FBO's attachment).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetFramebufferAttachmentParameteriv(
    target: u32,
    attachment: u32,
    pname: u32,
    params: *mut i32,
) {
    if params.is_null() {
        return;
    }
    let v = GlobalState::context(|s| {
        intro::framebuffer_attachment_parameter(&s.gl, target, attachment, pname)
    });
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
    let v = GlobalState::context(|s| intro::tex_level_parameter(&s.gl, target, level, pname));
    unsafe { *params = v };
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetTexLevelParameterfv(target: u32, level: i32, pname: u32, params: *mut f32) {
    if params.is_null() {
        return;
    }
    let v = GlobalState::context(|s| intro::tex_level_parameter(&s.gl, target, level, pname));
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
/// `glGetGraphicsResetStatus()` — whether this context's share group has been terminated.
///
/// This used to answer `GL_NO_ERROR` unconditionally, on the reasoning that no robustness reset is
/// modeled. A share group that a transport failure has lost IS a reset: every object in it is gone and no
/// later call can succeed. Answering "healthy" is the same lie `glGetError` was telling, and it is the
/// one an application that has just been handed `GL_CONTEXT_LOST` calls next to find out whether to
/// rebuild. Unlike the error, this is a persistent condition and is not consumed by reading it.
///
/// `GL_UNKNOWN_CONTEXT_RESET` rather than `GL_GUILTY_`/`GL_INNOCENT_CONTEXT_RESET`: the group is lost by
/// the whole transport, and which context's work provoked it is not recoverable afterwards.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetGraphicsResetStatus() -> u32 {
    if GlobalState::context_is_lost() {
        return hl_gl::result::GL_UNKNOWN_CONTEXT_RESET;
    }
    GL_NO_ERROR
}
/// `glGetBufferPointerv(target, pname, params)` — `GL_BUFFER_MAP_POINTER`: the pointer a live
/// `glMapBufferRange` / `glMapBufferOES` handed out for the buffer bound to `target`, or null when it is not
/// mapped (ES 3.0 §6.1.14 / `GL_OES_mapbuffer`). Any other `pname` is `GL_INVALID_ENUM`.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetBufferPointerv(target: u32, pname: u32, params: *mut *mut c_void) {
    const GL_BUFFER_MAP_POINTER: u32 = 0x88BD;
    let pointer = GlobalState::context(|s| {
        if pname != GL_BUFFER_MAP_POINTER {
            s.gl.set_gl_error(GL_INVALID_ENUM);
            return core::ptr::null_mut();
        }
        let name = s.gl.buffer_for_target(target);
        if name == 0 {
            s.gl.set_gl_error(GL_INVALID_OPERATION);
            return core::ptr::null_mut();
        }
        let Some((offset, _)) = s.gl.buffers.get(name).and_then(|buffer| buffer.mapped) else {
            return core::ptr::null_mut();
        };
        s.gl.buffers
            .mapped_ptr(name, offset)
            .map_or(core::ptr::null_mut(), |pointer| pointer.cast())
    });
    if !params.is_null() {
        unsafe { *params = pointer };
    }
}
/// `glGetBufferPointervOES(target, pname, params)` — `GL_OES_mapbuffer`'s spelling of the same query.
/// The extension is advertised and supplies `glMapBufferOES`/`glUnmapBufferOES`, so a client resolves this
/// name too and calls it without a null check; only the OES spelling was missing.
// Resolved through `eglGetProcAddress` beside `glMapBufferOES`; not part of the pinned export surface.
pub extern "C" fn glGetBufferPointervOES(target: u32, pname: u32, params: *mut *mut c_void) {
    glGetBufferPointerv(target, pname, params);
}
/// `glGetPointerv(pname, params)` — a KHR_debug callback/pointer query; no such pointer state is modeled.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetPointerv(_pname: u32, params: *mut *mut c_void) {
    if !params.is_null() {
        unsafe { *params = core::ptr::null_mut() };
    }
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetPointervKHR(pname: u32, params: *mut *mut c_void) {
    glGetPointerv(pname, params);
}
/// `glGetProgramBinary(...)` — no program-binary formats are advertised (`GL_NUM_PROGRAM_BINARY_FORMATS`
/// == 0), so the driver forces the source-compile path: an empty binary (length 0, format 0).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetProgramBinary(
    _program: u32,
    _buf_size: i32,
    length: *mut i32,
    binary_format: *mut u32,
    _binary: *mut c_void,
) {
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
pub extern "C" fn glProgramBinary(
    program: u32,
    _binary_format: u32,
    _binary: *const c_void,
    _length: i32,
) {
    GlobalState::context(|s| {
        if s.gl.programs.contains(program) {
            s.gl.set_gl_error(GL_INVALID_ENUM);
        } else {
            s.gl.set_gl_error(GL_INVALID_OPERATION);
        }
    });
}
