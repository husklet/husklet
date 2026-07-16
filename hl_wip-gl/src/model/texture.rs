//! GL texture objects + the texture table: `glGenTextures`/`glTexImage2D`/`glTexParameteri` tracking.
//!
//! Ported from `hl-shim-gl/src/state.rs` (`Texture`, the `MAXTEX` array) + the resource lowering in
//! `hl-shim-gl/src/lower.rs`. A GL texture keeps its RGBA8 pixels (converted from the app's upload
//! format) + filter/wrap state; the frame builder ([`crate::service::frame`]) lowers a sampled texture
//! to a `CreateTexture` + `CreateSampler` + a staging `CreateBuffer`/`WriteBuffer` + a
//! `CopyBufferToTexture`, exactly as `gl_shim.c` does at swap.

use super::glconst::*;
use hl_gpu::protocol::model::enums::{AddressMode, Filter, TextureFormat};
use std::collections::HashMap;

/// One live GL texture object: RGBA8 pixels + the min/mag filter + S/T wrap GL enums.
#[derive(Clone, PartialEq, Debug)]
pub struct GlTexture {
    pub w: i32,
    pub h: i32,
    /// RGBA8 pixels (`w*h*4` bytes), converted from the app's `glTexImage2D` upload format.
    pub data: Vec<u8>,
    pub min_filter: u32,
    pub mag_filter: u32,
    pub wrap_s: u32,
    pub wrap_t: u32,
    pub gen: u64,
    /// Immutable-format storage (`glTexStorage2D`/`3D`): once set, `glTexImage2D` on this texture is a
    /// `GL_INVALID_OPERATION` and only `glTexSubImage*` may update its pixels.
    pub immutable: bool,
    /// The mip-level count declared by `glTexStorage*` (`1` for a mutable texture — the base level only).
    pub levels: i32,
    /// The neutral-IR texel format this texture lowers to (a `CreateTexture` `format`, and — when the
    /// texture is a framebuffer color attachment — the render-target + surface format). Chosen from the
    /// `glTexImage2D` internal format; defaults to `Rgba8Unorm` (the RGBA8 upload the model materializes).
    pub ir_format: TextureFormat,
    /// Whether this texture's `data` plane holds REAL uploaded pixel content (a `glTexImage2D` /
    /// `glTexSubImage2D` / `glCopyTexSubImage2D` upload) rather than a zero-filled STORAGE allocation. A
    /// `glTexImage2D(…, NULL)` or `glTexStorage2D` allocates a zeroed plane so a later `glTexSubImage*` has a
    /// target — but that plane is NOT content: it is exactly the shape an FBO color attachment takes before
    /// it is rendered into. `has_data()` (`!data.is_empty()`) is TRUE for such an attachment even though its
    /// real pixels live only in the FBO render target, so the frame builder uses this flag (via
    /// [`GlTexture::has_real_pixels`]) to prefer the resident FBO render target over the zeroed plane when
    /// sampling an offscreen attachment across frames (Chrome's tile→window composite). `false` until a real
    /// upload lands; reset to `false` by a `glTexImage2D(…, NULL)` re-define (GL discards the old content).
    pub real_pixels: bool,
}

impl Default for GlTexture {
    fn default() -> Self {
        // GL's default sampler state: NEAREST_MIPMAP_LINEAR min, LINEAR mag, REPEAT wrap.
        Self {
            w: 0,
            h: 0,
            data: Vec::new(),
            min_filter: GL_NEAREST_MIPMAP_LINEAR,
            mag_filter: GL_LINEAR,
            wrap_s: GL_REPEAT,
            wrap_t: GL_REPEAT,
            gen: 0,
            immutable: false,
            levels: 1,
            ir_format: TextureFormat::Rgba8Unorm,
            real_pixels: false,
        }
    }
}

impl GlTexture {
    /// The neutral min-filter for this texture's GL min-filter (`gl_shim.c`: Linear for LINEAR /
    /// LINEAR_MIPMAP_*, else Nearest).
    pub fn ir_min_filter(&self) -> Filter {
        match self.min_filter {
            GL_LINEAR | GL_LINEAR_MIPMAP_NEAREST | GL_LINEAR_MIPMAP_LINEAR => Filter::Linear,
            _ => Filter::Nearest,
        }
    }

