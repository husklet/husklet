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
    /// The neutral-IR texel format this texture lowers to (a `CreateTexture` `format`, and — when the
    /// texture is a framebuffer color attachment — the render-target + surface format). Chosen from the
    /// `glTexImage2D` internal format; defaults to `Rgba8Unorm` (the RGBA8 upload the model materializes).
    pub ir_format: TextureFormat,
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
            ir_format: TextureFormat::Rgba8Unorm,
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
        let t = self.map.entry(name).or_default();
        t.w = w;
        t.h = h;
        t.ir_format = format;
        if !pixels.is_empty() {
            t.data = pixels.to_vec();
        } else if t.data.len() != (w * h * 4) as usize {
            t.data = vec![0u8; (w.max(0) * h.max(0) * 4) as usize];
        }
        t.gen += 1;
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
