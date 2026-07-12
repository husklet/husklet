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
}

impl Default for Program {
    fn default() -> Self {
        Program { used: false, vs: 0, fs: 0, linked: false, ubuf: [0; UBUF_BYTES], samp_units: [0; 4] }
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
}

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
