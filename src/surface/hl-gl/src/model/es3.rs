//! ES3 client-side object families: sampler objects, occlusion/transform-feedback QUERY objects,
//! transform-feedback objects, and separate-shader PROGRAM PIPELINE objects.
//!
//! Ported from `hl-shim-gl/src/state.rs` (`SamplerObj`, `QueryObj`, `TransformFeedbackObj`) + the
//! `gl_shim.c` name allocators. None of these families lower to GPU IR — a real driver emits no command
//! for a `glSamplerParameteri`/`glBeginQuery`/`glBindTransformFeedback`; they carry observable object
//! STATE the app polls back through `glGetSamplerParameter*` / `glGetQueryObjectuiv` /
//! `glGetTransformFeedbackVarying`. So the tables live here as pure model state (the [`crate::service`]
//! layer drives them, submits nothing), and the deferred frame IR is unaffected.

use super::glconst::*;
use hl_gpu::protocol::model::enums::{AddressMode, Filter};
use std::collections::{HashMap, HashSet};

// ==================================================================================================
// Sampler objects (glGenSamplers / glBindSampler / glSamplerParameter*)
// ==================================================================================================

/// One ES3 sampler object's full parameter state (ES 3.0 §6.10 default table). The min/max LOD are the
/// only non-enum, float-typed parameters; everything else is a GL enum stored as its `i32` value.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct SamplerObj {
    pub min_filter: i32,
    pub mag_filter: i32,
    pub wrap_s: i32,
    pub wrap_t: i32,
    pub wrap_r: i32,
    pub min_lod: f32,
    pub max_lod: f32,
    pub compare_mode: i32,
    pub compare_func: i32,
}

impl Default for SamplerObj {
    fn default() -> Self {
        SamplerObj {
            min_filter: GL_NEAREST_MIPMAP_LINEAR as i32,
            mag_filter: GL_LINEAR as i32,
            wrap_s: GL_REPEAT as i32,
            wrap_t: GL_REPEAT as i32,
            wrap_r: GL_REPEAT as i32,
            min_lod: -1000.0,
            max_lod: 1000.0,
            compare_mode: GL_NONE as i32,
            compare_func: GL_LEQUAL as i32,
        }
    }
}

impl SamplerObj {
    /// The neutral min-filter for this sampler object's GL min-filter (Linear for `LINEAR` /
    /// `LINEAR_MIPMAP_*`, else Nearest) — the exact mapping [`super::texture::GlTexture::ir_min_filter`]
    /// uses, so a bound sampler object lowers identically to the equivalent texture parameters.
    pub fn ir_min_filter(&self) -> Filter {
        match self.min_filter as u32 {
            GL_LINEAR | GL_LINEAR_MIPMAP_NEAREST | GL_LINEAR_MIPMAP_LINEAR => Filter::Linear,
            _ => Filter::Nearest,
        }
    }

    /// The neutral mag-filter (Linear only for exactly `LINEAR`).
    pub fn ir_mag_filter(&self) -> Filter {
        if self.mag_filter as u32 == GL_LINEAR {
            Filter::Linear
        } else {
            Filter::Nearest
        }
    }

    /// The neutral S wrap (ClampToEdge / MirrorRepeat / else Repeat).
    pub fn ir_wrap_s(&self) -> AddressMode {
        sampler_address_mode(self.wrap_s as u32)
    }

    /// The neutral T wrap.
    pub fn ir_wrap_t(&self) -> AddressMode {
        sampler_address_mode(self.wrap_t as u32)
    }

    /// Read one parameter as `f32` (the int-typed getter rounds this to nearest). `None` for an unknown
    /// `pname` (the caller raises `GL_INVALID_ENUM`).
    pub fn get(&self, pname: u32) -> Option<f32> {
        Some(match pname {
            GL_TEXTURE_MIN_FILTER => self.min_filter as f32,
            GL_TEXTURE_MAG_FILTER => self.mag_filter as f32,
            GL_TEXTURE_WRAP_S => self.wrap_s as f32,
            GL_TEXTURE_WRAP_T => self.wrap_t as f32,
            GL_TEXTURE_WRAP_R => self.wrap_r as f32,
            GL_TEXTURE_COMPARE_MODE => self.compare_mode as f32,
            GL_TEXTURE_COMPARE_FUNC => self.compare_func as f32,
            GL_TEXTURE_MIN_LOD => self.min_lod,
            GL_TEXTURE_MAX_LOD => self.max_lod,
            _ => return None,
        })
    }
}

