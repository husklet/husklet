//! Immutable draw-time state snapshots.

use super::{feedback::TransformFeedbackLayout, MAX_ATTR};
use crate::model::glconst::MAX_TEXTURE_UNITS;
use crate::model::texture::GlTexture;
use hl_gpu::protocol::model::enums::TextureFormat;
use std::sync::Arc;

pub const MAX_DRAW_BUFFERS: usize = 4;

/// Blend and channel-write state owned by one fragment-output/draw-buffer slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DrawBufferState {
    pub blend: bool,
    pub src_rgb: u32,
    pub dst_rgb: u32,
    pub src_alpha: u32,
    pub dst_alpha: u32,
    pub eq_rgb: u32,
    pub eq_alpha: u32,
    pub color_mask: u32,
}

impl Default for DrawBufferState {
    fn default() -> Self {
        Self {
            blend: false,
            src_rgb: crate::model::glconst::GL_ONE,
            dst_rgb: crate::model::glconst::GL_ZERO,
            src_alpha: crate::model::glconst::GL_ONE,
            dst_alpha: crate::model::glconst::GL_ZERO,
            eq_rgb: crate::model::glconst::GL_FUNC_ADD,
            eq_alpha: crate::model::glconst::GL_FUNC_ADD,
            color_mask: 0xf,
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct TransformFeedbackCapture {
    pub layout: TransformFeedbackLayout,
    pub bindings: [Option<crate::model::context::IndexedBinding>; 4],
    pub byte_offsets: [u64; 4],
    pub byte_lengths: [u64; 4],
    pub vertices: u32,
    pub primitives: u32,
}

/// One vertex-attribute pointer's bound state (`glVertexAttribPointer` + enable flag).
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Attr {
    pub enabled: bool,
    /// Component count (1..4).
    pub size: i32,
    pub normalized: bool,
    pub integer: bool,
    /// The GL component type enum (`GL_FLOAT`, `GL_UNSIGNED_BYTE`, …).
    pub kind: u32,
    pub stride: i32,
    pub offset: usize,
    /// The GL array-buffer name this attribute fetches from.
    pub buffer: u32,
    /// The instance-step divisor (`glVertexAttribDivisor`): `0` = advance per vertex, `>0` = per
    /// instance. The lowering materializes divisors above one into an exact per-instance stream.
    pub divisor: u32,
    /// Separate-format (`glVertexAttribFormat`) attributes fetch through this VAO binding index.
    /// Legacy `glVertexAttribPointer` attributes keep fetching directly from `buffer`.
    pub binding: Option<u32>,
}

/// A captured **client-side vertex array**: one enabled attribute drawn with NO vertex buffer object
/// bound (`Attr::buffer == 0`), i.e. `glVertexAttribPointer` was given a pointer into CLIENT memory (the
/// weston-simple-egl / immediate-ish GL pattern). The deferred model can only read that client memory at
/// the moment the draw is recorded (it may change before swap), so the bytes are snapshotted then —
/// de-interleaved and TIGHTLY packed for the vertex range the draw touches — and lowered at swap into a
/// transient per-draw VERTEX buffer + a one-attribute vertex-layout slot (`CreateBuffer`/`WriteBuffer` +
/// `SetVertexBuffer`, the same path a real VBO uses).
#[derive(Clone, PartialEq, Debug, Default)]
pub struct ClientArray {
    /// The attribute location this array feeds — exactly the `layout(location = N)` the GLSL translator
    /// emits, so the lowered vertex-layout attribute's `location` matches the shader.
    pub location: usize,
    /// Tightly-packed captured bytes: one `size * component_size(kind)` element per touched vertex, with
    /// the client stride removed (element `v` of the range at byte `v * size * component_size`).
    pub data: Vec<u8>,
    /// Component count (1..4), component type, and the normalize/integer flags — the vertex-format the
    /// pipeline slot declares (mirrors the `Attr` fields, so `vertex_format_wire` produces the same code).
    pub size: i32,
    pub kind: u32,
    pub normalized: bool,
    pub integer: bool,
    /// The attribute's instance-step divisor, carried so the transient slot steps per-instance when set.
    pub divisor: u32,
}

/// One VBO/EBO generation captured when a draw is recorded. Deferred lowering happens at swap, after the
/// app may have orphaned or overwritten the same GL buffer name, so draw-time bytes are part of the draw.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct BufferSnapshot {
    pub name: u32,
    pub generation: u64,
    pub data: Arc<Vec<u8>>,
}

/// Identity and shape of an offscreen framebuffer attachment when a draw was recorded. Framebuffer
/// bindings are mutable GL state: Chrome can redefine an attachment between tile passes while retaining
/// the same GL texture name. The generation is therefore part of render-target identity.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TargetSnapshot {
    pub texture: u32,
    pub generation: u64,
    pub shared_storage: Option<u64>,
    pub shared_revision: Option<u64>,
    pub width: i32,
    pub height: i32,
    pub format: TextureFormat,
}

/// Exact depth/stencil storage generations attached when framebuffer work was recorded.
///
/// Lowering is deferred until swap, so an application may delete and recreate an attached renderbuffer
/// before the earlier draw is lowered. Public GL names alone cannot distinguish those storage objects.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct DepthStencilSnapshot {
    pub depth: Option<(u32, u64)>,
    pub stencil: Option<(u32, u64)>,
}

