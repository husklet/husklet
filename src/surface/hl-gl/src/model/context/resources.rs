use super::*;

impl Default for GlContext {
    fn default() -> Self {
        Self::new()
    }
}

impl GlContext {
    pub fn new() -> Self {
        Self {
            surf: GlSurface::default(),
            buffers: Buffers::new(),
            textures: Textures::new(),
            programs: Programs::new(),
            framebuffers: Framebuffers::new(),
            renderbuffers: Renderbuffers::new(),
            samplers: Samplers::new(),
            queries: Queries::new(),
            transform_feedbacks: TransformFeedbacks::new(),
            program_pipelines: ProgramPipelines::new(),
            cur_prog: 0,
            array_buffer: 0,
            element_buffer: 0,
            general_buffers: HashMap::new(),
            active_texture: 0,
            tex_unit: [0; 8],
            attr: [Attr::default(); MAX_ATTR],
            clear_color: [0.0; 4],
            clear_depth: 1.0,
            viewport: [0; 4],
            scissor_enabled: false,
            scissor: [0; 4],
            blend: false,
            blend_src_rgb: glconst::GL_ONE,
            blend_dst_rgb: glconst::GL_ZERO,
            blend_src_alpha: glconst::GL_ONE,
            blend_dst_alpha: glconst::GL_ZERO,
            blend_eq_rgb: glconst::GL_FUNC_ADD,
            blend_eq_alpha: glconst::GL_FUNC_ADD,
            blend_color: [0.0; 4],
            depth: false,
            depth_func: glconst::GL_LESS,
            depth_write: true,
            stencil: false,
            stencil_func_front: glconst::GL_ALWAYS,
            stencil_func_back: glconst::GL_ALWAYS,
            stencil_fail_front: glconst::GL_KEEP,
            stencil_zfail_front: glconst::GL_KEEP,
            stencil_zpass_front: glconst::GL_KEEP,
            stencil_fail_back: glconst::GL_KEEP,
            stencil_zfail_back: glconst::GL_KEEP,
            stencil_zpass_back: glconst::GL_KEEP,
            stencil_ref: 0,
            stencil_read_mask: 0xffff_ffff,
            stencil_write_mask: 0xffff_ffff,
            clear_stencil: 0,
            cull_enabled: false,
            cull_face: glconst::GL_BACK,
            front_face: glconst::GL_CCW,
            color_mask: 0xf,
            bound_fbo: 0,
            read_fbo: 0,
            bound_rbo: 0,
            cur_vao: 0,
            vaos: HashMap::new(),
            next_vao: 1,
            pixel_store: PixelStore::default(),
            indexed_buffers: HashMap::new(),
            uniform_blocks: HashMap::new(),
            draw_buffers: vec![glconst::GL_BACK],
            read_buffer_src: glconst::GL_BACK,
            fence_ir: 0,
            fence_next_value: 1,
            fence_signaled_through: 0,
            syncs: HashMap::new(),
            next_sync_token: 1,
            gl_error: glconst::GL_NO_ERROR,
            draws: Vec::new(),
            blits: Vec::new(),
            next_buffer: 1,
            next_texture: 1,
            next_sampler: 1,
            next_shader: 1,
            next_pipeline: 1,
            next_bind_group: 1,
            next_surface: 1,
            next_fence: 1,
            default_tex_ir: 0,
            default_surface_ir: 0,
            default_target_wh: (0, 0),
            default_placeholder_tex: 0,
            default_placeholder_samp: 0,
            fbo_targets: HashMap::new(),
            depth_targets: HashMap::new(),
            tex_ir_cache: HashMap::new(),
            buf_ir_cache: HashMap::new(),
            prog_shader_cache: HashMap::new(),
            prog_pipeline_cache: HashMap::new(),
            pending_destroys: Vec::new(),
        }
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
        let targets: Vec<_> = self
            .fbo_targets
            .keys()
            .filter(|(name, _)| *name == gl_name)
            .copied()
            .collect();
        for key in targets {
            let (surface, texture) = self.fbo_targets.remove(&key).unwrap();
            // Any depth/stencil buffers minted for this color target die with it.
            let mut dead_depth: Vec<u32> = Vec::new();
            self.depth_targets.retain(|&(color, _), &mut depth| {
                if color == texture {
                    dead_depth.push(depth);
                    false
                } else {
                    true
                }
            });
            for depth in dead_depth {
                self.pending_destroys.push(Cmd::DestroyTexture(depth));
            }
            self.pending_destroys.push(Cmd::DestroyTexture(texture));
            self.pending_destroys.push(Cmd::DestroySurface(surface));
        }
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
        for (_gl_tex, (surface, texture)) in self.fbo_targets.drain() {
            self.pending_destroys.push(Cmd::DestroyTexture(texture));
            self.pending_destroys.push(Cmd::DestroySurface(surface));
        }
        for (_key, depth) in self.depth_targets.drain() {
            self.pending_destroys.push(Cmd::DestroyTexture(depth));
        }
        if self.default_tex_ir != 0 {
            self.pending_destroys
                .push(Cmd::DestroyTexture(self.default_tex_ir));
            self.pending_destroys
                .push(Cmd::DestroySurface(self.default_surface_ir));
            self.default_tex_ir = 0;
            self.default_surface_ir = 0;
            self.default_target_wh = (0, 0);
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

    /// The shared placeholder sampler's IR id (`0` = not yet created). The frame builder must NOT free this
    /// among a frame's per-draw ephemeral samplers — it is created once and reused across every frame (see
    /// [`Self::default_placeholder`]).
    pub fn placeholder_sampler_ir(&self) -> u32 {
        self.default_placeholder_samp
    }

    /// The stable IR shader-module ids `(vs_shader_ir, fs_shader_ir)` a linked render program (`prog`) at
    /// link generation `gen` lowers to. Returns `(vs_ir, fs_ir, needs_create)`: `needs_create` is true on the
    /// first sight of this program (or after a relink bumped `gen`), so the frame builder emits the two
    /// `CreateShader`s exactly then and reuses the resident ids — emitting NOTHING and re-compiling NOTHING on
    /// every later draw+frame that reuses the program. Mirrors [`Self::sampled_texture_ir`].
    pub fn program_shader_ir(&mut self, prog: u32, variant: u64, gen: u64) -> (u32, u32, bool) {
        let key = (prog, variant);
        if let Some(&(vs, fs, g)) = self.prog_shader_cache.get(&key) {
            if g == gen {
                hl_log::hl_count!(hl_log::tag::GL, "prog_shader_hit");
                return (vs, fs, false);
            }
        }
        hl_log::hl_count!(hl_log::tag::GL, "prog_shader_compile");
        let vs = self.alloc_shader_ir();
        let fs = self.alloc_shader_ir();
        self.prog_shader_cache.insert(key, (vs, fs, gen));
        (vs, fs, true)
    }

    /// The stable IR render-pipeline id for a program (`prog`) drawn with pipeline-state signature
    /// `state_key`, at link generation `gen`. Returns `(pipeline_ir, needs_create)`: created ONCE per
    /// `(program, state, link_gen)` and re-referenced by id thereafter — so a program re-drawn with the same
    /// fixed-function + vertex-layout state emits no new `CreateRenderPipeline`. Mirrors
    /// [`Self::program_shader_ir`].
    pub fn program_pipeline_ir(&mut self, prog: u32, state_key: u64, gen: u64) -> (u32, bool) {
        if let Some(&(ir, g)) = self.prog_pipeline_cache.get(&(prog, state_key)) {
            if g == gen {
                hl_log::hl_count!(hl_log::tag::GL, "prog_pipeline_hit");
                return (ir, false);
            }
        }
        hl_log::hl_count!(hl_log::tag::GL, "prog_pipeline_create");
        let ir = self.alloc_pipeline_ir();
        self.prog_pipeline_cache
            .insert((prog, state_key), (ir, gen));
        (ir, true)
    }

    /// The stable IR texture id a sampled GL texture (`gl_name`) at content generation `gen` lowers to.
    /// Returns `(texture_ir, needs_upload)`: `needs_upload` is true on the first sight of this texture and
    /// whenever its content generation changed since the last upload — the frame builder emits the
    /// `CreateTexture` + staging `WriteBuffer` + `CopyBufferToTexture` exactly then, and reuses the resident
    /// id (uploading nothing) on every later reference in this and subsequent frames.
    pub fn sampled_texture_ir(&mut self, gl_name: u32, gen: u64) -> (u32, bool) {
        if let Some(&(ir, up_gen)) = self.tex_ir_cache.get(&gl_name) {
            if up_gen == gen {
                hl_log::hl_count!(hl_log::tag::GL, "tex_cache_hit");
                return (ir, false);
            }
            // Content changed: a fresh id carries the new upload; the old resident id is RETIRED for destroy
            // (queued into the next frame's tail). Safe now that a NACKed frame rolls back atomically (#232):
            // a retained-across-NACK draw is gone, and every live reference to this GL name re-resolves to the
            // fresh id below — so nothing still points at the old id when its `DestroyTexture` runs. Without
            // this, a Chrome texture re-uploaded each frame leaks its prior generation's residency forever.
            hl_log::hl_count!(hl_log::tag::GL, "tex_upload");
            self.pending_destroys.push(Cmd::DestroyTexture(ir));
            let ir = self.alloc_texture_ir();
            self.tex_ir_cache.insert(gl_name, (ir, gen));
            return (ir, true);
        }
        hl_log::hl_count!(hl_log::tag::GL, "tex_upload");
        let ir = self.alloc_texture_ir();
        self.tex_ir_cache.insert(gl_name, (ir, gen));
        (ir, true)
    }

    /// The stable IR buffer id a GL data buffer (`gl_name`) at content generation `gen` lowers to for the
    /// given IR `usage` bits (VERTEX/INDEX). Returns `(buffer_ir, needs_upload)`, mirroring
    /// [`Self::sampled_texture_ir`]: created + `WriteBuffer`d once per content generation, re-bound by id
    /// thereafter.
    pub fn data_buffer_ir(&mut self, gl_name: u32, usage: u32, gen: u64) -> (u32, bool) {
        if let Some(&(ir, up_gen)) = self.buf_ir_cache.get(&(gl_name, usage)) {
            if up_gen == gen {
                hl_log::hl_count!(hl_log::tag::GL, "buf_cache_hit");
                return (ir, false);
            }
            // Content changed: retire the prior generation's IR buffer (queued for the next frame's tail) and
            // mint a fresh id for the new bytes — safe for the same reason as the texture path above.
            hl_log::hl_count!(hl_log::tag::GL, "buf_upload");
            self.pending_destroys.push(Cmd::DestroyBuffer(ir));
            let ir = self.alloc_buffer_ir();
            self.buf_ir_cache.insert((gl_name, usage), (ir, gen));
            return (ir, true);
        }
        hl_log::hl_count!(hl_log::tag::GL, "buf_upload");
        let ir = self.alloc_buffer_ir();
        self.buf_ir_cache.insert((gl_name, usage), (ir, gen));
        (ir, true)
    }

    // ---- IR id minting ---------------------------------------------------------------------------
}