/// GL wrap enum → neutral address mode (ClampToEdge / MirrorRepeat / else Repeat), matching the texture
/// path's `address_mode`.
fn sampler_address_mode(gl: u32) -> AddressMode {
    match gl {
        GL_CLAMP_TO_EDGE => AddressMode::ClampToEdge,
        GL_MIRRORED_REPEAT => AddressMode::MirrorRepeat,
        _ => AddressMode::Repeat,
    }
}

/// The per-context sampler-object table: reserved names (`glGenSamplers`), instantiated objects (created
/// lazily on first parameterize/bind), and the per-unit binding map. Name `0` is the reserved sentinel.
#[derive(Debug, Default)]
pub struct Samplers {
    reserved: HashSet<u32>,
    objects: HashMap<u32, SamplerObj>,
    binding: HashMap<u32, u32>,
    next_name: u32,
}

impl Samplers {
    pub fn new() -> Self {
        Self {
            reserved: HashSet::new(),
            objects: HashMap::new(),
            binding: HashMap::new(),
            next_name: 1,
        }
    }

    /// `glGenSamplers` — mint one fresh reserved name.
    pub fn gen(&mut self) -> u32 {
        let id = self.next_name;
        self.next_name += 1;
        self.reserved.insert(id);
        id
    }

    /// A name is usable iff `glGenSamplers` handed it out and it is not deleted (reserved OR live).
    pub fn known(&self, id: u32) -> bool {
        id != 0 && (self.objects.contains_key(&id) || self.reserved.contains(&id))
    }

    /// `glIsSampler` — true only once the name names a CREATED object (bound/parameterized), not merely
    /// reserved (the lazy-instantiation model GL's buffer/texture names use).
    pub fn is_sampler(&self, id: u32) -> bool {
        self.objects.contains_key(&id)
    }

    /// Instantiate (if needed) and mutably borrow the object, moving it out of the reserved set.
    pub fn instantiate(&mut self, id: u32) -> &mut SamplerObj {
        self.reserved.remove(&id);
        self.objects.entry(id).or_default()
    }

    pub fn get(&self, id: u32) -> Option<&SamplerObj> {
        self.objects.get(&id)
    }

    /// `glBindSampler(unit, id)` — bind `id` to `unit` (`0` clears the binding).
    pub fn bind(&mut self, unit: u32, id: u32) {
        if id == 0 {
            self.binding.remove(&unit);
        } else {
            self.binding.insert(unit, id);
        }
    }

    pub fn binding(&self, unit: u32) -> u32 {
        self.binding.get(&unit).copied().unwrap_or(0)
    }

    /// `glDeleteSamplers` (one name) — drop the object + reservation and unbind it from every unit.
    pub fn delete(&mut self, id: u32) {
        if id == 0 {
            return;
        }
        self.objects.remove(&id);
        self.reserved.remove(&id);
        self.binding.retain(|_, v| *v != id);
    }
}

// ==================================================================================================
// Query objects (glGenQueries / glBeginQuery / glEndQuery / glGetQueryObjectuiv)
// ==================================================================================================

/// One ES3 occlusion/transform-feedback query object. It tracks the typed target it was first used with,
/// whether it is inside a begin/end pair, and — once ended — the produced result. The result COUNTER
/// itself is not run by this deferred model's executor (no occlusion backend), so `result` is a truthful
/// `0`; the object LIFECYCLE + availability are real and enforced.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct QueryObj {
    /// The target bound on the first `glBeginQuery` (`0` = never used); a name is typed for its lifetime.
    pub target: u32,
    pub active: bool,
    /// A completed `glEndQuery` has produced a result (so `GL_QUERY_RESULT_AVAILABLE` reads true).
    pub ended: bool,
    /// The query result read back by `glGetQueryObjectuiv(GL_QUERY_RESULT)`. For an occlusion query this is
    /// the boolean any-samples-passed verdict computed from the scissor-clipped coverage of the draws in the
    /// begin/end scope (see [`Queries::end`]); for a transform-feedback query it stays `0` (not modeled).
    pub result: u32,
}

