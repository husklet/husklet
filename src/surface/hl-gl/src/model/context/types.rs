use super::*;

/// A recorded `glBlitFramebuffer` — a sub-rect copy from a read framebuffer's color attachment to a draw
/// framebuffer's. Rects are GL window coordinates (bottom-left origin), captured verbatim; the frame
/// builder resolves the two FBOs' render-target textures and lowers the equal-size (non-scaling) case to
/// `Enc::CopyTextureToTexture` and the scaling case (source extent ≠ destination extent) to
/// `Enc::BlitTexture` with `filter` (both flipping Y into the textures' top-left origin).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct BlitOp {
    /// The read (source) and draw (destination) framebuffer names bound when the blit was recorded.
    pub read_fbo: u32,
    pub draw_fbo: u32,
    /// Source rect `[x0, y0, x1, y1]` and destination rect, in GL bottom-left window coordinates.
    pub src: [i32; 4],
    pub dst: [i32; 4],
    /// The resampling filter for a SCALING blit (`glBlitFramebuffer`'s `filter` arg: `GL_LINEAR` →
    /// [`Filter::Linear`], `GL_NEAREST` → [`Filter::Nearest`]). Ignored for the equal-size copy path.
    pub filter: hl_gpu::protocol::model::enums::Filter,
}

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
        Self {
            attrs: [Attr::default(); MAX_ATTR],
            element_buffer: 0,
        }
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
