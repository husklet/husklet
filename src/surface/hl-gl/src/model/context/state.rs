use super::*;

/// Context-local GL state stored by an EGL context while another context is current.
///
/// [`GlContext`] also owns the share-group object tables and the process connection's IR namespace. Those
/// remain singular when EGL contexts share objects, while this explicit local value moves with
/// `eglMakeCurrent`.
pub struct ContextState {
    local: LocalState,
    sampler_bindings: Samplers,
}

impl ContextState {
    pub fn new() -> Self {
        Self {
            local: LocalState::default(),
            sampler_bindings: Samplers::new(),
        }
    }

    pub fn with_version(major: i32, minor: i32, no_error: bool) -> Self {
        Self {
            local: LocalState::with_version(major, minor, no_error),
            sampler_bindings: Samplers::new(),
        }
    }

    pub fn references_buffer(&self, name: u32) -> bool {
        self.local.array_buffer == name
            || self.local.element_buffer == name
            || self
                .local
                .general_buffers
                .values()
                .any(|value| *value == name)
            || self
                .local
                .indexed_buffers
                .values()
                .any(|binding| binding.buffer == name)
            || self
                .local
                .attr
                .iter()
                .any(|attribute| attribute.buffer == name)
            || self
                .local
                .vertex_bindings
                .iter()
                .any(|binding| binding.buffer == name)
            || self.local.vaos.iter().any(|(id, vao)| {
                *id != self.local.cur_vao
                    && (vao.element_buffer == name
                        || vao.attrs.iter().any(|attribute| attribute.buffer == name)
                        || vao
                            .vertex_bindings
                            .iter()
                            .any(|binding| binding.buffer == name))
            })
    }

    pub fn references_texture(&self, name: u32) -> bool {
        self.local.tex_unit.contains(&name) || self.local.framebuffers.references_texture(name)
    }

    pub fn references_sampler(&self, name: u32) -> bool {
        self.sampler_bindings.references(name)
    }

    pub fn references_program(&self, name: u32) -> bool {
        self.local.cur_prog == name
            || self.local.program_pipelines.references_program(name)
            || self.local.recording.references_program(name)
    }
}

impl Default for ContextState {
    fn default() -> Self {
        Self::new()
    }
}

impl GlContext {
    pub fn delete_texture_later(&mut self, name: u32) -> bool {
        if name == 0
            || self.pending_texture_deletes.contains(&name)
            || self.textures.get(name).is_none()
        {
            return false;
        }
        for texture in &mut self.local.tex_unit {
            if *texture == name {
                *texture = 0;
            }
        }
        self.local.framebuffers.detach_color_texture(name);
        self.pending_texture_deletes.insert(name);
        true
    }

    pub fn delete_buffer_later(&mut self, name: u32) -> bool {
        if name == 0
            || self.pending_buffer_deletes.contains(&name)
            || self.buffers.get(name).is_none()
        {
            return false;
        }
        if self.local.array_buffer == name {
            self.local.array_buffer = 0;
        }
        if self.local.element_buffer == name {
            self.local.element_buffer = 0;
        }
        self.local.general_buffers.retain(|_, value| *value != name);
        self.local
            .indexed_buffers
            .retain(|_, binding| binding.buffer != name);
        for attribute in &mut self.local.attr {
            if attribute.buffer == name {
                attribute.buffer = 0;
            }
        }
        for binding in &mut self.local.vertex_bindings {
            if binding.buffer == name {
                binding.buffer = 0;
            }
        }
        self.pending_buffer_deletes.insert(name);
        true
    }

    pub fn delete_sampler_later(&mut self, name: u32) -> bool {
        if name == 0 || self.pending_sampler_deletes.contains(&name) || !self.samplers.known(name) {
            return false;
        }
        self.samplers.unbind(name);
        self.pending_sampler_deletes.insert(name);
        true
    }

    pub fn delete_program_later(&mut self, name: u32) -> bool {
        if name == 0
            || self.pending_program_deletes.contains(&name)
            || !self.programs.contains(name)
        {
            return false;
        }
        self.pending_program_deletes.insert(name);
        true
    }

