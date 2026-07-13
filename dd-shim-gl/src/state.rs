//! The GLES/EGL state machine — a faithful Rust port of `gl_shim.c`'s process-global object tables and
//! scalar state. Like the C shim, these entry points *accumulate state*; no IR is emitted here (the IR
//! is lowered from this state at swap, in the present path this increment deliberately does not touch —
//! see `lower.rs` for the present-independent resource lowering used by the future swap path).
//!
//! Storage mirrors gl_shim.c exactly (fixed slot tables, ids == slot index from 1, same defaults, same
//! `gen` dirty-counters) so the eventual IR lowering is byte-equivalent.

use core::ffi::c_void;
use std::sync::{Mutex, MutexGuard, OnceLock};

use crate::glconst::*;

pub const MAXSH: usize = 64;
pub const MAXPROG: usize = 64;
pub const MAXBUF: usize = 64;
pub const MAXATTR: usize = 16;
pub const MAXTEX: usize = 32;
pub const MAXVAO: usize = 128;
pub const MAXFBO: usize = 64;
pub const MAXRBO: usize = 64;
pub const UBUF_BYTES: usize = 512;

/// A framebuffer object's color attachment (gl_shim.c `struct fbo`).
#[derive(Clone, Copy, Default)]
pub struct Fbo {
    pub used: bool,
    pub color_tex: u32,
    pub color_rbo: u32,
    pub color_level: i32,
    pub color_layer: i32,
    /// Depth / stencil renderbuffer attachments (0 = none). A combined `GL_DEPTH_STENCIL_ATTACHMENT`
    /// sets BOTH to the same renderbuffer. Used for framebuffer-completeness of depth/stencil buffers.
    pub depth_rbo: u32,
    pub stencil_rbo: u32,
}

/// A renderbuffer object (gl_shim.c `struct rbo`).
#[derive(Clone, Copy, Default)]
pub struct Rbo {
    pub used: bool,
    pub w: i32,
    pub h: i32,
    pub samples: i32,
    pub ifmt: u32,
    pub gen: u64,
}

#[derive(Clone, Default)]
pub struct Shader {
    pub used: bool,
    pub delete_pending: bool,
    pub kind: u32, // GL_VERTEX_SHADER / GL_FRAGMENT_SHADER
    pub src: Option<String>,
    /// Truthful GLSL compile outcome (`glGetShaderiv(GL_COMPILE_STATUS)`): false until a successful
    /// `glCompileShader`. `info_log` is the human-readable diagnostic on failure (empty on success).
    pub compile_ok: bool,
    pub info_log: String,
}

#[derive(Clone)]
pub struct Program {
    pub used: bool,
    pub delete_pending: bool,
    pub vs: u32,
    pub fs: u32,
    pub linked: bool,
    /// Truthful link outcome (`glGetProgramiv(GL_LINK_STATUS)`): requires a vertex AND fragment shader
    /// that both compiled. `info_log` is the diagnostic on failure (empty on success).
    pub link_ok: bool,
    pub info_log: String,
    pub ubuf: [u8; UBUF_BYTES], // uniform-block bytes (written by glUniform*)
    pub samp_units: [i32; 4],   // sampler uniform index -> GL texture unit (glUniform1i)
    // populated at glLinkProgram by the GLSL→MSL translator:
    pub msl: Option<String>,                    // combined MSL source (→ CreateShader)
    pub unis: Vec<crate::translate::Uni>,       // uniform-block layout (name→offset/size)
    pub ubuf_size: i32,                         // MSL Uniforms struct size (16-aligned)
    pub samp_names: Vec<String>,                // sampler uniform names, declaration order
}

impl Default for Program {
    fn default() -> Self {
        Program {
            used: false,
            delete_pending: false,
            vs: 0,
            fs: 0,
            linked: false,
            link_ok: false,
            info_log: String::new(),
            ubuf: [0; UBUF_BYTES],
            samp_units: [0; 4],
            msl: None,
            unis: Vec::new(),
            ubuf_size: 0,
            samp_names: Vec::new(),
        }
    }
}

impl Program {
    /// This program's vertex-shader GLSL source (for the declared-attribute layout), if attached.
    pub fn vs_src_from(&self, s: &GlState) -> Option<String> {
        if self.vs != 0 && (self.vs as usize) < MAXSH {
            s.sh[self.vs as usize].src.clone()
        } else {
            None
        }
    }
}

#[derive(Clone, Default)]
pub struct Buffer {
    /// `glGenBuffers` reserves a name; the object comes into existence on first bind.
    pub reserved: bool,
    pub used: bool,
    pub data: Vec<u8>,
    pub usage: u32,
    pub gen: u64, // bumped on every content mutation → dirty key for the swap-time upload skip
    /// True between `glMapBufferRange` and `glUnmapBuffer`. A draw that sources vertices or indices
    /// from a currently-mapped buffer is `GL_INVALID_OPERATION` (the client still owns the storage).
    pub mapped: bool,
}

#[derive(Clone, Default)]
pub struct Texture {
    /// `glGenTextures` reserves a name; the object comes into existence on first bind.
    pub reserved: bool,
    pub used: bool,
    pub w: i32,
    pub h: i32,
    pub data: Vec<u8>, // RGBA8, converted from the app's upload format
    pub immutable: bool,
    pub levels: i32,
    pub minf: u32,
    pub magf: u32,
    pub ws: u32,
    pub wt: u32,
    pub gen: u64,
}

/// An ES3 sampler object (`glGenSamplers`/`glBindSampler`/`glSamplerParameter*`). Carries the full
/// per-object filter/wrap/LOD/compare state so `glGetSamplerParameter*` reflects what was set, rather
/// than the old no-op that always returned 0. Defaults follow the ES 3.0 sampler-state table (6.10).
#[derive(Clone, Copy)]
pub struct SamplerObj {
    pub min_filter: i32,
    pub mag_filter: i32,
    pub wrap_s: i32,
    pub wrap_t: i32,
    pub wrap_r: i32,
    pub min_lod: f32,
    pub max_lod: f32,
    pub compare_mode: i32,
    pub compare_func: i32,
}

impl Default for SamplerObj {
    fn default() -> Self {
        SamplerObj {
            min_filter: 0x2702, // GL_NEAREST_MIPMAP_LINEAR
            mag_filter: 0x2601, // GL_LINEAR
            wrap_s: 0x2901,     // GL_REPEAT
            wrap_t: 0x2901,
            wrap_r: 0x2901,
            min_lod: -1000.0,
            max_lod: 1000.0,
            compare_mode: 0,      // GL_NONE
            compare_func: 0x0203, // GL_LEQUAL
        }
    }
}

/// An ES3 query object (`glGenQueries`/`glBeginQuery`/`glEndQuery`). Tracks the typed target it was
/// first used with, whether it is currently active, and the result plus the submission serial at
/// `glEndQuery` so `GL_QUERY_RESULT_AVAILABLE` becomes true only once that submission has completed
/// (the same serial contract the sync objects use). The occlusion/primitive count itself is not run by
/// the executor yet, so `result` is a truthful 0; the lifecycle and availability are real.
#[derive(Clone, Copy)]
pub struct QueryObj {
    /// The target this query name was bound to on its first `glBeginQuery` (0 = never used).
    pub target: u32,
    /// Currently inside a begin/end pair.
    pub active: bool,
    /// A result has been produced by a completed `glEndQuery`.
    pub ended: bool,
    /// Result value (samples passed / primitives written). No backend counter yet ⇒ truthful 0.
    pub result: u32,
    /// Submission serial captured at `glEndQuery`; the result is available once completion catches up.
    pub serial: u64,
}

impl Default for QueryObj {
    fn default() -> Self {
        QueryObj { target: 0, active: false, ended: false, result: 0, serial: 0 }
    }
}