/// The exact sampled texture generation visible when a draw was recorded.
///
/// GL object state remains mutable until the deferred frame boundary. Keeping the object plus any
/// generation-matched resident resources here prevents a later upload, redefine, or delete from changing
/// what an earlier draw samples. Texture pixels are shared copy-on-write, so taking this snapshot does not
/// copy an atlas per draw.
#[derive(Clone, PartialEq, Debug)]
pub struct TextureSnapshot {
    pub name: u32,
    pub generation: u64,
    pub texture: GlTexture,
    pub sampled_ir: Option<u32>,
    pub fbo_ir: Option<u32>,
}

/// An immutable snapshot of the bound draw state at the moment a draw (or clear) is recorded. The frame
/// builder replays the draw-list into IR at swap. Ported from `hl-shim-gl`'s `DrawCall` (trimmed to the
/// fields the core single-draw / clear path uses this pass).
#[derive(Clone, PartialEq, Debug)]
pub struct DrawCall {
    /// A `glClear`-recorded rect rather than a geometry draw.
    pub is_clear: bool,
    /// GL primitive mode (`GL_TRIANGLES` / `GL_TRIANGLE_STRIP` / …).
    pub mode: u32,
    pub first: i32,
    pub count: i32,
    pub indexed: bool,
    pub index_type: u32,
    pub index_offset: usize,
    pub instance_count: u32,
    pub base_vertex: i32,
    pub first_instance: u32,
    /// GL program name this draw renders with.
    pub prog: u32,
    /// The framebuffer bound when the draw was recorded (`0` = default window framebuffer; non-zero =
    /// render into that FBO's color attachment instead of the default surface).
    pub fbo: u32,
    /// Offscreen color attachment captured at draw time; `None` for the default framebuffer.
    pub target: Option<TargetSnapshot>,
    pub depth_stencil: DepthStencilSnapshot,
    /// Bound element-array-buffer name (for an indexed draw).
    pub elem_buf: u32,
    /// Per-location vertex-attribute snapshot.
    pub attrs: [Attr; MAX_ATTR],
    /// Current generic values for disabled vertex arrays.
    pub current_attrs: [[f32; 4]; MAX_ATTR],
    pub current_attr_kinds: [u8; MAX_ATTR],
    /// Bound texture (GL name) per texture unit, at draw time.
    pub tex_units: [u32; MAX_TEXTURE_UNITS],
    /// Bound cube-map texture (GL name) per texture unit, at draw time.
    pub cube_tex_units: [u32; MAX_TEXTURE_UNITS],
    /// Content generation for each snapshotted texture-unit name.
    pub tex_generations: [u64; MAX_TEXTURE_UNITS],
    pub cube_tex_generations: [u64; MAX_TEXTURE_UNITS],
    /// Exact object state and resident resources for the generations bound to this draw.
    pub textures: Vec<TextureSnapshot>,
    /// Texture component mappings captured with the draw.
    pub tex_swizzles: [[u32; 4]; MAX_TEXTURE_UNITS],
    pub cube_tex_swizzles: [[u32; 4]; MAX_TEXTURE_UNITS],
    /// The ES3 sampler OBJECT bound to each texture unit (`glBindSampler`), captured at draw time. A bound
    /// sampler object OVERRIDES the texture's own filter/wrap (ES 3.0 §3.8.13) — the frame builder lowers
    /// its params into the `SamplerDesc` instead of the texture's. `None` = no sampler object bound at the
    /// unit, so the texture parameters win (the byte-identical pre-sampler-object path).
    pub samp_objs: [Option<crate::model::es3::SamplerObj>; MAX_TEXTURE_UNITS],
    /// Sampler-uniform index → texture unit, at draw time.
    pub samp_units: Vec<i32>,
    pub viewport: [i32; 4],
    pub scissor_enabled: bool,
    pub scissor: [i32; 4],
    /// `GL_RASTERIZER_DISCARD` at draw time. Vertex processing still occurs, but no primitive reaches
    /// framebuffer rasterization. An active transform-feedback snapshot still retains the vertex work.
    pub rasterizer_discard: bool,
    pub transform_feedback: Option<TransformFeedbackCapture>,
    pub blend: bool,
    /// Blend factors/equations in force (GL enums), lowered to the pipeline blend state when `blend`.
    pub blend_src_rgb: u32,
    pub blend_dst_rgb: u32,
    pub blend_src_alpha: u32,
    pub blend_dst_alpha: u32,
    pub blend_eq_rgb: u32,
    pub blend_eq_alpha: u32,
    /// Independent state for each advertised draw-buffer slot.
    pub draw_buffer_states: [DrawBufferState; MAX_DRAW_BUFFERS],
    /// Constant blend color snapshotted at draw time (`glBlendColor`).
    pub blend_color: [f32; 4],
    pub depth: bool,
    /// Depth-compare function (GL enum) + depth-write mask, lowered to the pipeline depth state.
    pub depth_func: u32,
    pub depth_write: bool,
    /// Filled-polygon depth bias captured at draw time. WebGPU expresses the GL `units` term as an
    /// integer constant in minimum-depth increments and the `factor` term as a slope scale.
    pub polygon_offset_fill: bool,
    pub polygon_offset_factor: f32,
    pub polygon_offset_units: f32,
    /// `GL_STENCIL_TEST` enabled at draw time, and the front/back stencil test snapshot: per-face compare
    /// func + stencil-fail/depth-fail/depth-pass ops (GL enums), plus the front-face reference value and
    /// read/write masks (WebGPU carries a single reference + read/write mask for both faces). Lowered to the
    /// pipeline `DepthState` stencil fields + an `Enc::SetStencilReference`.
    pub stencil: bool,
    pub stencil_func_front: u32,
    pub stencil_func_back: u32,
    pub stencil_fail_front: u32,
    pub stencil_zfail_front: u32,
    pub stencil_zpass_front: u32,
    pub stencil_fail_back: u32,
    pub stencil_zfail_back: u32,
    pub stencil_zpass_back: u32,
    pub stencil_ref_front: i32,
    pub stencil_ref_back: i32,
    pub stencil_read_mask_front: u32,
    pub stencil_read_mask_back: u32,
    pub stencil_write_mask_front: u32,
    pub stencil_write_mask_back: u32,
    /// Face culling: whether `GL_CULL_FACE` is enabled, the culled face, and the front-face winding.
    pub cull_enabled: bool,
    pub cull_face: u32,
    pub front_face: u32,
    /// `glDepthRangef` at draw time — the window-depth range the viewport transform maps onto.
    pub depth_range: [f32; 2],
    /// `glColorMask` per-channel write enable at draw time, packed `R<<0 | G<<1 | B<<2 | A<<3` — lowered
    /// verbatim into every color target's `ColorTargetState::write_mask`. `0xf` = write all channels.
    pub color_mask: u32,
    /// `glDrawBuffers` selection at draw time, one bit per color-attachment slot (`1` = this slot receives
    /// the fragment output at that location). A `GL_NONE` entry clears its bit, and the slot's color target
    /// then lowers with a zero `write_mask` — GL discards the output for that attachment, leaving whatever
    /// the attachment already held. The initial state is "every slot writes", so `!0` is the default and an
    /// app that never calls `glDrawBuffers` lowers unchanged.
    pub draw_buffer_mask: u32,
    /// The clear color in force for this draw / clear.
    pub clear: [f64; 4],
    /// The `glClearDepthf` / `glClearStencil` values in force for this draw / clear. Snapshotted with the
    /// call, exactly like [`Self::clear`]: reading them off live context state at lowering time gave a
    /// depth clear whatever value the app happened to set LAST in the frame, so a
    /// `glClearDepthf(0.5); glClear(DEPTH); glClearDepthf(1.0)` sequence cleared to 1.0.
    pub clear_depth: f32,
    pub clear_stencil: i32,
    /// For a clear call: the (x, y, w, h) rect being cleared.
    pub clear_rect: [i32; 4],
    /// For a clear call: the `glClear` buffer mask (`GL_COLOR_BUFFER_BIT | GL_DEPTH_BUFFER_BIT |
    /// GL_STENCIL_BUFFER_BIT`) as the app passed it. Defaults to the color bit so a clear built without an
    /// explicit mask keeps the historical "color clear" meaning. Which planes the clear may actually WRITE
    /// also depends on the write masks snapshotted with it — see [`DrawCall::clears_color`].
    pub clear_mask: u32,
    /// Which color attachment this clear is SCOPED to. `None` — a `glClear`, which clears every attachment
    /// the draw-buffer selection names. `Some(i)` — a `glClearBuffer*(GL_COLOR, i, …)`, which clears
    /// attachment `i` and no other. A scoped clear therefore never wipes the whole framebuffer and never
    /// discards earlier draws to the OTHER attachments.
    pub clear_draw_buffer: Option<u32>,
    /// Captured client-side vertex arrays (enabled attribs drawn with NO VBO bound). EMPTY for an
    /// all-VBO draw — so a bound-VBO draw lowers byte-identically. Each entry lowers to a transient
    /// vertex buffer + a one-attribute vertex-layout slot (see [`ClientArray`]).
    pub client_vbufs: Vec<ClientArray>,
    /// Captured client-side INDEX bytes (`glDrawElements` with NO element-array-buffer bound: the index
    /// pointer is client memory). EMPTY otherwise. Already in the final index-buffer encoding — an
    /// unsigned-byte source is promoted to `u16` here (the index IR has no `u8` format), and `index_type`
    /// is rewritten to `GL_UNSIGNED_SHORT` to match. Lowered to a transient index buffer + `SetIndexBuffer`.
    pub client_indices: Vec<u8>,
    /// Bound vertex/index buffer generations used by this draw, captured before later GL mutations can
    /// change their contents. Chrome/Skia streams several batches through the same buffer name per frame.
    pub buffers: Vec<BufferSnapshot>,
    /// The std140 bytes of the app's uniform BLOCK for this draw, snapshotted at record time from the
    /// buffer bound via `glBindBufferBase(GL_UNIFORM_BUFFER, blockBinding, buffer)` to the program's block
    /// binding point (GskGpu/GTK4's per-frame `PushConstants { mat4 mvp; mat3x4 clip; vec2 scale; }`). The
    /// app already laid these out std140, so the frame builder binds them VERBATIM at IR binding 0 — this
    /// is what carries the real per-draw transform to the shader. EMPTY when the program feeds its uniforms
    /// the default-block `glUniform*` way (the ES2 `gl_multitex`/`gl_geometry` path), which stays on
    /// `Program::ubuf` unchanged. Snapshotted (not resolved at swap) because the app updates the UBO
    /// per-draw — the bytes must be captured at the draw they belong to, exactly like `client_vbufs`.
    pub ubo_bytes: Vec<u8>,
    /// The default-block `glUniform*` bytes (`Program::ubuf[..ubuf_size]`) snapshotted at record time.
    /// Like `ubo_bytes`, this is captured PER DRAW because `Program::ubuf` is mutable program state: a
    /// frame that draws the same program twice with different `glUniform*` values between the draws (e.g.
    /// a background color then an overlay color) would otherwise see every draw take the LAST-set values.
    /// EMPTY when the program feeds its uniforms via a `glBindBufferBase`d block (`ubo_bytes`) or has no
    /// default uniforms — the frame builder then falls back to `Program::ubuf` (byte-identical old path).
    pub ubuf_bytes: Vec<u8>,
}

