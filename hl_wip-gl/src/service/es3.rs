//! ES3 client-side object services: sampler objects, query objects, transform-feedback objects, and
//! separate-shader program-pipeline objects.
//!
//! These mutate the [`crate::model::es3`] tables on [`GlContext`] and submit NOTHING — a real driver
//! emits no GPU IR for any of them (they carry observable object STATE the app polls back). Ported from
//! `hl-shim-gl/src/gles.rs` (the `glGenSamplers`/`glBeginQuery`/`glBindTransformFeedback` bodies), keeping
//! the same lifecycle rules + honest GL errors (first-error-wins via [`GlContext::set_gl_error`]).

use crate::model::context::GlContext;
use crate::model::es3::SamplerObj;
use crate::model::glconst::*;

// ==================================================================================================
// Sampler objects
// ==================================================================================================

/// `glGenSamplers` (one name).
pub fn gen_sampler(ctx: &mut GlContext) -> u32 {
    ctx.samplers.gen()
}

/// Validate a sampler-parameter enum value for `pname`. `Some(GL_INVALID_ENUM)` for an out-of-range
/// enum-typed value or an unknown `pname`; `None` if acceptable (enum-valid, or a non-enum LOD).
fn sampler_param_error(pname: u32, v: i32) -> Option<u32> {
    let ok = match pname {
        GL_TEXTURE_MIN_FILTER => matches!(
            v as u32,
            GL_NEAREST
                | GL_LINEAR
                | GL_NEAREST_MIPMAP_NEAREST
                | GL_LINEAR_MIPMAP_NEAREST
                | GL_NEAREST_MIPMAP_LINEAR
                | GL_LINEAR_MIPMAP_LINEAR
        ),
        GL_TEXTURE_MAG_FILTER => matches!(v as u32, GL_NEAREST | GL_LINEAR),
        GL_TEXTURE_WRAP_S | GL_TEXTURE_WRAP_T | GL_TEXTURE_WRAP_R => {
            matches!(v as u32, GL_REPEAT | GL_CLAMP_TO_EDGE | GL_MIRRORED_REPEAT)
        }
        GL_TEXTURE_COMPARE_MODE => matches!(v as u32, GL_NONE | GL_COMPARE_REF_TO_TEXTURE),
        GL_TEXTURE_COMPARE_FUNC => matches!(
            v as u32,
            GL_NEVER | GL_LESS | GL_EQUAL | GL_LEQUAL | GL_GREATER | GL_NOTEQUAL | GL_GEQUAL | GL_ALWAYS
        ),
        GL_TEXTURE_MIN_LOD | GL_TEXTURE_MAX_LOD => return None, // non-enum LOD: any value
        _ => return Some(GL_INVALID_ENUM),
    };
    if ok {
        None
    } else {
        Some(GL_INVALID_ENUM)
    }
}

/// Core setter shared by every `glSamplerParameter{i,f,iv,fv,Iiv,Iuiv}` — validate before mutating (an
/// invalid enum leaves the object untouched). An unknown sampler name raises `GL_INVALID_OPERATION`.
pub fn sampler_parameter(ctx: &mut GlContext, sampler: u32, pname: u32, iv: i32, fv: f32) {
    if !ctx.samplers.known(sampler) {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    }
    if let Some(e) = sampler_param_error(pname, iv) {
        ctx.set_gl_error(e);
        return;
    }
    let obj = ctx.samplers.instantiate(sampler);
    match pname {
        GL_TEXTURE_MIN_FILTER => obj.min_filter = iv,
        GL_TEXTURE_MAG_FILTER => obj.mag_filter = iv,
        GL_TEXTURE_WRAP_S => obj.wrap_s = iv,
        GL_TEXTURE_WRAP_T => obj.wrap_t = iv,
        GL_TEXTURE_WRAP_R => obj.wrap_r = iv,
        GL_TEXTURE_COMPARE_MODE => obj.compare_mode = iv,
        GL_TEXTURE_COMPARE_FUNC => obj.compare_func = iv,
        GL_TEXTURE_MIN_LOD => obj.min_lod = fv,
        GL_TEXTURE_MAX_LOD => obj.max_lod = fv,
        _ => {}
    }
}

/// Read one sampler parameter as `f32` (the int getter rounds to nearest). `None` on error (the error is
/// already registered): an unknown name → `GL_INVALID_OPERATION`, an unknown `pname` → `GL_INVALID_ENUM`.
pub fn get_sampler_parameter(ctx: &mut GlContext, sampler: u32, pname: u32) -> Option<f32> {
    if !ctx.samplers.known(sampler) {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return None;
    }
    let obj = *ctx.samplers.instantiate(sampler);
    match obj.get(pname) {
        Some(v) => Some(v),
        None => {
            ctx.set_gl_error(GL_INVALID_ENUM);
            None
        }
    }
}

