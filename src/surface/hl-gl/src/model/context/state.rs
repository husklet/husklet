use super::*;

impl GlContext {
    // ---- vertex array objects (glGenVertexArrays / glBindVertexArray / …) ------------------------

    /// `glGenVertexArrays` (one name) — mint a fresh VAO name with empty captured state.
    pub fn gen_vertex_array(&mut self) -> u32 {
        let id = self.next_vao;
        self.next_vao += 1;
        self.vaos.insert(id, Vao::default());
        id
    }

    /// `glBindVertexArray(vao)` — snapshot the live attribute array + element-buffer binding into the
    /// currently-bound VAO, then load `vao`'s captured state into the live context. Binding an unknown
    /// name creates that VAO on demand (matching GL's "first bind creates the object") with empty state.
    pub fn bind_vertex_array(&mut self, vao: u32) {
        self.vaos.insert(
            self.cur_vao,
            Vao {
                attrs: self.attr,
                element_buffer: self.element_buffer,
            },
        );
        self.cur_vao = vao;
        match self.vaos.get(&vao) {
            Some(v) => {
                self.attr = v.attrs;
                self.element_buffer = v.element_buffer;
            }
            None => {
                self.attr = [Attr::default(); MAX_ATTR];
                self.element_buffer = 0;
                self.vaos.insert(vao, Vao::default());
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
        if self.cur_vao == vao {
            self.cur_vao = 0;
            let def = self.vaos.get(&0).copied().unwrap_or_default();
            self.attr = def.attrs;
            self.element_buffer = def.element_buffer;
        }
        self.vaos.remove(&vao).is_some()
    }

    /// `glIsVertexArray(vao)` — true once `vao` names a generated (non-default) VAO object.
    pub fn is_vertex_array(&self, vao: u32) -> bool {
        vao != 0 && self.vaos.contains_key(&vao)
    }

    /// The GL buffer name currently bound to `target` (`0` = none). `GL_ARRAY_BUFFER` /
    /// `GL_ELEMENT_ARRAY_BUFFER` read their dedicated bindings; every other target reads the general
    /// binding map (`glBindBuffer` of a UBO/SSBO/PBO/dispatch-indirect target).
    pub fn buffer_for_target(&self, target: u32) -> u32 {
        match target {
            glconst::GL_ARRAY_BUFFER => self.array_buffer,
            glconst::GL_ELEMENT_ARRAY_BUFFER => self.element_buffer,
            t => self.general_buffers.get(&t).copied().unwrap_or(0),
        }
    }

    /// The default-framebuffer draw-target width/height in pixels (the window-surface size).
    pub fn target_wh(&self) -> (i32, i32) {
        (self.surf.width as i32, self.surf.height as i32)
    }

    /// Reset the per-frame draw state after a successful swap (`eglSwapBuffers` tail).
    pub fn reset_frame(&mut self) {
        self.draws.clear();
        self.blits.clear();
    }

    // ---- error register (glGetError) -------------------------------------------------------------

    /// Record a GL error. GL keeps the FIRST error raised until `glGetError` clears it, so a later error
    /// does not overwrite a still-unread one (first-error-wins).
    pub fn set_gl_error(&mut self, e: u32) {
        if self.gl_error == glconst::GL_NO_ERROR {
            hl_log::hl_debug!(hl_log::tag::GL, "gl_error set=0x{:x}", e);
            self.gl_error = e;
        }
    }

    /// Read + clear the last GL error (`glGetError`), returning `GL_NO_ERROR` when none is pending.
    pub fn take_gl_error(&mut self) -> u32 {
        std::mem::replace(&mut self.gl_error, glconst::GL_NO_ERROR)
    }
}
