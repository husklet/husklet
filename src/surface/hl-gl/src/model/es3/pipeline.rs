use super::*;

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
    pub fn contains(&self, id: u32) -> bool {
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

    pub(crate) fn references_program(&self, program: u32) -> bool {
        self.objects.values().any(|pipeline| {
            pipeline.vertex_program == program
                || pipeline.fragment_program == program
                || pipeline.compute_program == program
                || pipeline.active_program == program
        })
    }
}
