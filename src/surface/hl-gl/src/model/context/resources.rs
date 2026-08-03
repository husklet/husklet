use super::*;

impl Default for GlContext {
    fn default() -> Self {
        Self::new()
    }
}

impl GlContext {
    pub fn new() -> Self {
        Self::with_allocator(std::sync::Arc::new(IrAllocator::new()))
    }

    pub fn with_allocator(allocator: std::sync::Arc<IrAllocator>) -> Self {
        Self {
            local: LocalState::default(),
            buffers: Buffers::new(),
            textures: Textures::new(),
            programs: Programs::new(),
            renderbuffers: Renderbuffers::new(),
            samplers: Samplers::new(),
            uniform_blocks: HashMap::new(),
            fence_ir: 0,
            fence_next_value: 1,
            fence_signaled_through: 0,
            syncs: HashMap::new(),
            next_sync_token: 1,
            allocator,
            frame_ids: std::sync::Mutex::new(Vec::new()),
            default_placeholder_tex: 0,
            default_placeholder_samp: 0,
            fbo_targets: HashMap::new(),
            external_targets: HashMap::new(),
            depth_targets: HashMap::new(),
            tex_ir_cache: HashMap::new(),
            shared_tex_ir_cache: HashMap::new(),
            shared_target_cache: HashMap::new(),
            buf_ir_cache: HashMap::new(),
            interop_buf_ir: HashMap::new(),
            prog_shader_cache: HashMap::new(),
            prog_pipeline_cache: HashMap::new(),
            sampler_ir_cache: Vec::new(),
            clear_shader_ir: None,
            clear_pipeline_cache: std::collections::HashMap::new(),
            pending_destroys: Vec::new(),
            pending_texture_deletes: HashSet::new(),
            pending_buffer_deletes: HashSet::new(),
            pending_sampler_deletes: HashSet::new(),
            pending_program_deletes: HashSet::new(),
        }
    }

    /// Capture the exact texture generation currently named by `gl_name` for a deferred draw.
    pub fn texture_snapshot(&self, gl_name: u32) -> Option<crate::model::program::TextureSnapshot> {
        let texture = self.textures.get(gl_name)?.clone();
        let generation = texture.gen;
        let sampled_generation = texture.sampled_generation();
        let sampled_ir = self
            .tex_ir_cache
            .get(&gl_name)
            .and_then(|&(ir, resident_generation)| {
                (resident_generation == sampled_generation).then_some(ir)
            });
        let fbo_ir = self
            .fbo_targets
            .get(&(gl_name, generation))
            .map(|&(_, texture)| texture);
        Some(crate::model::program::TextureSnapshot {
            name: gl_name,
            generation,
            texture,
            sampled_ir,
            fbo_ir,
        })
    }

    // ---- persistent-resource retirement (glDelete* / content change) -----------------------------

    /// Retire GL texture `gl_name`'s resident IR resources (`glDeleteTextures`, or the backing texture of a
    /// deleted renderbuffer): its cached sampled-texture id, its FBO render-target `(texture, surface)` if it
    /// was ever an offscreen attachment, and any depth buffers keyed to that render target. The ids are
    /// removed from the residency caches (so a subsequent bind of the same GL name re-resolves to a fresh id)
    /// and their `Destroy*` are queued for the next submitted frame. A no-op when the name owns no IR yet.
    pub fn retire_texture(&mut self, gl_name: u32) {
        if let Some((ir, _)) = self.tex_ir_cache.remove(&gl_name) {
            self.pending_destroys.push(Cmd::DestroyTexture(ir));
        }
        // A texture image or a renderbuffer's backing texture can own an explicit depth/stencil
        // attachment. Retire that host plane when the attached storage generation dies. Color-fallback
        // planes are handled below because their key contains the host color target, not this GL name.
        let mut dead_attached_depth = Vec::new();
        self.depth_targets.retain(|key, &mut depth| {
            let attached = key.depth.is_some_and(|(name, _)| name == gl_name)
                || key.stencil.is_some_and(|(name, _)| name == gl_name);
            if attached {
                dead_attached_depth.push(depth);
            }
            !attached
        });
        for depth in dead_attached_depth {
            self.pending_destroys.push(Cmd::DestroyTexture(depth));
        }
        let targets: Vec<_> = self
            .fbo_targets
            .keys()
            .filter(|(name, _)| *name == gl_name)
            .copied()
            .collect();
        for key in targets {
            let (surface, texture) = self.fbo_targets.remove(&key).unwrap();
            let external = self.external_targets.remove(&key);
            let still_live = external.is_some_and(|token| {
                self.external_targets.iter().any(|(other, candidate)| {
                    *candidate == token && self.fbo_targets.contains_key(other)
                })
            });
            if still_live {
                continue;
            }
            // Only fallback depth storage is owned by the color target. Explicit FBO depth/stencil
            // attachments have independent lifetime and survive color attachment replacement.
            let mut dead_depth: Vec<u32> = Vec::new();
            self.depth_targets.retain(|key, &mut depth| {
                if key.fallback_color == texture {
                    dead_depth.push(depth);
                    false
                } else {
                    true
                }
            });
            for depth in dead_depth {
                self.pending_destroys.push(Cmd::DestroyTexture(depth));
            }
            if external.is_some() {
                self.pending_destroys.push(Cmd::DestroySurface(surface));
            }
            let mut transferred = false;
            for residency in self
                .shared_target_cache
                .values_mut()
                .filter(|residency| residency.texture == texture)
            {
                residency.owned = true;
                transferred = true;
            }
            if !transferred {
                self.pending_destroys.push(Cmd::DestroyTexture(texture));
            }
        }
        self.external_targets
            .retain(|(name, _), _| *name != gl_name);
    }

