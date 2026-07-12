//! The GLES/EGL state machine — a faithful Rust port of `gl_shim.c`'s process-global object tables and
//! scalar state. Like the C shim, these entry points *accumulate state*; no IR is emitted here (the IR
//! is lowered from this state at swap, in the present path this increment deliberately does not touch —
//! see `lower.rs` for the present-independent resource lowering used by the future swap path).
//!
//! Storage mirrors gl_shim.c exactly (fixed slot tables, ids == slot index from 1, same defaults, same
//! `gen` dirty-counters) so the eventual IR lowering is byte-equivalent.

use std::sync::{Mutex, MutexGuard, OnceLock};

use crate::glconst::*;

pub const MAXSH: usize = 64;
pub const MAXPROG: usize = 64;
pub const MAXBUF: usize = 64;
pub const MAXATTR: usize = 16;
pub const MAXTEX: usize = 32;
pub const MAXVAO: usize = 128;
pub const UBUF_BYTES: usize = 512;

#[derive(Clone, Default)]
pub struct Shader {
    pub used: bool,
    pub kind: u32, // GL_VERTEX_SHADER / GL_FRAGMENT_SHADER
    pub src: Option<String>,
}

#[derive(Clone)]
pub struct Program {
    pub used: bool,
    pub vs: u32,
    pub fs: u32,
    pub linked: bool,
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
            vs: 0,
            fs: 0,
            linked: false,
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
    pub used: bool,
    pub data: Vec<u8>,
    pub usage: u32,
    pub gen: u64, // bumped on every content mutation → dirty key for the swap-time upload skip
}

#[derive(Clone, Default)]
pub struct Texture {
    pub used: bool,
    pub w: i32,
    pub h: i32,
    pub data: Vec<u8>, // RGBA8, converted from the app's upload format
    pub minf: u32,
    pub magf: u32,
    pub ws: u32,
    pub wt: u32,
    pub gen: u64,
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
/// engine surface/IOSurface id the frame renders into (1 in DD_IR_DUMP/host-tool mode).
#[derive(Clone, Copy, Default)]
pub struct Surface {
    pub have: bool,
    pub id: u32,
    pub width: u32,
    pub height: u32,
}

impl Default for Vao {
    fn default() -> Self {
        Vao { used: false, attrs: [Attr::default(); MAXATTR], elem_buf: 0 }
    }
}

/// The whole GL context state (single implicit context, as in gl_shim.c).
pub struct GlState {
    pub sh: Vec<Shader>,
    pub prog: Vec<Program>,
    pub buf: Vec<Buffer>,
    pub tex: Vec<Texture>,
    pub attr: [Attr; MAXATTR],
    pub vao: Vec<Vao>,
    pub cur_vao: u32,

    pub tex_unit: [u32; 8], // texture bound per active unit (GL_TEXTURE_2D)
    pub active_unit: usize,
    pub unpack_alignment: i32,
    pub unpack_row_length: i32,
    pub unpack_skip_rows: i32,
    pub unpack_skip_pixels: i32,