/// Which planes a recorded `glClear` may actually write. GL applies the CURRENT write masks to a clear
/// exactly as it does to a draw: `glColorMask` gates the color planes, `glDepthMask` the depth plane, and
/// `glStencilMask` the stencil plane. A clear whose plane is masked off entirely is a no-op and must reach
/// no attachment.
///
/// A PARTIALLY masked clear — some channels or bits enabled, some not — writes a subset that an attachment
/// LOAD OP cannot express, since a load op covers the whole attachment. It is painted by the rect draw
/// instead (see [`DrawCall::needs_rect_clear`]), whose pipeline carries the masks exactly.
impl DrawCall {
    /// This clear writes the WHOLE color value of every texel it covers.
    pub fn clears_color(&self) -> bool {
        self.is_clear
            && self.clear_mask & crate::model::glconst::GL_COLOR_BUFFER_BIT != 0
            && self.color_mask & 0xf == 0xf
    }

    /// This clear writes color attachment `slot`: an unscoped `glClear` reaches every attachment the
    /// draw-buffer selection still names, a `glClearBuffer*` only the one it was given.
    pub fn clears_color_slot(&self, slot: u32) -> bool {
        if !self.is_clear
            || self.clear_mask & crate::model::glconst::GL_COLOR_BUFFER_BIT == 0
            || self.color_mask_for_slot(slot) != 0xf
        {
            return false;
        }
        match self.clear_draw_buffer {
            Some(index) => index == slot,
            None => self.draw_buffer_mask & (1u32 << slot.min(31)) != 0,
        }
    }