    /// Retire GL data buffer `gl_name`'s resident IR buffers (`glDeleteBuffers`) — a buffer can be cached
    /// under both the VERTEX and INDEX usage roles, so every cache entry for this name is dropped and its
    /// `DestroyBuffer` queued. A no-op when the name owns no IR yet.
    pub fn retire_buffer(&mut self, gl_name: u32) {
        let mut dead: Vec<u32> = Vec::new();
        self.buf_ir_cache.retain(|&(name, _), &mut (ir, _)| {
            if name == gl_name {
                dead.push(ir);
                false
            } else {
                true
            }
        });
        for ir in dead {
            self.pending_destroys.push(Cmd::DestroyBuffer(ir));
        }
        if let Some((ir, _)) = self.interop_buf_ir.remove(&gl_name) {
            self.pending_destroys.push(Cmd::DestroyBuffer(ir));
        }
    }

    /// Retire GL program `gl_name`'s resident IR resources (`glDeleteProgram`): its two cached shader MODULES
    /// (`prog_shader_cache`) and EVERY render PIPELINE it minted across the fixed-function / vertex-layout
    /// state variants it was drawn with (`prog_pipeline_cache`). Both caches key on the GL program name, so
    /// every entry for `gl_name` is dropped and a `DestroyShader` / `DestroyPipeline` queued for the next
    /// submitted frame — so a deleted Skia/GskGpu program stops holding host residency instead of leaking its
    /// modules + pipelines forever (Chrome deletes + relinks programs across its lifetime). Removing the cache
    /// entries is ALSO what keeps a RECYCLED program name correct: GL reuses a freed name, and a fresh
    /// `glCreateProgram` + `glLinkProgram` on that name starts back at `link_gen == 1` — the SAME generation
    /// the dead program's stale entry carried — so without this removal [`Self::program_shader_ir`] /
    /// [`Self::program_pipeline_ir`] would HIT the stale entry and hand the new (different-source) program the
    /// dead program's shader/pipeline ids (a silent id collision, or an `UnknownId` once the dead ids are
    /// destroyed). A no-op when the name owns no IR yet. Mirrors [`Self::retire_texture`] /
    /// [`Self::retire_buffer`].
    pub fn retire_program(&mut self, gl_name: u32) {
        let variants: Vec<_> = self
            .prog_shader_cache
            .keys()
            .filter(|(program, _)| *program == gl_name)
            .copied()
            .collect();
        for key in variants {
            let (vs, fs, _) = self.prog_shader_cache.remove(&key).unwrap();
            self.pending_destroys.push(Cmd::DestroyShader(vs));
            self.pending_destroys.push(Cmd::DestroyShader(fs));
        }
        let mut dead: Vec<u32> = Vec::new();
        self.prog_pipeline_cache.retain(|&(name, _), &mut (ir, _)| {
            if name == gl_name {
                dead.push(ir);
                false
            } else {
                true
            }
        });
        for ir in dead {
            self.pending_destroys.push(Cmd::DestroyPipeline(ir));
        }
    }

