//! Native texture handle + tight-packed pixel readback and region upload.
//!
//! A protocol texture becomes a `wgpu::Texture` (+ default view) with the usages the IR can ask of it
//! (TEXTURE_BINDING | COPY_SRC | COPY_DST | RENDER_ATTACHMENT). Because `copy_texture_to_buffer` demands a
//! 256-byte-aligned row stride, readback always goes through a padded staging buffer that is then repacked
//! to a *tight* plane — matching the CPU oracle's `read_texture`, which returns exactly `width*height*bpt`
//! bytes with no row padding. Region uploads use `queue.write_texture`, which has no such stride rule.

use hl_gpu::protocol::model::descriptor::TextureDesc;
use hl_gpu::protocol::model::enums::{TextureDim, TextureFormat};
use hl_gpu::runtime::model::resources::SessionResources;
use hl_gpu::{GpuError, Result};
use hl_log::tag;

use crate::convert::{texel_bytes, texture_format};
use crate::WgpuExecutor;

/// The wgpu-native backing of one protocol texture.
pub struct WgpuTexture {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub width: u32,
    pub height: u32,
    /// Depth slices for a 3D texture (`depth_or_array_layers`); 1 for a plain 2D texture. A value > 1 is
    /// the 3D-volume signal the `CopyBufferToTexture` upload path uses to fill every slice.
    pub depth: u32,
    /// Number of mip levels materialized (>= 1). A value > 1 means a `CopyBufferToTexture` may target a
    /// non-zero `mip` and a sampler may select a LOD. Retained for introspection / future readback of a
    /// non-base mip.
    #[allow(dead_code)]
    pub mip_levels: u32,
    /// MSAA sample count (`>= 1`). `1` is a plain single-sampled texture; `> 1` is a multisampled render
    /// target that can only be a `RENDER_ATTACHMENT` (never copied to/from) and is consumed by a
    /// `ResolveTexture` op which averages its samples into a single-sample destination.
    pub sample_count: u32,
    pub format: TextureFormat,
}

/// Downcast a live texture id to its native handle.
pub fn native<'a>(res: &'a SessionResources, id: u32) -> Result<&'a WgpuTexture> {
    res.textures
        .get(id)?
        .downcast_ref::<WgpuTexture>()
        .ok_or(GpuError::Invalid("wgpu: texture native type mismatch"))
}

fn round256(n: u32) -> u32 {
    n.div_ceil(256) * 256
}

impl WgpuExecutor {
    /// Create a texture matching `desc`. A `D3` texture materializes `desc.depth` depth slices (a real 3D
    /// volume); every other dimension stays a single-layer 2D image (the subset the render suite exercises).
    /// `desc.mip_levels` mip levels are allocated, so a mipmapped source can be uploaded per level and
    /// sampled at an explicit LOD. The default view spans all mips and (for a 3D texture) picks the `D3`
    /// view dimension, which is exactly what a `texture3D`/`textureLod` sample validates against.
    pub(crate) fn make_texture(&self, desc: &TextureDesc) -> Result<WgpuTexture> {
        if desc.width == 0 || desc.height == 0 {
            return Err(GpuError::Invalid("zero-sized texture"));
        }
        let wfmt = texture_format(desc.format)?;
        // Only `D3` becomes a native 3D texture (depth = slice count). D1/D2/Cube stay 2D single-layer,
        // matching the pre-existing behaviour the frozen suite depends on.
        let (dimension, depth) = match desc.dim {
            TextureDim::D3 => (wgpu::TextureDimension::D3, desc.depth.max(1)),
            _ => (wgpu::TextureDimension::D2, 1),
        };
        let sample_count = desc.sample_count.max(1);
        // A multisampled texture is a MSAA render target only: WebGPU forbids `mipLevelCount > 1` and any
        // COPY usage on a `sampleCount > 1` texture (you resolve it, never copy it), so those are dropped —
        // it is exclusively a `RENDER_ATTACHMENT` drawn into then resolved by a `ResolveTexture` op.
        let mip_levels = if sample_count > 1 { 1 } else { desc.mip_levels.max(1) };
        // A 3D texture cannot be a render attachment in WebGPU/wgpu, so that usage is dropped for `D3`
        // (a volume is a sampled/copied resource here, never a color target). A single-sampled 2D texture
        // keeps the full set; a multisampled one keeps only RENDER_ATTACHMENT (copies are invalid on it).
        let mut usage = if sample_count > 1 {
            wgpu::TextureUsages::RENDER_ATTACHMENT
        } else {
            wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST
        };
        if dimension != wgpu::TextureDimension::D3 && sample_count == 1 {
            usage |= wgpu::TextureUsages::RENDER_ATTACHMENT;
        }
        let texture = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("hl-texture"),
            size: wgpu::Extent3d {
                width: desc.width,
                height: desc.height,
                depth_or_array_layers: depth,
            },
            mip_level_count: mip_levels,
            sample_count,
            dimension,
            format: wfmt,
            usage,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Ok(WgpuTexture {
            texture,
            view,
            width: desc.width,
            height: desc.height,
            depth,
            mip_levels,
            sample_count,
            format: desc.format,
        })
    }

