use super::*;
// ==================================================================================================
// GLES3.0: scoped buffer clears (glClearBuffer*)
// ==================================================================================================

/// `glClearBufferfv(buffer, drawbuffer, value)` — a `GL_COLOR` clear records a scoped full-surface clear
/// at the float color; a `GL_DEPTH` clear is an honest no-op (no depth attachment is modeled).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glClearBufferfv(buffer: u32, _drawbuffer: i32, value: *const f32) {
    if buffer == GL_COLOR && !value.is_null() {
        let c = unsafe { std::slice::from_raw_parts(value, 4) };
        GlobalState::access(|s| record::clear_buffer_color(&mut s.ctx, [c[0], c[1], c[2], c[3]]));
    }
}

/// `glClearBufferiv(buffer, drawbuffer, value)` — an integer color-buffer clear; records the clear with
/// the values cast to the model's float clear color (a `GL_STENCIL` clear is an honest no-op).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glClearBufferiv(buffer: u32, _drawbuffer: i32, value: *const i32) {
    if buffer == GL_COLOR && !value.is_null() {
        let c = unsafe { std::slice::from_raw_parts(value, 4) };
        GlobalState::access(|s| {
            record::clear_buffer_color(
                &mut s.ctx,
                [c[0] as f32, c[1] as f32, c[2] as f32, c[3] as f32],
            )
        });
    }
}

/// `glClearBufferuiv(buffer, drawbuffer, value)` — the unsigned-integer color-buffer clear.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glClearBufferuiv(buffer: u32, _drawbuffer: i32, value: *const u32) {
    if buffer == GL_COLOR && !value.is_null() {
        let c = unsafe { std::slice::from_raw_parts(value, 4) };
        GlobalState::access(|s| {
            record::clear_buffer_color(
                &mut s.ctx,
                [c[0] as f32, c[1] as f32, c[2] as f32, c[3] as f32],
            )
        });
    }
}

/// `glClearBufferfi(GL_DEPTH_STENCIL, drawbuffer, depth, stencil)` — a combined depth+stencil clear.
/// Records both the depth-clear and stencil-clear values; a stencil-testing pass lowers the stencil value
/// into its `Depth24PlusStencil8` attachment's clear (see `service::frame`).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glClearBufferfi(_buffer: u32, _drawbuffer: i32, depth: f32, stencil: i32) {
    GlobalState::access(|s| {
        record::clear_depth(&mut s.ctx, depth);
        record::clear_stencil(&mut s.ctx, stencil);
    });
}