    /// Retire the WHOLE working set this context has made resident on the host — every IR resource in every
    /// residency cache — queueing a `Destroy*` for each and clearing the caches. Called at CONTEXT TEARDOWN
    /// (the last live EGL context on the shared model is destroyed): Chrome (and Skia/GskGpu generally) frees
    /// its GL objects by DESTROYING THE CONTEXT, never by `glDeleteTexture`/`glDeleteProgram` — so without a
    /// context-granular sweep a lost-context cycle (Chrome loses a context, recreates its entire working set
    /// with FRESH GL names, repeats) piles another full working set onto the host residency ledger every cycle
    /// until the per-connection cap NACKs every swap. This refunds it.
    ///
    /// Every resident IR id is enumerated ONCE from the cache that owns it (a given IR id lives in exactly one
    /// cache — sampled textures, FBO render targets, and depth buffers are all distinct ids — so nothing is
    /// double-destroyed), its `Destroy*` queued for the next submitted frame's tail (same path as the
    /// `glDelete*` retirement), and its cache entry dropped so a later bind of a RECYCLED GL name re-resolves
    /// to a fresh id. The `default_*` / `placeholder_*` / `fence_ir` one-shot ids are reset to `0` so a
    /// still-running process (a new context on the same shared model) re-creates them on next use. The
    /// monotonic `next_*` id counters are deliberately NOT reset: a re-created resource must get a FRESH id
    /// that cannot collide with a just-retired one still pending destroy on the host.
    ///
    /// Share-group safe: the shim multiplexes ALL EGL contexts onto this one `GlContext` (one implicit share
    /// group), so this only fires when NO context remains — there is no other live context whose resources
    /// this could wrongly free.
    pub fn retire_all(&mut self) {
        for (_gl_name, (ir, _gen)) in self.tex_ir_cache.drain() {
            self.pending_destroys.push(Cmd::DestroyTexture(ir));
        }
        for (_identity, residency) in self.shared_tex_ir_cache.drain() {
            self.pending_destroys
                .push(Cmd::DestroyTexture(residency.texture));
        }
        for (_storage, residency) in self.shared_target_cache.drain() {
            if residency.owned {
                self.pending_destroys
                    .push(Cmd::DestroyTexture(residency.texture));
            }
        }
        for (_key, (ir, _gen)) in self.buf_ir_cache.drain() {
            self.pending_destroys.push(Cmd::DestroyBuffer(ir));
        }
        for (_key, (vs, fs, _gen)) in self.prog_shader_cache.drain() {
            self.pending_destroys.push(Cmd::DestroyShader(vs));
            self.pending_destroys.push(Cmd::DestroyShader(fs));
        }
        for (_key, (ir, _gen)) in self.prog_pipeline_cache.drain() {
            self.pending_destroys.push(Cmd::DestroyPipeline(ir));
        }
        for (_descriptor, ir) in self.sampler_ir_cache.drain(..) {
            self.pending_destroys.push(Cmd::DestroySampler(ir));
        }
        let mut external_surfaces = std::collections::HashSet::new();
        let mut target_textures = std::collections::HashSet::new();
        for (key, (surface, texture)) in self.fbo_targets.drain() {
            if self.external_targets.remove(&key).is_some() {
                external_surfaces.insert(surface);
            }
            target_textures.insert(texture);
        }
        self.pending_destroys
            .extend(target_textures.into_iter().map(Cmd::DestroyTexture));
        self.pending_destroys
            .extend(external_surfaces.into_iter().map(Cmd::DestroySurface));
        self.external_targets.clear();
        for (_key, depth) in self.depth_targets.drain() {
            self.pending_destroys.push(Cmd::DestroyTexture(depth));
        }
        for (_, target) in self.local.default_targets.drain() {
            self.pending_destroys
                .push(Cmd::DestroyTexture(target.texture));
            if target.token.is_some() {
                self.pending_destroys
                    .push(Cmd::DestroySurface(target.surface));
            }
        }
        if self.default_placeholder_tex != 0 {
            self.pending_destroys
                .push(Cmd::DestroyTexture(self.default_placeholder_tex));
            self.pending_destroys
                .push(Cmd::DestroySampler(self.default_placeholder_samp));
            self.default_placeholder_tex = 0;
            self.default_placeholder_samp = 0;
        }
        if self.fence_ir != 0 {
            self.pending_destroys.push(Cmd::DestroyFence(self.fence_ir));
            self.fence_ir = 0;
            self.fence_next_value = 1;
            self.fence_signaled_through = 0;
            self.syncs.clear();
        }
    }

