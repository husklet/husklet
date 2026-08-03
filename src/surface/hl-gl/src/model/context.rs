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
use super::es3::Samplers;
use super::glconst;
use super::program::{Attr, DrawCall, Programs, MAX_ATTR};
use super::renderbuffer::Renderbuffers;
use super::texture::{GlTexture, Textures};
use hl_gpu::protocol::model::descriptor::SamplerDesc;
use hl_gpu::Cmd;
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

#[derive(Clone)]
pub(super) struct SharedTextureResidency {
    texture: u32,
    storage: std::sync::Weak<crate::model::texture::SharedPixels>,
}

#[derive(Clone)]
pub(super) struct SharedTargetResidency {
    texture: u32,
    revision: u64,
    width: u32,
    height: u32,
    storage: std::sync::Weak<crate::model::texture::SharedPixels>,
    owned: bool,
}

pub struct GlContext {
    /// State owned by the active EGL context rather than the share group.
    pub(crate) local: LocalState,

    /// GL buffer objects (`glGenBuffers`/`glBufferData`).
    pub buffers: Buffers,
    /// GL texture objects (`glGenTextures`/`glTexImage2D`).
    pub textures: Textures,
    /// GL shader + program objects (`glCreateShader`/`glCreateProgram`/`glLinkProgram`).
    pub programs: Programs,
    /// GL renderbuffer objects (`glGenRenderbuffers`/`glRenderbufferStorage`) — texture-backed attachments.
    pub renderbuffers: Renderbuffers,
    /// ES3 sampler objects (`glGenSamplers`/`glSamplerParameter*`/`glBindSampler`). Client-side state.
    pub samplers: Samplers,

    /// Per-program named uniform blocks (`glGetUniformBlockIndex` assigns a stable index by name;
    /// `glUniformBlockBinding` sets the binding point). Keyed by program GL name. Populated lazily on the
    /// first `glGetUniformBlockIndex`/reflection query — the same lazy scheme the reference shim uses.
    pub uniform_blocks: HashMap<u32, Vec<UniformBlock>>,

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

    /// Display-owned IR names. Every share group connected to one executor uses this allocator, so
    /// independently active contexts cannot publish colliding resource identifiers.
    allocator: Arc<IrAllocator>,

    /// IR names allocated while lowering the current frame. A rejected batch publishes nothing (hl-gpu
    /// rolls its id tables back), so these return to the allocator and the retry reissues them in order.
    frame_ids: std::sync::Mutex<Vec<(allocator::Resource, u32)>>,

    /// The shared 1x1 placeholder sampled-texture IR ids, indexed by D2/D3/Cube, plus one
    /// default-sampler IR id (0 = not yet created). A
    /// GskGpu fragment program DECLARES + samples every one of its texture slots, so the executor's auto
    /// bind-group layout carries an entry for each; but for a given draw only some of those samplers have a
    /// real GL texture with uploaded pixels bound. The frame builder binds THIS transparent-black 1x1
    /// placeholder (+ this default sampler) at every declared-but-unbound sampler so the bind group covers
    /// every used binding of the layout (the executor's used-binding filter then trims to the sampled set).
    /// Each view dimension needs a distinct native texture, while the dimension-independent sampler is
    /// created once and reused across every placeholder, draw, and frame.
    default_placeholder_tex: [u32; 3],
    default_placeholder_samp: u32,

    /// Per-FBO offscreen render-target IR ids, keyed by color-attachment `(GL name, generation)` →
    /// `(surface_ir, texture_ir)`. Minted + `CreateTexture`/`CreateSurface`d on first use and reused on
    /// later frames (so re-rendering the same FBO does not re-create the target).
    fbo_targets: HashMap<(u32, u64), (u32, u32)>,
    /// Host-external identity for imported EGLImage texture generations.
    external_targets: HashMap<(u32, u64), hl_gpu::protocol::model::descriptor::SurfaceToken>,

