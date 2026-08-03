use super::local::SurfaceTarget;
use super::*;
use hl_gpu::protocol::model::enums::{TextureAspect, TextureDim, TextureFormat};

impl GlContext {
    pub(crate) fn forget_framebuffer_depth_target(&mut self, fbo: u32) {
        self.depth_target_current.retain(|(name, _), _| *name != fbo);
    }

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
            // A fresh presentable texture is ZERO-FILLED, so on this frame — and only this frame — a draw
            // that fails to lower leaves the desktop showing through rather than stale content. That
            // makes the re-mint the middle link of the transparent-rectangle chain, and it was invisible
            // in the domain log: an operator saw a dropped draw and a transparent region with nothing
            // connecting them.
            //
            // The trigger is named because resize is NOT the only one. The token is assigned per swap
            // from the native frame acquired for it, so any swap taking a different native frame
            // re-mints at UNCHANGED size — and a native-to-readback transition is a token change too,
            // since the readback path carries no token at all. A browser recreating a layer or the
            // presentation path flipping mode looks like this; a resize looks different.
            let trigger = match (
                target.size != (w, h),
                target.token != self.local.present_token,
            ) {
                (true, true) => "size and present token",
                (true, false) => "size",
                _ => "present token",
            };
            hl_log::hl_debug!(
                hl_log::tag::GL,
                "default target re-minted on {trigger} change: texture {} -> fresh (zero-filled), \
                 size {:?} -> {:?}, token {:?} -> {:?}",
                target.texture,
                target.size,
                (w, h),
                target.token,
                self.local.present_token
            );
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

    /// The resident default target of the DRAW surface as `(surface_ir, texture_ir)`. `eglSwapBuffers`
    /// presents this when a `glReadPixels` already rendered the frame.
    pub fn resident_default_draw_target(&self) -> Option<(u32, u32)> {
        let target = self
            .local
            .default_targets
            .get(&self.local.draw_surface_id)?;
        (target.texture != 0).then_some((target.surface, target.texture))
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

    /// The dimension-specific 1x1 placeholder sampled-texture and shared default-sampler IR ids used to fill a
    /// DECLARED-but-unbound sampler slot (see [`Self::default_placeholder_tex`]). Returns
    /// `(texture_ir, sampler_ir, create_texture, create_sampler)`. Textures are cached per view dimension;
    /// the sampler remains dimension-independent and is created only once.
    pub fn default_placeholder(
        &mut self,
        dim: TextureDim,
    ) -> hl_gpu::Result<(u32, u32, bool, bool)> {
        let index = match dim {
            TextureDim::D2 => 0,
            TextureDim::D3 => 1,
            TextureDim::Cube => 2,
            TextureDim::D1 => {
                return Err(hl_gpu::GpuError::Unsupported("gl: D1 placeholder"));
            }
        };
        let create_texture = self.default_placeholder_tex[index] == 0;
        if create_texture {
            self.default_placeholder_tex[index] = self.alloc_texture_ir()?;
        }
        let create_sampler = self.default_placeholder_samp == 0;
        if create_sampler {
            self.default_placeholder_samp = self.alloc_sampler_ir()?;
        }
        Ok((
            self.default_placeholder_tex[index],
            self.default_placeholder_samp,
            create_texture,
            create_sampler,
        ))
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
        let (name, texture) = self.framebuffer_color_texture(fbo, attachment)?;
        let target = self.resident_fbo_target_tex(name, texture.gen)?;
        Some((target, texture.w, texture.h, texture.ir_format))
    }

    /// The depth-buffer texture IR for the storage attached to `fbo`, at the depth format selected by
    /// `with_stencil` (`false` = `Depth32Float`, `true` = `Depth24PlusStencil8`).
    /// Returns `(depth_texture, needs_create)`: `needs_create` is true exactly on the first request for this
    /// `(attached storage generation, format)` pair, so the frame builder emits the depth `CreateTexture`
    /// once and reuses
    /// the id on later frames. Only allocated when a depth- or stencil-tested draw actually needs an
    /// attachment.
    pub fn depth_target(
        &mut self,
        fbo: u32,
        color_tex: u32,
        with_stencil: bool,
        attachments: crate::model::program::DepthStencilSnapshot,
    ) -> hl_gpu::Result<(
        u32,
        bool,
        Vec<(u32, hl_gpu::protocol::model::enums::TextureAspect)>,
    )> {
        let depth = (fbo != 0).then_some(attachments.depth).flatten();
        let stencil = (fbo != 0).then_some(attachments.stencil).flatten();
        let key = DepthTargetKey {
            fallback_color: if depth.is_none() && stencil.is_none() {
                color_tex
            } else {
                0
            },
            depth,
            stencil,
            with_stencil,
        };
        let current_key = (fbo, with_stencil);
        let (depth, needs_create) = if let Some(&depth) = self.depth_targets.get(&key) {
            (depth, false)
        } else {
            let depth = self.alloc_texture_ir()?;
            self.depth_targets.insert(key, depth);
            (depth, true)
        };
        let mut preserve = Vec::with_capacity(2);
        if let Some(storage) = key.depth {
            if let Some(&source) = self.depth_aspect_current.get(&storage) {
                if source != depth {
                    preserve.push((source, TextureAspect::DepthOnly));
                }
            }
            self.depth_aspect_current.insert(storage, depth);
        }
        if let Some(storage) = key.stencil {
            if let Some(&source) = self.stencil_aspect_current.get(&storage) {
                if source != depth {
                    preserve.push((source, TextureAspect::StencilOnly));
                }
            }
            self.stencil_aspect_current.insert(storage, depth);
        }
        let previous = self.depth_target_current.insert(current_key, (key, depth));
        if let Some((old, _)) = previous {
            if let Some(storage) = old.depth.filter(|storage| Some(*storage) != key.depth) {
                let still_attached = self
                    .depth_target_current
                    .values()
                    .any(|(current, _)| current.depth == Some(storage));
                if !still_attached {
                    self.depth_aspect_current.remove(&storage);
                }
            }
            if let Some(storage) = old.stencil.filter(|storage| Some(*storage) != key.stencil) {
                let still_attached = self
                    .depth_target_current
                    .values()
                    .any(|(current, _)| current.stencil == Some(storage));
                if !still_attached {
                    self.stencil_aspect_current.remove(&storage);
                }
            }
        }
        let obsolete = preserve
            .iter()
            .map(|(source, _)| *source)
            .filter(|source| {
                !self.depth_aspect_current.values().any(|current| current == source)
                    && !self.stencil_aspect_current.values().any(|current| current == source)
            })
            .collect::<std::collections::HashSet<_>>();
        for source in obsolete {
            self.depth_targets.retain(|_, target| *target != source);
            self.queue_texture_destroy(source);
        }
        Ok((depth, needs_create, preserve))
    }
}

#[cfg(test)]
fn preserved_aspect(
    old: DepthTargetKey,
    new: DepthTargetKey,
) -> Option<hl_gpu::protocol::model::enums::TextureAspect> {
    use hl_gpu::protocol::model::enums::TextureAspect;
    if old == new {
        None
    } else if old.depth == new.depth && old.stencil != new.stencil {
        Some(TextureAspect::DepthOnly)
    } else if old.stencil == new.stencil && old.depth != new.depth {
        Some(TextureAspect::StencilOnly)
    } else {
        None
    }
}

#[cfg(test)]
mod depth_preservation_tests {
    use super::*;
    use hl_gpu::protocol::model::enums::TextureAspect;

