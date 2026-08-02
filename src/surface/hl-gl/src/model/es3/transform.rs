use super::*;
use crate::model::context::IndexedBinding;

// ==================================================================================================
// Transform-feedback objects (glGenTransformFeedbacks / glBindTransformFeedback / glBegin…)
// ==================================================================================================

/// One ES3 transform-feedback object's begin/end/pause/resume state. The default object (name `0`)
/// always exists.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct TransformFeedbackObj {
    pub active: bool,
    pub paused: bool,
    pub bindings: [Option<IndexedBinding>; 4],
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
    pub fn contains(&self, id: u32) -> bool {
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

    pub fn set_binding(&mut self, index: u32, binding: Option<IndexedBinding>) {
        if let Some(slot) = self
            .bound_mut()
            .and_then(|object| object.bindings.get_mut(index as usize))
        {
            *slot = binding;
        }
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
