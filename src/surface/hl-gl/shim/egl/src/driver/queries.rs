use super::*;
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGenQueries(n: i32, ids: *mut u32) {
    if ids.is_null() || n <= 0 {
        return;
    }
    GlobalState::access(|s| unsafe {
        for i in 0..n as isize {
            *ids.offset(i) = s.ctx.queries.gen();
        }
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDeleteQueries(n: i32, ids: *const u32) {
    if n < 0 {
        GlobalState::access(|s| s.ctx.set_gl_error(GL_INVALID_VALUE));
        return;
    }
    if ids.is_null() {
        return;
    }
    GlobalState::access(|s| unsafe {
        for i in 0..n as isize {
            s.ctx.queries.delete(*ids.offset(i));
        }
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBeginQuery(target: u32, id: u32) {
    GlobalState::access(|s| es3::begin_query(&mut s.ctx, target, id));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glEndQuery(target: u32) {
    GlobalState::access(|s| s.ctx.end_query(target));
}

/// `glIsQuery(id)` — `GLboolean` in the codegen's `u8` ABI.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glIsQuery(id: u32) -> u8 {
    GlobalState::access(|s| s.ctx.queries.contains(id)) as u8
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetQueryiv(target: u32, pname: u32, params: *mut i32) {
    let v = GlobalState::access(|s| es3::get_queryiv(&mut s.ctx, target, pname));
    if let (Some(v), false) = (v, params.is_null()) {
        unsafe { *params = v };
    }
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetQueryObjectuiv(id: u32, pname: u32, params: *mut u32) {
    let v = GlobalState::access(|s| es3::get_query_objectuiv(&mut s.ctx, id, pname));
    if let (Some(v), false) = (v, params.is_null()) {
        unsafe { *params = v };
    }
}

// ==================================================================================================
// ES3 transform-feedback objects (client-side lifecycle + per-program varying capture)
// ==================================================================================================

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGenTransformFeedbacks(n: i32, ids: *mut u32) {
    if ids.is_null() || n <= 0 {
        return;
    }
    GlobalState::access(|s| unsafe {
        for i in 0..n as isize {
            *ids.offset(i) = s.ctx.transform_feedbacks.gen();
        }
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDeleteTransformFeedbacks(n: i32, ids: *const u32) {
    if n < 0 {
        GlobalState::access(|s| s.ctx.set_gl_error(GL_INVALID_VALUE));
        return;
    }
    if ids.is_null() {
        return;
    }
    GlobalState::access(|s| unsafe {
        for i in 0..n as isize {
            s.ctx.delete_transform_feedback(*ids.offset(i));
        }
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBindTransformFeedback(target: u32, id: u32) {
    GlobalState::access(|s| es3::bind_transform_feedback(&mut s.ctx, target, id));
}

/// `glIsTransformFeedback(id)` — `GLboolean` in the codegen's `u8` ABI.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glIsTransformFeedback(id: u32) -> u8 {
    GlobalState::access(|s| s.ctx.transform_feedbacks.contains(id)) as u8
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBeginTransformFeedback(primitive_mode: u32) {
    GlobalState::access(|s| s.ctx.begin_transform_feedback(primitive_mode));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glEndTransformFeedback() {
    GlobalState::access(|s| s.ctx.end_transform_feedback());
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glPauseTransformFeedback() {
    GlobalState::access(|s| s.ctx.pause_transform_feedback());
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glResumeTransformFeedback() {
    GlobalState::access(|s| s.ctx.resume_transform_feedback());
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glTransformFeedbackVaryings(
    program: u32,
    count: i32,
    varyings: *const *const c_char,
    buffer_mode: u32,
) {
    // Marshal the NUL-terminated name array up front (a null entry with count>0 is GL_INVALID_VALUE).
    if count < 0 {
        GlobalState::access(|s| s.ctx.set_gl_error(GL_INVALID_VALUE));
        return;
    }
    let mut names = Vec::with_capacity(count as usize);
    if count > 0 {
        if varyings.is_null() {
            GlobalState::access(|s| s.ctx.set_gl_error(GL_INVALID_VALUE));
            return;
        }
        for i in 0..count as isize {
            match unsafe { Text::read(*varyings.offset(i)) } {
                Some(name) => names.push(name),
                None => {
                    GlobalState::access(|s| s.ctx.set_gl_error(GL_INVALID_VALUE));
                    return;
                }
            }
        }
    }
    GlobalState::access(|s| {
        es3::transform_feedback_varyings(&mut s.ctx, program, names, buffer_mode)
    });
}

/// `glGetTransformFeedbackVarying(program, index, …)` — report the captured varying's name (real state)
/// plus a best-effort `size = 1`, `type = GL_FLOAT_VEC4` (no GLSL reflection). Out of range →
/// `GL_INVALID_VALUE` + empty name.
#[cfg_attr(gles_client, no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glGetTransformFeedbackVarying(
    program: u32,
    index: u32,
    buf_size: i32,
    length: *mut i32,
    size: *mut i32,
    type_: *mut u32,
    name: *mut c_char,
) {
    let varying = GlobalState::access(|s| es3::transform_feedback_varying(&s.ctx, program, index));
    match varying {
        Some(vname) => unsafe {
            if !size.is_null() {
                *size = 1;
            }
            if !type_.is_null() {
                *type_ = GL_FLOAT_VEC4;
            }
            write_c_name(vname.as_bytes(), buf_size, length, name);
        },
        None => {
            GlobalState::access(|s| s.ctx.set_gl_error(GL_INVALID_VALUE));
            unsafe {
                if !size.is_null() {
                    *size = 0;
                }
                if !type_.is_null() {
                    *type_ = 0;
                }
                write_c_name(&[], buf_size, length, name);
            }
        }
    }
}

/// Write a NUL-terminated name into `out` (capacity `buf_size`, incl. terminator) and report the char
/// count written (excl. NUL) in `length`. Null-safe on both out-params.
pub(super) unsafe fn write_c_name(bytes: &[u8], buf_size: i32, length: *mut i32, out: *mut c_char) {
    let mut written = 0i32;
    if !out.is_null() && buf_size > 0 {
        let cap = (buf_size - 1) as usize;
        let n = bytes.len().min(cap);
        for (i, &b) in bytes.iter().take(n).enumerate() {
            *out.add(i) = b as c_char;
        }
        *out.add(n) = 0;
        written = n as i32;
    }
    if !length.is_null() {
        *length = written;
    }
}

// ==================================================================================================
// ES3 separate-shader program pipelines (client-side object state)
// ==================================================================================================

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGenProgramPipelines(n: i32, pipelines: *mut u32) {
    if pipelines.is_null() || n <= 0 {
        return;
    }
    GlobalState::access(|s| unsafe {
        for i in 0..n as isize {
            *pipelines.offset(i) = s.ctx.program_pipelines.gen();
        }
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDeleteProgramPipelines(n: i32, pipelines: *const u32) {
    if n < 0 {
        GlobalState::access(|s| s.ctx.set_gl_error(GL_INVALID_VALUE));
        return;
    }
    if pipelines.is_null() {
        return;
    }
    GlobalState::access(|s| unsafe {
        for i in 0..n as isize {
            s.ctx.program_pipelines.delete(*pipelines.offset(i));
        }
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glBindProgramPipeline(pipeline: u32) {
    GlobalState::access(|s| s.ctx.bind_program_pipeline(pipeline));
}

/// `glIsProgramPipeline(pipeline)` — `GLboolean` in the codegen's `u8` ABI.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glIsProgramPipeline(pipeline: u32) -> u8 {
    GlobalState::access(|s| s.ctx.program_pipelines.contains(pipeline)) as u8
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUseProgramStages(pipeline: u32, stages: u32, program: u32) {
    GlobalState::access(|s| es3::use_program_stages(&mut s.ctx, pipeline, stages, program));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glActiveShaderProgram(pipeline: u32, program: u32) {
    GlobalState::access(|s| es3::active_shader_program(&mut s.ctx, pipeline, program));
}

/// `glProgramParameteri(program, pname, value)` — only `GL_PROGRAM_SEPARABLE` is modeled (a linked
/// program is separable by construction here, so the flag is accepted). An unknown program →
/// `GL_INVALID_VALUE`; an unmodeled `pname` → `GL_INVALID_ENUM`.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramParameteri(program: u32, pname: u32, value: i32) {
    let _ = value;
    GlobalState::access(|s| {
        if program == 0 || s.ctx.programs.program(program).is_none() {
            s.ctx.set_gl_error(GL_INVALID_VALUE);
        } else if pname != GL_PROGRAM_SEPARABLE {
            s.ctx.set_gl_error(GL_INVALID_ENUM);
        }
    });
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetProgramPipelineiv(pipeline: u32, pname: u32, params: *mut i32) {
    let v = GlobalState::access(|s| es3::get_program_pipelineiv(&mut s.ctx, pipeline, pname));
    if let (Some(v), false) = (v, params.is_null()) {
        unsafe { *params = v };
    }
}

/// `glGetProgramPipelineInfoLog` — the pipeline validates clean, so the log is empty (length 0).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetProgramPipelineInfoLog(
    _pipeline: u32,
    buf_size: i32,
    length: *mut i32,
    info_log: *mut c_char,
) {
    unsafe { write_empty_info_log(buf_size, length, info_log) };
}

/// `glValidateProgramPipeline(pipeline)` — an unknown pipeline raises `GL_INVALID_OPERATION`; a known one
/// validates clean (the pipeline carries no cross-stage interface to reject in this model).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glValidateProgramPipeline(pipeline: u32) {
    // A known pipeline validates clean; an unknown one raises GL_INVALID_OPERATION (via the getter).
    GlobalState::access(|s| {
        let _ = es3::get_program_pipelineiv(&mut s.ctx, pipeline, GL_VALIDATE_STATUS);
    });
}

/// `glCreateShaderProgramv(type, count, strings)` — create + compile + link a single-stage separable
/// program from the joined source (a real body: the ES3 convenience constructor). Returns the new program
/// name, or `0` on a bad `type` / empty source.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glCreateShaderProgramv(
    type_: u32,
    count: i32,
    strings: *const *const c_char,
) -> u32 {
    if !matches!(
        type_,
        GL_VERTEX_SHADER | GL_FRAGMENT_SHADER | GL_COMPUTE_SHADER
    ) {
        GlobalState::access(|s| s.ctx.set_gl_error(GL_INVALID_ENUM));
        return 0;
    }
    let src = unsafe { join_source(count, strings, core::ptr::null()) };
    GlobalState::access(|s| {
        let sh = record::create_shader(&mut s.ctx, type_);
        record::shader_source(&mut s.ctx, sh, &src);
        record::compile_shader(&mut s.ctx, sh);
        let prog = record::create_program(&mut s.ctx);
        record::attach_shader(&mut s.ctx, prog, sh);
        let _ = record::link_program(&mut s.ctx, prog);
        prog
    })
}