    /// Read back the whole tight-packed level-0 color plane of texture `id` — the texture-readback half of
    /// the conformance contract (the analogue of the CPU oracle's `read_texture`). Returns exactly
    /// `width*height*bytes_per_texel` bytes, no row padding.
    pub fn read_texture(&self, res: &SessionResources, id: u32) -> Result<Vec<u8>> {
        self.read_texture_tight(res, id)
    }

    /// Read the whole tight-packed level-0 color plane of texture `id` (exactly `width*height*bpt` bytes).
    pub(crate) fn read_texture_tight(&self, res: &SessionResources, id: u32) -> Result<Vec<u8>> {
        self.read_texture_tight_mip(res, id, 0)
    }

    /// Read the whole tight-packed color plane of a specific `mip` level of texture `id` — exactly
    /// `mip_width*mip_height*bpt` bytes, where the mip's dimensions are the base extent halved per level
    /// (floored at 1, the WebGPU mip pyramid). `mip == 0` is the full base plane (the `read_texture_tight`
    /// case). A `mip` at or past the materialized `mip_levels` is a typed `OutOfBounds` rather than a wgpu
    /// bounds panic. This is what makes `CopyTextureToBuffer { mip }` read the level it names instead of
    /// silently returning the base level.
    pub(crate) fn read_texture_tight_mip(
        &self,
        res: &SessionResources,
        id: u32,
        mip: u32,
    ) -> Result<Vec<u8>> {
        let _sp = hl_log::hl_span!(tag::PRESENT, "readback");
        let t = native(res, id)?;
        if mip >= t.mip_levels {
            return Err(GpuError::OutOfBounds);
        }
        let bpt = texel_bytes(t.format)? as u32;
        // The mip level's own dimensions (base extent halved per level, floored at 1).
        let mw = (t.width >> mip).max(1);
        let mh = (t.height >> mip).max(1);
        let tight_bpr = mw * bpt;
        let padded_bpr = round256(tight_bpr);
        hl_log::hl_add!(tag::PRESENT, "readback_bytes", (tight_bpr * mh) as u64);

        let staging = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("hl-tex-readback"),
            size: (padded_bpr * mh) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc =
            self.gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &t.texture,
                mip_level: mip,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bpr),
                    rows_per_image: Some(mh),
                },
            },
            wgpu::Extent3d { width: mw, height: mh, depth_or_array_layers: 1 },
        );
        self.gpu.queue.submit(Some(enc.finish()));

        let slice = staging.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        self.gpu.device.poll(wgpu::Maintain::Wait);
        let mapped = slice.get_mapped_range();
        let mut out = vec![0u8; (tight_bpr * mh) as usize];
        for row in 0..mh {
            let src = (row * padded_bpr) as usize;
            let dst = (row * tight_bpr) as usize;
            out[dst..dst + tight_bpr as usize]
                .copy_from_slice(&mapped[src..src + tight_bpr as usize]);
        }
        drop(mapped);
        staging.unmap();
        Ok(out)
    }

    /// Upload a tight-packed `width*height*depth` texel region into `mip` of texture `id` at origin
    /// `(x, y, z)`. `depth > 1` fills that many 3D slices (source rows advance `height` per slice), and
    /// `mip > 0` targets a mip level (whose own dimensions the caller passes as `width`/`height`).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn write_region(
        &self,
        res: &SessionResources,
        id: u32,
        x: u32,
        y: u32,
        z: u32,
        width: u32,
        height: u32,
        depth: u32,
        mip: u32,
        data: &[u8],
    ) -> Result<()> {
        let t = native(res, id)?;
        let bpt = texel_bytes(t.format)? as u32;
        self.gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &t.texture,
                mip_level: mip,
                origin: wgpu::Origin3d { x, y, z },
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * bpt),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: depth },
        );
        self.gpu.queue.submit(None::<wgpu::CommandBuffer>);
        Ok(())
    }
}
