use super::local::SurfaceTarget;
use super::*;
use hl_gpu::protocol::model::enums::TextureFormat;

impl GlContext {
    pub fn validate_external_target(
        &self,
        token: hl_gpu::protocol::model::descriptor::SurfaceToken,
        width: i32,
        height: i32,
    ) -> hl_gpu::Result<()> {
        for ((name, generation), existing) in &self.external_targets {
            if *existing != token {
                continue;
            }
            let Some(texture) = self.textures.get(*name) else {
                continue;
            };
            if texture.gen == *generation && (texture.w != width || texture.h != height) {
                return Err(hl_gpu::GpuError::Invalid(
                    "external surface token has an incompatible live layout",
                ));
            }
        }
        Ok(())
    }

    pub fn bind_external_target(
        &mut self,
        name: u32,
        generation: u64,
        token: hl_gpu::protocol::model::descriptor::SurfaceToken,
    ) {
        self.external_targets.insert((name, generation), token);
    }

    pub fn external_target(
        &self,
        name: u32,
        generation: u64,
    ) -> Option<hl_gpu::protocol::model::descriptor::SurfaceToken> {
        self.external_targets.get(&(name, generation)).copied()
    }

    pub fn fbo_surface(&self, name: u32, generation: u64) -> Option<u32> {
        self.fbo_targets
            .get(&(name, generation))
            .map(|(surface, _)| *surface)
    }

    pub fn alloc_buffer_ir(&self) -> hl_gpu::Result<u32> {
        self.issued(Resource::Buffer, self.allocator.buffer())
    }
    pub fn alloc_texture_ir(&self) -> hl_gpu::Result<u32> {
        self.issued(Resource::Texture, self.allocator.texture())
    }
    pub fn alloc_sampler_ir(&self) -> hl_gpu::Result<u32> {
        self.issued(Resource::Sampler, self.allocator.sampler())
    }
    pub fn alloc_shader_ir(&self) -> hl_gpu::Result<u32> {
        self.issued(Resource::Shader, self.allocator.shader())
    }
    pub fn alloc_pipeline_ir(&self) -> hl_gpu::Result<u32> {
        self.issued(Resource::Pipeline, self.allocator.pipeline())
    }
    pub fn alloc_bind_group_ir(&self) -> hl_gpu::Result<u32> {
        self.issued(Resource::BindGroup, self.allocator.bind_group())
    }
    pub fn alloc_fence_ir(&self) -> hl_gpu::Result<u32> {
        self.issued(Resource::Fence, self.allocator.fence())
    }
    pub fn alloc_frame_serial(&self) -> hl_gpu::Result<hl_gpu::FrameSerial> {
        self.allocator.frame()
    }

