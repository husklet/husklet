//! [`GlContext`] — the per-context aggregate the GL driver operates on.
//!
//! It owns the whole GL object model for one context: the window-surface descriptor, the buffer /
//! texture / shader+program tables, the currently-bound GL state (program, array/element buffers,
//! texture units, vertex-attribute pointers, clear/viewport/scissor/blend/depth), the recorded
//! draw-list, and every IR id counter. Ported from `hl-shim-gl/src/state.rs` (`GlState`) — the id
//! minting matches cuda's context so the emitted IR is deterministic.
//!
//! The context builds NO `Cmd`s and submits nothing; it only mints ids and records bookkeeping. The
//! [`crate::service`] layer calls these methods, then (only at swap) submits the lowered commands
//! through a [`hl_gpu::CommandSink`].

use super::buffer::Buffers;
use super::es3::{ProgramPipelines, Queries, Samplers, TransformFeedbacks};
use super::framebuffer::Framebuffers;
use super::glconst;
use super::program::{Attr, DrawCall, Programs, MAX_ATTR};
use super::renderbuffer::Renderbuffers;
use super::texture::Textures;
use std::collections::HashMap;

/// The presented window surface (the default framebuffer). Ported from `hl-shim-gl`'s `Surface`.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct GlSurface {
    /// Whether `eglCreateWindowSurface` has brought a surface up.
    pub have: bool,
    pub width: u32,
    pub height: u32,
}

/// The pixel-store pack/unpack parameters (`glPixelStorei`) an app sets before texture upload / readback.
/// Recorded for a faithful `glGetIntegerv` round-trip; the alignments default to GL's documented `4`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct PixelStore {
    pub unpack_alignment: i32,
    pub pack_alignment: i32,
    pub unpack_row_length: i32,
    pub unpack_skip_rows: i32,
    pub unpack_skip_pixels: i32,
    pub pack_row_length: i32,
    pub pack_skip_rows: i32,
    pub pack_skip_pixels: i32,
}

/// A Vertex Array Object's captured state: the per-location vertex-attribute array plus the
/// element-array-buffer binding. Binding a VAO swaps this state into the live context (`ctx.attr` /
/// `ctx.element_buffer`); a GLES3 app MUST bind a VAO before it can draw. Ported from `hl-shim-gl`'s `Vao`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Vao {
    /// The captured per-location vertex-attribute array (`glVertexAttribPointer` + enable + divisor).
    pub attrs: [Attr; MAX_ATTR],
    /// The captured `GL_ELEMENT_ARRAY_BUFFER` binding (element buffer is VAO state; array buffer is not).
    pub element_buffer: u32,
}

impl Default for Vao {
    fn default() -> Self {
        Self { attrs: [Attr::default(); MAX_ATTR], element_buffer: 0 }
    }
}

/// One indexed-buffer binding point (`glBindBufferBase`/`glBindBufferRange`) for a UBO/SSBO/atomic-counter
/// or transform-feedback target. `size == 0` means "the whole buffer from `offset`" (the `glBindBufferBase`
/// case). These feed a compute dispatch's bind group (`crate::service::compute`).
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct IndexedBinding {
    pub buffer: u32,
    pub offset: isize,
    pub size: isize,
}

/// One named uniform block of a program (`glGetUniformBlockIndex`/`glUniformBlockBinding`). The block's
/// member layout + data size live on the [`super::program::Program`] (the single implicit block this
/// model reflects); this record carries the block's declared name and its app-assigned binding point.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct UniformBlock {
    pub name: String,
    pub binding: u32,
}

impl Default for PixelStore {
    fn default() -> Self {
        Self {
            unpack_alignment: 4,
            pack_alignment: 4,
            unpack_row_length: 0,
            unpack_skip_rows: 0,
            unpack_skip_pixels: 0,
            pack_row_length: 0,
            pack_skip_rows: 0,
            pack_skip_pixels: 0,
        }
    }
}

pub struct GlContext {
    /// The default-framebuffer window surface.
    pub surf: GlSurface,