/// The per-context query-object table. Reserved names (`glGenQueries`), created objects, and the
/// per-target currently-active query id.
#[derive(Debug, Default)]
pub struct Queries {
    reserved: HashSet<u32>,
    objects: HashMap<u32, QueryObj>,
    active: HashMap<u32, u32>,
    next_name: u32,
    /// The occlusion accumulator armed by [`Queries::begin`] on an `GL_ANY_SAMPLES_PASSED[_CONSERVATIVE]`
    /// query: every geometry draw recorded inside the begin/end scope adds its scissor-clipped footprint
    /// (see [`Queries::accumulate`]), so [`Queries::end`] can resolve the query to REAL coverage instead of
    /// a fake constant `0`. `None` when no occlusion query is open (a transform-feedback query never arms
    /// it — that counter is not modeled, honest `0`). Mirrors the Vulkan occlusion fix (commit 5551f63a).
    occlusion_accum: Option<u64>,
}

impl Queries {
    pub fn new() -> Self {
        Self {
            reserved: HashSet::new(),
            objects: HashMap::new(),
            active: HashMap::new(),
            next_name: 1,
            occlusion_accum: None,
        }
    }

    /// `glGenQueries` — mint one fresh reserved name.
    pub fn gen(&mut self) -> u32 {
        let id = self.next_name;
        self.next_name += 1;
        self.reserved.insert(id);
        id
    }

    /// A name is usable iff `glGenQueries` handed it out and it is not deleted.
    pub fn known(&self, id: u32) -> bool {
        id != 0 && (self.objects.contains_key(&id) || self.reserved.contains(&id))
    }

    /// `glIsQuery` — true once the name names a CREATED (begun) object, not merely reserved.
    pub fn is_query(&self, id: u32) -> bool {
        self.objects.contains_key(&id)
    }

    pub fn get(&self, id: u32) -> Option<&QueryObj> {
        self.objects.get(&id)
    }

    /// The query currently active for `target` (`0` = none).
    pub fn active_for(&self, target: u32) -> u32 {
        self.active.get(&target).copied().unwrap_or(0)
    }

    /// `glBeginQuery(target, id)` — mark `id` active for `target`. The caller has already validated the
    /// name + that no query is active for the target. An occlusion target (`GL_ANY_SAMPLES_PASSED[_
    /// CONSERVATIVE]`) arms the coverage accumulator so each draw inside the scope contributes its footprint.
    pub fn begin(&mut self, target: u32, id: u32) {
        self.reserved.remove(&id);
        let q = self.objects.entry(id).or_default();
        q.target = target;
        q.active = true;
        q.ended = false;
        q.result = 0;
        self.active.insert(target, id);
        self.occlusion_accum = matches!(
            target,
            GL_ANY_SAMPLES_PASSED | GL_ANY_SAMPLES_PASSED_CONSERVATIVE
        )
        .then_some(0);
    }

    /// Add a draw's scissor-clipped sample footprint to the open occlusion query's running total (no-op if
    /// no occlusion query is armed). Called by [`crate::service::record`] for every geometry draw.
    pub fn accumulate(&mut self, coverage: u64) {
        if let Some(a) = self.occlusion_accum.as_mut() {
            *a = a.saturating_add(coverage);
        }
    }

