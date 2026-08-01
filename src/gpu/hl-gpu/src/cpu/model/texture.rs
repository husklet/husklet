//! The CPU-native texture: its descriptor + tight-packed level-0 pixels, one plane per array layer.
//! Stored behind a `TextureId`. Ported from the `Texture` struct in `hl-gpu/src/software.rs`.

use crate::protocol::model::descriptor::TextureDesc;
use crate::protocol::model::enums::{TextureDim, TextureFormat};

#[derive(Clone)]
pub struct Texture {
    pub desc: TextureDesc,
    /// Tight-packed level-0 pixels, LAYER-MAJOR: `layers` consecutive planes of
    /// `bytes_per_texel * width * height [* sample_count]`.
    ///
    /// A single-layer texture is one plane and is byte-identical to what this field held when it could
    /// only ever be one — every existing offset of the form `(y * width + x) * bpt` still addresses layer
    /// 0 unchanged, which is why the layered case could be added without disturbing the paths that are
    /// layer-blind by contract (see [`Texture::layer_plane`]).
    pub pixels: Vec<u8>,
}

impl Texture {
    /// Planes materialized (`>= 1`): array layers for a 2D texture, depth slices for a 3D one, faces for
    /// a cube, and always exactly one for 1D.
    ///
    /// This mirrors the executor's own mapping in `hl-gpu-wgpu`'s `texture.rs`, which is the source of
    /// truth: it forces a 1D texture to a single layer whatever the descriptor says, and treats a cube
    /// whose count was left at the 0/1 default as the canonical six faces. Deriving it independently
    /// here would be a second representation of one rule; deriving it DIFFERENTLY would allocate a
    /// different number of planes from the subject for the same descriptor.
    pub fn layers(&self) -> u32 {
        Self::planes(&self.desc)
    }

    /// [`Texture::layers`] for a descriptor that has not been materialized yet.
    pub fn planes(desc: &TextureDesc) -> u32 {
        match desc.dim {
            TextureDim::D1 => 1,
            TextureDim::Cube if desc.depth <= 1 => 6,
            _ => desc.depth.max(1),
        }
    }

    /// Mip levels materialized (`>= 1`).
    pub fn levels(&self) -> u32 {
        self.desc.mip_levels.max(1)
    }

    /// A mip level's dimensions: the base extent halved per level, floored, never below one.
    ///
    /// The `max(1)` is the whole rule and it is where an off-by-one lives: level 2 of a 5x3 texture is
    /// 1x1, not 1x0, and a plane of zero rows would make the level occupy no bytes and every level after
    /// it start in the wrong place. The executor uses exactly this expression for its own readback.
    pub fn level_size(desc: &TextureDesc, mip: u32) -> (u32, u32) {
        (
            (desc.width >> mip).max(1),
            (desc.height >> mip).max(1),
        )
    }

    /// Bytes per texel AS THIS REFERENCE MATERIALIZES IT.
    ///
    /// Not `TextureFormat::bytes_per_texel`, which has no answer for a depth format: this oracle stores a
    /// `Depth32Float` plane as four bytes per texel and a `Depth24PlusStencil8` plane as eight, so the
    /// rasterizer can run the depth and stencil tests against them. Allocation and plane addressing must
    /// agree about that or a depth texture's planes are sized by one rule and indexed by another —
    /// which is exactly what happened: the readback of a depth attachment started reporting
    /// `OutOfBounds` because the addressing side had no size for the format at all.
    pub fn texel_bytes(desc: &TextureDesc) -> Option<usize> {
        match desc.format {
            TextureFormat::Depth32Float => Some(4),
            TextureFormat::Depth24PlusStencil8 => Some(8),
            other => other.bytes_per_texel(),
        }
    }

    /// Byte offset and length of one (`mip`, `layer`) plane within [`Texture::pixels`].
    ///
    /// Storage is LEVEL-major and layer-minor: every layer of level 0, then every layer of level 1, and
    /// so on. Level 0 layer 0 therefore still begins at offset zero and is still `width * height * bpt`
    /// long, which is why the whole-plane paths that address the base subresource by construction did
    /// not have to change when levels were added — the same property that let layers in earlier.
    pub fn plane_at(&self, mip: u32, layer: u32) -> Option<std::ops::Range<usize>> {
        if mip >= self.levels() || layer >= self.layers() {
            return None;
        }
        let bpt = Self::texel_bytes(&self.desc)?;
        let samples = self.desc.sample_count.max(1) as usize;
        let layers = self.layers() as usize;
        let plane_of = |m: u32| {
            let (w, h) = Self::level_size(&self.desc, m);
            w as usize * h as usize * bpt * samples
        };
        let start: usize = (0..mip).map(|m| plane_of(m) * layers).sum::<usize>()
            + layer as usize * plane_of(mip);
        Some(start..start + plane_of(mip))
    }

    /// Total bytes for every level and layer of `desc`.
    pub fn total_bytes(desc: &TextureDesc, bpt: usize) -> Option<usize> {
        let layers = Self::planes(desc) as usize;
        let samples = desc.sample_count.max(1) as usize;
        (0..desc.mip_levels.max(1)).try_fold(0usize, |acc, m| {
            let (w, h) = Self::level_size(desc, m);
            (w as usize)
                .checked_mul(h as usize)
                .and_then(|v| v.checked_mul(bpt))
                .and_then(|v| v.checked_mul(samples))
                .and_then(|v| v.checked_mul(layers))
                .and_then(|v| acc.checked_add(v))
        })
    }

    /// Byte range of `layer`'s plane within [`Texture::pixels`], or `None` if there is no such layer.
    ///
    /// Every write path allowed to address a layer goes through this. The paths that are NOT —
    /// texture-to-texture copy, blit, resolve, and the rasterizer — refuse a non-base layer in validation
    /// and address layer 0 directly, which is the same range this returns for layer 0. That split is
    /// deliberate and mirrors the executor, which likewise serves a layered clear and a layered region
    /// read and refuses a non-base subresource everywhere else.
    pub fn layer_plane(&self, layer: u32) -> Option<std::ops::Range<usize>> {
        self.plane_at(0, layer)
    }
}
