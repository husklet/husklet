//! Native texture handle + tight-packed pixel readback and region upload.
//!
//! A protocol texture becomes a `wgpu::Texture` (+ default view) with the usages the IR can ask of it
//! (TEXTURE_BINDING | COPY_SRC | COPY_DST | RENDER_ATTACHMENT). Because `copy_texture_to_buffer` demands a
//! 256-byte-aligned row stride, readback always goes through a padded staging buffer that is then repacked
//! to a *tight* plane — matching the CPU oracle's `read_texture`, which returns exactly `width*height*bpt`
//! bytes with no row padding. Region uploads use `queue.write_texture`, which has no such stride rule.

use hl_gpu::protocol::model::descriptor::TextureDesc;
use hl_gpu::protocol::model::enums::TextureFormat;
use hl_gpu::runtime::model::resources::SessionResources;
use hl_gpu::{GpuError, Result};

use crate::convert::{texel_bytes, texture_format};
use crate::WgpuExecutor;

/// The wgpu-native backing of one protocol texture.
pub struct WgpuTexture {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub width: u32,
    pub height: u32,
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
    /// Create a 2D texture matching `desc` (single-layer, single-mip — the subset the oracle materializes).
    pub(crate) fn make_texture(&self, desc: &TextureDesc) -> Result<WgpuTexture> {
        if desc.width == 0 || desc.height == 0 {
            return Err(GpuError::Invalid("zero-sized texture"));
        }
        let wfmt = texture_format(desc.format)?;
        let texture = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("hl-texture"),
            size: wgpu::Extent3d {
                width: desc.width,
                height: desc.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: desc.sample_count.max(1),
            dimension: wgpu::TextureDimension::D2,
            format: wfmt,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Ok(WgpuTexture { texture, view, width: desc.width, height: desc.height, format: desc.format })
    }

    /// Read back the whole tight-packed level-0 color plane of texture `id` — the texture-readback half of
    /// the conformance contract (the analogue of the CPU oracle's `read_texture`). Returns exactly
    /// `width*height*bytes_per_texel` bytes, no row padding.
    pub fn read_texture(&self, res: &SessionResources, id: u32) -> Result<Vec<u8>> {
        self.read_texture_tight(res, id)
    }

    /// Read the whole tight-packed level-0 color plane of texture `id` (exactly `width*height*bpt` bytes).
    pub(crate) fn read_texture_tight(&self, res: &SessionResources, id: u32) -> Result<Vec<u8>> {
        let t = native(res, id)?;
        let bpt = texel_bytes(t.format)? as u32;
        let tight_bpr = t.width * bpt;
        let padded_bpr = round256(tight_bpr);

        let staging = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("hl-tex-readback"),
            size: (padded_bpr * t.height) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc =
            self.gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &t.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bpr),
                    rows_per_image: Some(t.height),
                },
            },
            wgpu::Extent3d { width: t.width, height: t.height, depth_or_array_layers: 1 },
        );
        self.gpu.queue.submit(Some(enc.finish()));

        let slice = staging.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        self.gpu.device.poll(wgpu::Maintain::Wait);
        let mapped = slice.get_mapped_range();
        let mut out = vec![0u8; (tight_bpr * t.height) as usize];
        for row in 0..t.height {
            let src = (row * padded_bpr) as usize;
            let dst = (row * tight_bpr) as usize;
            out[dst..dst + tight_bpr as usize]
                .copy_from_slice(&mapped[src..src + tight_bpr as usize]);
        }
        drop(mapped);
        staging.unmap();
        Ok(out)
    }

    /// Upload a tight-packed `width*height` texel region into texture `id` at origin `(x, y)`.
    pub(crate) fn write_region(
        &self,
        res: &SessionResources,
        id: u32,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        data: &[u8],
    ) -> Result<()> {
        let t = native(res, id)?;
        let bpt = texel_bytes(t.format)? as u32;
        self.gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &t.texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * bpt),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        self.gpu.queue.submit(None::<wgpu::CommandBuffer>);
        Ok(())
    }
}