    pub fn color_mask_for_slot(&self, slot: u32) -> u32 {
        self.draw_buffer_states
            .get(slot as usize)
            .map_or(0, |state| state.color_mask & 0xf)
    }

    pub fn color_clear_is_partial_slot(&self, slot: u32) -> bool {
        self.is_clear
            && self.clear_mask & crate::model::glconst::GL_COLOR_BUFFER_BIT != 0
            && !matches!(self.color_mask_for_slot(slot), 0 | 0xf)
            && match self.clear_draw_buffer {
                Some(index) => index == slot,
                None => self.draw_buffer_mask & (1u32 << slot.min(31)) != 0,
            }
    }

    /// This clear names the STENCIL plane but only some of its bits. GL applies `glStencilMask` to a
    /// clear exactly as to a draw, so only the enabled bits change; treating any non-zero mask as licence
    /// to write all eight is a different image the moment an application packs more than one thing into
    /// the stencil plane, which is the only reason to have a mask.
    pub fn stencil_clear_is_partial(&self) -> bool {
        self.is_clear
            && self.clear_mask & crate::model::glconst::GL_STENCIL_BUFFER_BIT != 0
            && !matches!(self.stencil_write_mask_front & 0xff, 0 | 0xff)
    }

    /// This clear writes something, by any route — a whole plane through a load op, or a subset through
    /// the rect draw. A clear that writes nothing at all never needs recording.
    pub fn writes_any_plane(&self) -> bool {
        let writes_any_color = (0..self.draw_buffer_states.len() as u32)
            .any(|slot| self.clears_color_slot(slot) || self.color_clear_is_partial_slot(slot));
        writes_any_color
            || self.clears_depth()
            || self.clears_stencil()
    }

