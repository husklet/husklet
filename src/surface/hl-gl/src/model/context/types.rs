use super::*;

/// A recorded `glBlitFramebuffer` — a sub-rect copy from a read framebuffer's color attachment to a draw
/// framebuffer's. Rects are GL window coordinates (bottom-left origin), captured verbatim; the frame
/// builder resolves the two FBOs' render-target textures and lowers the equal-size, same-format,
/// UNMIRRORED case to `Enc::CopyTextureToTexture`; a scaling, converting or mirrored blit lowers to
/// `Enc::BlitTexture` with `filter` and the net per-axis `Mirror` (both flipping Y into the textures'
/// top-left origin). Rects are captured verbatim precisely so an inverted one still reads as inverted.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct BlitOp {
    /// The read (source) and draw (destination) framebuffer names bound when the blit was recorded.
    pub read_fbo: u32,
    pub draw_fbo: u32,
    /// Color attachments captured when `glBlitFramebuffer` executed. FBO attachment state is mutable,
    /// so replay at swap must not resolve these names against a later attachment.
    pub read_target: Option<crate::model::program::TargetSnapshot>,
    pub draw_target: Option<crate::model::program::TargetSnapshot>,
    /// Resolved host targets transferred from an accepted partial flush. These keep a cross-boundary blit
    /// attached to the exact producer generation even after its GL attachment was deleted.
    pub read_ir: Option<u32>,
    pub draw_ir: Option<u32>,
    /// Source rect `[x0, y0, x1, y1]` and destination rect, in GL bottom-left window coordinates.
    pub src: [i32; 4],
    pub dst: [i32; 4],
    /// The resampling filter for a SCALING blit (`glBlitFramebuffer`'s `filter` arg: `GL_LINEAR` →
    /// [`Filter::Linear`], `GL_NEAREST` → [`Filter::Nearest`]). Ignored for the equal-size copy path.
    pub filter: hl_gpu::protocol::model::enums::Filter,
}

/// A framebuffer-to-texture copy captured at its GL call site. The source framebuffer attachment is
/// snapshotted because attachment state is mutable; the destination names a texture image subresource.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct CopyTexOp {
    pub read_fbo: u32,
    pub read_target: Option<crate::model::program::TargetSnapshot>,
    pub read_ir: Option<u32>,
    pub texture: u32,
    pub generation: u64,
    pub cube: bool,
    pub face: u32,
    pub level: u32,
    pub src: [i32; 2],
    pub dst: [i32; 2],
    pub extent: [i32; 2],
}

/// A CPU sub-image write whose destination already has newer GPU-rendered content. The old generation's
/// render target is copied into the replacement generation before `pixels` overlays the requested rect.
#[derive(Clone, PartialEq, Debug)]
pub struct TexSubImageOp {
    pub fbo: u32,
    pub texture: u32,
    pub source_generation: u64,
    pub destination_generation: u64,
    pub offset: [i32; 2],
    pub extent: [i32; 2],
    pub texture_extent: [i32; 2],
    pub format: hl_gpu::protocol::model::enums::TextureFormat,
    pub pixels: std::sync::Arc<Vec<u8>>,
}

/// One framebuffer-affecting GL operation in exact call order.
#[derive(Clone, PartialEq, Debug)]
pub enum FrameOp {
    Draw(Box<crate::model::program::DrawCall>),
    Blit(BlitOp),
    CopyTex(CopyTexOp),
    TexSubImage(TexSubImageOp),
}

/// The presented window surface (the default framebuffer). Ported from `hl-shim-gl`'s `Surface`.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct GlSurface {
    /// Whether `eglCreateWindowSurface` has brought a surface up.
    pub have: bool,
    pub width: u32,
    pub height: u32,
}

/// The EGL surface contract governing framebuffer-0 submission.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SurfaceKind {
    /// A native window presents framebuffer `0` at `eglSwapBuffers`.
    #[default]
    Window,
    /// Pbuffer and surfaceless contexts complete framebuffer `0` at `glFlush`/`glFinish`.
    Offscreen,
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
    pub unpack_image_height: i32,
    pub unpack_skip_images: i32,
    pub pack_row_length: i32,
    pub pack_skip_rows: i32,
    pub pack_skip_pixels: i32,
}

/// The byte placement of one pixel-pack operation after applying the current `GL_PACK_*` state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PackLayout {
    row_bytes: usize,
    row_stride: usize,
    start_offset: usize,
    required_size: usize,
}

impl PackLayout {
    pub fn row_bytes(self) -> usize {
        self.row_bytes
    }

    pub fn row_stride(self) -> usize {
        self.row_stride
    }

    pub fn start_offset(self) -> usize {
        self.start_offset
    }

    pub fn required_size(self) -> usize {
        self.required_size
    }
}

impl PixelStore {
    /// The distance in bytes between the starts of two consecutive rows a readback writes.
    ///
    /// GLES2 §4.3.1: a pack operation starts every row at a multiple of `GL_PACK_ALIGNMENT`, so a row
    /// whose bytes do not already fill a whole number of alignment units is followed by padding the
    /// caller expects to be skipped. Only byte-typed readback is modeled — `glReadPixels` rejects every
    /// other type — so the element size is one and the stride is the tightly packed row rounded up.
    ///
    /// The padding sits BETWEEN rows. The last row is not padded, and a caller's buffer is only
    /// `stride * (rows - 1) + row_bytes` long, so a writer must copy `row_bytes` per row rather than
    /// `stride`.
    pub fn pack_stride(&self, row_bytes: usize) -> usize {
        let alignment = self.pack_alignment.max(1) as usize;
        row_bytes.div_ceil(alignment) * alignment
    }