/// An ES3 transform-feedback object (`glGenTransformFeedbacks`/`glBindTransformFeedback`). The default
/// object (name 0) always exists; named objects are created on first bind. `active`/`paused` model the
/// begin/end/pause/resume state machine.
#[derive(Clone, Copy, Default)]
pub struct TransformFeedbackObj {
    pub active: bool,
    pub paused: bool,
}

/// One indexed buffer binding point (`glBindBufferBase`/`glBindBufferRange` for `GL_UNIFORM_BUFFER` /
/// `GL_TRANSFORM_FEEDBACK_BUFFER`). `size == 0` means "the whole buffer" (the glBindBufferBase form).
#[derive(Clone, Copy, Default)]
pub struct IndexedBinding {
    pub buffer: u32,
    pub offset: isize,
    pub size: isize,
}

/// One uniform block of a program (`glGetUniformBlockIndex` assigns the index; `glUniformBlockBinding`
/// sets the binding). Without GLSL uniform-block reflection the shim assigns block indices lazily and
/// stably per queried name — a real, self-consistent index namespace, defaulting each block's binding
/// to 0 as the spec requires.
#[derive(Clone, Default)]
pub struct UniformBlock {
    pub name: String,
    pub binding: u32,
}

#[derive(Clone, Copy, Default)]
pub struct Attr {
    pub enabled: bool,
    pub size: i32,
    pub normalized: bool,
    pub integer: bool,
    pub kind: u32, // GL type enum
    pub stride: i32,
    pub offset: usize,
    pub buffer: u32,
}

#[derive(Clone)]
pub struct Vao {
    pub used: bool,
    pub attrs: [Attr; MAXATTR],
    pub elem_buf: u32,
}

/// A draw-time snapshot of one source vertex/index buffer (gl_shim.c `snap_vbo_*`/`snap_ibo_*`): apps
/// may disable attribs / mutate buffers before swap, so the replay path renders from these copies.
#[derive(Clone)]
pub struct BufSnap {
    pub src: u32,
    pub gen: u64,
    pub data: Vec<u8>,
}

/// One recorded draw or clear, snapshotting the state that produces its IR at swap (gl_shim.c
/// `struct draw_call`). Only the fields the ported paths consume are carried.
#[derive(Clone)]
pub struct DrawCall {
    pub is_clear: bool,
    pub mode: u32,
    pub first: i32,
    pub count: i32,
    pub indexed: bool,
    pub index_type: u32,
    pub index_offset: usize,
    pub prog: u32,
    pub elem_buf: u32,
    pub target_tex: u32, // 0 = default window surface, else the GL texture on the draw FBO color0
    pub clear_rect: [i32; 4],
    pub attrs: [Attr; MAXATTR],
    pub tex_units: [u32; 8],
    pub samp_units: [i32; 4],
    pub viewport: [i32; 4],
    pub scissor_enabled: bool,
    pub scissor: [i32; 4],
    pub blend: bool,
    pub blend_src_rgb: u32,
    pub blend_dst_rgb: u32,
    pub blend_src_alpha: u32,
    pub blend_dst_alpha: u32,
    pub blend_eq_rgb: u32,
    pub blend_eq_alpha: u32,
    pub clear: [f32; 4],
    pub clear_serial: i32,
    pub ubuf: [u8; UBUF_BYTES],
    /// Draw-time VBO snapshots (deduped by source buffer id), and the index buffer snapshot.
    pub snap_vbo: Vec<BufSnap>,
    pub snap_ibo: Option<BufSnap>,
}

impl Default for DrawCall {
    fn default() -> Self {
        DrawCall {
            is_clear: false,
            mode: 0,
            first: 0,
            count: 0,
            indexed: false,
            index_type: 0,
            index_offset: 0,
            prog: 0,
            elem_buf: 0,
            target_tex: 0,
            clear_rect: [0; 4],
            attrs: [Attr::default(); MAXATTR],
            tex_units: [0; 8],
            samp_units: [0; 4],
            viewport: [0; 4],
            scissor_enabled: false,
            scissor: [0; 4],
            blend: false,
            blend_src_rgb: 0,
            blend_dst_rgb: 0,
            blend_src_alpha: 0,
            blend_dst_alpha: 0,
            blend_eq_rgb: 0,
            blend_eq_alpha: 0,
            clear: [0.0; 4],
            clear_serial: 0,
            ubuf: [0; UBUF_BYTES],
            snap_vbo: Vec::new(),
            snap_ibo: None,
        }
    }
}

/// The presented default framebuffer / window surface (gl_shim.c `g_surf` + geometry). `id` is the
/// engine surface/IOSurface id the frame renders into (1 in DD_IR_DUMP/host-tool mode); `stride`/`fd`
/// come from the renderD128 alloc and drive the wayland dma-buf commit. The `logical_*`/`geom_*`/
/// `attach_*` fields are the compositor-facing geometry resolved at bring-up.
#[derive(Clone, Copy, Default)]
pub struct Surface {
    pub have: bool,
    pub id: u32,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub fd: i32,
    /// Allocation generation from the engine (renderD128 alloc reply); stamped into the dmabuf modifier
    /// so the compositor rejects a stale reference to a recycled IOSurface id. 0 == unversioned.
    pub generation: u32,
    pub logical_w: i32,
    pub logical_h: i32,
    pub geom_x: i32,
    pub geom_y: i32,
    pub attach_x: i32,
    pub attach_y: i32,
}

impl Default for Vao {
    fn default() -> Self {
        Vao { used: false, attrs: [Attr::default(); MAXATTR], elem_buf: 0 }
    }
}

/// The whole GL context state (single implicit context, as in gl_shim.c).
pub struct GlState {
    /// The GL error flag (`glGetError`). Holds the *first* error since the last query (GL semantics);
    /// [`set_gl_error`] never overwrites a pending error, and `glGetError` reads-and-clears it.
    pub error: u32,
    pub sh: Vec<Shader>,
    pub prog: Vec<Program>,
    pub buf: Vec<Buffer>,
    pub tex: Vec<Texture>,
    pub attr: [Attr; MAXATTR],
    pub vao: Vec<Vao>,
    pub cur_vao: u32,
    pub vao_seq: u32,
    /// Monotonic name allocators for objects the shim tracks by id only (no backing state), matching
    /// gl_shim.c's `g_samp_seq`/`g_query_seq`/`g_xfb_seq` (all start at 1).
    pub samp_seq: u32,
    pub query_seq: u32,
    pub xfb_seq: u32,
    /// Sampler-object state (real ES3 objects). `samp_reserved` holds names handed out by
    /// `glGenSamplers` that have not yet been instantiated (first bind / first param). `samplers`
    /// holds created objects. `samp_binding[unit]` is the sampler bound to each texture unit (0 = none).
    pub samp_reserved: std::collections::HashSet<u32>,
    pub samplers: std::collections::HashMap<u32, SamplerObj>,
    pub samp_binding: std::collections::HashMap<u32, u32>,
    /// Query-object state (real ES3 objects). `query_reserved` holds names from `glGenQueries` not yet
    /// used; `queries` holds created objects; `active_query[target]` is the query currently inside a
    /// begin/end pair for that target (occlusion / transform-feedback), 0 or absent = none active.
    pub query_reserved: std::collections::HashSet<u32>,
    pub queries: std::collections::HashMap<u32, QueryObj>,
    pub active_query: std::collections::HashMap<u32, u32>,
    /// Transform-feedback objects. `tfs` always holds the default object (key 0). `tf_reserved` holds
    /// names from `glGenTransformFeedbacks` not yet bound; `tf_bound` is the currently bound object.
    pub tf_reserved: std::collections::HashSet<u32>,
    pub tfs: std::collections::HashMap<u32, TransformFeedbackObj>,
    pub tf_bound: u32,
    /// Indexed buffer binding points, keyed by binding index, for the two ES3 indexed targets.
    pub ubo_bindings: std::collections::HashMap<u32, IndexedBinding>,
    pub tfbo_bindings: std::collections::HashMap<u32, IndexedBinding>,
    /// Per-program uniform-block table (position = block index) and transform-feedback varying capture
    /// list + buffer mode, keyed by program name.
    pub prog_uniform_blocks: std::collections::HashMap<u32, Vec<UniformBlock>>,
    pub prog_tf_varyings: std::collections::HashMap<u32, (Vec<String>, u32)>,