/// `glBindSampler(unit, sampler)` — bind (or clear, `sampler==0`). An unknown name raises
/// `GL_INVALID_OPERATION`.
pub fn bind_sampler(ctx: &mut GlContext, unit: u32, sampler: u32) {
    if sampler == 0 {
        ctx.samplers.bind(unit, 0);
        return;
    }
    if !ctx.samplers.known(sampler) {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    }
    ctx.samplers.instantiate(sampler);
    ctx.samplers.bind(unit, sampler);
}

/// `glDeleteSamplers` (one name; deleting `0` is silently ignored).
pub fn delete_sampler(ctx: &mut GlContext, sampler: u32) {
    ctx.samplers.delete(sampler);
}

/// `glIsSampler(sampler)`.
pub fn is_sampler(ctx: &GlContext, sampler: u32) -> bool {
    ctx.samplers.is_sampler(sampler)
}

/// The default sampler state (for a test to compare a fresh object against).
pub fn default_sampler() -> SamplerObj {
    SamplerObj::default()
}

// ==================================================================================================
// Query objects
// ==================================================================================================

/// A valid `glBeginQuery` target for this driver: the ES3-CORE occlusion targets (`GL_ANY_SAMPLES_PASSED[_
/// CONSERVATIVE]`) and the transform-feedback primitives-written target. TIMER queries (`GL_TIME_ELAPSED`,
/// `GL_TIMESTAMP`) are NOT core ES3 — they belong to the `EXT_disjoint_timer_query` extension, which this
/// driver does NOT advertise (`crate::service::query::EXTENSIONS` is empty). So `glBeginQuery(GL_TIME_
/// ELAPSED_EXT, …)` honestly raises `GL_INVALID_ENUM` here rather than faking a monotonic counter — an
/// unadvertised extension must not silently appear to work. (See tests/gl_timer_query in hl_wip.)
fn is_query_target(target: u32) -> bool {
    matches!(
        target,
        GL_ANY_SAMPLES_PASSED | GL_ANY_SAMPLES_PASSED_CONSERVATIVE | GL_TRANSFORM_FEEDBACK_PRIMITIVES_WRITTEN
    )
}

/// `glGenQueries` (one name).
pub fn gen_query(ctx: &mut GlContext) -> u32 {
    ctx.queries.gen()
}

/// `glBeginQuery(target, id)`. Honest errors: a bad target → `GL_INVALID_ENUM`; an unknown/zero id, a
/// query already active on the target, this id already active, or a type mismatch → `GL_INVALID_OPERATION`.
pub fn begin_query(ctx: &mut GlContext, target: u32, id: u32) {
    if !is_query_target(target) {
        ctx.set_gl_error(GL_INVALID_ENUM);
        return;
    }
    if id == 0 || !ctx.queries.known(id) {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    }
    if ctx.queries.active_for(target) != 0 {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    }
    if let Some(q) = ctx.queries.get(id) {
        if q.active || (q.target != 0 && q.target != target) {
            ctx.set_gl_error(GL_INVALID_OPERATION);
            return;
        }
    }
    ctx.queries.begin(target, id);
}

/// `glEndQuery(target)`. A bad target → `GL_INVALID_ENUM`; no active query on the target →
/// `GL_INVALID_OPERATION`.
pub fn end_query(ctx: &mut GlContext, target: u32) {
    if !is_query_target(target) {
        ctx.set_gl_error(GL_INVALID_ENUM);
        return;
    }
    if ctx.queries.active_for(target) == 0 {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    }
    ctx.queries.end(target);
}

/// `glDeleteQueries` (one name; deleting `0` is ignored).
pub fn delete_query(ctx: &mut GlContext, id: u32) {
    ctx.queries.delete(id);
}

/// `glIsQuery(id)`.
pub fn is_query(ctx: &GlContext, id: u32) -> bool {
    ctx.queries.is_query(id)
}

/// `glGetQueryiv(target, pname)` — only `GL_CURRENT_QUERY` is defined; returns the active query id (or
/// `0`). A bad target or pname raises `GL_INVALID_ENUM` and returns `None`.
pub fn get_queryiv(ctx: &mut GlContext, target: u32, pname: u32) -> Option<i32> {
    if !is_query_target(target) || pname != GL_CURRENT_QUERY {
        ctx.set_gl_error(GL_INVALID_ENUM);
        return None;
    }
    Some(ctx.queries.active_for(target) as i32)
}