    /// This clear cannot be expressed as an attachment LOAD OP and must be painted by the rect draw
    /// instead (see [`crate::service::frame`]): a load op covers the whole attachment and writes every
    /// channel and bit of it.
    ///
    /// A scissored COLOUR clear is excluded on purpose — `Enc::ClearRect` already paints exactly its rect
    /// with every channel enabled, and it is cheaper than a draw.
    pub fn needs_rect_clear(&self) -> bool {
        if !self.is_clear {
            return false;
        }
        self.color_clear_is_partial()
            || self.stencil_clear_is_partial()
            || (self.scissor_enabled && (self.clears_depth() || self.clears_stencil()))
    }

    /// This clear names the color buffer but only SOME of its channels, so it is painted by the rect draw
    /// rather than by a load op (see [`Self::needs_rect_clear`]).
    pub fn color_clear_is_partial(&self) -> bool {
        self.is_clear
            && self.clear_mask & crate::model::glconst::GL_COLOR_BUFFER_BIT != 0
            && !matches!(self.color_mask & 0xf, 0 | 0xf)
    }

    /// This clear writes the depth plane (`glDepthMask(GL_FALSE)` makes it a no-op).
    pub fn clears_depth(&self) -> bool {
        self.is_clear
            && self.clear_mask & crate::model::glconst::GL_DEPTH_BUFFER_BIT != 0
            && self.depth_write
    }

