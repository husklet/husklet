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
use super::program::{Attr, DrawCall, Programs, MAX_ATTR};
use super::texture::Textures;

/// The presented window surface (the default framebuffer). Ported from `hl-shim-gl`'s `Surface`.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct GlSurface {
    /// Whether `eglCreateWindowSurface` has brought a surface up.
    pub have: bool,
    pub width: u32,
    pub height: u32,
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
    pub viewport: [i32; 4],
    pub scissor_enabled: bool,
    pub scissor: [i32; 4],
    pub blend: bool,
    pub depth: bool,

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
            cur_prog: 0,
            array_buffer: 0,
            element_buffer: 0,
            active_texture: 0,
            tex_unit: [0; 8],
            attr: [Attr::default(); MAX_ATTR],
            clear_color: [0.0; 4],
            viewport: [0; 4],
            scissor_enabled: false,
            scissor: [0; 4],
            blend: false,
            depth: false,
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

    /// The draw-target width/height in pixels (the surface size for the default framebuffer).
    pub fn target_wh(&self) -> (i32, i32) {
        (self.surf.width as i32, self.surf.height as i32)
    }

    /// Reset the per-frame draw state after a successful swap (`eglSwapBuffers` tail).
    pub fn reset_frame(&mut self) {
        self.draws.clear();
    }
}