    /// GL buffer objects (`glGenBuffers`/`glBufferData`).
    pub buffers: Buffers,
    /// GL texture objects (`glGenTextures`/`glTexImage2D`).
    pub textures: Textures,
    /// GL shader + program objects (`glCreateShader`/`glCreateProgram`/`glLinkProgram`).
    pub programs: Programs,
    /// GL framebuffer objects (`glGenFramebuffers`/`glFramebufferTexture2D`) — offscreen render targets.
    pub framebuffers: Framebuffers,
    /// GL renderbuffer objects (`glGenRenderbuffers`/`glRenderbufferStorage`) — texture-backed attachments.
    pub renderbuffers: Renderbuffers,
    /// ES3 sampler objects (`glGenSamplers`/`glSamplerParameter*`/`glBindSampler`). Client-side state.
    pub samplers: Samplers,
    /// ES3 occlusion/transform-feedback query objects (`glGenQueries`/`glBeginQuery`/…). Client-side.
    pub queries: Queries,
    /// ES3 transform-feedback objects + per-program varying capture (`glBindTransformFeedback`/…).
    pub transform_feedbacks: TransformFeedbacks,
    /// Separate-shader program-pipeline objects (`glGenProgramPipelines`/`glUseProgramStages`/…).
    pub program_pipelines: ProgramPipelines,

    // ---- currently-bound GL state ----------------------------------------------------------------
    /// The program bound by `glUseProgram`.
    pub cur_prog: u32,
    /// The buffer bound to `GL_ARRAY_BUFFER`.
    pub array_buffer: u32,
    /// The buffer bound to `GL_ELEMENT_ARRAY_BUFFER`.
    pub element_buffer: u32,
    /// The buffer bound to each non-array/element target (`GL_UNIFORM_BUFFER`, `GL_SHADER_STORAGE_BUFFER`,
    /// `GL_PIXEL_PACK_BUFFER`, `GL_DISPATCH_INDIRECT_BUFFER`, …) by `glBindBuffer`. Used to resolve the
    /// target of `glMapBufferRange`/`glDispatchComputeIndirect` for the long tail of ES3 buffer targets.
    pub general_buffers: HashMap<u32, u32>,
    /// The active texture unit index (`glActiveTexture` - `GL_TEXTURE0`).
    pub active_texture: usize,
    /// The GL texture bound to each texture unit (`glBindTexture`).
    pub tex_unit: [u32; 8],
    /// Per-location vertex-attribute pointer state.
    pub attr: [Attr; MAX_ATTR],
    pub clear_color: [f32; 4],
    /// The depth-buffer clear value (`glClearDepthf`). Recorded for completeness; the default framebuffer
    /// models no depth attachment, so it is not lowered to a pass clear (honest no-op — see `service::frame`).
    pub clear_depth: f32,
    pub viewport: [i32; 4],
    pub scissor_enabled: bool,
    pub scissor: [i32; 4],
    /// `GL_BLEND` enabled + its factors/equations (`glBlendFunc`/`glBlendFuncSeparate`/`glBlendEquation`).
    pub blend: bool,
    pub blend_src_rgb: u32,
    pub blend_dst_rgb: u32,
    pub blend_src_alpha: u32,
    pub blend_dst_alpha: u32,
    pub blend_eq_rgb: u32,
    pub blend_eq_alpha: u32,
    /// `GL_DEPTH_TEST` enabled + its compare func (`glDepthFunc`) and write mask (`glDepthMask`).
    pub depth: bool,
    pub depth_func: u32,
    pub depth_write: bool,
    /// `GL_CULL_FACE` enabled + the culled face (`glCullFace`) and front-face winding (`glFrontFace`).
    pub cull_enabled: bool,
    pub cull_face: u32,
    pub front_face: u32,
    /// The draw framebuffer bound by `glBindFramebuffer` (`GL_FRAMEBUFFER`/`GL_DRAW_FRAMEBUFFER`; `0` =
    /// the default window framebuffer). A recorded draw's render target follows this binding.
    pub bound_fbo: u32,
    /// The read framebuffer bound by `glBindFramebuffer(GL_READ_FRAMEBUFFER, …)` (`GL_FRAMEBUFFER` binds
    /// both). The `glReadPixels`/`glBlitFramebuffer` source; `0` = the default window framebuffer.
    pub read_fbo: u32,
    /// The renderbuffer bound by `glBindRenderbuffer` (`GL_RENDERBUFFER`; `0` = none). Names the target of
    /// the next `glRenderbufferStorage`.
    pub bound_rbo: u32,

