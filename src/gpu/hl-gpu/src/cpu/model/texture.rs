//! The CPU-native texture: its descriptor + tight-packed pixels for every mip level and plane, where a
//! plane is an array layer, a 3D depth slice, or a cube face. Stored behind a `TextureId`. Ported from
//! the `Texture` struct in `hl-gpu/src/software.rs`.

use crate::protocol::model::descriptor::TextureDesc;
use crate::protocol::model::enums::{TextureDim, TextureFormat};

#[derive(Clone)]
pub struct Texture {
    pub desc: TextureDesc,
    /// Tight-packed pixels, LEVEL-major and plane-minor: every plane of level 0, then every plane of
    /// level 1, and so on. A plane is `bytes_per_texel * level_width * level_height [* sample_count]`.
    ///
    /// [`Texture::plane_at`] is the only correct way to address one, and it states the same ordering —
    /// this doc said LAYER-major until 2026-08-01, contradicting both the code and `plane_at`'s own doc
    /// two screens below, which is the kind of disagreement that survives review because each half reads
    /// authoritative on its own.
    ///
    /// A single-plane, single-level texture is byte-identical to what this field held when it could only
    /// ever be one: every offset of the form `(y * width + x) * bpt` still addresses it unchanged, which
    /// is why layers, then levels, then depth-spanning blits could each be added without disturbing the
    /// paths that address the base subresource by construction.
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

#[cfg(test)]
mod texel_size_agreement {
    use super::*;
    use crate::protocol::model::enums::TextureFormat;

    /// `Texture::texel_bytes` and `TextureFormat::software_texel_bytes` must never disagree on a format
    /// the software backend will actually index.
    ///
    /// Suspected 2026-08-01 as the cause of a blit regression and REFUTED here, which is why the test
    /// exists rather than the fix. The two do differ: `texel_bytes` maps `Depth32Float` to 4 and
    /// `Depth24PlusStencil8` to 8, while `bytes_per_texel` — and so `software_texel_bytes` — returns
    /// nothing for both. That difference is SAFE only because the software path refuses those formats
    /// outright before it indexes anything, so a plane sized by one function is never addressed with the
    /// other's stride.
    ///
    /// "Safe by a refusal elsewhere" is exactly the property that rots quietly: the day a depth format is
    /// given a software path, plane 0 will look correct and every plane after it will be misplaced by the
    /// difference between 4 and 0. Single-plane textures cannot show it, which is most of this suite.
    /// So the invariant is pinned as a REQUIREMENT — wherever the software backend can size a texel, both
    /// functions must give the same answer.
    #[test]
    fn both_texel_sizes_agree_wherever_the_software_backend_can_size_one() {
        let mut checked = 0;
        for wire in 1..=64u32 {
            let Ok(format) = TextureFormat::from_u32(wire) else {
                continue;
            };
            let Ok(software) = format.software_texel_bytes() else {
                continue; // the software backend refuses this format; it never indexes it
            };
            let desc = TextureDesc {
                width: 1,
                height: 1,
                depth: 1,
                mip_levels: 1,
                sample_count: 1,
                dim: crate::protocol::model::enums::TextureDim::D2,
                format,
                usage: 0,
                label: String::new(),
            };
            assert_eq!(
                Texture::texel_bytes(&desc),
                Some(software),
                "{format:?}: plane sizing and texel indexing disagree, so every plane after the first \
                 would be misplaced"
            );
            checked += 1;
        }
        // An agreement test that checked nothing would pass just as loudly.
        assert!(checked >= 12, "only {checked} formats exercised; the sweep found almost nothing");
    }
}