    /// `glCullFace(GL_FRONT_AND_BACK)` with `GL_CULL_FACE` enabled: GL discards EVERY triangle, whichever
    /// way it winds. WebGPU's cull mode names one face only, so such a draw cannot be expressed as a
    /// pipeline state and is dropped before lowering instead. Culling applies to triangles alone — points
    /// and lines are never culled — so the primitive mode is part of the test.
    pub fn discards_every_primitive(&self) -> bool {
        (!self.is_clear && self.rasterizer_discard)
            || (self.cull_enabled
                && self.cull_face == crate::model::glconst::GL_FRONT_AND_BACK
                && matches!(
                    self.mode,
                    crate::model::glconst::GL_TRIANGLES
                        | crate::model::glconst::GL_TRIANGLE_STRIP
                        | 0x0006 /* GL_TRIANGLE_FAN */
                ))
    }

    /// Whether this GEOMETRY draw leaves a result in the depth or stencil plane — a result a later colour
    /// clear does not erase and a later draw may test against.
    ///
    /// A stencil-testing draw with any non-`GL_KEEP` op and a non-zero write mask writes stencil; a
    /// depth-testing draw with `glDepthMask(GL_TRUE)` writes depth. Clears are excluded: they are not
    /// draws, and which planes they touch is answered by `clears_*`.
    pub fn writes_depth_or_stencil(&self) -> bool {
        if self.is_clear {
            return false;
        }
        let keep = crate::model::glconst::GL_KEEP;
        let stencil = self.stencil
            && (self.stencil_write_mask_front | self.stencil_write_mask_back) & 0xff != 0
            && [
                self.stencil_fail_front,
                self.stencil_zfail_front,
                self.stencil_zpass_front,
                self.stencil_fail_back,
                self.stencil_zfail_back,
                self.stencil_zpass_back,
            ]
            .iter()
            .any(|op| *op != keep);
        stencil || (self.depth && self.depth_write)
    }

    /// This clear writes the stencil plane (a zero `glStencilMask` makes it a no-op).
    pub fn clears_stencil(&self) -> bool {
        self.is_clear
            && self.clear_mask & crate::model::glconst::GL_STENCIL_BUFFER_BIT != 0
            && (self.stencil_write_mask_front | self.stencil_write_mask_back) & 0xff != 0
    }
}