    pub tex_unit: [u32; 8], // texture bound per active unit (GL_TEXTURE_2D)
    pub active_unit: usize,
    pub unpack_alignment: i32,
    pub unpack_row_length: i32,
    pub unpack_skip_rows: i32,
    pub unpack_skip_pixels: i32,
    pub pack_alignment: i32,
    pub pack_row_length: i32,
    pub pack_skip_rows: i32,
    pub pack_skip_pixels: i32,

    pub cur_prog: u32,
    pub arr_buf: u32,
    pub elem_buf: u32,
    pub pack_buf: u32,
    pub draw_fbo: u32,
    pub read_fbo: u32,
    pub rbo_bound: u32,
    pub fbo: Vec<Fbo>,
    pub rbo: Vec<Rbo>,

    pub depth: bool,
    pub blend: bool,
    pub cull: bool,
    pub blend_src_rgb: u32,
    pub blend_dst_rgb: u32,
    pub blend_src_alpha: u32,
    pub blend_dst_alpha: u32,
    pub blend_eq_rgb: u32,
    pub blend_eq_alpha: u32,

    pub clear: [f32; 4],
    pub clear_serial: i32,
    pub viewport: [i32; 4],
    pub scissor_enabled: bool,
    pub scissor: [i32; 4],

    pub ubuf: [u8; UBUF_BYTES], // current uniform-block bytes

    // ---- draw-list + present state (the frame the swap path lowers to IR) ----
    pub draws: Vec<DrawCall>,
    pub surf: Surface,
    pub default_surface_valid: bool,
    pub default_full_clear_since_swap: bool,
    /// Draw-time snapshot of the attribute array (apps disable attribs before swapping; the swap uses
    /// the snapshot — gl_shim.c `g_attr_snap`).
    pub attr_snap: [Attr; MAXATTR],
    pub have_draw_snap: bool,
    // last glDrawArrays/Elements of the frame (single-draw path).
    pub draw_mode: i32,
    pub draw_first: i32,
    pub draw_count: i32,
    pub draw_indexed: bool,
    pub index_type: u32,
    pub index_offset: usize,
    // pending window geometry (from eglCreateWindowSurface / wl_egl_window), resolved at surface_up.
    pub pending_logical_w: i32,
    pub pending_logical_h: i32,
    pub pending_attach_x: i32,
    pub pending_attach_y: i32,
}

pub const MAXDRAWS: usize = 512;

impl Default for GlState {
    fn default() -> Self {
        GlState {
            error: GL_NO_ERROR,
            sh: vec![Shader::default(); MAXSH],
            prog: vec![Program::default(); MAXPROG],
            buf: vec![Buffer::default(); MAXBUF],
            tex: vec![Texture::default(); MAXTEX],
            attr: [Attr::default(); MAXATTR],
            vao: vec![Vao::default(); MAXVAO],
            cur_vao: 0,
            vao_seq: 1,
            samp_seq: 1,
            query_seq: 1,
            xfb_seq: 1,
            samp_reserved: std::collections::HashSet::new(),
            samplers: std::collections::HashMap::new(),
            samp_binding: std::collections::HashMap::new(),
            query_reserved: std::collections::HashSet::new(),
            queries: std::collections::HashMap::new(),
            active_query: std::collections::HashMap::new(),
            tf_reserved: std::collections::HashSet::new(),
            tfs: {
                let mut m = std::collections::HashMap::new();
                m.insert(0u32, TransformFeedbackObj::default()); // the default TF object always exists
                m
            },
            tf_bound: 0,
            ubo_bindings: std::collections::HashMap::new(),
            tfbo_bindings: std::collections::HashMap::new(),
            prog_uniform_blocks: std::collections::HashMap::new(),
            prog_tf_varyings: std::collections::HashMap::new(),
            tex_unit: [0; 8],
            active_unit: 0,
            unpack_alignment: 4,
            unpack_row_length: 0,
            unpack_skip_rows: 0,
            unpack_skip_pixels: 0,
            pack_alignment: 4,
            pack_row_length: 0,
            pack_skip_rows: 0,
            pack_skip_pixels: 0,
            cur_prog: 0,
            arr_buf: 0,
            elem_buf: 0,
            pack_buf: 0,
            draw_fbo: 0,
            read_fbo: 0,
            rbo_bound: 0,
            fbo: vec![Fbo::default(); MAXFBO],
            rbo: vec![Rbo::default(); MAXRBO],
            depth: false,
            blend: false,
            cull: false,
            blend_src_rgb: GL_ONE,
            blend_dst_rgb: GL_ZERO,
            blend_src_alpha: GL_ONE,
            blend_dst_alpha: GL_ZERO,
            blend_eq_rgb: GL_FUNC_ADD,
            blend_eq_alpha: GL_FUNC_ADD,
            clear: [0.0, 0.0, 0.0, 1.0],
            clear_serial: 0,
            viewport: [0; 4],
            scissor_enabled: false,
            scissor: [0; 4],
            ubuf: [0; UBUF_BYTES],
            draws: Vec::new(),
            surf: Surface::default(),
            default_surface_valid: false,
            default_full_clear_since_swap: false,
            attr_snap: [Attr::default(); MAXATTR],
            have_draw_snap: false,
            draw_mode: -1,
            draw_first: 0,
            draw_count: 0,
            draw_indexed: false,
            index_type: 0,
            index_offset: 0,
            pending_logical_w: 0,
            pending_logical_h: 0,
            pending_attach_x: 0,
            pending_attach_y: 0,
        }
    }
}

impl GlState {
    /// Store the live vertex-attribute array + element buffer into the current VAO (gl_shim.c
    /// `vao_store_current`). Called after any attrib/element-buffer mutation.
    pub fn vao_store_current(&mut self) {
        let v = self.cur_vao as usize;
        if v < MAXVAO {
            self.vao[v].used = true;
            self.vao[v].attrs = self.attr;
            self.vao[v].elem_buf = self.elem_buf;
        }
    }

    /// Allocate a VAO id from the monotonic cursor (gl_shim.c `glGenVertexArrays` + `g_vao_seq`).
    pub fn gen_vao(&mut self) -> u32 {
        for id in self.vao_seq..MAXVAO as u32 {
            if !self.vao[id as usize].used {
                self.vao[id as usize] = Vao { used: true, ..Default::default() };
                self.vao_seq = id + 1;
                return id;
            }
        }
        0
    }

    /// Load a VAO's attribute array + element buffer into the live state (gl_shim.c `vao_load`).
    pub fn vao_load(&mut self, vao: u32) {
        let v = vao as usize;
        if v < MAXVAO && self.vao[v].used {
            self.attr = self.vao[v].attrs;
            self.elem_buf = self.vao[v].elem_buf;
        } else {
            self.attr = [Attr::default(); MAXATTR];
            self.elem_buf = 0;
            if v < MAXVAO {
                self.vao[v].used = true;
            }
        }
    }

    /// Allocate the lowest free slot in `[1, max)` where `used(i)` is false; return 0 if none.
    fn alloc_slot(used: impl Fn(usize) -> bool, max: usize) -> u32 {
        (1..max).find(|&i| !used(i)).map(|i| i as u32).unwrap_or(0)
    }

