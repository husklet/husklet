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

use crate::convert::Format;
use crate::WgpuExecutor;

/// The wgpu-native backing of one protocol texture.
pub struct WgpuTexture {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub width: u32,
    pub height: u32,
    /// `depth_or_array_layers`: the depth-slice count for a 3D texture, the array-layer count for a 2D-array,
    /// or 6 for a cube (its faces); 1 for a plain 2D / 1D texture. A value > 1 is the signal the
    /// `CopyBufferToTexture` upload path uses to fill every slice / layer / face (origin.z selects it).
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

impl WgpuTexture {
    /// Downcast a live protocol texture to its wgpu backing.
    pub fn get(res: &SessionResources, id: u32) -> Result<&Self> {
        res.textures
            .get(id)?
            .downcast_ref::<Self>()
            .ok_or(GpuError::Invalid("wgpu: texture native type mismatch"))
    }

    fn row_pitch(bytes: u32) -> u32 {
        bytes.div_ceil(256) * 256
    }
}

impl WgpuExecutor {
    /// Create a texture matching `desc.dim` — honoring the true texture shape, not collapsing everything to
    /// 2D. Each protocol `TextureDim` maps to its real wgpu texture + default-view dimension:
    ///
    /// * `D1`   → a wgpu 1D texture (`desc.height` must be 1; 1D forbids mips/MSAA), `D1` view.
    /// * `D2`   → a wgpu 2D texture; `desc.depth` is the **array-layer** count (`1` = a plain 2D image), so
    ///            `depth > 1` is a 2D-array whose default view is `D2Array` (else `D2`).
    /// * `D3`   → a wgpu 3D texture, `desc.depth` depth slices, `D3` view.
    /// * `Cube` → a wgpu 2D texture with exactly **6 array layers** (the faces) and a `Cube` default view —
    ///            which is what a `samplerCube` bind-group binding, built from the shader's auto layout,
    ///            requires. Collapsing this to a 2D texture (the old behaviour) made every cube draw fail
    ///            device validation at bind time.
    ///
    /// `desc.mip_levels` mip levels are allocated so a mipmapped source can be uploaded per level and sampled
    /// at an explicit LOD. The default view spans all mips and all layers/faces.
    pub(crate) fn make_texture(&self, desc: &TextureDesc) -> Result<WgpuTexture> {
        if desc.width == 0 || desc.height == 0 {
            return Err(GpuError::Invalid("zero-sized texture"));
        }
        let wfmt = Format::from(desc.format).native();
        // Map the protocol dimension to (wgpu texture dimension, layer/slice count, default-view dimension).
        // `depth_or_array_layers` is the array-layer count for D1/D2/Cube and the slice count for D3.
        let layers = desc.depth.max(1);
        let (dimension, depth, view_dim) = match desc.dim {
            TextureDim::D1 => {
                if desc.height != 1 {
                    return Err(GpuError::Invalid("1D texture must have height == 1"));
                }
                (
                    wgpu::TextureDimension::D1,
                    1,
                    wgpu::TextureViewDimension::D1,
                )
            }
            TextureDim::D2 if layers > 1 => (
                wgpu::TextureDimension::D2,
                layers,
                wgpu::TextureViewDimension::D2Array,
            ),
            TextureDim::D2 => (
                wgpu::TextureDimension::D2,
                1,
                wgpu::TextureViewDimension::D2,
            ),
            TextureDim::D3 => (
                wgpu::TextureDimension::D3,
                layers,
                wgpu::TextureViewDimension::D3,
            ),
            TextureDim::Cube => {
                // A cube map is exactly 6 square faces. `desc.depth` carries the face count; a descriptor that
                // left it at the 0/1 default is treated as the canonical 6, but any explicit non-6 count is a
                // hard error (there is no cube-array support here).
                let faces = if desc.depth <= 1 { 6 } else { desc.depth };
                if faces != 6 {
                    return Err(GpuError::Invalid("cube texture must have exactly 6 faces"));
                }
                if desc.width != desc.height {
                    return Err(GpuError::Invalid("cube texture faces must be square"));
                }
                (
                    wgpu::TextureDimension::D2,
                    6,
                    wgpu::TextureViewDimension::Cube,
                )
            }
        };
        let sample_count = desc.sample_count.max(1);
        // A multisampled texture is a MSAA render target only: WebGPU forbids `mipLevelCount > 1` and any
        // COPY usage on a `sampleCount > 1` texture (you resolve it, never copy it), so those are dropped —
        // it is exclusively a `RENDER_ATTACHMENT` drawn into then resolved by a `ResolveTexture` op. A 1D
        // texture likewise forbids `mipLevelCount > 1`.
        let mip_levels = if sample_count > 1 || dimension == wgpu::TextureDimension::D1 {
            1
        } else {
            desc.mip_levels.max(1)
        };
        let mut usage = if sample_count > 1 {
            wgpu::TextureUsages::RENDER_ATTACHMENT
        } else {
            wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST
        };
        // Only a single-sampled plain 2D image gets RENDER_ATTACHMENT. A 3D volume cannot be a render
        // attachment; a 1D texture cannot either; and a cube / 2D-array here is a sampled/copied resource
        // whose default view is a Cube / D2Array (not a single-layer 2D view a color pass could target), so
        // it is never a color target. This keeps the usage set valid for every shape.
        if dimension == wgpu::TextureDimension::D2
            && view_dim == wgpu::TextureViewDimension::D2
            && sample_count == 1
        {
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
        // The default view must carry the true view dimension: wgpu builds the sampler binding against the
        // shader's declared texture dimension (Cube / D2Array / D3 / D1), and a mismatched view is rejected
        // when the bind group is created at draw time.
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(view_dim),
            ..Default::default()
        });
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
        let _sp = hl_log::hl_span!(hl_log::tag::PRESENT, "readback");
        let t = WgpuTexture::get(res, id)?;
        // A multisampled texture is built RENDER_ATTACHMENT-only (WebGPU forbids COPY usage on
        // sampleCount>1), so copy_texture_to_buffer against it is a hard wgpu validation error that would
        // panic the executor thread and NACK the whole frame (guest sees DEVICE_LOST). A readback request
        // must never do that: report it as unsupported and let the caller skip it. Real MSAA content is
        // read only after a resolve to a single-sample target (Enc::ResolveTexture), never here.
        if t.sample_count > 1 {
            return Err(GpuError::Unsupported(
                "read_texture: multisampled texture cannot be copied to a buffer; resolve first",
            ));
        }
        if mip >= t.mip_levels {
            return Err(GpuError::OutOfBounds);
        }
        let bpt = Format::from(t.format).texel_bytes()? as u32;
        // The mip level's own dimensions (base extent halved per level, floored at 1).
        let mw = (t.width >> mip).max(1);
        let mh = (t.height >> mip).max(1);
        let tight_bpr = mw * bpt;
        let padded_bpr = WgpuTexture::row_pitch(tight_bpr);
        hl_log::hl_add!(
            hl_log::tag::PRESENT,
            "readback_bytes",
            (tight_bpr * mh) as u64
        );

        let staging = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("hl-tex-readback"),
            size: (padded_bpr * mh) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
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
            wgpu::Extent3d {
                width: mw,
                height: mh,
                depth_or_array_layers: 1,
            },
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
    /// `(x, y, z)`. `depth > 1` fills that many destination layers — 3D depth slices, 2D-array layers, or
    /// cube faces (origin.z selects the first one; source rows advance `height` per layer). `mip > 0` targets
    /// a mip level (whose own dimensions the caller passes as `width`/`height`).
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
        let t = WgpuTexture::get(res, id)?;
        let bpt = Format::from(t.format).texel_bytes()? as u32;
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
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: depth,
            },
        );
        self.gpu.queue.submit(None::<wgpu::CommandBuffer>);
        Ok(())
    }
}