    /// The Vertex Array Object currently bound by `glBindVertexArray` (`0` = the default VAO).
    pub cur_vao: u32,
    /// The per-name captured VAO state (attrib array + element buffer). The live `attr`/`element_buffer`
    /// fields hold `cur_vao`'s working copy; a bind snapshots the live copy here and loads the target's.
    vaos: HashMap<u32, Vao>,
    /// Monotonic VAO name counter (name `0` is the reserved default VAO, never minted).
    next_vao: u32,

    /// The pack/unpack pixel-store parameters (`glPixelStorei`).
    pub pixel_store: PixelStore,

    /// Indexed-buffer bindings (`glBindBufferBase`/`glBindBufferRange`), keyed by `(target, index)`.
    /// The UBO/SSBO bindings feed a `glDispatchCompute`'s bind group (`crate::service::compute`).
    pub indexed_buffers: HashMap<(u32, u32), IndexedBinding>,

    /// Per-program named uniform blocks (`glGetUniformBlockIndex` assigns a stable index by name;
    /// `glUniformBlockBinding` sets the binding point). Keyed by program GL name. Populated lazily on the
    /// first `glGetUniformBlockIndex`/reflection query — the same lazy scheme the reference shim uses.
    pub uniform_blocks: HashMap<u32, Vec<UniformBlock>>,

    /// The MRT draw-buffer list (`glDrawBuffers`) + the read-buffer source (`glReadBuffer`). This model
    /// renders a single color target, so the list is recorded for a faithful round-trip but only the
    /// first attachment is materialized — an honest partial.
    pub draw_buffers: Vec<u32>,
    pub read_buffer_src: u32,

    // ---- sync objects (glFenceSync / glClientWaitSync / …) over the IR fence timeline ----------------
    /// The IR fence id backing every sync object (`0` = not yet created; minted + `CreateFence`d on the
    /// first `glFenceSync`). One monotonic fence timeline carries every fence sync this context creates.
    pub fence_ir: u32,
    /// The next timeline value a `glFenceSync` signals the fence to (monotonic, starts at 1).
    pub fence_next_value: u64,
    /// The highest fence timeline value a `glClientWaitSync`/`glWaitSync` has observed as reached. A sync
    /// whose value is `<=` this reads back already-signaled without re-waiting.
    pub fence_signaled_through: u64,
    /// Live sync objects: opaque sync token → the fence timeline value it was inserted at.
    pub syncs: HashMap<usize, u64>,
    /// The opaque sync-token allocator (non-zero, so a `GLsync` is never null).
    next_sync_token: usize,

    /// The last GL error (`glGetError` reads + clears it). GL keeps the FIRST error raised until read,
    /// so [`Self::set_gl_error`] is first-error-wins.
    pub gl_error: u32,

    /// The recorded draw-list, replayed into IR at `eglSwapBuffers`.
    pub draws: Vec<DrawCall>,

    // ---- IR id counters (mint monotonic ids for the emitted commands; mirrors cuda's context) -----
    next_buffer: u32,
    next_texture: u32,
    next_sampler: u32,
    next_shader: u32,
    next_pipeline: u32,
    next_bind_group: u32,
    next_surface: u32,
    next_fence: u32,

    /// The default render-target texture + presentable surface IR ids, minted once and cached (0 =
    /// not yet created). The frame builder emits their `CreateTexture`/`CreateSurface` on first use.
    default_tex_ir: u32,
    default_surface_ir: u32,

    /// The shared 1x1 placeholder sampled-texture + default-sampler IR ids (0 = not yet created). A
    /// GskGpu fragment program DECLARES + samples every one of its texture slots, so the executor's auto
    /// bind-group layout carries an entry for each; but for a given draw only some of those samplers have a
    /// real GL texture with uploaded pixels bound. The frame builder binds THIS transparent-black 1x1
    /// placeholder (+ this default sampler) at every declared-but-unbound sampler so the bind group covers
    /// every used binding of the layout (the executor's used-binding filter then trims to the sampled set).
    /// Created ONCE (its `CreateTexture` + staging upload + `CreateSampler`) and reused across every draw
    /// and frame — one placeholder texture + one sampler serve every empty sampler slot everywhere.
    default_placeholder_tex: u32,
    default_placeholder_samp: u32,