    /// Persistent depth-buffer IR ids, keyed by the attached depth/stencil storage generations rather
    /// than by the color target. Replacing an FBO's color attachment does not replace its independently
    /// attached depth renderbuffer, so keying this cache by color silently discarded depth contents.
    /// `fallback_color` is non-zero only for the default framebuffer or an attachment-less legacy path.
    /// A depth-tested draw (`glEnable(GL_DEPTH_TEST)`) builds a pipeline with a
    /// depth-stencil state, and wgpu REQUIRES the render pass to carry a matching depth attachment; the
    /// frame builder mints one host depth texture per attached storage generation, `CreateTexture`s it once,
    /// and reuses its id on later frames. The
    /// `with_stencil` half of the key separates a `Depth32Float` depth-only buffer from a
    /// `Depth24PlusStencil8` depth+stencil buffer, so a stencil-testing pass gets a stencil-aspect
    /// attachment while a plain depth pass keeps its depth-only one (the two carry different formats).
    depth_targets: HashMap<DepthTargetKey, u32>,

    /// Residency cache for sampled GL textures uploaded from CPU pixels: GL texture name → `(texture_ir,
    /// uploaded_gen)`. A texture is `CreateTexture`d + staged + `CopyBufferToTexture`d ONCE (per content
    /// generation) and re-referenced by its stable IR id on every later draw and frame. Without this a
    /// real toolkit frame (GskGL binds one glyph/mask atlas across hundreds of draws) re-uploads the whole
    /// atlas plane per draw — gigabytes of redundant `WriteBuffer` that blow the negotiated frame cap.
    tex_ir_cache: HashMap<u32, (u32, (u64, u64, bool))>,
    /// Resident CPU snapshots of imported linear-image storage, keyed by
    /// `(storage, revision, width, height, format)`.
    shared_tex_ir_cache: HashMap<(u64, u64, u32, u32, u32), SharedTextureResidency>,
    /// Latest accepted GPU render target for each imported storage. This is separate from CPU snapshot
    /// residency because its native target format need not match the normalized sampled-upload format.
    shared_target_cache: HashMap<u64, SharedTargetResidency>,
    /// Residency cache for GL data buffers (vertex/index), keyed by `(GL buffer name, IR usage bits)` →
    /// `(buffer_ir, uploaded_gen)`. Same idea as [`Self::tex_ir_cache`]: a buffer whose bytes did not
    /// change is created + uploaded once and re-bound by id. Keyed on usage too so the rare GL buffer bound
    /// as BOTH a vertex and an index source gets a correctly-typed IR buffer for each role.
    buf_ir_cache: HashMap<(u32, u32), (u32, u64)>,
    /// Canonical backing exported to CUDA, shared by every later GL use of this buffer name.
    interop_buf_ir: HashMap<u32, (u32, u64, usize)>,

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
    /// Immutable GPU samplers keyed by their complete descriptor. GL sampler and texture parameter
    /// mutations resolve to a different descriptor, so an existing resident sampler never changes.
    sampler_ir_cache: Vec<(SamplerDesc, u32)>,
    /// The INTERNAL clear shaders — a `gl_VertexID` full-target triangle and a fragment stage emitting
    /// `vec4(1.0)` — created once per context and shared by every rect clear. `None` until first use.
    clear_shader_ir: Option<(u32, u32)>,
    /// Internal clear pipelines, keyed by everything that distinguishes one: the colour target format,
    /// the pass's depth format, and the colour/depth/stencil write masks. The clear VALUES are not part
    /// of the key — depth rides the viewport's collapsed range, stencil the dynamic reference, and colour
    /// the blend constant — which is what keeps this to one shader pair and a handful of pipelines.
    clear_pipeline_cache: HashMap<ClearPipelineKey, u32>,

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
    pending_texture_deletes: HashSet<u32>,
    pending_buffer_deletes: HashSet<u32>,
    pending_sampler_deletes: HashSet<u32>,
    pending_program_deletes: HashSet<u32>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct DepthTargetKey {
    fallback_color: u32,
    depth: Option<(u32, u64)>,
    stencil: Option<(u32, u64)>,
    with_stencil: bool,
}

impl GlContext {
    /// Returns the currently bound program name (`0` means no program).
    pub fn current_program(&self) -> u32 {
        self.local.cur_prog
    }