    /// The neutral mag-filter (`gl_shim.c`: Linear only for exactly LINEAR).
    pub fn ir_mag_filter(&self) -> Filter {
        if self.mag_filter == GL_LINEAR {
            Filter::Linear
        } else {
            Filter::Nearest
        }
    }

    /// The neutral S wrap (`gl_shim.c`: ClampToEdge / MirrorRepeat / else Repeat).
    pub fn ir_wrap_s(&self) -> AddressMode {
        address_mode(self.wrap_s)
    }

    /// The neutral T wrap.
    pub fn ir_wrap_t(&self) -> AddressMode {
        address_mode(self.wrap_t)
    }

    /// Has this texture usable sampled content (materialized pixels)?
    pub fn has_data(&self) -> bool {
        !self.data.is_empty()
    }

    /// Has this texture received a REAL pixel upload (`glTexImage2D`-with-pixels / `glTexSubImage2D` /
    /// `glCopyTexSubImage2D`), as opposed to only a zero-filled STORAGE allocation (`glTexImage2D(…, NULL)` /
    /// `glTexStorage2D`)? A texture that is an FBO color attachment allocated with NULL storage has a zeroed
    /// `data` plane (`has_data()` true) but NO real content — its pixels live in the FBO render target — so
    /// the frame builder samples the resident render target rather than the zeroed plane when this is `false`.
    pub fn has_real_pixels(&self) -> bool {
        self.real_pixels
    }
}

fn address_mode(gl: u32) -> AddressMode {
    match gl {
        GL_CLAMP_TO_EDGE => AddressMode::ClampToEdge,
        GL_MIRRORED_REPEAT => AddressMode::MirrorRepeat,
        _ => AddressMode::Repeat,
    }
}

/// The per-context texture table: GL name → [`GlTexture`], with a monotonic name counter. Name `0` is
/// the reserved "no texture" binding.
#[derive(Debug, Default)]
pub struct Textures {
    map: HashMap<u32, GlTexture>,
    next_name: u32,
}

impl Textures {
    pub fn new() -> Self {
        Self { map: HashMap::new(), next_name: 1 }
    }

    /// `glGenTextures` — mint a fresh GL texture name.
    pub fn gen(&mut self) -> u32 {
        let name = self.next_name;
        self.next_name += 1;
        self.map.entry(name).or_default();
        name
    }

    /// `glTexImage2D` — (re)define the texture's RGBA8 pixels + extent + neutral texel `format`, bumping
    /// its generation. `pixels` is the already-converted RGBA8 image (`w*h*4` bytes) or empty for a
    /// storage-only define (e.g. an FBO color attachment allocated before it is rendered into).
    pub fn image_2d(&mut self, name: u32, w: i32, h: i32, pixels: &[u8], format: TextureFormat) {
        // The zeroed-storage byte size, computed in usize with a checked multiply so a large (or
        // adversarial) w*h never overflows an i32 and panics — an out-of-range extent saturates to a
        // no-alloc `None` rather than crashing the driver (the record layer raises GL_INVALID_VALUE).
        let want = (w.max(0) as usize).checked_mul(h.max(0) as usize).and_then(|n| n.checked_mul(4));
        let t = self.map.entry(name).or_default();
        t.w = w;
        t.h = h;
        t.ir_format = format;
        if !pixels.is_empty() {
            t.data = pixels.to_vec();
            t.real_pixels = true;
        } else if let Some(want) = want {
            if t.data.len() != want {
                t.data = vec![0u8; want];
            }
            // A NULL-storage (re)define carries no content — GL discards any prior pixels — so the plane is
            // no longer real content (it is the pre-render shape of an FBO color attachment).
            t.real_pixels = false;
        }
        t.gen += 1;
    }