    pub fn gen_buffer(&mut self) -> u32 {
        let id = Self::alloc_slot(|i| self.buf[i].reserved || self.buf[i].used, MAXBUF);
        if id != 0 {
            let b = &mut self.buf[id as usize];
            *b = Buffer { reserved: true, gen: b.gen + 1, ..Default::default() };
        }
        id
    }

    pub fn gen_texture(&mut self) -> u32 {
        let id = Self::alloc_slot(|i| self.tex[i].reserved || self.tex[i].used, MAXTEX);
        if id != 0 {
            let g = self.tex[id as usize].gen;
            self.tex[id as usize] = Texture {
                reserved: true,
                minf: GL_LINEAR,
                magf: GL_LINEAR,
                ws: GL_REPEAT,
                wt: GL_REPEAT,
                gen: g + 1,
                ..Default::default()
            };
        }
        id
    }

    pub fn gen_shader(&mut self, kind: u32) -> u32 {
        let id = Self::alloc_slot(|i| self.sh[i].used, MAXSH);
        if id != 0 {
            self.sh[id as usize] = Shader { used: true, kind, src: None, ..Default::default() };
        }
        id
    }

    pub fn create_program(&mut self) -> u32 {
        let id = Self::alloc_slot(|i| self.prog[i].used, MAXPROG);
        if id != 0 {
            self.prog[id as usize] = Program { used: true, ..Default::default() };
        }
        id
    }

    /// The active texture-unit's bound GL_TEXTURE_2D object (the target of TexImage2D/TexParameter).
    pub fn bound_tex(&self) -> u32 {
        self.tex_unit[self.active_unit]
    }

    /// Render-target width for a draw target: the texture's width if `tex` is a live texture, else the
    /// default window surface width (gl_shim.c `draw_target_w`). NOTE: offscreen FBO targets are not
    /// yet ported (no `glBindFramebuffer`), so `draw_fbo` stays 0 and draws hit the default surface —
    /// the es2*/glmark2 default-framebuffer case. Chrome's offscreen-FBO path lands later.
    pub fn draw_target_w(&self, tex: u32) -> i32 {
        if tex != 0 && (tex as usize) < MAXTEX && self.tex[tex as usize].used {
            self.tex[tex as usize].w
        } else {
            self.surf.width as i32
        }
    }
    pub fn draw_target_h(&self, tex: u32) -> i32 {
        if tex != 0 && (tex as usize) < MAXTEX && self.tex[tex as usize].used {
            self.tex[tex as usize].h
        } else {
            self.surf.height as i32
        }
    }

    /// The render target of the bound draw FBO: its color texture, or 0 for the default window surface
    /// (gl_shim.c: `draw_fbo>0 && fbo[draw_fbo].used ? fbo.color_tex : 0`).
    pub fn draw_fbo_target(&self) -> u32 {
        let f = self.draw_fbo as usize;
        if self.draw_fbo > 0 && f < MAXFBO && self.fbo[f].used {
            self.fbo[f].color_tex
        } else {
            0
        }
    }

    /// Compute completeness for the color-only framebuffer subset this shim can render. The default
    /// framebuffer is managed by EGL and is complete here. User FBOs require one defined, live,
    /// color-renderable texture or renderbuffer attachment.
    pub fn framebuffer_status(&self, fbo: u32) -> u32 {
        if fbo == 0 {
            return GL_FRAMEBUFFER_COMPLETE;
        }
        let f = fbo as usize;
        if f >= MAXFBO || !self.fbo[f].used {
            return GL_FRAMEBUFFER_INCOMPLETE_MISSING_ATTACHMENT;
        }
        let fb = &self.fbo[f];
        // --- color attachment: unchanged behavior (byte-parity), but also yields the reference
        //     dimensions the depth/stencil attachments must agree with. ---
        let color_dims: Option<(i32, i32)> = if fb.color_tex != 0 {
            let t = fb.color_tex as usize;
            if t < MAXTEX && self.tex[t].used && fb.color_level == 0 && self.tex[t].w > 0 && self.tex[t].h > 0 {
                Some((self.tex[t].w, self.tex[t].h))
            } else {
                return GL_FRAMEBUFFER_INCOMPLETE_ATTACHMENT;
            }
        } else if fb.color_rbo != 0 {
            let r = fb.color_rbo as usize;
            let color_renderable = r < MAXRBO
                && self.rbo[r].used
                && self.rbo[r].w > 0
                && self.rbo[r].h > 0
                && !matches!(self.rbo[r].ifmt, 0x81A5 | 0x81A6 | 0x81A7 | 0x8D48 | 0x88F0);
            if color_renderable {
                Some((self.rbo[r].w, self.rbo[r].h))
            } else {
                return GL_FRAMEBUFFER_INCOMPLETE_ATTACHMENT;
            }
        } else {
            None
        };

        // --- depth / stencil attachments: each present one must be a live renderbuffer whose format
        //     actually supplies the required aspect, else the attachment is incomplete. ---
        let ds_dims = |rbo: u32, need_depth: bool, need_stencil: bool| -> Result<Option<(i32, i32)>, ()> {
            if rbo == 0 {
                return Ok(None);
            }
            let r = rbo as usize;
            if r >= MAXRBO || !self.rbo[r].used || self.rbo[r].w <= 0 || self.rbo[r].h <= 0 {
                return Err(());
            }
            let ifmt = self.rbo[r].ifmt;
            let has_depth = matches!(ifmt, 0x81A5 | 0x81A6 | 0x81A7 | 0x88F0 | 0x8CAC | 0x8CAD);
            let has_stencil = matches!(ifmt, 0x8D48 | 0x88F0 | 0x8CAD);
            if (need_depth && !has_depth) || (need_stencil && !has_stencil) {
                return Err(());
            }
            Ok(Some((self.rbo[r].w, self.rbo[r].h)))
        };
        let depth_dims = match ds_dims(fb.depth_rbo, true, false) {
            Ok(d) => d,
            Err(()) => return GL_FRAMEBUFFER_INCOMPLETE_ATTACHMENT,
        };
        let stencil_dims = match ds_dims(fb.stencil_rbo, false, true) {
            Ok(d) => d,
            Err(()) => return GL_FRAMEBUFFER_INCOMPLETE_ATTACHMENT,
        };

        // At least one attachment must exist.
        let dims = [color_dims, depth_dims, stencil_dims];
        if dims.iter().all(|d| d.is_none()) {
            return GL_FRAMEBUFFER_INCOMPLETE_MISSING_ATTACHMENT;
        }
        // Every present attachment must share the same dimensions (ES2 completeness rule).
        let first = dims.iter().flatten().next().copied();
        if dims.iter().flatten().any(|&d| Some(d) != first) {
            return GL_FRAMEBUFFER_INCOMPLETE_DIMENSIONS;
        }
        GL_FRAMEBUFFER_COMPLETE
    }

    /// An FBO's color texture *with data* (gl_shim.c `fbo_color_texture`) — for readback/blit.
    pub fn fbo_color_texture(&self, fbo: u32) -> u32 {
        let f = fbo as usize;
        if fbo == 0 || f >= MAXFBO || !self.fbo[f].used {
            return 0;
        }
        let tex = self.fbo[f].color_tex as usize;
        if tex >= MAXTEX || !self.tex[tex].used || self.tex[tex].data.is_empty() {
            return 0;
        }
        self.fbo[f].color_tex
    }