    /// The queued persistent `Destroy*` commands (see [`Self::pending_destroys`]). The service layer appends
    /// these to a frame AFTER its `Submit`s (and before any `Present`), then [`Self::clear_pending_destroys`]
    /// once the submit succeeds — so a NACK (which returns before the clear) re-emits them on the retry.
    pub fn pending_destroys(&self) -> &[Cmd] {
        &self.pending_destroys
    }

    /// Whether any persistent `Destroy*` are queued.
    pub fn has_pending_destroys(&self) -> bool {
        !self.pending_destroys.is_empty()
    }

    /// Clear the queued persistent `Destroy*` — called ONLY after the frame carrying them submitted OK.
    pub fn clear_pending_destroys(&mut self) {
        self.pending_destroys.clear();
    }

    /// Replace the retirement queue after a partial submission acknowledges only its unpinned subset.
    pub fn replace_pending_destroys(&mut self, destroys: Vec<Cmd>) {
        self.pending_destroys = destroys;
    }

    /// Queue a frame-local texture for retirement at the retained draw's accepted submission tail.
    ///
    /// Standalone cleanup boundaries keep queued textures pinned by deferred draws; see
    /// [`GlContext::flush_retirements`].
    pub fn queue_texture_destroy(&mut self, texture: u32) {
        self.pending_destroys.push(Cmd::DestroyTexture(texture));
    }

    /// Retire a frame-local capture buffer at the next accepted cleanup boundary.
    pub fn queue_buffer_destroy(&mut self, buffer: u32) {
        self.pending_destroys.push(Cmd::DestroyBuffer(buffer));
    }

    pub(crate) fn queue_destroy(&mut self, command: Cmd) {
        self.pending_destroys.push(command);
    }

    /// The shared placeholder sampler's IR id (`0` = not yet created). The frame builder must NOT free this
    /// among a frame's per-draw ephemeral samplers — it is created once and reused across every frame (see
    /// [`Self::default_placeholder`]).
    pub fn placeholder_sampler_ir(&self) -> u32 {
        self.default_placeholder_samp
    }

    /// Resolve an immutable sampler descriptor to one persistent IR sampler.
    ///
    /// GL texture/sampler mutation is naturally correct: the complete descriptor changes and receives
    /// a different id, while draws that retain an older descriptor keep referring to its immutable sampler.
    pub fn sampler_ir(
        &mut self,
        descriptor: &hl_gpu::protocol::model::descriptor::SamplerDesc,
    ) -> hl_gpu::Result<(u32, bool)> {
        if let Some((_, ir)) = self
            .sampler_ir_cache
            .iter()
            .find(|(candidate, _)| candidate == descriptor)
        {
            return Ok((*ir, false));
        }
        let ir = self.alloc_sampler_ir()?;
        self.sampler_ir_cache.push((descriptor.clone(), ir));
        Ok((ir, true))
    }

    /// The two INTERNAL clear shader modules, `(vs_ir, fs_ir, needs_create)`. Created once per context
    /// and shared by every rect clear: the values a clear carries are all dynamic encoder state, so one
    /// shader pair serves every depth, stencil and colour clear at every value.
    pub fn clear_shader_ir(&mut self) -> hl_gpu::Result<(u32, u32, bool)> {
        if let Some((vs, fs)) = self.clear_shader_ir {
            return Ok((vs, fs, false));
        }
        let vs = self.alloc_shader_ir()?;
        let fs = self.alloc_shader_ir()?;
        self.clear_shader_ir = Some((vs, fs));
        Ok((vs, fs, true))
    }

    /// The internal clear pipeline for one `key`, `(pipeline_ir, needs_create)`.
    pub fn clear_pipeline_ir(
        &mut self,
        key: crate::model::context::ClearPipelineKey,
    ) -> hl_gpu::Result<(u32, bool)> {
        if let Some(&ir) = self.clear_pipeline_cache.get(&key) {
            return Ok((ir, false));
        }
        let ir = self.alloc_pipeline_ir()?;
        self.clear_pipeline_cache.insert(key, ir);
        Ok((ir, true))
    }