    /// Per-FBO offscreen render-target IR ids, keyed by the FBO's color-attachment GL texture name →
    /// `(surface_ir, texture_ir)`. Minted + `CreateTexture`/`CreateSurface`d on first use and reused on
    /// later frames (so re-rendering the same FBO does not re-create the target).
    fbo_targets: HashMap<u32, (u32, u32)>,

    /// Per-render-target depth-buffer IR ids, keyed by the pass's COLOR-target texture IR → the
    /// `Depth32Float` depth texture IR. A depth-tested draw (`glEnable(GL_DEPTH_TEST)`) builds a pipeline
    /// with a depth-stencil state, and wgpu REQUIRES the render pass to carry a matching depth attachment;
    /// the frame builder mints one depth texture per color target here, `CreateTexture`s it once, and
    /// reuses its id on later frames (so re-rendering the same target does not re-create the depth buffer).
    depth_targets: HashMap<u32, u32>,

    /// Residency cache for sampled GL textures uploaded from CPU pixels: GL texture name → `(texture_ir,
    /// uploaded_gen)`. A texture is `CreateTexture`d + staged + `CopyBufferToTexture`d ONCE (per content
    /// generation) and re-referenced by its stable IR id on every later draw and frame. Without this a
    /// real toolkit frame (GskGL binds one glyph/mask atlas across hundreds of draws) re-uploads the whole
    /// atlas plane per draw — gigabytes of redundant `WriteBuffer` that blow the negotiated frame cap.
    tex_ir_cache: HashMap<u32, (u32, u64)>,
    /// Residency cache for GL data buffers (vertex/index), keyed by `(GL buffer name, IR usage bits)` →
    /// `(buffer_ir, uploaded_gen)`. Same idea as [`Self::tex_ir_cache`]: a buffer whose bytes did not
    /// change is created + uploaded once and re-bound by id. Keyed on usage too so the rare GL buffer bound
    /// as BOTH a vertex and an index source gets a correctly-typed IR buffer for each role.
    buf_ir_cache: HashMap<(u32, u32), (u32, u64)>,