    /// CPU-side clear of the bound FBO's color texture over the scissor rect (gl_shim.c
    /// `clear_bound_color_texture`) — keeps the texture's uploaded data (and thus its IR staging) in
    /// sync with `glClear` when an offscreen FBO is bound. No-op for the default framebuffer.
    pub fn clear_bound_color_texture(&mut self, color: [f32; 4]) {
        let f = self.draw_fbo as usize;
        if !(self.draw_fbo > 0 && f < MAXFBO && self.fbo[f].used) {
            return;
        }
        let t = self.fbo[f].color_tex as usize;
        if t >= MAXTEX || !self.tex[t].used || self.tex[t].data.is_empty() {
            return;
        }
        let px = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        let (r, g, b, a) = (px(color[0]), px(color[1]), px(color[2]), px(color[3]));
        let (x, y, w, h, _) = self.clear_scissor_rect();
        let tw = self.tex[t].w as usize;
        let data = &mut self.tex[t].data;
        for yy in y..y + h {
            for xx in x..x + w {
                let di = (yy as usize * tw + xx as usize) * 4;
                if di + 4 <= data.len() {
                    data[di] = r;
                    data[di + 1] = g;
                    data[di + 2] = b;
                    data[di + 3] = a;
                }
            }
        }
        self.tex[t].gen += 1;
    }

    /// Allocate zeroed RGBA8 storage for a live texture (gl_shim.c `tex_alloc_rgba`) — returns whether
    /// storage was (re)allocated. Used by immutable storage (`glTexStorage*`), 3D/array upload
    /// (`glTexImage3D`), and copy-from-framebuffer (`glCopyTexImage2D`).
    pub fn tex_alloc_rgba(&mut self, id: u32, w: i32, h: i32) -> bool {
        let i = id as usize;
        if id == 0 || i >= MAXTEX || !self.tex[i].used || w <= 0 || h <= 0 {
            return false;
        }
        let sz = (w as usize) * (h as usize) * 4;
        let t = &mut self.tex[i];
        t.data = vec![0u8; sz];
        t.w = w;
        t.h = h;
        t.gen += 1;
        true
    }

    /// CPU-side textured rect blit (gl_shim.c `copy_texture_rect`) — for `glBlitFramebuffer`.
    pub fn copy_texture_rect(&mut self, src_id: u32, dst_id: u32, sx0: i32, sy0: i32, sx1: i32, sy1: i32, dx0: i32, dy0: i32, dx1: i32, dy1: i32) {
        let (si, dic) = (src_id as usize, dst_id as usize);
        if si >= MAXTEX || dic >= MAXTEX || !self.tex[si].used || !self.tex[dic].used || self.tex[si].data.is_empty() || self.tex[dic].data.is_empty() {
            return;
        }
        let (dw, dh) = (dx1 - dx0, dy1 - dy0);
        if dw == 0 || dh == 0 || sx0 == sx1 || sy0 == sy1 {
            return;
        }
        let (adw, adh) = (dw.abs(), dh.abs());
        let (sw, sh) = (self.tex[si].w, self.tex[si].h);
        let (dtw, dth) = (self.tex[dic].w, self.tex[dic].h);
        let clampi = |v: i32, lo: i32, hi: i32| v.clamp(lo, hi);
        for j in 0..adh {
            let dy = dy0 + if dh < 0 { -j } else { j };
            if dy < 0 || dy >= dth {
                continue;
            }
            let sy = clampi(sy0 + ((j as i64 * (sy1 - sy0) as i64) / adh as i64) as i32, 0, sh - 1);
            for i in 0..adw {
                let dx = dx0 + if dw < 0 { -i } else { i };
                if dx < 0 || dx >= dtw {
                    continue;
                }
                let sx = clampi(sx0 + ((i as i64 * (sx1 - sx0) as i64) / adw as i64) as i32, 0, sw - 1);
                let sp = (sy as usize * sw as usize + sx as usize) * 4;
                let dp = (dy as usize * dtw as usize + dx as usize) * 4;
                let (a, b) = (self.tex[si].data[sp..sp + 4].to_vec(), dp);
                self.tex[dic].data[b..b + 4].copy_from_slice(&a);
            }
        }
        self.tex[dic].gen += 1;
    }

    /// Resolve the clear rectangle honoring GL_SCISSOR_TEST (gl_shim.c `clear_scissor_rect`); returns
    /// `(x, y, w, h, scissored)` where `scissored` is true iff the scissor actually sub-rects the
    /// full target (a Metal load-clear is full-target, so a real sub-rect must become a ClearRect).
    pub fn clear_scissor_rect(&self) -> (i32, i32, i32, i32, bool) {
        let target = self.draw_fbo_target();
        let (tw, th) = (self.draw_target_w(target), self.draw_target_h(target));
        let (mut x, mut y, mut w, mut h) = (0, 0, tw, th);
        if self.scissor_enabled && self.scissor[2] > 0 && self.scissor[3] > 0 {
            x = self.scissor[0];
            y = self.scissor[1];
            w = self.scissor[2];
            h = self.scissor[3];
        }
        if x < 0 {
            w += x;
            x = 0;
        }
        if y < 0 {
            h += y;
            y = 0;
        }
        if x > tw {
            x = tw;
        }
        if y > th {
            y = th;
        }
        if x + w > tw {
            w = tw - x;
        }
        if y + h > th {
            h = th - y;
        }
        w = w.max(0);
        h = h.max(0);
        let scissored = self.scissor_enabled && (x != 0 || y != 0 || w != tw || h != th);
        (x, y, w, h, scissored)
    }

    /// Record a clear into the frame draw-list (gl_shim.c `record_clear_call`).
    pub fn record_clear_call(&mut self, x: i32, y: i32, w: i32, h: i32) {
        if self.draws.len() >= MAXDRAWS {
            return;
        }
        let target_tex = self.draw_fbo_target();
        self.draws.push(DrawCall {
            is_clear: true,
            target_tex,
            clear_rect: [x, y, w, h],
            clear: self.clear,
            clear_serial: self.clear_serial,
            ..Default::default()
        });
    }

    /// Record a draw into the frame draw-list, snapshotting the state it renders with (gl_shim.c
    /// `record_draw_call`). Vertex-buffer snapshots (the replay path) are omitted — the single-draw
    /// path reads live buffers at swap.
    pub fn record_draw_call(&mut self, mode: u32, first: i32, count: i32, indexed: bool, index_type: u32, indices: usize) {
        if self.draws.len() >= MAXDRAWS {
            return;
        }
        let samp_units = if (self.cur_prog as usize) < MAXPROG && self.prog[self.cur_prog as usize].used {
            self.prog[self.cur_prog as usize].samp_units
        } else {
            [0; 4]
        };
        let ubuf = if (self.cur_prog as usize) < MAXPROG && self.prog[self.cur_prog as usize].used {
            self.prog[self.cur_prog as usize].ubuf
        } else {
            self.ubuf
        };
        // Snapshot the buffers this draw renders from (gl_shim.c snapshot_draw_buffers), deduped by src.
        let mut snap_vbo: Vec<BufSnap> = Vec::new();
        for a in self.attr.iter() {
            if !a.enabled {
                continue;
            }
            let src = a.buffer as usize;
            if a.buffer == 0 || src >= MAXBUF || !self.buf[src].used || self.buf[src].data.is_empty() {
                continue;
            }
            if snap_vbo.iter().any(|s| s.src == a.buffer) || snap_vbo.len() >= MAXATTR {
                continue;
            }
            snap_vbo.push(BufSnap { src: a.buffer, gen: self.buf[src].gen, data: self.buf[src].data.clone() });
        }
        let snap_ibo = if indexed && self.elem_buf > 0 && (self.elem_buf as usize) < MAXBUF {
            let eb = self.elem_buf as usize;
            if self.buf[eb].used && !self.buf[eb].data.is_empty() {
                Some(BufSnap { src: self.elem_buf, gen: self.buf[eb].gen, data: self.buf[eb].data.clone() })
            } else {
                None
            }
        } else {
            None
        };
        let target_tex = self.draw_fbo_target();
        self.draws.push(DrawCall {
            is_clear: false,
            mode,
            first,
            count,
            indexed,
            index_type,
            index_offset: indices,
            prog: self.cur_prog,
            elem_buf: self.elem_buf,
            target_tex,
            clear_rect: [0; 4],
            attrs: self.attr,
            tex_units: self.tex_unit,
            samp_units,
            viewport: self.viewport,
            scissor_enabled: self.scissor_enabled,
            scissor: self.scissor,
            blend: self.blend,
            blend_src_rgb: self.blend_src_rgb,
            blend_dst_rgb: self.blend_dst_rgb,
            blend_src_alpha: self.blend_src_alpha,
            blend_dst_alpha: self.blend_dst_alpha,
            blend_eq_rgb: self.blend_eq_rgb,
            blend_eq_alpha: self.blend_eq_alpha,
            clear: self.clear,
            clear_serial: self.clear_serial,
            ubuf,
            snap_vbo,
            snap_ibo,
        });
    }