    /// `glEndQuery(target)` — end the active query on `target`. An `GL_ANY_SAMPLES_PASSED[_CONSERVATIVE]`
    /// query resolves to the boolean `GL_TRUE`/`GL_FALSE` (`1`/`0`) the ES3 spec defines for that target:
    /// `1` iff any draw inside the scope had non-zero scissor-clipped coverage, `0` if everything was
    /// scissored/occluded away. A non-occlusion (transform-feedback) query keeps the honest `0` (its counter
    /// is not modeled). This replaces the old always-`0` constant with coverage that reflects reality.
    pub fn end(&mut self, target: u32) {
        let id = self.active_for(target);
        self.active.insert(target, 0);
        let result = match self.occlusion_accum.take() {
            Some(cov) => (cov > 0) as u32,
            None => 0,
        };
        if let Some(q) = self.objects.get_mut(&id) {
            q.active = false;
            q.ended = true;
            q.result = result;
        }
    }

    /// `glDeleteQueries` (one name) — ending an active query first, then dropping the object.
    pub fn delete(&mut self, id: u32) {
        if id == 0 {
            return;
        }
        if let Some(q) = self.objects.get(&id) {
            if q.active {
                let t = q.target;
                self.active.insert(t, 0);
            }
        }
        self.objects.remove(&id);
        self.reserved.remove(&id);
    }
}

// ==================================================================================================
// Transform-feedback objects (glGenTransformFeedbacks / glBindTransformFeedback / glBegin…)
// ==================================================================================================

/// One ES3 transform-feedback object's begin/end/pause/resume state. The default object (name `0`)
/// always exists.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct TransformFeedbackObj {
    pub active: bool,
    pub paused: bool,
}

/// The per-context transform-feedback table + the bound object + the per-program varying capture list
/// (`glTransformFeedbackVaryings`, round-tripped through `glGetTransformFeedbackVarying`).
///
/// HONEST GAP (documented, not faked): the object LIFECYCLE (bind/begin/pause/resume/end) and the varying
/// NAME reflection are real observable state, but per-vertex varying DATA capture into the bound
/// `GL_TRANSFORM_FEEDBACK_BUFFER` is NOT modeled — this deferred driver lowers draws to GPU IR and has no
/// CPU vertex-shader executor to evaluate each vertex's varyings, so the capture buffer is left untouched
/// rather than filled with fabricated values. See `hl_wip/tests/gl_transform_feedback.rs`, which drives the
/// lifecycle + reflection and asserts the buffer stays its sentinel (no fake capture).
#[derive(Debug)]
pub struct TransformFeedbacks {
    reserved: HashSet<u32>,
    objects: HashMap<u32, TransformFeedbackObj>,
    bound: u32,
    next_name: u32,
    /// program name → (captured varying names, buffer mode).
    varyings: HashMap<u32, (Vec<String>, u32)>,
}

impl Default for TransformFeedbacks {
    fn default() -> Self {
        Self::new()
    }
}

impl TransformFeedbacks {
    pub fn new() -> Self {
        let mut objects = HashMap::new();
        objects.insert(0, TransformFeedbackObj::default()); // the default object always exists
        Self {
            reserved: HashSet::new(),
            objects,
            bound: 0,
            next_name: 1,
            varyings: HashMap::new(),
        }
    }

    /// `glGenTransformFeedbacks` — mint one fresh reserved name.
    pub fn gen(&mut self) -> u32 {
        let id = self.next_name;
        self.next_name += 1;
        self.reserved.insert(id);
        id
    }

    /// A name is bindable iff it is `glGenTransformFeedbacks`-reserved or already an object.
    pub fn known(&self, id: u32) -> bool {
        self.objects.contains_key(&id) || self.reserved.contains(&id)
    }

    /// `glIsTransformFeedback` — true for a created named object (not the default `0`, not reserved).
    pub fn is_transform_feedback(&self, id: u32) -> bool {
        id != 0 && self.objects.contains_key(&id)
    }

    pub fn bound(&self) -> u32 {
        self.bound
    }

    /// The currently-bound object's state.
    pub fn bound_obj(&self) -> TransformFeedbackObj {
        self.objects.get(&self.bound).copied().unwrap_or_default()
    }

    fn bound_mut(&mut self) -> Option<&mut TransformFeedbackObj> {
        let b = self.bound;
        self.objects.get_mut(&b)
    }