    /// The `(GL program name, variant)` a resident IR shader module belongs to, if any.
    ///
    /// The inverse of [`Self::program_shader_ir`], and the only way back from a command the host refused
    /// to the GL object the application would recognise. The variant is part of the answer on purpose: a
    /// program is translated once per specialisation (sampler types, `gl_FragCoord` correction, target
    /// flip), so a refusal names ONE specialisation and reporting only the program name would read as
    /// "this program is broken" when the other specialisations of it may translate perfectly well.
    pub fn shader_ir_program(&self, shader_ir: u32) -> Option<(u32, u64)> {
        self.prog_shader_cache
            .iter()
            .find(|(_, &(vs, fs, _))| vs == shader_ir || fs == shader_ir)
            .map(|(&(program, variant), _)| (program, variant))
    }

    pub fn is_resident_sampler(&self, ir: u32) -> bool {
        self.sampler_ir_cache
            .iter()
            .any(|(_, candidate)| *candidate == ir)
    }

    /// The stable IR shader-module ids `(vs_shader_ir, fs_shader_ir)` a linked render program (`prog`) at
    /// link generation `gen` lowers to. Returns `(vs_ir, fs_ir, needs_create)`: `needs_create` is true on the
    /// first sight of this program (or after a relink bumped `gen`), so the frame builder emits the two
    /// `CreateShader`s exactly then and reuses the resident ids — emitting NOTHING and re-compiling NOTHING on
    /// every later draw+frame that reuses the program. Mirrors [`Self::sampled_texture_ir`].
    pub fn program_shader_ir(
        &mut self,
        prog: u32,
        variant: u64,
        gen: u64,
    ) -> hl_gpu::Result<(u32, u32, bool)> {
        let key = (prog, variant);
        if let Some(&(vs, fs, g)) = self.prog_shader_cache.get(&key) {
            if g == gen {
                hl_log::hl_count!(hl_log::tag::GL, "prog_shader_hit");
                return Ok((vs, fs, false));
            }
        }
        hl_log::hl_count!(hl_log::tag::GL, "prog_shader_compile");
        let vs = self.alloc_shader_ir()?;
        let fs = self.alloc_shader_ir()?;
        self.prog_shader_cache.insert(key, (vs, fs, gen));
        Ok((vs, fs, true))
    }

    /// The stable IR render-pipeline id for a program (`prog`) drawn with pipeline-state signature
    /// `state_key`, at link generation `gen`. Returns `(pipeline_ir, needs_create)`: created ONCE per
    /// `(program, state, link_gen)` and re-referenced by id thereafter — so a program re-drawn with the same
    /// fixed-function + vertex-layout state emits no new `CreateRenderPipeline`. Mirrors
    /// [`Self::program_shader_ir`].
    pub fn program_pipeline_ir(
        &mut self,
        prog: u32,
        state_key: u64,
        gen: u64,
    ) -> hl_gpu::Result<(u32, bool)> {
        if let Some(&(ir, g)) = self.prog_pipeline_cache.get(&(prog, state_key)) {
            if g == gen {
                hl_log::hl_count!(hl_log::tag::GL, "prog_pipeline_hit");
                return Ok((ir, false));
            }
        }
        hl_log::hl_count!(hl_log::tag::GL, "prog_pipeline_create");
        let ir = self.alloc_pipeline_ir()?;
        self.prog_pipeline_cache
            .insert((prog, state_key), (ir, gen));
        Ok((ir, true))
    }