    /// Returns the current GL viewport.
    pub fn viewport(&self) -> [i32; 4] {
        self.local.pipeline.viewport
    }

    pub fn blend_enabled(&self) -> bool {
        self.local.pipeline.blend
    }

    pub fn client_version(&self) -> (i32, i32) {
        (self.local.client_major, self.local.client_minor)
    }

    pub fn surface(&self) -> GlSurface {
        self.local.surf
    }

    pub fn surface_kind(&self) -> SurfaceKind {
        self.local.surface_kind
    }

    pub fn read_surface(&self) -> GlSurface {
        self.local.read_surf
    }

    pub fn read_surface_kind(&self) -> SurfaceKind {
        self.local.read_surface_kind
    }

    pub fn set_surface_kind(&mut self, kind: SurfaceKind) {
        self.local.surface_kind = kind;
    }

    pub fn set_surface_available(&mut self, available: bool) {
        self.local.surf.have = available;
    }

    pub fn bind_surface(&mut self, surface: GlSurface, kind: SurfaceKind) {
        self.local.surf = surface;
        self.local.surface_kind = kind;
        self.local.draw_surface_id = 0;
        self.local.read_surf = surface;
        self.local.read_surface_kind = kind;
        self.local.read_surface_id = 0;
    }

    pub fn bind_surfaces(
        &mut self,
        draw_id: u64,
        draw: GlSurface,
        draw_kind: SurfaceKind,
        read_id: u64,
        read: GlSurface,
        read_kind: SurfaceKind,
    ) {
        self.local.draw_surface_id = draw_id;
        self.local.surf = draw;
        self.local.surface_kind = draw_kind;
        self.local.read_surface_id = read_id;
        self.local.read_surf = read;
        self.local.read_surface_kind = read_kind;
    }

    pub fn bind_draw_surface(&mut self, id: u64, surface: GlSurface, kind: SurfaceKind) {
        self.local.draw_surface_id = id;
        self.local.surf = surface;
        self.local.surface_kind = kind;
    }

    pub fn default_surfaces_match(&self) -> bool {
        self.local.draw_surface_id == self.local.read_surface_id
    }

    pub fn set_surface(&mut self, surface: GlSurface) {
        self.local.surf = surface;
    }

    pub fn set_present_frame(
        &mut self,
        token: Option<hl_gpu::protocol::model::descriptor::SurfaceToken>,
        serial: Option<hl_gpu::protocol::model::descriptor::FrameSerial>,
    ) {
        self.local.present_token = token;
        self.local.present_serial = serial;
    }

    pub fn bound_framebuffer(&self) -> u32 {
        self.local.bound_fbo
    }

    pub fn read_framebuffer(&self) -> u32 {
        self.local.read_fbo
    }

    pub fn framebuffer_color_attachment(&self, framebuffer: u32) -> u32 {
        self.local.framebuffers.color_attachment(framebuffer)
    }

    /// Resolve the texture OBJECT retained by a colour attachment. Its GL-visible name may already have
    /// been deleted and reused, so an FBO cannot safely resolve through the current name table.
    pub fn framebuffer_color_texture(
        &self,
        framebuffer: u32,
        index: u32,
    ) -> Option<(u32, &GlTexture)> {
        let attachment = self
            .local
            .framebuffers
            .color_attachment_object(framebuffer, index)?;
        let texture = if attachment.object == 0 {
            self.textures.get(attachment.name)
        } else {
            self.textures.get_object(attachment.object)
        }?;
        Some((attachment.name, texture))
    }