/// `glGetQueryObjectuiv(id, pname)` — `GL_QUERY_RESULT_AVAILABLE` (this deferred model completes a query
/// synchronously at `glEndQuery`, so an ended query is immediately available) or `GL_QUERY_RESULT`. For an
/// `GL_ANY_SAMPLES_PASSED[_CONSERVATIVE]` query the result is the boolean the ES3 spec defines: `1` iff any
/// draw inside the begin/end scope had non-zero scissor-clipped coverage, `0` when everything was
/// scissored/occluded away (see [`crate::model::es3::Queries::end`]). A transform-feedback query keeps `0`
/// (its counter is not modeled). `None` for an unknown/active query (`GL_INVALID_OPERATION`) or a bad
/// pname (`GL_INVALID_ENUM`).
pub fn get_query_objectuiv(ctx: &mut GlContext, id: u32, pname: u32) -> Option<u32> {
    let (ended, result) = match ctx.queries.get(id) {
        Some(q) if !q.active => (q.ended, q.result),
        _ => {
            ctx.set_gl_error(GL_INVALID_OPERATION);
            return None;
        }
    };
    match pname {
        GL_QUERY_RESULT_AVAILABLE => Some(ended as u32),
        GL_QUERY_RESULT => Some(result),
        _ => {
            ctx.set_gl_error(GL_INVALID_ENUM);
            None
        }
    }
}

// ==================================================================================================
// Transform-feedback objects
// ==================================================================================================

/// `glGenTransformFeedbacks` (one name).
pub fn gen_transform_feedback(ctx: &mut GlContext) -> u32 {
    ctx.transform_feedbacks.gen()
}

/// `glBindTransformFeedback(target, id)`. A bad target → `GL_INVALID_ENUM`; binding while feedback is
/// active-and-not-paused, or an unknown id, → `GL_INVALID_OPERATION`.
pub fn bind_transform_feedback(ctx: &mut GlContext, target: u32, id: u32) {
    if target != GL_TRANSFORM_FEEDBACK {
        ctx.set_gl_error(GL_INVALID_ENUM);
        return;
    }
    let cur = ctx.transform_feedbacks.bound_obj();
    if cur.active && !cur.paused {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    }
    if id != 0 && !ctx.transform_feedbacks.known(id) {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    }
    ctx.transform_feedbacks.bind(id);
}

/// `glDeleteTransformFeedbacks` (one name). Deleting the default `0` is ignored; deleting an active
/// object raises `GL_INVALID_OPERATION`.
pub fn delete_transform_feedback(ctx: &mut GlContext, id: u32) {
    if id == 0 {
        return;
    }
    // Deleting the currently-bound object while it is active is a spec error.
    if ctx.transform_feedbacks.bound() == id && ctx.transform_feedbacks.bound_obj().active {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    }
    ctx.transform_feedbacks.delete(id);
}

/// `glIsTransformFeedback(id)`.
pub fn is_transform_feedback(ctx: &GlContext, id: u32) -> bool {
    ctx.transform_feedbacks.is_transform_feedback(id)
}

/// `glBeginTransformFeedback(primitiveMode)`. A bad mode → `GL_INVALID_ENUM`; already active →
/// `GL_INVALID_OPERATION`.
pub fn begin_transform_feedback(ctx: &mut GlContext, primitive_mode: u32) {
    if !matches!(primitive_mode, GL_POINTS | GL_LINES | GL_TRIANGLES) {
        ctx.set_gl_error(GL_INVALID_ENUM);
        return;
    }
    if ctx.transform_feedbacks.bound_obj().active {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    }
    ctx.transform_feedbacks.set_active(true, false);
}

/// `glEndTransformFeedback()`. Not active → `GL_INVALID_OPERATION`.
pub fn end_transform_feedback(ctx: &mut GlContext) {
    if !ctx.transform_feedbacks.bound_obj().active {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    }
    ctx.transform_feedbacks.set_active(false, false);
}

/// `glPauseTransformFeedback()`. Not active, or already paused → `GL_INVALID_OPERATION`.
pub fn pause_transform_feedback(ctx: &mut GlContext) {
    let o = ctx.transform_feedbacks.bound_obj();
    if !o.active || o.paused {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    }
    ctx.transform_feedbacks.set_active(true, true);
}

/// `glResumeTransformFeedback()`. Not active, or not paused → `GL_INVALID_OPERATION`.
pub fn resume_transform_feedback(ctx: &mut GlContext) {
    let o = ctx.transform_feedbacks.bound_obj();
    if !o.active || !o.paused {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    }
    ctx.transform_feedbacks.set_active(true, false);
}

