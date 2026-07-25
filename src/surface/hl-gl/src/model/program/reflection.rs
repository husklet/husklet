//! Program reflection queries.

use super::Program;

impl Program {
    /// `glGetUniformLocation(name)` — resolve `name` to the location the `glUniform*` recording ops key
    /// on: the uniform's DECLARATION INDEX in its reflected table (`unis` for a data uniform, `samp_names`
    /// for a sampler uniform), matching the index [`crate::service::record::uniform_at`] /
    /// [`crate::service::record::uniform_sampler`] expect. Returns `-1` if the name is not an active
    /// uniform (unlinked program, or a name the reflection did not find). Data uniforms are searched
    /// first, then samplers.
    pub fn uniform_location(&self, name: &str) -> i32 {
        if !self.linked {
            return -1;
        }
        if let Some(i) = self.unis.iter().position(|u| u.name == name) {
            return i as i32;
        }
        if let Some(i) = self.samp_names.iter().position(|s| s == name) {
            return i as i32;
        }
        -1
    }

    /// `glGetAttribLocation(name)` — the attribute's declaration-order index in the vertex shader, which
    /// is exactly the `[[attribute(L)]]` slot the translator emits (so it matches the index a
    /// `glVertexAttribPointer(L, …)` binds). Returns `-1` for an unknown attribute or an unlinked program.
    pub fn attrib_location(&self, name: &str) -> i32 {
        if !self.linked {
            return -1;
        }
        crate::adapter::glsl::Source::new(&self.vs_src)
            .vertex_attrs()
            .iter()
            .position(|a| a.name == name)
            .map(|i| i as i32)
            .unwrap_or(-1)
    }
}