    /// `glBindTransformFeedback(id)` — the caller validated the name + that no active-unpaused feedback is
    /// in progress. Creates the object on first bind.
    pub fn bind(&mut self, id: u32) {
        if id != 0 {
            self.reserved.remove(&id);
            self.objects.entry(id).or_default();
        }
        self.bound = id;
    }

    /// `glDeleteTransformFeedbacks` (one name). Deleting the bound object reverts to the default `0`.
    pub fn delete(&mut self, id: u32) {
        if id == 0 {
            return;
        }
        self.objects.remove(&id);
        self.reserved.remove(&id);
        if self.bound == id {
            self.bound = 0;
        }
    }

    /// Begin/end/pause/resume the bound object's state machine (callers pre-validate the transition).
    pub fn set_active(&mut self, active: bool, paused: bool) {
        if let Some(o) = self.bound_mut() {
            o.active = active;
            o.paused = paused;
        }
    }

    /// Record a program's `glTransformFeedbackVaryings` capture list.
    pub fn set_varyings(&mut self, program: u32, names: Vec<String>, buffer_mode: u32) {
        self.varyings.insert(program, (names, buffer_mode));
    }

    /// The `index`-th captured varying name for `program`, or `None` (out of range / never specified).
    pub fn varying(&self, program: u32, index: u32) -> Option<&str> {
        self.varyings
            .get(&program)
            .and_then(|(v, _)| v.get(index as usize))
            .map(|s| s.as_str())
    }
}

// ==================================================================================================
// Program pipeline objects (glGenProgramPipelines / glUseProgramStages / glBindProgramPipeline)
// ==================================================================================================

/// One separate-shader program-pipeline object: the program bound to each stage + the active program
/// `glActiveShaderProgram` selects (the target of stage-independent `glProgramUniform*`).
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct ProgramPipeline {
    pub vertex_program: u32,
    pub fragment_program: u32,
    pub compute_program: u32,
    pub active_program: u32,
}

/// The per-context program-pipeline table + the bound pipeline. Name `0` = no pipeline bound.
#[derive(Debug, Default)]
pub struct ProgramPipelines {
    reserved: HashSet<u32>,
    objects: HashMap<u32, ProgramPipeline>,
    bound: u32,
    next_name: u32,
}

impl ProgramPipelines {
    pub fn new() -> Self {
        Self {
            reserved: HashSet::new(),
            objects: HashMap::new(),
            bound: 0,
            next_name: 1,
        }
    }

    /// `glGenProgramPipelines` — mint one fresh reserved name.
    pub fn gen(&mut self) -> u32 {
        let id = self.next_name;
        self.next_name += 1;
        self.reserved.insert(id);
        id
    }

    pub fn known(&self, id: u32) -> bool {
        id != 0 && (self.objects.contains_key(&id) || self.reserved.contains(&id))
    }

    /// `glIsProgramPipeline` — true once the name names a created (bound) object, not merely reserved.
    pub fn is_pipeline(&self, id: u32) -> bool {
        self.objects.contains_key(&id)
    }

    /// Instantiate (if needed) and mutably borrow the pipeline object.
    pub fn instantiate(&mut self, id: u32) -> &mut ProgramPipeline {
        self.reserved.remove(&id);
        self.objects.entry(id).or_default()
    }

    pub fn get(&self, id: u32) -> Option<&ProgramPipeline> {
        self.objects.get(&id)
    }

    pub fn bound(&self) -> u32 {
        self.bound
    }

    /// `glBindProgramPipeline(id)` — bind (creating on first bind); `0` unbinds.
    pub fn bind(&mut self, id: u32) {
        if id != 0 {
            self.reserved.remove(&id);
            self.objects.entry(id).or_default();
        }
        self.bound = id;
    }

    /// `glDeleteProgramPipelines` (one name). Deleting the bound pipeline reverts the binding to `0`.
    pub fn delete(&mut self, id: u32) {
        if id == 0 {
            return;
        }
        self.objects.remove(&id);
        self.reserved.remove(&id);
        if self.bound == id {
            self.bound = 0;
        }
    }
}