    /// The texel format of the colour buffer `glReadPixels` would read: the READ framebuffer's colour
    /// attachment, or the default surface's plane when no framebuffer is bound.
    ///
    /// ES 3.0 §4.3.1 defines the accepted readback `format`/`type` pairs, and
    /// `GL_IMPLEMENTATION_COLOR_READ_FORMAT`/`_TYPE`, in terms of this buffer — so both questions have to
    /// ask what is actually bound rather than answering from a constant.
    pub fn read_colour_buffer_format(&self) -> hl_gpu::protocol::model::enums::TextureFormat {
        use hl_gpu::protocol::model::enums::TextureFormat;
        if self.local.read_fbo == 0 {
            // The default window target, which the frame builder allocates as `Bgra8Unorm`
            // (`service::frame::passes`). It is fixed-point, so no readback pair changes because of it.
            return TextureFormat::Bgra8Unorm;
        }
        self.framebuffer_color_texture(self.local.read_fbo, 0)
            .map(|(_, texture)| texture)
            .map_or(TextureFormat::Rgba8Unorm, |texture| texture.ir_format)
    }

    /// Whether that colour buffer is a floating-point one, which is what decides whether a `GL_FLOAT`
    /// readback is the spec's required pair or an illegal combination.
    pub fn read_colour_buffer_is_float(&self) -> bool {
        use hl_gpu::protocol::model::enums::TextureFormat;
        matches!(
            self.read_colour_buffer_format(),
            TextureFormat::Rgba16Float | TextureFormat::Rgba32Float | TextureFormat::R32Float
        )
    }

    pub fn read_colour_buffer_numeric_class(
        &self,
    ) -> hl_gpu::protocol::model::enums::TextureNumericClass {
        self.read_colour_buffer_format().numeric_class()
    }

    pub fn gen_framebuffer(&mut self) -> u32 {
        self.local.framebuffers.gen()
    }

    pub fn recording_counts(&self) -> (usize, usize) {
        (
            self.local.recording.draws.len(),
            self.local.recording.blits.len(),
        )
    }

    pub fn draws(&self) -> &[DrawCall] {
        &self.local.recording.draws
    }

    pub fn clear_recording(&mut self) {
        self.local.recording.clear();
    }

    /// Replace the program snapshot on the most recently recorded draw while preserving recording order.
    pub fn replace_last_recorded_program(&mut self, program: u32) -> bool {
        self.local.recording.replace_last_draw_program(program)
    }

    pub fn recorded_framebuffers(&self) -> impl DoubleEndedIterator<Item = u32> + '_ {
        self.local.recording.draws.iter().map(|draw| draw.fbo)
    }

    pub fn gen_query(&mut self) -> u32 {
        self.local.queries.gen()
    }

    pub fn delete_query(&mut self, query: u32) {
        self.local.queries.delete(query);
    }

    pub fn is_query(&self, query: u32) -> bool {
        self.local.queries.contains(query)
    }

    pub fn gen_transform_feedback(&mut self) -> u32 {
        self.local.transform_feedbacks.gen()
    }

    pub fn is_transform_feedback(&self, feedback: u32) -> bool {
        self.local.transform_feedbacks.contains(feedback)
    }

    pub fn transform_feedback_state(&self) -> crate::model::es3::TransformFeedbackObj {
        self.local.transform_feedbacks.bound_obj()
    }

    pub fn gen_program_pipeline(&mut self) -> u32 {
        self.local.program_pipelines.gen()
    }

    pub fn delete_program_pipeline(&mut self, pipeline: u32) {
        self.local.program_pipelines.delete(pipeline);
    }

    pub fn is_program_pipeline(&self, pipeline: u32) -> bool {
        self.local.program_pipelines.contains(pipeline)
    }

