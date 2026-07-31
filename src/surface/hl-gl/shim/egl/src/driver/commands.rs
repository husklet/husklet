use super::*;
// ==================================================================================================
// GLES3.0: scoped buffer clears (glClearBuffer*)
// ==================================================================================================

fn clear_selector(ctx: &mut GlContext, buffer: u32, drawbuffer: i32, allowed: &[u32]) -> bool {
    if !allowed.contains(&buffer) {
        ctx.set_gl_error(GL_INVALID_ENUM);
        return false;
    }
    let valid_drawbuffer = if buffer == GL_COLOR {
        (0..query::MAX_DRAW_BUFFERS).contains(&drawbuffer)
    } else {
        drawbuffer == 0
    };
    if !valid_drawbuffer {
        ctx.set_gl_error(GL_INVALID_VALUE);
    }
    valid_drawbuffer
}

fn null_clear_value(ctx: &mut GlContext) {
    ctx.set_gl_error(GL_INVALID_VALUE);
}

/// `glClearBufferfv(buffer, drawbuffer, value)` — clears a floating-point color buffer or depth buffer.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glClearBufferfv(buffer: u32, drawbuffer: i32, value: *const f32) {
    GlobalState::context(|s| {
        if !clear_selector(&mut s.gl, buffer, drawbuffer, &[GL_COLOR, GL_DEPTH]) {
            return;
        }
        if value.is_null() {
            null_clear_value(&mut s.gl);
            return;
        }
        if buffer == GL_COLOR {
            // SAFETY: the GLES contract requires four color components and null was rejected above.
            let c = unsafe { std::slice::from_raw_parts(value, 4) };
            // This backend exposes one color attachment. Other valid draw-buffer selectors name absent
            // attachments and therefore have no effect.
            if drawbuffer == 0 {
                record::clear_buffer_color(&mut s.gl, [c[0], c[1], c[2], c[3]]);
            }
        } else {
            // SAFETY: GL_DEPTH consumes exactly one component and null was rejected above.
            record::clear_depth(&mut s.gl, unsafe { *value });
            // A DEPTH selector clears depth ONLY — recording a color clear here repainted the color buffer.
            record::clear_buffers(&mut s.gl, GL_DEPTH_BUFFER_BIT);
        }
    });
}

/// `glClearBufferiv(buffer, drawbuffer, value)` — clears a signed-integer color buffer or stencil buffer.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glClearBufferiv(buffer: u32, drawbuffer: i32, value: *const i32) {
    GlobalState::context(|s| {
        if !clear_selector(&mut s.gl, buffer, drawbuffer, &[GL_COLOR, GL_STENCIL]) {
            return;
        }
        if value.is_null() {
            null_clear_value(&mut s.gl);
            return;
        }
        if buffer == GL_COLOR {
            // SAFETY: the GLES contract requires four color components and null was rejected above.
            let c = unsafe { std::slice::from_raw_parts(value, 4) };
            if drawbuffer == 0 {
                record::clear_buffer_color(
                    &mut s.gl,
                    [c[0] as f32, c[1] as f32, c[2] as f32, c[3] as f32],
                );
            }
        } else {
            // SAFETY: GL_STENCIL consumes exactly one component and null was rejected above.
            record::clear_stencil(&mut s.gl, unsafe { *value });
            record::clear_buffers(&mut s.gl, GL_STENCIL_BUFFER_BIT);
        }
    });
}

/// `glClearBufferuiv(buffer, drawbuffer, value)` — the unsigned-integer color-buffer clear.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glClearBufferuiv(buffer: u32, drawbuffer: i32, value: *const u32) {
    GlobalState::context(|s| {
        if !clear_selector(&mut s.gl, buffer, drawbuffer, &[GL_COLOR]) {
            return;
        }
        if value.is_null() {
            null_clear_value(&mut s.gl);
            return;
        }
        // SAFETY: the GLES contract requires four color components and null was rejected above.
        let c = unsafe { std::slice::from_raw_parts(value, 4) };
        if drawbuffer == 0 {
            record::clear_buffer_color(
                &mut s.gl,
                [c[0] as f32, c[1] as f32, c[2] as f32, c[3] as f32],
            );
        }
    });
}

/// `glClearBufferfi(GL_DEPTH_STENCIL, drawbuffer, depth, stencil)` — a combined depth+stencil clear.
/// Records both the depth-clear and stencil-clear values; a stencil-testing pass lowers the stencil value
/// into its `Depth24PlusStencil8` attachment's clear (see `service::frame`).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glClearBufferfi(buffer: u32, drawbuffer: i32, depth: f32, stencil: i32) {
    GlobalState::context(|s| {
        if !clear_selector(&mut s.gl, buffer, drawbuffer, &[GL_DEPTH_STENCIL]) {
            return;
        }
        record::clear_depth(&mut s.gl, depth);
        record::clear_stencil(&mut s.gl, stencil);
        record::clear_buffers(&mut s.gl, GL_DEPTH_BUFFER_BIT | GL_STENCIL_BUFFER_BIT);
    });
}

#[cfg(test)]
mod clear_tests {
    use super::*;

