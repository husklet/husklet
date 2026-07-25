use super::*;

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
    pub fn supports(target: u32) -> bool {
        matches!(
            target,
            GL_ANY_SAMPLES_PASSED
                | GL_ANY_SAMPLES_PASSED_CONSERVATIVE
                | GL_TRANSFORM_FEEDBACK_PRIMITIVES_WRITTEN
        )
    }

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
    pub fn contains(&self, id: u32) -> bool {
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