    /// Element size in bytes of a vertex-attribute component type (gl_shim.c `attr_elem_size`).
    pub fn attr_elem_size(kind: u32) -> usize {
        match kind {
            GL_FLOAT | GL_UNSIGNED_INT | GL_INT => 4,
            GL_UNSIGNED_SHORT | GL_SHORT => 2,
            _ => 1,
        }
    }

    /// Index into a draw's VBO snapshots for source buffer `src`, or None (gl_shim.c
    /// `draw_vbo_snapshot_index`).
    pub fn draw_vbo_snapshot_index(d: &DrawCall, src: u32) -> Option<usize> {
        if src == 0 {
            return None;
        }
        d.snap_vbo.iter().position(|s| s.src == src && !s.data.is_empty())
    }

    /// Group a draw's enabled attributes by source VBO into slots (gl_shim.c `draw_vbo_slots`).
    /// Returns `(slot_vbo, attr_slot, slot_stride)`. A buffer with neither a snapshot nor a live copy
    /// is skipped; an all-empty stride defaults to 16.
    pub fn draw_vbo_slots(&self, d: &DrawCall) -> (Vec<u32>, [i32; MAXATTR], Vec<u32>) {
        let mut slot_vbo: Vec<u32> = Vec::new();
        let mut attr_slot = [-1i32; MAXATTR];
        for i in 0..MAXATTR {
            let a = &d.attrs[i];
            if !a.enabled || a.buffer == 0 {
                continue;
            }
            let b = a.buffer as usize;
            let has_live = b < MAXBUF && self.buf[b].used && !self.buf[b].data.is_empty();
            if Self::draw_vbo_snapshot_index(d, a.buffer).is_none() && !has_live {
                continue;
            }
            let sl = slot_vbo.iter().position(|&x| x == a.buffer).unwrap_or_else(|| {
                slot_vbo.push(a.buffer);
                slot_vbo.len() - 1
            });
            attr_slot[i] = sl as i32;
        }
        let mut slot_stride = vec![0u32; slot_vbo.len()];
        for i in 0..MAXATTR {
            let sl = attr_slot[i];
            if sl < 0 {
                continue;
            }
            let a = &d.attrs[i];
            let mut st = a.stride as u32;
            if st == 0 {
                st = a.size as u32 * Self::attr_elem_size(a.kind) as u32;
            }
            if st > slot_stride[sl as usize] {
                slot_stride[sl as usize] = st;
            }
        }
        for st in slot_stride.iter_mut() {
            if *st == 0 {
                *st = 16;
            }
        }
        (slot_vbo, attr_slot, slot_stride)
    }

    /// Resolve the compositor-facing geometry (logical size + centering offset) from the pending
    /// window size and env overrides (gl_shim.c `resolve_surface_geometry`).
    fn resolve_geometry(&self, bw: u32, bh: u32) -> (i32, i32, i32, i32) {
        let (mut lw, mut lh, mut source) = (self.pending_logical_w, self.pending_logical_h, 1);
        if lw <= 0 || lh <= 0 || lw > bw as i32 || lh > bh as i32 {
            lw = bw as i32;
            lh = bh as i32;
            source = 0;
        }
        // env override (DD_SHIM_LOGICAL_SIZE / CHROME_WINDOW_SIZE), only when logical == backing.
        if lw == bw as i32 && lh == bh as i32 {
            if let Some((fw, fh)) = env_logical_size() {
                if fw > 0 && fh > 0 && fw <= bw as i32 && fh <= bh as i32 {
                    lw = fw;
                    lh = fh;
                    source = 2;
                }
            }
        }
        let (mut gx, mut gy) = (0, 0);
        if source == 2 && lw < bw as i32 {
            gx = (bw as i32 - lw) / 2;
        }
        if source == 2 && lh < bh as i32 {
            gy = (bh as i32 - lh) / 2;
        }
        (lw, lh, gx, gy)
    }

    /// Bring up the presented surface (gl_shim.c `surface_up`). Sets the surface dimensions + resolved
    /// geometry and default viewport/scissor. The renderD128 alloc + wayland handshake (deployed path)
    /// and the DD_IR_DUMP id=1 shortcut are driven by `eglCreateWindowSurface`.
    pub fn surface_up(&mut self, w: u32, h: u32) {
        self.default_surface_valid = false;
        self.default_full_clear_since_swap = false;
        let (lw, lh, gx, gy) = self.resolve_geometry(w, h);
        if self.viewport[2] <= 0 || self.viewport[3] <= 0 {
            self.viewport = [0, 0, w as i32, h as i32];
        }
        if self.scissor[2] <= 0 || self.scissor[3] <= 0 {
            self.scissor = [0, 0, w as i32, h as i32];
        }
        self.surf = Surface {
            have: true,
            id: 1,
            width: w,
            height: h,
            stride: 0,
            fd: -1,
            generation: 0, // host-tool / DD_IR_DUMP path (no engine alloc) → unversioned
            logical_w: lw,
            logical_h: lh,
            geom_x: gx,
            geom_y: gy,
            attach_x: self.pending_attach_x,
            attach_y: self.pending_attach_y,
        };
    }

    /// Bytes-per-pixel of an upload format (gl_shim.c `tex_bpp`).
    pub fn tex_bpp(fmt: u32) -> usize {
        if fmt == GL_RGBA || fmt == GL_BGRA_EXT {
            4
        } else if fmt == GL_RGB {
            3
        } else {
            1
        }
    }