    pub fn deleted_textures(&self) -> impl Iterator<Item = u32> + '_ {
        self.pending_texture_deletes.iter().copied()
    }

    pub fn deleted_buffers(&self) -> impl Iterator<Item = u32> + '_ {
        self.pending_buffer_deletes.iter().copied()
    }

    pub fn deleted_samplers(&self) -> impl Iterator<Item = u32> + '_ {
        self.pending_sampler_deletes.iter().copied()
    }

    pub fn deleted_programs(&self) -> impl Iterator<Item = u32> + '_ {
        self.pending_program_deletes.iter().copied()
    }

    pub fn is_texture_name(&self, name: u32) -> bool {
        name != 0
            && !self.pending_texture_deletes.contains(&name)
            && self.textures.get(name).is_some()
    }

    pub fn is_buffer_name(&self, name: u32) -> bool {
        name != 0
            && !self.pending_buffer_deletes.contains(&name)
            && self.buffers.get(name).is_some()
    }

    pub fn is_sampler_name(&self, name: u32) -> bool {
        !self.pending_sampler_deletes.contains(&name) && self.samplers.contains(name)
    }

    pub fn is_program_name(&self, name: u32) -> bool {
        !self.pending_program_deletes.contains(&name) && self.programs.contains(name)
    }

    pub(crate) fn texture_is_deleted(&self, name: u32) -> bool {
        self.pending_texture_deletes.contains(&name)
    }

    pub(crate) fn buffer_is_deleted(&self, name: u32) -> bool {
        self.pending_buffer_deletes.contains(&name)
    }

    pub(crate) fn sampler_is_deleted(&self, name: u32) -> bool {
        self.pending_sampler_deletes.contains(&name)
    }

    pub(crate) fn program_is_deleted(&self, name: u32) -> bool {
        self.pending_program_deletes.contains(&name)
    }

    pub fn references_buffer(&self, name: u32) -> bool {
        self.local.array_buffer == name
            || self.local.element_buffer == name
            || self
                .local
                .general_buffers
                .values()
                .any(|value| *value == name)
            || self
                .local
                .indexed_buffers
                .values()
                .any(|binding| binding.buffer == name)
            || self
                .local
                .attr
                .iter()
                .any(|attribute| attribute.buffer == name)
            || self
                .local
                .vertex_bindings
                .iter()
                .any(|binding| binding.buffer == name)
            || self.local.vaos.iter().any(|(id, vao)| {
                *id != self.local.cur_vao
                    && (vao.element_buffer == name
                        || vao.attrs.iter().any(|attribute| attribute.buffer == name)
                        || vao
                            .vertex_bindings
                            .iter()
                            .any(|binding| binding.buffer == name))
            })
    }

    pub fn references_texture(&self, name: u32) -> bool {
        self.local.tex_unit.contains(&name) || self.local.framebuffers.references_texture(name)
    }

    pub fn references_sampler(&self, name: u32) -> bool {
        self.samplers.references(name)
    }

    pub fn references_program(&self, name: u32) -> bool {
        self.local.cur_prog == name
            || self.local.program_pipelines.references_program(name)
            || self.local.recording.references_program(name)
    }

    pub fn reclaim_texture(&mut self, name: u32) -> bool {
        if !self.pending_texture_deletes.remove(&name) {
            return false;
        }
        self.retire_texture(name);
        self.textures.delete(name)
    }

    pub fn reclaim_buffer(&mut self, name: u32) -> bool {
        if !self.pending_buffer_deletes.remove(&name) {
            return false;
        }
        self.retire_buffer(name);
        self.buffers.delete(name)
    }

    pub fn reclaim_sampler(&mut self, name: u32) -> bool {
        if !self.pending_sampler_deletes.remove(&name) {
            return false;
        }
        self.samplers.delete(name);
        true
    }

    pub fn reclaim_program(&mut self, name: u32) -> bool {
        if !self.pending_program_deletes.remove(&name) {
            return false;
        }
        self.uniform_blocks.remove(&name);
        self.destroy_program(name);
        true
    }

    /// Exchange the context-local portion of this active context with a saved EGL context.
    ///
    /// Object tables, object name allocators, sync objects, resident IR caches, and IR id allocators stay
    /// on `self`: they form the implicit share group and the one collision-free host-resource namespace.
    /// Bindings, non-shared container objects, errors, and recorded work move with the EGL context.
    pub fn switch_state(&mut self, saved: &mut ContextState) {
        std::mem::swap(&mut self.local, &mut saved.local);
        self.samplers.switch_bindings(&mut saved.sampler_bindings);
    }

    /// Queue destruction of host resources owned only by an inactive EGL context.
    pub fn retire_state(&mut self, saved: &mut ContextState) {
        for (_, target) in saved.local.default_targets.drain() {
            self.pending_destroys
                .push(Cmd::DestroyTexture(target.texture));
            if target.token.is_some() {
                self.pending_destroys
                    .push(Cmd::DestroySurface(target.surface));
            }
        }
    }

    // ---- vertex array objects (glGenVertexArrays / glBindVertexArray / …) ------------------------

    /// `glGenVertexArrays` (one name) — mint a fresh VAO name with empty captured state.
    pub fn gen_vertex_array(&mut self) -> u32 {
        let id = self.local.next_vao;
        self.local.next_vao += 1;
        self.local.vaos.insert(id, Vao::default());
        id
    }

    /// `glBindVertexArray(vao)` — snapshot the live attribute array + element-buffer binding into the
    /// currently-bound VAO, then load `vao`'s captured state into the live context. Binding an unknown
    /// name creates that VAO on demand (matching GL's "first bind creates the object") with empty state.
    pub fn bind_vertex_array(&mut self, vao: u32) {
        self.local.vaos.insert(
            self.local.cur_vao,
            Vao {
                attrs: self.local.attr,
                vertex_bindings: self.local.vertex_bindings,
                element_buffer: self.local.element_buffer,
            },
        );
        self.local.cur_vao = vao;
        match self.local.vaos.get(&vao) {
            Some(v) => {
                self.local.attr = v.attrs;
                self.local.vertex_bindings = v.vertex_bindings;
                self.local.element_buffer = v.element_buffer;
            }
            None => {
                self.local.attr = [Attr::default(); MAX_ATTR];
                self.local.vertex_bindings = [VertexBinding::default(); MAX_ATTR];
                self.local.element_buffer = 0;
                self.local.vaos.insert(vao, Vao::default());
            }
        }
    }

    /// `glDeleteVertexArrays` (one name). Deleting the currently-bound VAO reverts the binding to the
    /// default VAO `0` (GL semantics) and loads its captured state. The default VAO `0` cannot be deleted.
    /// Returns `false` for an unknown / zero name.
    pub fn delete_vertex_array(&mut self, vao: u32) -> bool {
        if vao == 0 {
            return false;
        }
        if self.local.cur_vao == vao {
            self.local.cur_vao = 0;
            let def = self.local.vaos.get(&0).copied().unwrap_or_default();
            self.local.attr = def.attrs;
            self.local.vertex_bindings = def.vertex_bindings;
            self.local.element_buffer = def.element_buffer;
        }
        self.local.vaos.remove(&vao).is_some()
    }

    /// `glIsVertexArray(vao)` — true once `vao` names a generated (non-default) VAO object.
    pub fn is_vertex_array(&self, vao: u32) -> bool {
        vao != 0 && self.local.vaos.contains_key(&vao)
    }

    /// The GL buffer name currently bound to `target` (`0` = none). `GL_ARRAY_BUFFER` /
    /// `GL_ELEMENT_ARRAY_BUFFER` read their dedicated bindings; every other target reads the general
    /// binding map (`glBindBuffer` of a UBO/SSBO/PBO/dispatch-indirect target).
    pub fn buffer_for_target(&self, target: u32) -> u32 {
        match target {
            glconst::GL_ARRAY_BUFFER => self.local.array_buffer,
            glconst::GL_ELEMENT_ARRAY_BUFFER => self.local.element_buffer,
            t => self.local.general_buffers.get(&t).copied().unwrap_or(0),
        }
    }

    /// The default-framebuffer draw-target width/height in pixels (the window-surface size).
    pub fn target_wh(&self) -> (i32, i32) {
        (self.local.surf.width as i32, self.local.surf.height as i32)
    }

    /// The default-framebuffer read-target width/height in pixels.
    pub fn read_target_wh(&self) -> (i32, i32) {
        (
            self.local.read_surf.width as i32,
            self.local.read_surf.height as i32,
        )
    }

    /// Reset the per-frame draw state after a successful swap (`eglSwapBuffers` tail).
    pub fn reset_frame(&mut self) {
        self.local.recording.clear();
        self.local.default_present_pending = false;
    }

    /// Mark this frame's default framebuffer as already rendered by a `glReadPixels`, so the next
    /// `eglSwapBuffers` posts that render rather than an empty frame (GL: `glReadPixels` is not a frame
    /// boundary; `eglSwapBuffers` posts the default framebuffer's contents).
    pub fn defer_default_present(&mut self) {
        self.local.default_present_pending = true;
    }

    /// Take the deferred-present mark set by [`Self::defer_default_present`].
    pub fn take_deferred_default_present(&mut self) -> bool {
        std::mem::take(&mut self.local.default_present_pending)
    }

    pub(crate) fn record_draw(&mut self, draw: DrawCall) {
        self.local.recording.push_draw(draw);
    }

    pub(crate) fn record_blit(&mut self, blit: BlitOp) {
        self.local.recording.push_blit(blit);
    }

    // ---- error register (glGetError) -------------------------------------------------------------

    /// Record a GL error. GL keeps the FIRST error raised until `glGetError` clears it, so a later error
    /// does not overwrite a still-unread one (first-error-wins).
    pub fn set_gl_error(&mut self, e: u32) {
        if !self.local.no_error && self.local.gl_error == glconst::GL_NO_ERROR {
            hl_log::hl_debug!(hl_log::tag::GL, "gl_error set=0x{:x}", e);
            self.local.gl_error = e;
        }
    }

    /// Read + clear the last GL error (`glGetError`), returning `GL_NO_ERROR` when none is pending.
    pub fn take_gl_error(&mut self) -> u32 {
        std::mem::replace(&mut self.local.gl_error, glconst::GL_NO_ERROR)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_switches_bindings_but_keeps_shared_objects_and_ir_ids() {
        let mut shared = GlContext::new();
        let mut first = ContextState::new();
        let mut second = ContextState::new();

        shared.switch_state(&mut first);
        shared.buffers.ensure(7);
        shared.local.array_buffer = 7;
        let sampler = shared.samplers.gen();
        shared.samplers.bind(0, sampler);
        let first_ir = shared.alloc_buffer_ir().expect("buffer id");
        shared.switch_state(&mut first);

        shared.switch_state(&mut second);
        assert_eq!(shared.local.array_buffer, 0);
        assert!(shared.buffers.get(7).is_some());
        assert!(shared.samplers.known(sampler));
        assert_eq!(shared.samplers.binding(0), 0);
        shared.local.array_buffer = 9;
        let second_ir = shared.alloc_buffer_ir().expect("buffer id");
        shared.switch_state(&mut second);

        shared.switch_state(&mut first);
        assert_eq!(shared.local.array_buffer, 7);
        assert_eq!(shared.samplers.binding(0), sampler);
        assert!(second_ir > first_ir);
    }

    #[test]
    fn retiring_inactive_context_releases_its_default_target() {
        let mut shared = GlContext::new();
        let mut context = ContextState::new();

        shared.switch_state(&mut context);
        shared.local.present_token =
            Some(hl_gpu::protocol::model::descriptor::SurfaceToken::new(7).unwrap());
        let (surface, texture, _) = shared.default_target(640, 480).expect("default target");
        shared.switch_state(&mut context);
        shared.retire_state(&mut context);

        assert_eq!(
            shared.pending_destroys(),
            &[Cmd::DestroyTexture(texture), Cmd::DestroySurface(surface)]
        );
        shared.switch_state(&mut context);
        assert!(shared.default_target(640, 480).expect("default target").2);
    }

    #[test]
    fn changing_surface_token_retires_same_sized_default_target() {
        let mut context = GlContext::new();
        context.local.present_token =
            Some(hl_gpu::protocol::model::descriptor::SurfaceToken::new(11).unwrap());
        let (old_surface, old_texture, created) =
            context.default_target(640, 480).expect("old target");
        assert!(created);

        context.local.present_token =
            Some(hl_gpu::protocol::model::descriptor::SurfaceToken::new(12).unwrap());
        let (new_surface, new_texture, created) =
            context.default_target(640, 480).expect("new target");
        assert!(created);
        assert_ne!((old_surface, old_texture), (new_surface, new_texture));
        assert_eq!(
            context.pending_destroys(),
            &[
                Cmd::DestroyTexture(old_texture),
                Cmd::DestroySurface(old_surface)
            ]
        );
    }
}