    #[test]
    fn selectors_distinguish_enum_and_drawbuffer_errors() {
        let mut context = GlContext::new();
        assert!(!clear_selector(&mut context, 0xDEAD, 0, &[GL_COLOR]));
        assert_eq!(context.take_gl_error(), GL_INVALID_ENUM);

        assert!(!clear_selector(
            &mut context,
            GL_COLOR,
            query::MAX_DRAW_BUFFERS,
            &[GL_COLOR]
        ));
        assert_eq!(context.take_gl_error(), GL_INVALID_VALUE);

        assert!(!clear_selector(&mut context, GL_DEPTH, 1, &[GL_DEPTH]));
        assert_eq!(context.take_gl_error(), GL_INVALID_VALUE);
        assert!(clear_selector(
            &mut context,
            GL_COLOR,
            query::MAX_DRAW_BUFFERS - 1,
            &[GL_COLOR]
        ));
    }

    #[test]
    fn depth_stencil_values_are_captured_by_one_scoped_clear() {
        let mut context = GlContext::new();
        record::clear_depth(&mut context, 0.25);
        record::clear_stencil(&mut context, 7);
        record::clear(&mut context);

        assert_eq!(context.draws().len(), 1);
        let mut depth = [0.0; 4];
        query::get_floatv(&context, GL_DEPTH_CLEAR_VALUE, &mut depth);
        assert_eq!(depth[0], 0.25);
        let mut stencil = [0; 4];
        query::get_integerv(&context, GL_STENCIL_CLEAR_VALUE, &mut stencil);
        assert_eq!(stencil[0], 7);
    }

    #[test]
    fn integer_color_conversion_preserves_signedness_before_format_clamping() {
        let signed = [i32::MIN, -1, 1, i32::MAX].map(|value| value as f32);
        let unsigned = [0, 1, u32::MAX - 1, u32::MAX].map(|value| value as f32);
        assert!(signed[0].is_sign_negative());
        assert_eq!(signed[1], -1.0);
        assert_eq!(signed[2], 1.0);
        assert_eq!(unsigned[0], 0.0);
        assert_eq!(unsigned[1], 1.0);
        assert!(unsigned[3] > i32::MAX as f32);
    }
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
    GlobalState::context(|s| {
        record::draw_elements_base_vertex(
            &mut s.gl,
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
    GlobalState::context(|s| {
        record::draw_elements_instanced_base_vertex(
            &mut s.gl,
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
    GlobalState::context(|s| {
        record::draw_range_elements(
            &mut s.gl,
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
    GlobalState::context(|s| {
        record::draw_range_elements(
            &mut s.gl,
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
    GlobalState::context(|s| record::draw_arrays_indirect(&mut s.gl, mode, indirect as usize));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDrawElementsIndirect(mode: u32, type_: u32, indirect: *const c_void) {
    GlobalState::context(|s| {
        record::draw_elements_indirect(&mut s.gl, mode, type_, indirect as usize)
    });
}

// ==================================================================================================
// program / shader lifecycle + object-existence queries (glDelete* / glIs* / glGet*)
// ==================================================================================================

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDeleteProgram(program: u32) {
    GlobalState::context(|s| s.delete_program(program));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDeleteShader(shader: u32) {
    GlobalState::context(|s| record::delete_shader(&mut s.gl, shader));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDetachShader(program: u32, shader: u32) {
    GlobalState::context(|s| record::detach_shader(&mut s.gl, program, shader));
}
/// `glValidateProgram(program)` — a linked program validates clean in this model; an unknown program
/// raises `GL_INVALID_VALUE` (the getter path already reports `GL_VALIDATE_STATUS` from the link state).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glValidateProgram(program: u32) {
    GlobalState::context(|s| {
        if !s.gl.programs.contains(program) {
            s.gl.set_gl_error(GL_INVALID_VALUE);
        }
    });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glIsProgram(program: u32) -> u32 {
    GlobalState::context(|s| s.gl.is_program_name(program)) as u32
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glIsShader(shader: u32) -> u32 {
    GlobalState::context(|s| s.gl.programs.shader_exists(shader)) as u32
}
/// `glIsBuffer(buffer)` — true once `buffer` names a live buffer object (this model materializes the
/// object at `glGenBuffers`, so a generated name reads back as a buffer).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glIsBuffer(buffer: u32) -> u32 {
    GlobalState::context(|s| s.gl.is_buffer_name(buffer)) as u32
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glIsTexture(texture: u32) -> u32 {
    GlobalState::context(|s| s.gl.is_texture_name(texture)) as u32
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glIsEnabled(cap: u32) -> u32 {
    GlobalState::context(|s| s.gl.is_enabled(cap)) as u32
}
/// `glIsEnabledi(target, index)` — this model tracks no per-index (indexed) enable state, so it reports
/// the non-indexed capability's state (the honest answer for a single-target model).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glIsEnabledi(target: u32, _index: u32) -> u32 {
    GlobalState::context(|s| s.gl.is_enabled(target)) as u32
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
    let attached: Vec<u32> = GlobalState::context(|s| {
        s.gl.programs
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
    let src = GlobalState::context(|s| s.gl.get_shader_source(shader));
    unsafe { write_c_name(src.as_bytes(), buf_size, length, source) };
}
/// `glGetFragDataLocation(program, name)` — the fragment output's color index (real reflection).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetFragDataLocation(program: u32, name: *const c_char) -> i32 {
    let want = match unsafe { Text::read(name) } {
        Some(n) => n,
        None => return -1,
    };
    GlobalState::context(|s| intro::frag_data_location(&s.gl, program, &want))
}

// ==================================================================================================
// fixed-function state — REAL where the model tracks it, honest no-op where it backs no state
// ==================================================================================================