    /// The stable IR texture id a sampled GL texture (`gl_name`) at content generation `gen` lowers to.
    /// Returns `(texture_ir, needs_upload)`: `needs_upload` is true on the first sight of this texture and
    /// whenever its content generation changed since the last upload — the frame builder emits the
    /// `CreateTexture` + staging `WriteBuffer` + `CopyBufferToTexture` exactly then, and reuses the resident
    /// id (uploading nothing) on every later reference in this and subsequent frames.
    pub fn sampled_texture_ir(
        &mut self,
        gl_name: u32,
        generation: (u64, u64),
    ) -> hl_gpu::Result<(u32, bool)> {
        if let Some(&(ir, up_gen)) = self.tex_ir_cache.get(&gl_name) {
            if up_gen == generation {
                hl_log::hl_count!(hl_log::tag::GL, "tex_cache_hit");
                return Ok((ir, false));
            }
            // Content changed: a fresh id carries the new upload; the old resident id is RETIRED for destroy
            // (queued into the next frame's tail). Safe now that a NACKed frame rolls back atomically (#232):
            // a retained-across-NACK draw is gone, and every live reference to this GL name re-resolves to the
            // fresh id below — so nothing still points at the old id when its `DestroyTexture` runs. Without
            // this, a Chrome texture re-uploaded each frame leaks its prior generation's residency forever.
            hl_log::hl_count!(hl_log::tag::GL, "tex_upload");
            self.pending_destroys.push(Cmd::DestroyTexture(ir));
            let ir = self.alloc_texture_ir()?;
            self.tex_ir_cache.insert(gl_name, (ir, generation));
            return Ok((ir, true));
        }
        hl_log::hl_count!(hl_log::tag::GL, "tex_upload");
        let ir = self.alloc_texture_ir()?;
        self.tex_ir_cache.insert(gl_name, (ir, generation));
        Ok((ir, true))
    }

    pub(crate) fn shared_texture_ir(
        &mut self,
        key: (u64, u64, u32, u32, u32),
        residency: std::sync::Weak<crate::model::texture::SharedPixels>,
    ) -> hl_gpu::Result<(u32, bool)> {
        if let Some(target) = self.shared_target_cache.get(&key.0) {
            if target.revision == key.1
                && target.width == key.2
                && target.height == key.3
                && target.storage.upgrade().is_some()
            {
                hl_log::hl_count!(hl_log::tag::GL, "shared_target_cache_hit");
                return Ok((target.texture, false));
            }
        }
        if let Some(target) = self.shared_target_cache.remove(&key.0) {
            if target.owned {
                self.pending_destroys
                    .push(Cmd::DestroyTexture(target.texture));
            }
        }
        if let Some(residency) = self.shared_tex_ir_cache.get(&key) {
            hl_log::hl_count!(hl_log::tag::GL, "shared_tex_cache_hit");
            return Ok((residency.texture, false));
        }
        self.invalidate_shared_texture(key.0);
        let texture = self.alloc_texture_ir()?;
        self.shared_tex_ir_cache.insert(
            key,
            SharedTextureResidency {
                texture,
                storage: residency,
            },
        );
        hl_log::hl_count!(hl_log::tag::GL, "shared_tex_upload");
        Ok((texture, true))
    }

    pub(crate) fn promote_shared_texture(
        &mut self,
        storage: u64,
        revision: u64,
        width: u32,
        height: u32,
        texture: u32,
        residency: std::sync::Weak<crate::model::texture::SharedPixels>,
    ) {
        let retired = self
            .shared_tex_ir_cache
            .extract_if(|(candidate, ..), _| *candidate == storage)
            .map(|(_, resident)| resident.texture)
            .collect::<Vec<_>>();
        self.pending_destroys
            .extend(retired.into_iter().map(Cmd::DestroyTexture));
        if let Some(previous) = self.shared_target_cache.insert(
            storage,
            SharedTargetResidency {
                texture,
                revision,
                width,
                height,
                storage: residency,
                owned: false,
            },
        ) {
            if previous.owned && previous.texture != texture {
                self.pending_destroys
                    .push(Cmd::DestroyTexture(previous.texture));
            }
        }
    }

    /// Transfer retained render targets from the removed GL/FBO owner to shared-storage residency.
    pub fn own_shared_targets(&mut self, textures: &[u32]) {
        for residency in self.shared_target_cache.values_mut() {
            if textures.contains(&residency.texture) {
                residency.owned = true;
            }
        }
    }

    /// Release imported-storage residency whose last storage owner disappeared.
    ///
    /// Call only after an accepted batch's deferred snapshots have been released. Destroy commands remain
    /// pending until a sink accepts them, so a failed cleanup submission can be retried transactionally.
    pub fn prune_shared_textures(&mut self) {
        let retired = self
            .shared_tex_ir_cache
            .extract_if(|_, residency| residency.storage.upgrade().is_none())
            .map(|(_, residency)| residency.texture)
            .collect::<Vec<_>>();
        self.pending_destroys
            .extend(retired.into_iter().map(Cmd::DestroyTexture));
        let retired = self
            .shared_target_cache
            .extract_if(|_, residency| residency.storage.upgrade().is_none())
            .filter_map(|(_, residency)| residency.owned.then_some(residency.texture))
            .collect::<Vec<_>>();
        self.pending_destroys
            .extend(retired.into_iter().map(Cmd::DestroyTexture));
    }