    pub fn program_pipeline(&self, pipeline: u32) -> Option<&crate::model::es3::ProgramPipeline> {
        self.local.program_pipelines.get(pipeline)
    }

    pub fn vertex_buffer_binding(&self, binding: usize) -> Option<VertexBinding> {
        self.local.vertex_bindings.get(binding).copied()
    }

    pub fn pixel_store_state(&self) -> PixelStore {
        self.local.pixel_store
    }

    pub fn bound_renderbuffer(&self) -> u32 {
        self.local.bound_rbo
    }

    pub fn bound_texture(&self) -> u32 {
        self.local.tex_unit[self.local.active_texture]
    }

    /// The texel format of the CPU shadow plane an upload into the active unit's texture must fill. An
    /// unbound or unknown name answers `Rgba8Unorm`, which is the plane a texture materialized here would
    /// be given anyway; the record layer refuses the upload for its own reasons in that case.
    pub fn bound_plane(&self) -> hl_gpu::protocol::model::enums::TextureFormat {
        self.textures.get(self.bound_texture()).map_or(
            hl_gpu::protocol::model::enums::TextureFormat::Rgba8Unorm,
            |texture| texture.ir_format,
        )
    }

    pub fn active_texture_unit(&self) -> usize {
        self.local.active_texture
    }

    pub fn texture_at(&self, unit: usize) -> u32 {
        self.local.tex_unit[unit]
    }

    pub fn attributes(&self) -> &[Attr; MAX_ATTR] {
        &self.local.attr
    }

    /// The generic (disabled-array) vertex attribute values `glVertexAttrib*f` last set.
    pub fn current_vertex_attributes(&self) -> &[[f32; 4]; MAX_ATTR] {
        &self.local.current_attr
    }

    pub fn current_vertex_array(&self) -> u32 {
        self.local.cur_vao
    }

    pub fn draw_buffers(&self) -> &[u32] {
        &self.local.draw_buffers
    }

    /// The `glDrawBuffers` selection as a per-slot write bitmask (see [`DrawCall::draw_buffer_mask`]).
    /// Slots beyond the recorded list keep their initial "writes" state, so a shorter list never disables
    /// an attachment the app did not name.
    pub fn draw_buffer_mask(&self) -> u32 {
        let mut mask = !0u32;
        for (slot, &buffer) in self.local.draw_buffers.iter().enumerate().take(32) {
            if buffer == crate::model::glconst::GL_NONE {
                mask &= !(1u32 << slot);
            }
        }
        mask
    }

    /// Whether the bound DRAW framebuffer can be depth-tested at all. The default framebuffer's depth plane
    /// is supplied by EGL and is always present in this model; a user FBO must have been given a
    /// `GL_DEPTH_ATTACHMENT`, and without one every depth test passes and writes nothing (ES 3.0 §4.1.5).
    pub fn draw_framebuffer_has_depth(&self) -> bool {
        self.local.bound_fbo == 0 || self.local.framebuffers.has_depth(self.local.bound_fbo)
    }

    /// The stencil counterpart of [`Self::draw_framebuffer_has_depth`] (ES 3.0 §4.1.4).
    pub fn draw_framebuffer_has_stencil(&self) -> bool {
        self.local.bound_fbo == 0 || self.local.framebuffers.has_stencil(self.local.bound_fbo)
    }

    pub fn read_buffer_source(&self) -> u32 {
        self.local.read_buffer_src
    }

    pub fn allocation_exhausted(&self) -> bool {
        self.allocator.is_exhausted()
    }
}

mod allocator;
mod local;
use local::LocalState;
pub use local::SurfaceTarget;
mod pipeline;
use pipeline::PipelineState;
mod recording;
mod resources;
use recording::Recording;
mod state;
mod transaction;
pub use state::ContextState;
mod targets;
mod types;

pub use allocator::{IrAllocator, Resource};

pub use types::*;
