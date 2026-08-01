//! The CPU-native texture: its descriptor + tight-packed level-0 pixels, one plane per array layer.
//! Stored behind a `TextureId`. Ported from the `Texture` struct in `hl-gpu/src/software.rs`.

use crate::protocol::model::descriptor::TextureDesc;

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
    /// Array layers materialized (`>= 1`). Only a 2D texture may have more than one; `create_texture`
    /// refuses every other dimension.
    pub fn layers(&self) -> u32 {
        self.desc.depth.max(1)
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