    pub(crate) fn invalidate_shared_texture(&mut self, storage: u64) {
        let retired = self
            .shared_tex_ir_cache
            .extract_if(|(candidate, ..), _| *candidate == storage)
            .map(|(_, residency)| residency.texture)
            .collect::<Vec<_>>();
        self.pending_destroys
            .extend(retired.into_iter().map(Cmd::DestroyTexture));
        if let Some(residency) = self.shared_target_cache.remove(&storage) {
            if residency.owned {
                self.pending_destroys
                    .push(Cmd::DestroyTexture(residency.texture));
            }
        }
    }

    /// The stable IR buffer id a GL data buffer (`gl_name`) at content generation `gen` lowers to for the
    /// given IR `usage` bits (VERTEX/INDEX). Returns `(buffer_ir, needs_upload)`, mirroring
    /// [`Self::sampled_texture_ir`]: created + `WriteBuffer`d once per content generation, re-bound by id
    /// thereafter.
    pub fn data_buffer_ir(
        &mut self,
        gl_name: u32,
        usage: u32,
        gen: u64,
    ) -> hl_gpu::Result<(u32, bool)> {
        if let Some(&(ir, _)) = self.interop_buf_ir.get(&gl_name) {
            return Ok((ir, false));
        }
        if let Some(&(ir, up_gen)) = self.buf_ir_cache.get(&(gl_name, usage)) {
            if up_gen == gen {
                hl_log::hl_count!(hl_log::tag::GL, "buf_cache_hit");
                return Ok((ir, false));
            }
            // Content changed: retire the prior generation's IR buffer (queued for the next frame's tail) and
            // mint a fresh id for the new bytes — safe for the same reason as the texture path above.
            hl_log::hl_count!(hl_log::tag::GL, "buf_upload");
            self.pending_destroys.push(Cmd::DestroyBuffer(ir));
            let ir = self.alloc_buffer_ir()?;
            self.buf_ir_cache.insert((gl_name, usage), (ir, gen));
            return Ok((ir, true));
        }
        hl_log::hl_count!(hl_log::tag::GL, "buf_upload");
        let ir = self.alloc_buffer_ir()?;
        self.buf_ir_cache.insert((gl_name, usage), (ir, gen));
        Ok((ir, true))
    }

    /// Mint or return the single IR backing used while a GL buffer is registered with CUDA.
    pub fn interop_buffer_ir(&mut self, gl_name: u32) -> hl_gpu::Result<(u32, bool)> {
        let gl_buffer = self.buffers.get(gl_name).ok_or(hl_gpu::GpuError::Invalid("GL buffer"))?;
        if gl_buffer.mapped.is_some() {
            return Err(hl_gpu::GpuError::Invalid("mapped GL buffer"));
        }
        if let Some(&(ir, _)) = self.interop_buf_ir.get(&gl_name) {
            return Ok((ir, false));
        }
        let ir = self.alloc_buffer_ir()?;
        self.interop_buf_ir.insert(gl_name, (ir, gl_buffer.gen));
        Ok((ir, true))
    }

    // ---- IR id minting ---------------------------------------------------------------------------
}

#[cfg(test)]
mod interop_tests {
    use super::*;
    use hl_gpu::protocol::model::enums::buffer_usage;

    #[test]
    fn cuda_export_becomes_the_canonical_backing_for_later_gl_draws() {
        let mut context = GlContext::new();
        context.buffers.ensure(7);
        context.buffers.set_data(7, glconst::GL_ARRAY_BUFFER, &[1, 2, 3, 4], 0);
        let (interop, create) = context.interop_buffer_ir(7).unwrap();
        assert!(create);
        assert_eq!(context.data_buffer_ir(7, buffer_usage::VERTEX, 1).unwrap(), (interop, false));
        assert_eq!(context.data_buffer_ir(7, buffer_usage::INDEX, 1).unwrap(), (interop, false));
    }
}