/// `glTransformFeedbackVaryings(program, names, bufferMode)` — record the capture list on `program`.
/// An unknown program → `GL_INVALID_VALUE`; a bad buffer mode → `GL_INVALID_ENUM`.
pub fn transform_feedback_varyings(ctx: &mut GlContext, program: u32, names: Vec<String>, buffer_mode: u32) {
    if program == 0 || ctx.programs.program(program).is_none() {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    if !matches!(buffer_mode, GL_INTERLEAVED_ATTRIBS | GL_SEPARATE_ATTRIBS) {
        ctx.set_gl_error(GL_INVALID_ENUM);
        return;
    }
    ctx.transform_feedbacks.set_varyings(program, names, buffer_mode);
}

/// The captured varying name at `index` for `program` (`glGetTransformFeedbackVarying`), or `None`
/// (out-of-range / never specified) — the caller raises `GL_INVALID_VALUE` and reports an empty name.
pub fn transform_feedback_varying(ctx: &GlContext, program: u32, index: u32) -> Option<String> {
    ctx.transform_feedbacks.varying(program, index).map(|s| s.to_string())
}

// ==================================================================================================
// Program pipeline objects (separate shaders)
// ==================================================================================================

/// `glGenProgramPipelines` (one name).
pub fn gen_program_pipeline(ctx: &mut GlContext) -> u32 {
    ctx.program_pipelines.gen()
}

/// `glBindProgramPipeline(id)`. An unknown non-zero id → `GL_INVALID_OPERATION`.
pub fn bind_program_pipeline(ctx: &mut GlContext, id: u32) {
    if id != 0 && !ctx.program_pipelines.known(id) {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    }
    ctx.program_pipelines.bind(id);
}

/// `glDeleteProgramPipelines` (one name; `0` ignored).
pub fn delete_program_pipeline(ctx: &mut GlContext, id: u32) {
    ctx.program_pipelines.delete(id);
}

/// `glIsProgramPipeline(id)`.
pub fn is_program_pipeline(ctx: &GlContext, id: u32) -> bool {
    ctx.program_pipelines.is_pipeline(id)
}

/// `glUseProgramStages(pipeline, stages, program)` — set the program for each named stage bit of
/// `pipeline` (`program==0` clears them). Honest errors: an unknown pipeline → `GL_INVALID_OPERATION`;
/// `stages` carrying a bit outside the known set (and not `GL_ALL_SHADER_BITS`) → `GL_INVALID_VALUE`; a
/// non-zero `program` that names no program object → `GL_INVALID_OPERATION`.
pub fn use_program_stages(ctx: &mut GlContext, pipeline: u32, stages: u32, program: u32) {
    if !ctx.program_pipelines.known(pipeline) {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    }
    let known_bits = GL_VERTEX_SHADER_BIT | GL_FRAGMENT_SHADER_BIT | GL_COMPUTE_SHADER_BIT;
    if stages != GL_ALL_SHADER_BITS && (stages & !known_bits) != 0 {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    if program != 0 && ctx.programs.program(program).is_none() {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    }
    let obj = ctx.program_pipelines.instantiate(pipeline);
    if stages & GL_VERTEX_SHADER_BIT != 0 {
        obj.vertex_program = program;
    }
    if stages & GL_FRAGMENT_SHADER_BIT != 0 {
        obj.fragment_program = program;
    }
    if stages & GL_COMPUTE_SHADER_BIT != 0 {
        obj.compute_program = program;
    }
}

/// `glActiveShaderProgram(pipeline, program)` — set the active program (the `glProgramUniform*` target).
/// An unknown pipeline → `GL_INVALID_OPERATION`; a non-zero `program` naming no program → `GL_INVALID_OPERATION`.
pub fn active_shader_program(ctx: &mut GlContext, pipeline: u32, program: u32) {
    if !ctx.program_pipelines.known(pipeline) {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    }
    if program != 0 && ctx.programs.program(program).is_none() {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    }
    ctx.program_pipelines.instantiate(pipeline).active_program = program;
}

/// `glGetProgramPipelineiv(pipeline, pname)` — the program bound to a stage / the active program / the
/// (always-empty) info-log length / validate status. An unknown pipeline → `GL_INVALID_OPERATION` and
/// `None`; an unknown pname reads `0`.
pub fn get_program_pipelineiv(ctx: &mut GlContext, pipeline: u32, pname: u32) -> Option<i32> {
    if !ctx.program_pipelines.known(pipeline) {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return None;
    }
    let obj = ctx.program_pipelines.get(pipeline).copied().unwrap_or_default();
    Some(match pname {
        GL_VERTEX_SHADER => obj.vertex_program as i32,
        GL_FRAGMENT_SHADER => obj.fragment_program as i32,
        GL_COMPUTE_SHADER => obj.compute_program as i32,
        GL_ACTIVE_PROGRAM => obj.active_program as i32,
        GL_INFO_LOG_LENGTH => 0,
        GL_VALIDATE_STATUS => GL_TRUE as i32,
        _ => 0,
    })
}