impl Default for DrawCall {
    fn default() -> Self {
        Self {
            is_clear: false,
            mode: 0,
            first: 0,
            count: 0,
            indexed: false,
            index_type: 0,
            index_offset: 0,
            instance_count: 1,
            base_vertex: 0,
            first_instance: 0,
            prog: 0,
            fbo: 0,
            target: None,
            depth_stencil: DepthStencilSnapshot::default(),
            elem_buf: 0,
            attrs: [Attr::default(); MAX_ATTR],
            current_attrs: [[0.0, 0.0, 0.0, 1.0]; MAX_ATTR],
            current_attr_kinds: [0; MAX_ATTR],
            tex_units: [0; MAX_TEXTURE_UNITS],
            cube_tex_units: [0; MAX_TEXTURE_UNITS],
            tex_generations: [0; MAX_TEXTURE_UNITS],
            cube_tex_generations: [0; MAX_TEXTURE_UNITS],
            textures: Vec::new(),
            tex_swizzles: [[
                crate::model::glconst::GL_RED,
                crate::model::glconst::GL_GREEN,
                crate::model::glconst::GL_BLUE,
                crate::model::glconst::GL_ALPHA,
            ]; MAX_TEXTURE_UNITS],
            cube_tex_swizzles: [[
                crate::model::glconst::GL_RED,
                crate::model::glconst::GL_GREEN,
                crate::model::glconst::GL_BLUE,
                crate::model::glconst::GL_ALPHA,
            ]; MAX_TEXTURE_UNITS],
            samp_objs: [None; MAX_TEXTURE_UNITS],
            samp_units: Vec::new(),
            viewport: [0; 4],
            scissor_enabled: false,
            scissor: [0; 4],
            rasterizer_discard: false,
            transform_feedback: None,
            blend: false,
            blend_src_rgb: crate::model::glconst::GL_ONE,
            blend_dst_rgb: crate::model::glconst::GL_ZERO,
            blend_src_alpha: crate::model::glconst::GL_ONE,
            blend_dst_alpha: crate::model::glconst::GL_ZERO,
            blend_eq_rgb: crate::model::glconst::GL_FUNC_ADD,
            blend_eq_alpha: crate::model::glconst::GL_FUNC_ADD,
            draw_buffer_states: [DrawBufferState::default(); MAX_DRAW_BUFFERS],
            blend_color: [0.0; 4],
            depth: false,
            depth_func: crate::model::glconst::GL_LESS,
            depth_write: true,
            polygon_offset_fill: false,
            polygon_offset_factor: 0.0,
            polygon_offset_units: 0.0,
            stencil: false,
            stencil_func_front: crate::model::glconst::GL_ALWAYS,
            stencil_func_back: crate::model::glconst::GL_ALWAYS,
            stencil_fail_front: crate::model::glconst::GL_KEEP,
            stencil_zfail_front: crate::model::glconst::GL_KEEP,
            stencil_zpass_front: crate::model::glconst::GL_KEEP,
            stencil_fail_back: crate::model::glconst::GL_KEEP,
            stencil_zfail_back: crate::model::glconst::GL_KEEP,
            stencil_zpass_back: crate::model::glconst::GL_KEEP,
            stencil_ref_front: 0,
            stencil_ref_back: 0,
            stencil_read_mask_front: 0xffff_ffff,
            stencil_read_mask_back: 0xffff_ffff,
            stencil_write_mask_front: 0xffff_ffff,
            stencil_write_mask_back: 0xffff_ffff,
            cull_enabled: false,
            cull_face: crate::model::glconst::GL_BACK,
            front_face: crate::model::glconst::GL_CCW,
            color_mask: 0xf,
            depth_range: [0.0, 1.0],
            draw_buffer_mask: !0,
            clear: [0.0; 4],
            clear_depth: 1.0,
            clear_stencil: 0,
            clear_rect: [0; 4],
            clear_mask: crate::model::glconst::GL_COLOR_BUFFER_BIT,
            clear_draw_buffer: None,
            client_vbufs: Vec::new(),
            client_indices: Vec::new(),
            buffers: Vec::new(),
            ubo_bytes: Vec::new(),
            ubuf_bytes: Vec::new(),
        }
    }
}
