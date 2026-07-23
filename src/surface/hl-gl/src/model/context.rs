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
use hl_gpu::Cmd;
use std::collections::HashMap;

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
    pub blend_color: [f32; 4],
    /// `GL_DEPTH_TEST` enabled + its compare func (`glDepthFunc`) and write mask (`glDepthMask`).
    pub depth: bool,
    pub depth_func: u32,
    pub depth_write: bool,
    /// `GL_STENCIL_TEST` enabled + the front/back stencil test state. Set by
    /// `glStencilFunc`/`glStencilFuncSeparate` (compare func + reference + value read mask),
    /// `glStencilOp`/`glStencilOpSeparate` (stencil-fail / depth-fail / depth-pass ops), and
    /// `glStencilMask`/`glStencilMaskSeparate` (write mask). WebGPU carries a SINGLE reference value + a
    /// single read/write mask for both faces (only the per-face compare + ops differ), so the front-face
    /// reference/masks are the ones lowered — an honest partial for the rare per-face-mask app.
    pub stencil: bool,
    /// Per-face stencil compare function (`glStencilFunc*`), GL enum (`GL_ALWAYS`/`GL_EQUAL`/…).
    pub stencil_func_front: u32,
    pub stencil_func_back: u32,
    /// Per-face stencil ops (`glStencilOp*`): stencil-fail / depth-fail / depth-pass, GL enums.
    pub stencil_fail_front: u32,
    pub stencil_zfail_front: u32,
    pub stencil_zpass_front: u32,
    pub stencil_fail_back: u32,
    pub stencil_zfail_back: u32,
    pub stencil_zpass_back: u32,
    /// Front-face reference value (`glStencilFunc*`), the dynamic value the compare tests against and a
    /// `GL_REPLACE` op writes — lowered to `Enc::SetStencilReference`.
    pub stencil_ref: i32,
    /// Front-face value read mask (`glStencilFunc*`) — WebGPU `stencilReadMask`.
    pub stencil_read_mask: u32,
    /// Front-face write mask (`glStencilMask*`) — WebGPU `stencilWriteMask`.
    pub stencil_write_mask: u32,
    /// The stencil-buffer clear value (`glClearStencil`), lowered to `DepthAttachment.clear_stencil`.
    pub clear_stencil: i32,
    /// `GL_CULL_FACE` enabled + the culled face (`glCullFace`) and front-face winding (`glFrontFace`).
    pub cull_enabled: bool,
    pub cull_face: u32,
    pub front_face: u32,
    /// `glColorMask` per-channel write enable, packed into the low 4 bits as `R<<0 | G<<1 | B<<2 | A<<3`
    /// (the exact `ColorTargetState::write_mask` encoding). Default `0xf` (all channels written). A guest
    /// that masks a channel — e.g. `glColorMask(1,1,1,0)` to leave the framebuffer alpha untouched, or an
    /// all-false mask for a depth/stencil-only pass — lowers this into the pipeline's color-target write
    /// mask instead of being silently dropped.
    pub color_mask: u32,
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

    /// The recorded `glBlitFramebuffer` operations, in record order, applied AFTER the frame's render
    /// passes (see [`crate::service::frame`]). Each copies a sub-rect from a read FBO's color attachment to
    /// a draw FBO's — lowered to `Enc::CopyTextureToTexture` for the equal-size (non-scaling) case.
    pub blits: Vec<BlitOp>,

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
    /// The `(width, height)` the cached default target was CREATED at. When the window surface later
    /// resizes (Chrome negotiates its real window size a few frames in, after an initial tile-sized
    /// surface), the cached texture is the WRONG size: draws/read-back use the new size while the texture
    /// stays the old one, so the composited window is read back at a mismatched stride and SHEARS. On a
    /// mismatch [`Self::default_target`] retires the stale target and mints a fresh one at the new size.
    default_target_wh: (i32, i32),

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

    /// Per-FBO offscreen render-target IR ids, keyed by color-attachment `(GL name, generation)` →
    /// `(surface_ir, texture_ir)`. Minted + `CreateTexture`/`CreateSurface`d on first use and reused on
    /// later frames (so re-rendering the same FBO does not re-create the target).
    fbo_targets: HashMap<(u32, u64), (u32, u32)>,

    /// Per-render-target depth-buffer IR ids, keyed by `(color-target texture IR, with_stencil)` → the
    /// depth texture IR. A depth-tested draw (`glEnable(GL_DEPTH_TEST)`) builds a pipeline with a
    /// depth-stencil state, and wgpu REQUIRES the render pass to carry a matching depth attachment; the
    /// frame builder mints one depth texture per color target here, `CreateTexture`s it once, and reuses its
    /// id on later frames (so re-rendering the same target does not re-create the depth buffer). The
    /// `with_stencil` half of the key separates a `Depth32Float` depth-only buffer from a
    /// `Depth24PlusStencil8` depth+stencil buffer, so a stencil-testing pass gets a stencil-aspect
    /// attachment while a plain depth pass keeps its depth-only one (the two carry different formats).
    depth_targets: HashMap<(u32, bool), u32>,

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
    prog_shader_cache: HashMap<(u32, u64), (u32, u32, u64)>,
    /// Residency cache for a program's render PIPELINE, keyed by `(GL program name, pipeline-state signature)`
    /// → `(pipeline_ir, link_gen)`. The pipeline depends on the program's shaders PLUS the draw's
    /// fixed-function + vertex-layout state (target format, blend, depth, topology, cull/front-face, vertex
    /// buffers), so the key folds a hash of that state in: a program re-drawn with the SAME state reuses its
    /// resident pipeline (no `CreateRenderPipeline`), while a genuinely new state variant creates one more.
    /// Invalidated on relink via `link_gen`. Mirrors [`Self::prog_shader_cache`].
    prog_pipeline_cache: HashMap<(u32, u64), (u32, u64)>,

    /// Queued `Destroy*` IR for PERSISTENT resources the app has released — `glDeleteTextures`/`Buffers`/
    /// `Renderbuffers` retire the resident IR ids the deleted GL object owned (its cached sampled-texture /
    /// data-buffer / FBO render-target / depth-target ids), and a texture/buffer content change abandons its
    /// prior generation's id. These accumulate here and are flushed into the NEXT submitted frame (the
    /// offscreen `glFlush` flush or the `eglSwapBuffers` swap) AFTER that frame's `Submit`s and BEFORE its
    /// `Present`, so the host frees the residency the moment the GPU work referencing them has run. Without
    /// this the host's per-connection residency ledger climbs on every fresh Chrome tile/atlas texture until
    /// it hits the 512 MiB / 65 536-object cap and every swap NACKs `ResourceLimit("connection residency")`.
    ///
    /// SAFE now that a NACKed frame rolls back atomically (executor #232): a retained-across-NACK draw is
    /// gone, so it cannot reference a destroyed id; and a deleted GL name is removed from the residency caches
    /// below, so any later reference re-resolves to a FRESH id — never the destroyed one. The #226 agent had
    /// to leave this un-retired only because NACK-retain corrupted ids; that is fixed.
    pending_destroys: Vec<Cmd>,
}

mod resources;
mod state;
mod targets;
mod types;

pub use types::*;