    /// Store `pixels` (in upload `fmt`) into texture `id`'s RGBA8 backing at (xo,yo)-(w,h), honoring
    /// the unpack alignment / row-length / skip state. Faithful port of gl_shim.c `tex_store_pixels`
    /// (minus the DD_PREMULTIPLY_UPLOAD debug knob). `pixels == None` clears to (0,0,0,255)-shaped
    /// default per the C code (r=g=b=0, a=255).
    pub fn tex_store_pixels(&mut self, id: u32, xo: i32, yo: i32, w: i32, h: i32, fmt: u32, pixels: Option<&[u8]>) {
        let (tw, th) = {
            let t = &self.tex[id as usize];
            (t.w, t.h)
        };
        if self.tex[id as usize].data.is_empty() || w <= 0 || h <= 0 {
            return;
        }
        if xo < 0 || yo < 0 || xo + w > tw || yo + h > th {
            return;
        }
        let bpp = Self::tex_bpp(fmt);
        let row_pixels = if self.unpack_row_length > 0 { self.unpack_row_length as usize } else { w as usize };
        let mut row_bytes = row_pixels * bpp;
        if self.unpack_alignment > 1 {
            let a = self.unpack_alignment as usize;
            row_bytes = (row_bytes + a - 1) & !(a - 1);
        }
        let skip_rows = self.unpack_skip_rows as usize;
        let skip_pixels = self.unpack_skip_pixels as usize;
        let tw_us = tw as usize;
        for y in 0..h as usize {
            for x in 0..w as usize {
                let (mut r, mut g, mut b, mut a) = (0u8, 0u8, 0u8, 255u8);
                if let Some(p) = pixels {
                    let base = (y + skip_rows) * row_bytes + skip_pixels * bpp + x * bpp;
                    if base + bpp <= p.len() {
                        let s = &p[base..];
                        match fmt {
                            GL_RGBA => {
                                r = s[0];
                                g = s[1];
                                b = s[2];
                                a = s[3];
                            }
                            GL_BGRA_EXT => {
                                b = s[0];
                                g = s[1];
                                r = s[2];
                                a = s[3];
                            }
                            GL_RGB => {
                                r = s[0];
                                g = s[1];
                                b = s[2];
                            }
                            GL_RED => r = s[0],
                            GL_ALPHA => a = s[0],
                            GL_LUMINANCE => {
                                r = s[0];
                                g = s[0];
                                b = s[0];
                            }
                            _ => {
                                r = s[0];
                                g = s[0];
                                b = s[0];
                            }
                        }
                    }
                }
                let di = ((yo as usize + y) * tw_us + (xo as usize + x)) * 4;
                let data = &mut self.tex[id as usize].data;
                data[di] = r;
                data[di + 1] = g;
                data[di + 2] = b;
                data[di + 3] = a;
            }
        }
    }
}

/// The small EGL-side state that is genuinely process-global (the single implicit window surface's
/// logical size). Per-context version + per-thread error/current live in the context model below.
pub struct EglState {
    pub surface_logical_w: i32,
    pub surface_logical_h: i32,
}

impl Default for EglState {
    fn default() -> Self {
        EglState { surface_logical_w: 0, surface_logical_h: 0 }
    }
}

// ===================================================================================================
// Typed EGL context / share-group model (audit §9.3)
//
// `eglCreateContext` returns a UNIQUE handle (the address of a heap `EglCtx`). Each context references a
// *share group* — a `GlState` (the GL object namespace + bindings) shared by every context created with
// that group as `share_context`. Unrelated contexts get independent groups, so their objects never
// alias. The CURRENT context is per-thread (`CURRENT_CTX`), so two threads can be current on different
// contexts concurrently; `gl()` resolves to the calling thread's context's group.
//
// To keep every `gl()` caller returning a `'static` guard, each group's `Mutex<GlState>` is leaked
// (groups are few and live for the process). The very first standalone context reuses the process
// DEFAULT group, so a single-context app (and the byte-parity harness) behaves exactly as before.
// ===================================================================================================

/// One EGL context: a unique handle whose object namespace is its `group`.
pub struct EglCtx {
    pub group: &'static Mutex<GlState>,
    pub major: i32,
    pub minor: i32,
}

struct Registry {
    live: std::collections::HashSet<usize>,
    default_claimed: bool,
}

fn registry() -> &'static Mutex<Registry> {
    static R: OnceLock<Mutex<Registry>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(Registry { live: std::collections::HashSet::new(), default_claimed: false }))
}

/// The process DEFAULT share group — the single global `GlState` the shim used before the context
/// model, reused by the first standalone context and by any `gl()` call made with no current context
/// (existing unit tests, the resource lowering path).
fn default_group() -> &'static Mutex<GlState> {
    static S: OnceLock<Mutex<GlState>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(GlState::default()))
}

fn new_group() -> &'static Mutex<GlState> {
    Box::leak(Box::new(Mutex::new(GlState::default())))
}

thread_local! {
    /// The calling thread's current `EglCtx` (null == EGL_NO_CONTEXT).
    static CURRENT_CTX: std::cell::Cell<*mut EglCtx> = const { std::cell::Cell::new(core::ptr::null_mut()) };
    /// The calling thread's bound draw/read surfaces (from `eglMakeCurrent`).
    static CURRENT_DRAW: std::cell::Cell<*mut c_void> = const { std::cell::Cell::new(core::ptr::null_mut()) };
    static CURRENT_READ: std::cell::Cell<*mut c_void> = const { std::cell::Cell::new(core::ptr::null_mut()) };
    /// The calling thread's EGL error (first-error retention, cleared on `eglGetError`).
    static EGL_ERROR: std::cell::Cell<i32> = const { std::cell::Cell::new(0x3000 /* EGL_SUCCESS */) };
}

/// The share group backing GL calls on this thread: the current context's group, or the default group
/// when no context is current.
fn current_group() -> &'static Mutex<GlState> {
    let c = CURRENT_CTX.with(|c| c.get());
    if c.is_null() {
        default_group()
    } else {
        unsafe { (*c).group }
    }
}

/// Create a context. `share` (if a live context handle) joins that context's share group; otherwise a
/// fresh group is allocated — except the very first standalone context, which reuses the default group.
pub fn egl_create_context(share: *mut c_void, major: i32, minor: i32) -> *mut c_void {
    let mut reg = registry().lock().unwrap_or_else(|e| e.into_inner());
    let group: &'static Mutex<GlState> = if !share.is_null() && reg.live.contains(&(share as usize)) {
        unsafe { (*(share as *mut EglCtx)).group }
    } else if !reg.default_claimed {
        reg.default_claimed = true;
        default_group()
    } else {
        new_group()
    };
    let ctx = Box::into_raw(Box::new(EglCtx { group, major, minor }));
    reg.live.insert(ctx as usize);
    ctx as *mut c_void
}

/// Whether `ctx` is a live context handle.
pub fn egl_ctx_is_live(ctx: *mut c_void) -> bool {
    registry().lock().unwrap_or_else(|e| e.into_inner()).live.contains(&(ctx as usize))
}

/// Bind (or unbind) the calling thread's current context and its draw/read surfaces (null ctx ==
/// unbind, which also clears the surfaces). Returns false only for a non-live, non-null handle.
pub fn egl_make_current(ctx: *mut c_void, draw: *mut c_void, read: *mut c_void) -> bool {
    if ctx.is_null() {
        CURRENT_CTX.with(|c| c.set(core::ptr::null_mut()));
        CURRENT_DRAW.with(|c| c.set(core::ptr::null_mut()));
        CURRENT_READ.with(|c| c.set(core::ptr::null_mut()));
        return true;
    }
    if !egl_ctx_is_live(ctx) {
        return false;
    }
    CURRENT_CTX.with(|c| c.set(ctx as *mut EglCtx));
    CURRENT_DRAW.with(|c| c.set(draw));
    CURRENT_READ.with(|c| c.set(read));
    true
}

/// The calling thread's current context handle (null == EGL_NO_CONTEXT).
pub fn egl_current_context() -> *mut c_void {
    CURRENT_CTX.with(|c| c.get()) as *mut c_void
}

/// The calling thread's current draw / read surface handle (null when no context is current).
pub fn egl_current_draw_surface() -> *mut c_void {
    CURRENT_DRAW.with(|c| c.get())
}
pub fn egl_current_read_surface() -> *mut c_void {
    CURRENT_READ.with(|c| c.get())
}

/// Client major version of a context handle (or the current context if `ctx` is null), defaulting to 2.
pub fn egl_ctx_major(ctx: *mut c_void) -> i32 {
    let h = if ctx.is_null() { egl_current_context() } else { ctx };
    if !h.is_null() && egl_ctx_is_live(h) {
        unsafe { (*(h as *mut EglCtx)).major }
    } else {
        2
    }
}

/// Client minor version of a context handle (or the current context if `ctx` is null), defaulting to 0.
pub fn egl_ctx_minor(ctx: *mut c_void) -> i32 {
    let h = if ctx.is_null() { egl_current_context() } else { ctx };
    if !h.is_null() && egl_ctx_is_live(h) {
        unsafe { (*(h as *mut EglCtx)).minor }
    } else {
        0
    }
}