    fn key(depth_gen: u64, stencil_gen: u64) -> DepthTargetKey {
        DepthTargetKey {
            fallback_color: 0,
            depth: Some((10, depth_gen)),
            stencil: Some((20, stencil_gen)),
            with_stencil: true,
        }
    }

    #[test]
    fn recreate_depth_preserves_stencil() {
        assert_eq!(preserved_aspect(key(1, 1), key(2, 1)), Some(TextureAspect::StencilOnly));
    }

    #[test]
    fn recreate_stencil_preserves_depth() {
        assert_eq!(preserved_aspect(key(1, 1), key(1, 2)), Some(TextureAspect::DepthOnly));
    }

    #[test]
    fn shared_attachments_reuse_without_copy() {
        assert_eq!(preserved_aspect(key(1, 1), key(1, 1)), None);
    }

    #[test]
    fn shared_depth_tracks_the_latest_target_across_framebuffers() {
        let mut ctx = GlContext::new();
        let depth = ctx.textures.gen();
        let stencil_a = ctx.textures.gen();
        let stencil_b = ctx.textures.gen();
        for texture in [depth, stencil_a, stencil_b] {
            assert!(ctx.textures.image_2d(
                texture,
                8,
                8,
                &[],
                TextureFormat::Rgba8Unorm,
            ));
        }
        let a = ctx.local.framebuffers.gen();
        let b = ctx.local.framebuffers.gen();
        ctx.local.framebuffers.attach_depth(a, depth, false);
        ctx.local.framebuffers.attach_stencil(a, stencil_a, false);
        ctx.local.framebuffers.attach_depth(b, depth, false);
        ctx.local.framebuffers.attach_stencil(b, stencil_b, false);

        let attachments_a = crate::model::program::DepthStencilSnapshot {
            depth: Some((10, 1)),
            stencil: Some((20, 1)),
        };
        let attachments_b = crate::model::program::DepthStencilSnapshot {
            depth: Some((10, 1)),
            stencil: Some((21, 1)),
        };
        let (target_a, _, first) = ctx.depth_target(a, 0, true, attachments_a).unwrap();
        assert!(first.is_empty());
        let (target_b, _, into_b) = ctx.depth_target(b, 0, true, attachments_b).unwrap();
        assert_eq!(into_b, vec![(target_a, TextureAspect::DepthOnly)]);
        let (target_a_again, _, back_into_a) = ctx.depth_target(a, 0, true, attachments_a).unwrap();
        assert_eq!(target_a_again, target_a);
        assert_eq!(back_into_a, vec![(target_b, TextureAspect::DepthOnly)]);
    }

    #[test]
    fn rejected_recreation_restores_aspect_lifetime_state() {
        let mut ctx = GlContext::new();
        let before = ctx.frame_state();
        let old = crate::model::program::DepthStencilSnapshot {
            depth: Some((10, 1)),
            stencil: Some((20, 1)),
        };
        let new = crate::model::program::DepthStencilSnapshot {
            depth: Some((10, 2)),
            stencil: Some((20, 1)),
        };
        let (old_target, _, _) = ctx.depth_target(1, 0, true, old).unwrap();
        let (_, _, preserve) = ctx.depth_target(1, 0, true, new).unwrap();
        assert_eq!(preserve, vec![(old_target, TextureAspect::StencilOnly)]);
        assert!(ctx
            .pending_destroys()
            .iter()
            .any(|command| matches!(command, Cmd::DestroyTexture(target) if *target == old_target)));

        ctx.restore_frame_state(before);
        assert!(ctx.depth_targets.is_empty());
        assert!(ctx.depth_target_current.is_empty());
        assert!(ctx.depth_aspect_current.is_empty());
        assert!(ctx.stencil_aspect_current.is_empty());
        assert!(ctx.pending_destroys().is_empty());
    }
}