    /// The bytes a readback of `rows` rows of `row_bytes` needs in the caller's buffer: every row but the
    /// last is followed by its alignment padding. This is what a bounded readback (`glReadnPixels`) must
    /// check `bufSize` against — the tightly packed size would accept a buffer the write then overruns.
    pub fn pack_size(&self, row_bytes: usize, rows: usize) -> usize {
        match rows.checked_sub(1) {
            None => 0,
            Some(before) => self.pack_stride(row_bytes) * before + row_bytes,
        }
    }

    /// Resolve the complete destination layout for a `glReadPixels` pack operation.
    ///
    /// `GL_PACK_ROW_LENGTH` controls the distance between row starts, while `GL_PACK_SKIP_ROWS` and
    /// `GL_PACK_SKIP_PIXELS` move the first destination texel. Arithmetic is checked because these values
    /// originate in application state and are used to validate robust-buffer sizes before an unsafe copy.
    pub fn pack_layout(
        &self,
        width: usize,
        rows: usize,
        bytes_per_pixel: usize,
    ) -> Option<PackLayout> {
        let row_pixels = if self.pack_row_length > 0 {
            self.pack_row_length as usize
        } else {
            width
        };
        let row_bytes = width.checked_mul(bytes_per_pixel)?;
        let storage_row_bytes = row_pixels.checked_mul(bytes_per_pixel)?;
        let row_stride = self.pack_stride(storage_row_bytes);
        let skip_rows = usize::try_from(self.pack_skip_rows).ok()?;
        let skip_pixels = usize::try_from(self.pack_skip_pixels).ok()?;
        let start_offset = skip_rows
            .checked_mul(row_stride)?
            .checked_add(skip_pixels.checked_mul(bytes_per_pixel)?)?;
        let required_size = if rows == 0 || row_bytes == 0 {
            start_offset
        } else {
            start_offset
                .checked_add((rows - 1).checked_mul(row_stride)?)?
                .checked_add(row_bytes)?
        };
        Some(PackLayout {
            row_bytes,
            row_stride,
            start_offset,
            required_size,
        })
    }
}

/// A Vertex Array Object's captured state: the per-location vertex-attribute array plus the
/// element-array-buffer binding. Binding a VAO swaps this state into the live context (`ctx.attr` /
/// `ctx.element_buffer`); a GLES3 app MUST bind a VAO before it can draw. Ported from `hl-shim-gl`'s `Vao`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Vao {
    /// The captured per-location vertex-attribute array (`glVertexAttribPointer` + enable + divisor).
    pub attrs: [Attr; MAX_ATTR],
    /// Separate-format vertex-buffer bindings (`glBindVertexBuffer`), indexed by binding point.
    pub vertex_bindings: [VertexBinding; MAX_ATTR],
    /// The captured `GL_ELEMENT_ARRAY_BUFFER` binding (element buffer is VAO state; array buffer is not).
    pub element_buffer: u32,
}

impl Default for Vao {
    fn default() -> Self {
        Self {
            attrs: [Attr::default(); MAX_ATTR],
            vertex_bindings: [VertexBinding::default(); MAX_ATTR],
            element_buffer: 0,
        }
    }
}

/// One VAO-owned separate-format vertex-buffer binding.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct VertexBinding {
    pub buffer: u32,
    pub offset: usize,
    pub stride: i32,
    pub divisor: u32,
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TransformFeedbackReadback {
    pub(crate) ir: u32,
    pub(crate) buffer: u32,
    pub(crate) offset: usize,
    pub(crate) len: usize,
}

/// One named uniform block of a program (`glGetUniformBlockIndex`/`glUniformBlockBinding`). The block's
/// member layout + data size live on the [`super::program::Program`] (the single implicit block this
/// model reflects); this record carries the block's declared name and its app-assigned binding point.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct UniformBlock {
    pub name: String,
    pub binding: u32,
    /// The block's std140 size, reflected from its declaration at link.
    pub data_size: i32,
    /// How many uniforms the block declares.
    pub members: i32,
}

impl Default for PixelStore {
    fn default() -> Self {
        Self {
            unpack_alignment: 4,
            pack_alignment: 4,
            unpack_row_length: 0,
            unpack_skip_rows: 0,
            unpack_skip_pixels: 0,
            unpack_image_height: 0,
            unpack_skip_images: 0,
            pack_row_length: 0,
            pack_skip_rows: 0,
            pack_skip_pixels: 0,
        }
    }
}

/// What distinguishes one internal clear pipeline from another (see [`GlContext::clear_pipeline_ir`]).
///
/// The clear VALUES are deliberately absent: depth rides the viewport's collapsed depth range, stencil
/// the dynamic `SetStencilReference`, and colour the `SetBlendConstant`. Only the things baked into a
/// pipeline — the target format and the write masks — can vary the pipeline.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ClearPipelineKey {
    pub color_format: u32,
    pub depth_format: u32,
    pub color_write_mask: u32,
    pub depth_write: bool,
    pub stencil_write_mask: u32,
}