    pub cur_prog: u32,
    pub arr_buf: u32,
    pub elem_buf: u32,
    pub draw_fbo: u32,
    pub read_fbo: u32,
    pub rbo_bound: u32,

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
}

pub const MAXDRAWS: usize = 512;

impl Default for GlState {
    fn default() -> Self {
        GlState {
            sh: vec![Shader::default(); MAXSH],
            prog: vec![Program::default(); MAXPROG],
            buf: vec![Buffer::default(); MAXBUF],
            tex: vec![Texture::default(); MAXTEX],
            attr: [Attr::default(); MAXATTR],
            vao: vec![Vao::default(); MAXVAO],
            cur_vao: 0,
            tex_unit: [0; 8],
            active_unit: 0,
            unpack_alignment: 4,
            unpack_row_length: 0,
            unpack_skip_rows: 0,
            unpack_skip_pixels: 0,
            cur_prog: 0,
            arr_buf: 0,
            elem_buf: 0,
            draw_fbo: 0,
            read_fbo: 0,
            rbo_bound: 0,
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

    /// Allocate the lowest free slot in `[1, max)` where `used(i)` is false; return 0 if none.
    fn alloc_slot(used: impl Fn(usize) -> bool, max: usize) -> u32 {
        (1..max).find(|&i| !used(i)).map(|i| i as u32).unwrap_or(0)
    }

    pub fn gen_buffer(&mut self) -> u32 {
        let id = Self::alloc_slot(|i| self.buf[i].used, MAXBUF);
        if id != 0 {
            let b = &mut self.buf[id as usize];
            *b = Buffer { used: true, gen: b.gen + 1, ..Default::default() };
        }
        id
    }

    pub fn gen_texture(&mut self) -> u32 {
        let id = Self::alloc_slot(|i| self.tex[i].used, MAXTEX);
        if id != 0 {
            let g = self.tex[id as usize].gen;
            self.tex[id as usize] = Texture {
                used: true,
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
            self.sh[id as usize] = Shader { used: true, kind, src: None };
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

    /// Resolve the clear rectangle honoring GL_SCISSOR_TEST (gl_shim.c `clear_scissor_rect`); returns
    /// `(x, y, w, h, scissored)` where `scissored` is true iff the scissor actually sub-rects the
    /// full target (a Metal load-clear is full-target, so a real sub-rect must become a ClearRect).
    pub fn clear_scissor_rect(&self) -> (i32, i32, i32, i32, bool) {
        // draw_fbo is 0 in the ported subset (no FBO binding) → default surface target.
        let (tw, th) = (self.draw_target_w(0), self.draw_target_h(0));
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
        self.draws.push(DrawCall {
            is_clear: true,
            target_tex: 0, // default framebuffer (no FBO tracking yet)
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
            target_tex: 0, // default framebuffer (no FBO tracking yet)
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

    /// Bring up the presented surface (gl_shim.c `surface_up`). In DD_IR_DUMP/host-tool mode the engine
    /// surface id is simply 1 (no renderD128/wayland). Sets default viewport/scissor to the full
    /// surface if unset.
    pub fn surface_up(&mut self, w: u32, h: u32) {
        self.default_surface_valid = false;
        self.default_full_clear_since_swap = false;
        if self.viewport[2] <= 0 || self.viewport[3] <= 0 {
            self.viewport = [0, 0, w as i32, h as i32];
        }
        if self.scissor[2] <= 0 || self.scissor[3] <= 0 {
            self.scissor = [0, 0, w as i32, h as i32];
        }
        self.surf = Surface { have: true, id: 1, width: w, height: h };
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

/// The small EGL-side state (context version + last error). Kept separate from `GlState`.
pub struct EglState {
    pub error: i32,
    pub ctx_major: i32,
    pub ctx_minor: i32,
    pub surface_logical_w: i32,
    pub surface_logical_h: i32,
}

impl Default for EglState {
    fn default() -> Self {
        EglState { error: EGL_SUCCESS, ctx_major: 2, ctx_minor: 0, surface_logical_w: 0, surface_logical_h: 0 }
    }
}

fn gl_cell() -> &'static Mutex<GlState> {
    static S: OnceLock<Mutex<GlState>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(GlState::default()))
}

fn egl_cell() -> &'static Mutex<EglState> {
    static S: OnceLock<Mutex<EglState>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(EglState::default()))
}

/// Lock the global GL state. GLES is single-context/single-threaded per app; a poisoned lock (only
/// possible after a panic, which aborts at the FFI boundary anyway) is unreachable in practice.
pub fn gl() -> MutexGuard<'static, GlState> {
    gl_cell().lock().unwrap_or_else(|e| e.into_inner())
}

pub fn egl() -> MutexGuard<'static, EglState> {
    egl_cell().lock().unwrap_or_else(|e| e.into_inner())
}

/// Whether the shim advertises ES3 (env `DD_SHIM_ES3`), matching gl_shim.c `shim_es3()`.
pub fn shim_es3() -> bool {
    std::env::var_os("DD_SHIM_ES3").is_some()
}