    /// Residency cache for a linked GL program's two IR shader MODULES, keyed by GL program name →
    /// `(vs_shader_ir, fs_shader_ir, link_gen)`. GskGpu compiles + links each of its programs ONCE and then
    /// reuses it across every draw of every frame; without this cache the frame builder minted fresh IR
    /// shader ids and re-emitted `CreateShader` for the SAME program on every draw, so the host re-ran naga's
    /// glsl→wgsl compile hundreds of times per frame (a real GTK frame reuses ~11 programs across ~260 draws
    /// → ~520 redundant compiles). A program's shader modules are `CreateShader`d ONCE per link generation
    /// and re-referenced by their stable IR ids on every later draw+frame. Keyed on `link_gen` (bumped by
    /// `glLinkProgram`) so a re-linked program gets fresh modules. Mirrors [`Self::tex_ir_cache`].
    prog_shader_cache: HashMap<u32, (u32, u32, u64)>,
    /// Residency cache for a program's render PIPELINE, keyed by `(GL program name, pipeline-state signature)`
    /// → `(pipeline_ir, link_gen)`. The pipeline depends on the program's shaders PLUS the draw's
    /// fixed-function + vertex-layout state (target format, blend, depth, topology, cull/front-face, vertex
    /// buffers), so the key folds a hash of that state in: a program re-drawn with the SAME state reuses its
    /// resident pipeline (no `CreateRenderPipeline`), while a genuinely new state variant creates one more.
    /// Invalidated on relink via `link_gen`. Mirrors [`Self::prog_shader_cache`].
    prog_pipeline_cache: HashMap<(u32, u64), (u32, u64)>,
}

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
            depth: false,
            depth_func: glconst::GL_LESS,
            depth_write: true,
            cull_enabled: false,
            cull_face: glconst::GL_BACK,
            front_face: glconst::GL_CCW,
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
            default_placeholder_tex: 0,
            default_placeholder_samp: 0,
            fbo_targets: HashMap::new(),
            depth_targets: HashMap::new(),
            tex_ir_cache: HashMap::new(),
            buf_ir_cache: HashMap::new(),
            prog_shader_cache: HashMap::new(),
            prog_pipeline_cache: HashMap::new(),
        }
    }

    /// The stable IR shader-module ids `(vs_shader_ir, fs_shader_ir)` a linked render program (`prog`) at
    /// link generation `gen` lowers to. Returns `(vs_ir, fs_ir, needs_create)`: `needs_create` is true on the
    /// first sight of this program (or after a relink bumped `gen`), so the frame builder emits the two
    /// `CreateShader`s exactly then and reuses the resident ids — emitting NOTHING and re-compiling NOTHING on
    /// every later draw+frame that reuses the program. Mirrors [`Self::sampled_texture_ir`].
    pub fn program_shader_ir(&mut self, prog: u32, gen: u64) -> (u32, u32, bool) {
        if let Some(&(vs, fs, g)) = self.prog_shader_cache.get(&prog) {
            if g == gen {
                hl_log::hl_count!(hl_log::tag::GL, "prog_shader_hit");
                return (vs, fs, false);
            }
        }
        hl_log::hl_count!(hl_log::tag::GL, "prog_shader_compile");
        let vs = self.alloc_shader_ir();
        let fs = self.alloc_shader_ir();
        self.prog_shader_cache.insert(prog, (vs, fs, gen));
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
        self.prog_pipeline_cache.insert((prog, state_key), (ir, gen));
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
            // Content changed: a fresh id carries the new upload (the old resident id is simply abandoned —
            // content updates to a given texture are rare, so this does not accumulate).
            hl_log::hl_count!(hl_log::tag::GL, "tex_upload");
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
            hl_log::hl_count!(hl_log::tag::GL, "buf_upload");
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
    pub fn default_target(&mut self) -> (u32, u32, bool) {
        if self.default_tex_ir == 0 {
            self.default_tex_ir = self.alloc_texture_ir();
            self.default_surface_ir = self.next_surface;
            self.next_surface += 1;
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
            (self.default_placeholder_tex, self.default_placeholder_samp, true)
        } else {
            (self.default_placeholder_tex, self.default_placeholder_samp, false)
        }
    }

    /// The offscreen render-target texture + presentable surface IR ids for the FBO whose color
    /// attachment is GL texture `gl_tex`. Returns `(surface, texture, needs_create)`: `needs_create` is
    /// true exactly on the first request for this attachment, so the frame builder emits the
    /// `CreateTexture`/`CreateSurface` once and reuses the ids on later frames.
    pub fn fbo_target(&mut self, gl_tex: u32) -> (u32, u32, bool) {
        if let Some(&(surface, texture)) = self.fbo_targets.get(&gl_tex) {
            (surface, texture, false)
        } else {
            let texture = self.alloc_texture_ir();
            let surface = self.next_surface;
            self.next_surface += 1;
            self.fbo_targets.insert(gl_tex, (surface, texture));
            (surface, texture, true)
        }
    }

    /// The `Depth32Float` depth-buffer texture IR for the render pass whose COLOR target is texture IR
    /// `color_tex`. Returns `(depth_texture, needs_create)`: `needs_create` is true exactly on the first
    /// request for this color target, so the frame builder emits the depth `CreateTexture` once and reuses
    /// the id on later frames. Only allocated when a depth-tested draw actually needs an attachment.
    pub fn depth_target(&mut self, color_tex: u32) -> (u32, bool) {
        if let Some(&depth) = self.depth_targets.get(&color_tex) {
            (depth, false)
        } else {
            let depth = self.alloc_texture_ir();
            self.depth_targets.insert(color_tex, depth);
            (depth, true)
        }
    }

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
        self.vaos.insert(self.cur_vao, Vao { attrs: self.attr, element_buffer: self.element_buffer });
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