/// Destroy a context: drop it from the live set (unbinding this thread if it was current) and free it.
/// The share group is intentionally retained (leaked) since sibling contexts may still reference it.
pub fn egl_destroy_context(ctx: *mut c_void) -> bool {
    if ctx.is_null() {
        return false;
    }
    let removed = registry().lock().unwrap_or_else(|e| e.into_inner()).live.remove(&(ctx as usize));
    if !removed {
        return false;
    }
    if egl_current_context() == ctx {
        CURRENT_CTX.with(|c| c.set(core::ptr::null_mut()));
        CURRENT_DRAW.with(|c| c.set(core::ptr::null_mut()));
        CURRENT_READ.with(|c| c.set(core::ptr::null_mut()));
    }
    unsafe { drop(Box::from_raw(ctx as *mut EglCtx)) };
    true
}

/// Raise the calling thread's EGL error, honoring first-error retention (the first error since the last
/// `eglGetError` is kept; later ones are dropped until the flag is read).
pub fn egl_set_error(err: i32) {
    EGL_ERROR.with(|c| {
        if c.get() == EGL_SUCCESS {
            c.set(err);
        }
    });
}

/// Read-and-clear the calling thread's EGL error (`eglGetError`).
pub fn egl_take_error() -> i32 {
    EGL_ERROR.with(|c| {
        let e = c.get();
        c.set(EGL_SUCCESS);
        e
    })
}

// ===================================================================================================
// Typed EGL surface arena (audit §11: distinct lifetimes / dimensions / types)
//
// `eglCreate{Window,Pbuffer}Surface` allocate a generation-checked handle into an arena (not one
// immortal singleton). A handle encodes (slot index, generation); destroying a surface bumps its slot
// generation so the OLD handle no longer validates — a stale or forged handle resolves to `None`, which
// the EGL entry points report as EGL_BAD_SURFACE. Each entry carries its real type (window/pbuffer) and
// dimensions, so `eglQuerySurface` returns per-surface size.
// ===================================================================================================

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SurfaceKind {
    Window,
    Pbuffer,
}

struct SurfaceEntry {
    alive: bool,
    generation: u32,
    kind: SurfaceKind,
    width: i32,
    height: i32,
}

fn surface_arena() -> &'static Mutex<Vec<SurfaceEntry>> {
    static A: OnceLock<Mutex<Vec<SurfaceEntry>>> = OnceLock::new();
    A.get_or_init(|| Mutex::new(Vec::new()))
}

// A handle packs (slot+1) in the high bits and the generation in the low 20 bits, so it is always
// non-null and a destroyed handle (stale generation) fails validation.
const SURF_GEN_BITS: u32 = 20;
const SURF_GEN_MASK: usize = (1 << SURF_GEN_BITS) - 1;

fn encode_surface(slot: usize, generation: u32) -> *mut c_void {
    (((slot + 1) << SURF_GEN_BITS) | (generation as usize & SURF_GEN_MASK)) as *mut c_void
}
fn decode_surface(h: *mut c_void) -> Option<(usize, u32)> {
    let v = h as usize;
    if v == 0 {
        return None;
    }
    let slot = (v >> SURF_GEN_BITS).checked_sub(1)?;
    Some((slot, (v & SURF_GEN_MASK) as u32))
}

/// Allocate a typed surface, returning its unique generation-checked handle.
pub fn egl_create_surface(kind: SurfaceKind, width: i32, height: i32) -> *mut c_void {
    let mut arena = surface_arena().lock().unwrap_or_else(|e| e.into_inner());
    // Reuse a dead slot (bumping its generation) before growing the arena.
    if let Some(slot) = arena.iter().position(|e| !e.alive) {
        let e = &mut arena[slot];
        e.alive = true;
        e.generation = e.generation.wrapping_add(1) & SURF_GEN_MASK as u32;
        e.kind = kind;
        e.width = width;
        e.height = height;
        return encode_surface(slot, e.generation);
    }
    let slot = arena.len();
    arena.push(SurfaceEntry { alive: true, generation: 1, kind, width, height });
    encode_surface(slot, 1)
}

/// Resolve a live surface handle to `(kind, width, height)`, or `None` if stale/forged.
pub fn egl_surface_lookup(h: *mut c_void) -> Option<(SurfaceKind, i32, i32)> {
    let (slot, generation) = decode_surface(h)?;
    let arena = surface_arena().lock().unwrap_or_else(|e| e.into_inner());
    let e = arena.get(slot)?;
    if e.alive && e.generation == generation {
        Some((e.kind, e.width, e.height))
    } else {
        None
    }
}

/// Destroy a live surface (bumping its slot generation so the handle can never validate again).
/// Returns false for a stale/forged handle.
pub fn egl_destroy_surface(h: *mut c_void) -> bool {
    let Some((slot, generation)) = decode_surface(h) else { return false };
    let mut arena = surface_arena().lock().unwrap_or_else(|e| e.into_inner());
    match arena.get_mut(slot) {
        Some(e) if e.alive && e.generation == generation => {
            e.alive = false;
            e.generation = e.generation.wrapping_add(1) & SURF_GEN_MASK as u32;
            true
        }
        _ => false,
    }
}

fn egl_cell() -> &'static Mutex<EglState> {
    static S: OnceLock<Mutex<EglState>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(EglState::default()))
}

/// Lock the calling thread's current GL share-group state (see [`current_group`]). A poisoned lock
/// (only possible after a panic, which aborts at the FFI boundary anyway) is unreachable in practice.
pub fn gl() -> MutexGuard<'static, GlState> {
    current_group().lock().unwrap_or_else(|e| e.into_inner())
}

pub fn egl() -> MutexGuard<'static, EglState> {
    egl_cell().lock().unwrap_or_else(|e| e.into_inner())
}

/// Raise the GL error flag, honoring GL semantics: the flag records the *first* error since the last
/// `glGetError`; a subsequent error is dropped until the flag is read and cleared. Used by
/// `glGetError` and by the generated truthful-failure stubs (`crate::stub::fail_gl`).
pub fn set_gl_error(err: u32) {
    let mut s = gl();
    if s.error == GL_NO_ERROR {
        s.error = err;
    }
}

/// Read-and-clear the GL error flag (`glGetError`).
pub fn take_gl_error() -> u32 {
    let mut s = gl();
    let e = s.error;
    s.error = GL_NO_ERROR;
    e
}

/// Whether the shim advertises ES3 (env `DD_SHIM_ES3`), matching gl_shim.c `shim_es3()`.
pub fn shim_es3() -> bool {
    std::env::var_os("DD_SHIM_ES3").is_some()
}

/// Parse a `WxH` / `W,H` size (gl_shim.c `parse_size_pair`): both in (0, 8192].
fn parse_size_pair(s: &str) -> Option<(i32, i32)> {
    let sep = s.find([',', 'x', 'X'])?;
    let w: i64 = s[..sep].parse().ok()?;
    let h: i64 = s[sep + 1..].trim().parse().ok()?;
    if w > 0 && h > 0 && w <= 8192 && h <= 8192 {
        Some((w as i32, h as i32))
    } else {
        None
    }
}

/// The logical window size from env (gl_shim.c `env_logical_size`): `DD_SHIM_LOGICAL_SIZE`, else
/// `CHROME_WINDOW_SIZE`.
fn env_logical_size() -> Option<(i32, i32)> {
    std::env::var("DD_SHIM_LOGICAL_SIZE")
        .ok()
        .and_then(|s| parse_size_pair(&s))
        .or_else(|| std::env::var("CHROME_WINDOW_SIZE").ok().and_then(|s| parse_size_pair(&s)))
}
