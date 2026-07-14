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
use super::framebuffer::Framebuffers;
use super::glconst;
use super::program::{Attr, DrawCall, Programs, MAX_ATTR};
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

    // ---- currently-bound GL state ----------------------------------------------------------------
    /// The program bound by `glUseProgram`.
    pub cur_prog: u32,
    /// The buffer bound to `GL_ARRAY_BUFFER`.
    pub array_buffer: u32,
    /// The buffer bound to `GL_ELEMENT_ARRAY_BUFFER`.
    pub element_buffer: u32,
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
    /// The framebuffer bound by `glBindFramebuffer` (`0` = the default window framebuffer).
    pub bound_fbo: u32,

    /// The pack/unpack pixel-store parameters (`glPixelStorei`).
    pub pixel_store: PixelStore,

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

    /// The default render-target texture + presentable surface IR ids, minted once and cached (0 =
    /// not yet created). The frame builder emits their `CreateTexture`/`CreateSurface` on first use.
    default_tex_ir: u32,
    default_surface_ir: u32,

    /// Per-FBO offscreen render-target IR ids, keyed by the FBO's color-attachment GL texture name →
    /// `(surface_ir, texture_ir)`. Minted + `CreateTexture`/`CreateSurface`d on first use and reused on
    /// later frames (so re-rendering the same FBO does not re-create the target).
    fbo_targets: HashMap<u32, (u32, u32)>,
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
            cur_prog: 0,
            array_buffer: 0,
            element_buffer: 0,
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
            pixel_store: PixelStore::default(),
            gl_error: glconst::GL_NO_ERROR,
            draws: Vec::new(),
            next_buffer: 1,
            next_texture: 1,
            next_sampler: 1,
            next_shader: 1,
            next_pipeline: 1,
            next_bind_group: 1,
            next_surface: 1,
            default_tex_ir: 0,
            default_surface_ir: 0,
            fbo_targets: HashMap::new(),
        }
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
            self.gl_error = e;
        }
    }

    /// Read + clear the last GL error (`glGetError`), returning `GL_NO_ERROR` when none is pending.
    pub fn take_gl_error(&mut self) -> u32 {
        std::mem::replace(&mut self.gl_error, glconst::GL_NO_ERROR)
    }
}