    /// `glTexStorage2D`/`glTexStorage3D` — allocate immutable RGBA8 storage (`w*h*4` zeroed bytes),
    /// mark the texture immutable, and record the declared `levels`. Mirrors `gl_shim.c`, which allocates
    /// the RGBA8 base plane so a later `glTexSubImage*` has a target (format/levels are not otherwise
    /// materialized). Returns `false` if the `w*h*4` byte size overflows.
    pub fn storage_2d(&mut self, name: u32, w: i32, h: i32, levels: i32, format: TextureFormat) -> bool {
        let Some(size) = (w as usize).checked_mul(h as usize).and_then(|n| n.checked_mul(4)) else {
            return false;
        };
        let t = self.map.entry(name).or_default();
        t.w = w;
        t.h = h;
        t.ir_format = format;
        t.data = vec![0u8; size];
        t.immutable = true;
        t.levels = levels;
        // Zeroed immutable storage is not real content until a `glTexSubImage*` fills it (an FBO attachment
        // allocated this way renders its pixels into the render target, not this plane).
        t.real_pixels = false;
        t.gen += 1;
        true
    }

    /// Allocate (or grow to) a `w*h*4` RGBA8 base plane for a 2D-array / 3D texture upload
    /// (`glTexImage3D`/`glTexStorage3D`) — `gl_shim.c` materializes only the layer-0 plane. Returns
    /// `false` on a size overflow.
    pub fn alloc_rgba(&mut self, name: u32, w: i32, h: i32) -> bool {
        let Some(size) = (w.max(0) as usize).checked_mul(h.max(0) as usize).and_then(|n| n.checked_mul(4))
        else {
            return false;
        };
        let t = self.map.entry(name).or_default();
        t.w = w;
        t.h = h;
        if t.data.len() != size {
            t.data = vec![0u8; size];
        }
        t.real_pixels = false;
        t.gen += 1;
        true
    }

    /// `glTexSubImage2D` / `glCopyTexSubImage2D` — overwrite the `[xo,xo+w) × [yo,yo+h)` sub-rect of an
    /// existing texture's RGBA8 plane with `rgba` (`w*h*4` tightly-packed bytes). Silently clips to the
    /// texture bounds (the caller validates the range first). Returns `false` for an unknown/dataless name.
    pub fn sub_image_2d(&mut self, name: u32, xo: i32, yo: i32, w: i32, h: i32, rgba: &[u8]) -> bool {
        let Some(t) = self.map.get_mut(&name) else { return false };
        if t.data.is_empty() || w <= 0 || h <= 0 || xo < 0 || yo < 0 {
            return false;
        }
        let (tw, th) = (t.w as usize, t.h as usize);
        let (xo, yo, w, h) = (xo as usize, yo as usize, w as usize, h as usize);
        if xo + w > tw || yo + h > th || rgba.len() < w * h * 4 {
            return false;
        }
        for row in 0..h {
            let dst = ((yo + row) * tw + xo) * 4;
            let src = row * w * 4;
            t.data[dst..dst + w * 4].copy_from_slice(&rgba[src..src + w * 4]);
        }
        t.real_pixels = true;
        t.gen += 1;
        true
    }

    pub fn get_mut(&mut self, name: u32) -> Option<&mut GlTexture> {
        self.map.get_mut(&name)
    }

    /// `glTexParameteri` — set one filter/wrap parameter.
    pub fn set_param(&mut self, name: u32, pname: u32, value: u32) {
        if let Some(t) = self.map.get_mut(&name) {
            match pname {
                GL_TEXTURE_MIN_FILTER => t.min_filter = value,
                GL_TEXTURE_MAG_FILTER => t.mag_filter = value,
                GL_TEXTURE_WRAP_S => t.wrap_s = value,
                GL_TEXTURE_WRAP_T => t.wrap_t = value,
                _ => {}
            }
        }
    }

    pub fn get(&self, name: u32) -> Option<&GlTexture> {
        self.map.get(&name)
    }

    /// `glDeleteTextures` — drop the object.
    pub fn delete(&mut self, name: u32) -> bool {
        self.map.remove(&name).is_some()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}