// ==================================================================================================
// GLES3.x: draw extensions (base-vertex / range / indirect) — real recorded draws
// ==================================================================================================

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDrawElementsBaseVertex(
    mode: u32,
    count: i32,
    type_: u32,
    indices: *const c_void,
    basevertex: i32,
) {
    GlobalState::access(|s| {
        record::draw_elements_base_vertex(
            &mut s.ctx,
            mode,
            count,
            type_,
            indices as usize,
            basevertex,
        )
    });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDrawElementsInstancedBaseVertex(
    mode: u32,
    count: i32,
    type_: u32,
    indices: *const c_void,
    instancecount: i32,
    basevertex: i32,
) {
    GlobalState::access(|s| {
        record::draw_elements_instanced_base_vertex(
            &mut s.ctx,
            mode,
            count,
            type_,
            indices as usize,
            instancecount,
            basevertex,
        )
    });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDrawRangeElements(
    mode: u32,
    start: u32,
    end: u32,
    count: i32,
    type_: u32,
    indices: *const c_void,
) {
    GlobalState::access(|s| {
        record::draw_range_elements(
            &mut s.ctx,
            mode,
            start,
            end,
            count,
            type_,
            indices as usize,
            0,
        )
    });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDrawRangeElementsBaseVertex(
    mode: u32,
    start: u32,
    end: u32,
    count: i32,
    type_: u32,
    indices: *const c_void,
    basevertex: i32,
) {
    GlobalState::access(|s| {
        record::draw_range_elements(
            &mut s.ctx,
            mode,
            start,
            end,
            count,
            type_,
            indices as usize,
            basevertex,
        )
    });
}
/// `glDrawArraysIndirect(mode, indirect)` — `indirect` is a byte offset INTO the buffer bound to
/// `GL_DRAW_INDIRECT_BUFFER` (a GLES3.1 draw always sources the indirect params from a buffer object).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDrawArraysIndirect(mode: u32, indirect: *const c_void) {
    GlobalState::access(|s| record::draw_arrays_indirect(&mut s.ctx, mode, indirect as usize));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDrawElementsIndirect(mode: u32, type_: u32, indirect: *const c_void) {
    GlobalState::access(|s| {
        record::draw_elements_indirect(&mut s.ctx, mode, type_, indirect as usize)
    });
}

// ==================================================================================================
// program / shader lifecycle + object-existence queries (glDelete* / glIs* / glGet*)
// ==================================================================================================

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDeleteProgram(program: u32) {
    GlobalState::access(|s| record::delete_program(&mut s.ctx, program));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDeleteShader(shader: u32) {
    GlobalState::access(|s| record::delete_shader(&mut s.ctx, shader));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDetachShader(program: u32, shader: u32) {
    GlobalState::access(|s| record::detach_shader(&mut s.ctx, program, shader));
}
/// `glValidateProgram(program)` — a linked program validates clean in this model; an unknown program
/// raises `GL_INVALID_VALUE` (the getter path already reports `GL_VALIDATE_STATUS` from the link state).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glValidateProgram(program: u32) {
    GlobalState::access(|s| {
        if !s.ctx.programs.contains(program) {
            s.ctx.set_gl_error(GL_INVALID_VALUE);
        }
    });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glIsProgram(program: u32) -> u32 {
    GlobalState::access(|s| s.ctx.programs.contains(program)) as u32
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glIsShader(shader: u32) -> u32 {
    GlobalState::access(|s| s.ctx.programs.shader_exists(shader)) as u32
}
/// `glIsBuffer(buffer)` — true once `buffer` names a live buffer object (this model materializes the
/// object at `glGenBuffers`, so a generated name reads back as a buffer).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glIsBuffer(buffer: u32) -> u32 {
    GlobalState::access(|s| buffer != 0 && s.ctx.buffers.get(buffer).is_some()) as u32
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glIsTexture(texture: u32) -> u32 {
    GlobalState::access(|s| texture != 0 && s.ctx.textures.get(texture).is_some()) as u32
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glIsEnabled(cap: u32) -> u32 {
    GlobalState::access(|s| s.ctx.is_enabled(cap)) as u32
}
/// `glIsEnabledi(target, index)` — this model tracks no per-index (indexed) enable state, so it reports
/// the non-indexed capability's state (the honest answer for a single-target model).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glIsEnabledi(target: u32, _index: u32) -> u32 {
    GlobalState::access(|s| s.ctx.is_enabled(target)) as u32
}
/// `glGetAttachedShaders(program, maxCount, count, shaders)` — the program's attached vertex/fragment/
/// compute shader names (real reflection of the attachment slots).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetAttachedShaders(
    program: u32,
    max_count: i32,
    count: *mut i32,
    shaders: *mut u32,
) {
    let attached: Vec<u32> = GlobalState::access(|s| {
        s.ctx
            .programs
            .program(program)
            .map(|p| [p.vs, p.fs, p.cs].into_iter().filter(|&x| x != 0).collect())
            .unwrap_or_default()
    });
    let n = (max_count.max(0) as usize).min(attached.len());
    if !shaders.is_null() {
        for (i, &sh) in attached.iter().take(n).enumerate() {
            unsafe { *shaders.add(i) = sh };
        }
    }
    if !count.is_null() {
        unsafe { *count = n as i32 };
    }
}
/// `glGetShaderSource(shader, bufSize, length, source)` — the exact GLSL-ES source stored at
/// `glShaderSource` (real; `glGetShaderiv(GL_SHADER_SOURCE_LENGTH)` reports its matching length).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetShaderSource(
    shader: u32,
    buf_size: i32,
    length: *mut i32,
    source: *mut c_char,
) {
    let src = GlobalState::access(|s| s.ctx.get_shader_source(shader));
    unsafe { write_c_name(src.as_bytes(), buf_size, length, source) };
}
/// `glGetFragDataLocation(program, name)` — the fragment output's color index (real reflection).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetFragDataLocation(program: u32, name: *const c_char) -> i32 {
    let want = match unsafe { Text::read(name) } {
        Some(n) => n,
        None => return -1,
    };
    GlobalState::access(|s| intro::frag_data_location(&s.ctx, program, &want))
}

// ==================================================================================================
// fixed-function state — REAL where the model tracks it, honest no-op where it backs no state
// ==================================================================================================
