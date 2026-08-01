//! The CPU-native texture: its descriptor + tight-packed level-0 pixels, one plane per array layer.
//! Stored behind a `TextureId`. Ported from the `Texture` struct in `hl-gpu/src/software.rs`.

use crate::protocol::model::descriptor::TextureDesc;
use crate::protocol::model::enums::TextureDim;

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

    /// Byte length of one layer's plane.
    pub fn plane_bytes(&self) -> usize {
        self.pixels.len() / self.layers() as usize
    }

    /// Byte range of `layer`'s plane within [`Texture::pixels`], or `None` if there is no such layer.
    ///
    /// Every write path allowed to address a layer goes through this. The paths that are NOT —
    /// texture-to-texture copy, blit, resolve, and the rasterizer — refuse a non-base layer in validation
    /// and address layer 0 directly, which is the same range this returns for layer 0. That split is
    /// deliberate and mirrors the executor, which likewise serves a layered clear and a layered region
    /// read and refuses a non-base subresource everywhere else.
    pub fn layer_plane(&self, layer: u32) -> Option<std::ops::Range<usize>> {
        if layer >= self.layers() {
            return None;
        }
        let plane = self.plane_bytes();
        let start = layer as usize * plane;
        Some(start..start + plane)
    }
}