    /// The names issued while lowering the current frame.
    pub(super) fn frame_ledger(&self) -> std::sync::MutexGuard<'_, Vec<(Resource, u32)>> {
        self.frame_ids.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Record a freshly issued IR name against the frame being lowered, so a rejected batch can return it.
    fn issued(&self, kind: Resource, id: hl_gpu::Result<u32>) -> hl_gpu::Result<u32> {
        if let Ok(name) = id {
            self.frame_ledger().push((kind, name));
        }
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
    pub fn default_target(&mut self, w: i32, h: i32) -> hl_gpu::Result<(u32, u32, bool)> {
        let key = self.local.draw_surface_id;
        let mut target = self.local.default_targets.remove(&key).unwrap_or_default();
        // A cached target minted at a DIFFERENT size than the window now is (a resize) is retired: the
        // window surface starts at a tile-sized extent and Chrome negotiates its real size a few frames in,
        // and a stale-sized default texture makes the composited window read back at a mismatched stride
        // (the whole frame SHEARS). Retire it (frame-tail `Destroy*`, same path as `retire_all`) and fall
        // through to mint a fresh id at the new size — a FRESH id, so it cannot collide with the retired one
        // still pending destroy on the host.
        if target.texture != 0
            && (target.size != (w, h) || target.token != self.local.present_token)
        {
            self.pending_destroys
                .push(Cmd::DestroyTexture(target.texture));
            if target.token.is_some() {
                self.pending_destroys
                    .push(Cmd::DestroySurface(target.surface));
            }
            target = SurfaceTarget::default();
        }
        let created = target.texture == 0;
        if created {
            target.texture = self.alloc_texture_ir()?;
            target.surface = self.issued(Resource::Surface, self.allocator.surface())?;
            target.size = (w, h);
            target.token = self.local.present_token;
        }
        let result = (target.surface, target.texture, created);
        self.local.default_targets.insert(key, target);
        Ok(result)
    }

    pub fn resident_default_read_target(&self) -> Option<(u32, i32, i32, TextureFormat)> {
        let target = self
            .local
            .default_targets
            .get(&self.local.read_surface_id)?;
        (target.texture != 0).then_some((
            target.texture,
            target.size.0,
            target.size.1,
            TextureFormat::Bgra8Unorm,
        ))
    }

    pub fn install_surface_target(&mut self, surface: u64, target: SurfaceTarget) {
        if target != SurfaceTarget::default() {
            self.local.default_targets.insert(surface, target);
        }
    }

    pub fn take_surface_target(&mut self, surface: u64) -> SurfaceTarget {
        self.local
            .default_targets
            .remove(&surface)
            .unwrap_or_default()
    }

    /// The shared 1x1 placeholder sampled-texture + default-sampler IR ids used to fill a
    /// DECLARED-but-unbound sampler slot (see [`Self::default_placeholder_tex`]). Returns
    /// `(texture_ir, sampler_ir, needs_create)`: `needs_create` is true exactly on the first call, so the
    /// frame builder emits the `CreateTexture` + staging upload + `CreateSampler` once and reuses the ids
    /// on every later empty sampler slot in this and subsequent frames.
    pub fn default_placeholder(&mut self) -> hl_gpu::Result<(u32, u32, bool)> {
        if self.default_placeholder_tex == 0 {
            let texture = self.alloc_texture_ir()?;
            let sampler = self.alloc_sampler_ir()?;
            self.default_placeholder_tex = texture;
            self.default_placeholder_samp = sampler;
            Ok((texture, sampler, true))
        } else {
            Ok((
                self.default_placeholder_tex,
                self.default_placeholder_samp,
                false,
            ))
        }
    }

    /// The offscreen render-target texture + presentable surface IR ids for the FBO whose color
    /// attachment is GL texture `gl_tex`. Returns `(surface, texture, needs_create)`: `needs_create` is
    /// true exactly on the first request for this attachment, so the frame builder emits the
    /// `CreateTexture`/`CreateSurface` once and reuses the ids on later frames.
    pub fn fbo_target(&mut self, gl_tex: u32, generation: u64) -> hl_gpu::Result<(u32, u32, bool)> {
        let key = (gl_tex, generation);
        if let Some(&(surface, texture)) = self.fbo_targets.get(&key) {
            Ok((surface, texture, false))
        } else {
            let shared_target = self.external_targets.get(&key).and_then(|token| {
                self.external_targets.iter().find_map(|(other, candidate)| {
                    (*other != key && candidate == token)
                        .then(|| self.fbo_targets.get(other).copied())
                        .flatten()
                })
            });
            if let Some((surface, texture)) = shared_target {
                self.fbo_targets.insert(key, (surface, texture));
                return Ok((surface, texture, false));
            }
            let texture = self.alloc_texture_ir()?;
            let surface = self.issued(Resource::Surface, self.allocator.surface())?;
            self.fbo_targets.insert(key, (surface, texture));
            Ok((surface, texture, true))
        }
    }

    /// Resolve a target captured by a deferred draw without making a deleted GL object resident again.
    pub fn recorded_fbo_target(
        &mut self,
        gl_tex: u32,
        generation: u64,
    ) -> hl_gpu::Result<(u32, u32, bool, bool)> {
        let live = self
            .textures
            .get(gl_tex)
            .is_some_and(|texture| texture.gen == generation);
        if live || self.fbo_targets.contains_key(&(gl_tex, generation)) {
            let (surface, texture, create) = self.fbo_target(gl_tex, generation)?;
            return Ok((surface, texture, create, false));
        }
        let texture = self.alloc_texture_ir()?;
        let surface = self.issued(Resource::Surface, self.allocator.surface())?;
        Ok((surface, texture, true, true))
    }

    /// The persistent render-target texture IR a prior render pass wrote for the FBO whose color attachment
    /// is GL texture `gl_tex`, if one has been materialized (via [`Self::fbo_target`]). Used by the frame
    /// builder to sample an offscreen attachment's RENDERED pixels ACROSS frames — e.g. after a
    /// `glFlush`/`glFinish` executed the offscreen tile passes into their targets (see
    /// [`crate::service::swap::flush`]) and a LATER window pass composites them. Returns `None`
    /// when `gl_tex` has never been an FBO render target (so the caller keeps its normal upload/placeholder
    /// resolution). Read-only (does not mint) so a mere sample never allocates a target.
    pub fn resident_fbo_target_tex(&self, gl_tex: u32, generation: u64) -> Option<u32> {
        let key = (gl_tex, generation);
        self.fbo_targets
            .get(&key)
            .map(|&(_surface, texture)| texture)
            .or_else(|| {
                let token = self.external_targets.get(&key)?;
                self.external_targets.iter().find_map(|(other, candidate)| {
                    (other != &key && candidate == token)
                        .then(|| self.fbo_targets.get(other).map(|&(_, texture)| texture))
                        .flatten()
                })
            })
    }

    /// The persistent color target currently selected for reads from `fbo`, after an earlier
    /// `glFlush`/`glFinish` consumed its draw list.
    pub fn resident_fbo_read_target(
        &self,
        fbo: u32,
        attachment: u32,
    ) -> Option<(u32, i32, i32, hl_gpu::protocol::model::enums::TextureFormat)> {
        if fbo == 0 {
            return self.resident_default_read_target();
        }
        let name = self
            .local
            .framebuffers
            .color_attachment_index(fbo, attachment);
        let texture = self.textures.get(name)?;
        let target = self.resident_fbo_target_tex(name, texture.gen)?;
        Some((target, texture.w, texture.h, texture.ir_format))
    }

    /// The depth-buffer texture IR for the render pass whose COLOR target is texture IR `color_tex`, at the
    /// depth format selected by `with_stencil` (`false` = `Depth32Float`, `true` = `Depth24PlusStencil8`).
    /// Returns `(depth_texture, needs_create)`: `needs_create` is true exactly on the first request for this
    /// `(color target, format)` pair, so the frame builder emits the depth `CreateTexture` once and reuses
    /// the id on later frames. Only allocated when a depth- or stencil-tested draw actually needs an
    /// attachment.
    pub fn depth_target(
        &mut self,
        color_tex: u32,
        with_stencil: bool,
    ) -> hl_gpu::Result<(u32, bool)> {
        if let Some(&depth) = self.depth_targets.get(&(color_tex, with_stencil)) {
            Ok((depth, false))
        } else {
            let depth = self.alloc_texture_ir()?;
            self.depth_targets.insert((color_tex, with_stencil), depth);
            Ok((depth, true))
        }
    }
}
