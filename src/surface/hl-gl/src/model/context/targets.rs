use super::*;

impl GlContext {
    pub fn alloc_buffer_ir(&mut self) -> u32 {
        let id = self.next_buffer;
        self.next_buffer += 1;
        id
    }
    pub fn alloc_texture_ir(&mut self) -> u32 {
        let id = self.next_texture;
        self.next_texture += 1;
        id
    }
    pub fn alloc_sampler_ir(&mut self) -> u32 {
        let id = self.next_sampler;
        self.next_sampler += 1;
        id
    }
    pub fn alloc_shader_ir(&mut self) -> u32 {
        let id = self.next_shader;
        self.next_shader += 1;
        id
    }
    pub fn alloc_pipeline_ir(&mut self) -> u32 {
        let id = self.next_pipeline;
        self.next_pipeline += 1;
        id
    }
    pub fn alloc_bind_group_ir(&mut self) -> u32 {
        let id = self.next_bind_group;
        self.next_bind_group += 1;
        id
    }
    pub fn alloc_fence_ir(&mut self) -> u32 {
        let id = self.next_fence;
        self.next_fence += 1;
        id
    }

    /// Mint a fresh opaque sync-object token (non-zero, so a `GLsync` is never null).
    pub fn mint_sync_token(&mut self) -> usize {
        let t = self.next_sync_token;
        self.next_sync_token += 1;
        t
    }

    /// The default render-target texture + presentable surface IR ids. Returns `(surface, texture,
    /// needs_create)`: `needs_create` is true exactly on the first call, so the frame builder emits the
    /// `CreateTexture` + `CreateSurface` once and reuses the ids on every later frame.
    pub fn default_target(&mut self, w: i32, h: i32) -> (u32, u32, bool) {
        // A cached target minted at a DIFFERENT size than the window now is (a resize) is retired: the
        // window surface starts at a tile-sized extent and Chrome negotiates its real size a few frames in,
        // and a stale-sized default texture makes the composited window read back at a mismatched stride
        // (the whole frame SHEARS). Retire it (frame-tail `Destroy*`, same path as `retire_all`) and fall
        // through to mint a fresh id at the new size — a FRESH id, so it cannot collide with the retired one
        // still pending destroy on the host.
        if self.default_tex_ir != 0 && self.default_target_wh != (w, h) {
            self.pending_destroys
                .push(Cmd::DestroyTexture(self.default_tex_ir));
            self.pending_destroys
                .push(Cmd::DestroySurface(self.default_surface_ir));
            self.default_tex_ir = 0;
            self.default_surface_ir = 0;
        }
        if self.default_tex_ir == 0 {
            self.default_tex_ir = self.alloc_texture_ir();
            self.default_surface_ir = self.next_surface;
            self.next_surface += 1;
            self.default_target_wh = (w, h);
            (self.default_surface_ir, self.default_tex_ir, true)
        } else {
            (self.default_surface_ir, self.default_tex_ir, false)
        }
    }

    /// The shared 1x1 placeholder sampled-texture + default-sampler IR ids used to fill a
    /// DECLARED-but-unbound sampler slot (see [`Self::default_placeholder_tex`]). Returns
    /// `(texture_ir, sampler_ir, needs_create)`: `needs_create` is true exactly on the first call, so the
    /// frame builder emits the `CreateTexture` + staging upload + `CreateSampler` once and reuses the ids
    /// on every later empty sampler slot in this and subsequent frames.
    pub fn default_placeholder(&mut self) -> (u32, u32, bool) {
        if self.default_placeholder_tex == 0 {
            self.default_placeholder_tex = self.alloc_texture_ir();
            self.default_placeholder_samp = self.alloc_sampler_ir();
            (
                self.default_placeholder_tex,
                self.default_placeholder_samp,
                true,
            )
        } else {
            (
                self.default_placeholder_tex,
                self.default_placeholder_samp,
                false,
            )
        }
    }

    /// The offscreen render-target texture + presentable surface IR ids for the FBO whose color
    /// attachment is GL texture `gl_tex`. Returns `(surface, texture, needs_create)`: `needs_create` is
    /// true exactly on the first request for this attachment, so the frame builder emits the
    /// `CreateTexture`/`CreateSurface` once and reuses the ids on later frames.
    pub fn fbo_target(&mut self, gl_tex: u32, generation: u64) -> (u32, u32, bool) {
        let key = (gl_tex, generation);
        if let Some(&(surface, texture)) = self.fbo_targets.get(&key) {
            (surface, texture, false)
        } else {
            let texture = self.alloc_texture_ir();
            let surface = self.next_surface;
            self.next_surface += 1;
            self.fbo_targets.insert(key, (surface, texture));
            (surface, texture, true)
        }
    }

    /// The persistent render-target texture IR a prior render pass wrote for the FBO whose color attachment
    /// is GL texture `gl_tex`, if one has been materialized (via [`Self::fbo_target`]). Used by the frame
    /// builder to sample an offscreen attachment's RENDERED pixels ACROSS frames — e.g. after a
    /// `glFlush`/`glFinish` executed the offscreen tile passes into their targets (see
    /// [`crate::service::swap::flush_offscreen`]) and a LATER window pass composites them. Returns `None`
    /// when `gl_tex` has never been an FBO render target (so the caller keeps its normal upload/placeholder
    /// resolution). Read-only (does not mint) so a mere sample never allocates a target.
    pub fn resident_fbo_target_tex(&self, gl_tex: u32, generation: u64) -> Option<u32> {
        self.fbo_targets
            .get(&(gl_tex, generation))
            .map(|&(_surface, texture)| texture)
    }

    /// The depth-buffer texture IR for the render pass whose COLOR target is texture IR `color_tex`, at the
    /// depth format selected by `with_stencil` (`false` = `Depth32Float`, `true` = `Depth24PlusStencil8`).
    /// Returns `(depth_texture, needs_create)`: `needs_create` is true exactly on the first request for this
    /// `(color target, format)` pair, so the frame builder emits the depth `CreateTexture` once and reuses
    /// the id on later frames. Only allocated when a depth- or stencil-tested draw actually needs an
    /// attachment.
    pub fn depth_target(&mut self, color_tex: u32, with_stencil: bool) -> (u32, bool) {
        if let Some(&depth) = self.depth_targets.get(&(color_tex, with_stencil)) {
            (depth, false)
        } else {
            let depth = self.alloc_texture_ir();
            self.depth_targets.insert((color_tex, with_stencil), depth);
            (depth, true)
        }
    }
}
