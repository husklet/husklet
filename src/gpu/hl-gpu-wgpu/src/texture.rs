//! Native texture handle + tight-packed pixel readback and region upload.
//!
//! A protocol texture becomes a `wgpu::Texture` (+ default view) with the usages the IR can ask of it
//! (TEXTURE_BINDING | COPY_SRC | COPY_DST | RENDER_ATTACHMENT). Because `copy_texture_to_buffer` demands a
//! 256-byte-aligned row stride, readback always goes through a padded staging buffer that is then repacked
//! to a *tight* plane — matching the CPU oracle's `read_texture`, which returns exactly `width*height*bpt`
//! bytes with no row padding. Region uploads use `queue.write_texture`, which has no such stride rule.

use hl_gpu::protocol::model::descriptor::{TextureDesc, TextureViewDesc};
use hl_gpu::protocol::model::enums::{TextureAspect, TextureDim, TextureFormat};
use hl_gpu::runtime::model::resources::SessionResources;
use hl_gpu::{GpuError, Result};
#[cfg(target_os = "macos")]
use std::sync::Arc;

use crate::convert::Format;
use crate::WgpuExecutor;

fn view_aspect(format: TextureFormat, aspect: TextureAspect) -> Result<wgpu::TextureAspect> {
    match (format, aspect) {
        (TextureFormat::Depth32Float, TextureAspect::All) => Ok(wgpu::TextureAspect::All),
        (TextureFormat::Depth32Float, TextureAspect::DepthOnly) => {
            Ok(wgpu::TextureAspect::DepthOnly)
        }
        (TextureFormat::Depth24PlusStencil8, TextureAspect::All) => Ok(wgpu::TextureAspect::All),
        (TextureFormat::Depth24PlusStencil8, TextureAspect::DepthOnly) => {
            Ok(wgpu::TextureAspect::DepthOnly)
        }
        (TextureFormat::Depth24PlusStencil8, TextureAspect::StencilOnly) => {
            Ok(wgpu::TextureAspect::StencilOnly)
        }
        (TextureFormat::Depth32Float, TextureAspect::StencilOnly) => Err(GpuError::Invalid(
            "stencil-only view requires a stencil format",
        )),
        (_, TextureAspect::DepthOnly | TextureAspect::StencilOnly) => Err(GpuError::Invalid(
            "depth/stencil view aspect requires a compatible depth/stencil format",
        )),
        (_, TextureAspect::All) => Ok(wgpu::TextureAspect::All),
    }
}

/// The wgpu-native backing of one protocol texture.
pub struct WgpuTexture {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub width: u32,
    pub height: u32,
    pub dim: TextureDim,
    /// `depth_or_array_layers`: the depth-slice count for a 3D texture, the array-layer count for a 2D-array,
    /// or 6 for a cube (its faces); 1 for a plain 2D / 1D texture. A value > 1 is the signal the
    /// `CopyBufferToTexture` upload path uses to fill every slice / layer / face (origin.z selects it).
    pub depth: u32,
    /// Number of mip levels materialized (>= 1). A value > 1 means a `CopyBufferToTexture` may target a
    /// non-zero `mip` and a sampler may select a LOD. Retained for introspection / future readback of a
    /// non-base mip.
    pub mip_levels: u32,
    /// MSAA sample count (`>= 1`). `1` is a plain single-sampled texture; `> 1` is a multisampled render
    /// target that can only be a `RENDER_ATTACHMENT` (never copied to/from) and is consumed by a
    /// `ResolveTexture` op which averages its samples into a single-sample destination.
    pub sample_count: u32,
    pub format: TextureFormat,
    /// Protocol usage declared when the texture was created. Native allocations deliberately carry a
    /// broader mechanical usage set, so operation validation must consult this value rather than wgpu's.
    pub usage: u32,
    /// Whether this texture was created with `RENDER_ATTACHMENT`, i.e. whether it can be a colour or
    /// depth attachment, a resolve target, or a blit destination.
    ///
    /// Recorded rather than re-derived so the guard on the consuming paths is literally the same predicate
    /// as the grant below and cannot drift from it. Re-deriving the shape rule at each consumer is how a
    /// guard comes to test something adjacent to what it guards.
    pub render_attachment: bool,
    #[cfg(target_os = "macos")]
    pub iosurface: Option<Arc<hl_iosurface::Surface>>,
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
    pub(crate) fn make_texture_view(
        &self,
        res: &SessionResources,
        desc: &TextureViewDesc,
    ) -> Result<WgpuTexture> {
        let source = WgpuTexture::get(res, desc.texture)?;
        if !matches!(
            (source.dim, desc.dim),
            (TextureDim::D1, TextureDim::D1)
                | (TextureDim::D2, TextureDim::D2)
                | (TextureDim::D3, TextureDim::D3)
                | (TextureDim::Cube, TextureDim::Cube | TextureDim::D2)
        ) {
            return Err(GpuError::Invalid(
                "texture view dimension is incompatible with its source texture",
            ));
        }
        if desc.format != source.format || desc.mip_count == 0 || desc.layer_count == 0 {
            return Err(GpuError::Invalid(
                "invalid texture view format or empty range",
            ));
        }
        if desc
            .base_mip
            .checked_add(desc.mip_count)
            .is_none_or(|end| end > source.mip_levels)
            || desc
                .base_layer
                .checked_add(desc.layer_count)
                .is_none_or(|end| end > source.depth)
        {
            return Err(GpuError::OutOfBounds);
        }
        let dimension = match desc.dim {
            TextureDim::D1 if desc.layer_count == 1 => wgpu::TextureViewDimension::D1,
            TextureDim::D2 if desc.layer_count == 1 => wgpu::TextureViewDimension::D2,
            TextureDim::D2 => wgpu::TextureViewDimension::D2Array,
            TextureDim::D3
                if desc.base_layer == 0
                    && desc.layer_count == (source.depth >> desc.base_mip).max(1) =>
            {
                wgpu::TextureViewDimension::D3
            }
            TextureDim::Cube if desc.layer_count == 6 => wgpu::TextureViewDimension::Cube,
            TextureDim::Cube if desc.layer_count.is_multiple_of(6) => {
                wgpu::TextureViewDimension::CubeArray
            }
            _ => {
                return Err(GpuError::Invalid(
                    "invalid texture view dimension or layer range",
                ))
            }
        };
        if matches!(desc.dim, TextureDim::Cube)
            && (!desc.base_layer.is_multiple_of(6) || source.width != source.height)
        {
            return Err(GpuError::Invalid(
                "cube view must start on a six-face boundary",
            ));
        }
        let aspect = view_aspect(desc.format, desc.aspect)?;
        let view = source.texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("hl-texture-view"),
            format: Some(Format::from(desc.format).native()),
            dimension: Some(dimension),
            usage: None,
            aspect,
            base_mip_level: desc.base_mip,
            mip_level_count: Some(desc.mip_count),
            base_array_layer: desc.base_layer,
            array_layer_count: (dimension != wgpu::TextureViewDimension::D3)
                .then_some(desc.layer_count),
        });
        Ok(WgpuTexture {
            texture: source.texture.clone(),
            view,
            width: (source.width >> desc.base_mip).max(1),
            height: (source.height >> desc.base_mip).max(1),
            dim: desc.dim,
            depth: desc.layer_count,
            mip_levels: desc.mip_count,
            sample_count: source.sample_count,
            format: source.format,
            usage: source.usage,
            // A view's attachability is the PARENT texture's: the usage lives on the wgpu texture, and a
            // view cannot add one its texture was not created with.
            render_attachment: source.render_attachment,
            #[cfg(target_os = "macos")]
            iosurface: source.iosurface.clone(),
        })
    }

    /// Create a texture matching `desc.dim` — honoring the true texture shape, not collapsing everything to
    /// 2D. Each protocol `TextureDim` maps to its real wgpu texture + default-view dimension:
    ///
    /// * `D1`   → a wgpu 1D texture (`desc.height` must be 1; 1D forbids mips/MSAA), `D1` view.
    /// * `D2`   → a wgpu 2D texture; `desc.depth` is the **array-layer** count (`1` = a plain 2D image), so
    ///   `depth > 1` is a 2D-array whose default view is `D2Array` (else `D2`).
    /// * `D3`   → a wgpu 3D texture, `desc.depth` depth slices, `D3` view.
    /// * `Cube` → a wgpu 2D texture with exactly **6 array layers** (the faces) and a `Cube` default view —
    ///   which is what a `samplerCube` bind-group binding, built from the shader's auto layout,
    ///   requires. Collapsing this to a 2D texture (the old behaviour) made every cube draw fail
    ///   device validation at bind time.
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
                // left it at the 0/1 default is treated as one canonical cube. Any explicit layer count must
                // be a multiple of six; more than six layers form a cube array.
                let faces = if desc.depth <= 1 { 6 } else { desc.depth };
                if faces % 6 != 0 {
                    return Err(GpuError::Invalid(
                        "cube texture layer count must be a multiple of 6",
                    ));
                }
                if desc.width != desc.height {
                    return Err(GpuError::Invalid("cube texture faces must be square"));
                }
                (
                    wgpu::TextureDimension::D2,
                    faces,
                    if faces == 6 {
                        wgpu::TextureViewDimension::Cube
                    } else {
                        wgpu::TextureViewDimension::CubeArray
                    },
                )
            }
        };
        let sample_count = desc.sample_count.max(1);
        let compressed = desc.format.block_geometry().is_some();
        if compressed && sample_count != 1 {
            return Err(GpuError::Invalid(
                "compressed textures cannot be multisampled",
            ));
        }
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
        if sample_count == 1
            && !compressed
            && desc.usage & hl_gpu::protocol::model::enums::texture_usage::STORAGE != 0
        {
            usage |= wgpu::TextureUsages::STORAGE_BINDING;
        }
        // Only a single-sampled plain 2D image gets RENDER_ATTACHMENT. A 3D volume cannot be a render
        // attachment and neither can a 1D texture. A cube / 2D-array is excluded because every consumer of
        // this usage except the blit binds the texture's DEFAULT view, which for those shapes is a Cube /
        // D2Array — not a single-layer 2D view a colour pass can target.
        //
        // That exclusion used to be stated as a property of the texture ("its default view is not a
        // single-layer 2D view"), which a later per-layer view in the blit path made false: a blit
        // destination builds exactly such a view. The premise is narrower than it read. What actually
        // blocks an array attachment is that `ColorAttachment` carries no layer selector, so the render
        // pass has nothing to build a per-layer view FROM, and the software reference refuses to create a
        // layered texture at all — so widening this would be an executor-only capability the differential
        // could never compare. Both are encoding questions, not usage-bit questions.
        //
        // This decides only the usage set. It is NOT a guard: a texture without the bit can still be named
        // as an attachment, which `submit::render` refuses by consulting `render_attachment` below.
        if dimension == wgpu::TextureDimension::D2
            && view_dim == wgpu::TextureViewDimension::D2
            && sample_count == 1
            && !compressed
        {
            usage |= wgpu::TextureUsages::RENDER_ATTACHMENT;
        }
        let descriptor = wgpu::TextureDescriptor {
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
        };
        #[cfg(target_os = "macos")]
        let (texture, iosurface) = if let Some(allocator) = &self.iosurface {
            if allocator.supports(desc, dimension, view_dim, mip_levels, sample_count) {
                let (texture, surface) = allocator.texture(&self.gpu, &descriptor)?;
                (texture, Some(Arc::new(surface)))
            } else {
                (self.gpu.device.create_texture(&descriptor), None)
            }
        } else {
            (self.gpu.device.create_texture(&descriptor), None)
        };
        #[cfg(not(target_os = "macos"))]
        let texture = self.gpu.device.create_texture(&descriptor);
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
            dim: desc.dim,
            depth,
            mip_levels,
            sample_count,
            format: desc.format,
            usage: desc.usage,
            render_attachment: usage.contains(wgpu::TextureUsages::RENDER_ATTACHMENT),
            #[cfg(target_os = "macos")]
            iosurface,
        })
    }

    #[cfg(target_os = "macos")]
    /// Return the stable IOSurface identity backing `id`, if this executor and texture use native
    /// presentation. The texture resource owns the IOSurface; the id remains valid until texture destruction.
    pub fn iosurface_id(&self, res: &SessionResources, id: u32) -> Result<Option<u32>> {
        Ok(WgpuTexture::get(res, id)?
            .iosurface
            .as_ref()
            .map(|surface| surface.id()))
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
        // The mip level's own dimensions (base extent halved per level, floored at 1).
        let mw = (t.width >> mip).max(1);
        let mh = (t.height >> mip).max(1);
        let (tight_bpr, block_rows) = Format::from(t.format).copy_layout(mw, mh)?;
        let padded_bpr = WgpuTexture::row_pitch(tight_bpr);
        hl_log::hl_add!(
            hl_log::tag::PRESENT,
            "readback_bytes",
            (tight_bpr * block_rows) as u64
        );

        let staging = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("hl-tex-readback"),
            size: (padded_bpr * block_rows) as u64,
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
                    rows_per_image: Some(block_rows),
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
        self.wait_for_completion();
        let mapped = slice.get_mapped_range();
        let mut out = vec![0u8; (tight_bpr * block_rows) as usize];
        for row in 0..block_rows {
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
        if mip >= t.mip_levels {
            return Err(GpuError::OutOfBounds);
        }
        let mip_width = (t.width >> mip).max(1);
        let mip_height = (t.height >> mip).max(1);
        if x.checked_add(width).is_none_or(|end| end > mip_width)
            || y.checked_add(height).is_none_or(|end| end > mip_height)
            || z.checked_add(depth).is_none_or(|end| end > t.depth)
        {
            return Err(GpuError::OutOfBounds);
        }
        if let Some((block_width, block_height, _)) = t.format.block_geometry() {
            if !x.is_multiple_of(block_width)
                || !y.is_multiple_of(block_height)
                || (!width.is_multiple_of(block_width) && x + width != mip_width)
                || (!height.is_multiple_of(block_height) && y + height != mip_height)
            {
                return Err(GpuError::Invalid(
                    "compressed texture upload is not block aligned",
                ));
            }
        }
        let (bytes_per_row, rows_per_image) = Format::from(t.format).copy_layout(width, height)?;
        let expected = usize::try_from(bytes_per_row)
            .ok()
            .and_then(|row| row.checked_mul(rows_per_image as usize))
            .and_then(|image| image.checked_mul(depth as usize))
            .ok_or(GpuError::OutOfBounds)?;
        if data.len() < expected {
            return Err(GpuError::OutOfBounds);
        }
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
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(rows_per_image),
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

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn read_region(
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
    ) -> Result<Vec<u8>> {
        let t = WgpuTexture::get(res, id)?;
        if t.sample_count != 1 {
            return Err(GpuError::Unsupported(
                "read texture region: multisampled texture must be resolved first",
            ));
        }
        if mip >= t.mip_levels {
            return Err(GpuError::OutOfBounds);
        }
        let mip_width = (t.width >> mip).max(1);
        let mip_height = (t.height >> mip).max(1);
        if x.checked_add(width).is_none_or(|end| end > mip_width)
            || y.checked_add(height).is_none_or(|end| end > mip_height)
            || z.checked_add(depth).is_none_or(|end| end > t.depth)
        {
            return Err(GpuError::OutOfBounds);
        }
        let (tight_bpr, rows_per_image) = Format::from(t.format).copy_layout(width, height)?;
        let padded_bpr = WgpuTexture::row_pitch(tight_bpr);
        let staging_size = u64::from(padded_bpr)
            .checked_mul(u64::from(rows_per_image))
            .and_then(|plane| plane.checked_mul(u64::from(depth)))
            .ok_or(GpuError::OutOfBounds)?;
        let staging = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("hl-region-readback"),
            size: staging_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("hl-region-readback"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &t.texture,
                mip_level: mip,
                origin: wgpu::Origin3d { x, y, z },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bpr),
                    rows_per_image: Some(rows_per_image),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: depth,
            },
        );
        self.gpu.queue.submit(Some(encoder.finish()));
        let slice = staging.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        self.wait_for_completion();
        let mapped = slice.get_mapped_range();
        let tight_plane = usize::try_from(tight_bpr)
            .ok()
            .and_then(|row| row.checked_mul(rows_per_image as usize))
            .ok_or(GpuError::OutOfBounds)?;
        let mut output = vec![
            0;
            tight_plane
                .checked_mul(depth as usize)
                .ok_or(GpuError::OutOfBounds)?
        ];
        for layer in 0..depth as usize {
            for row in 0..rows_per_image as usize {
                let source = (layer * rows_per_image as usize + row) * padded_bpr as usize;
                let destination = layer * tight_plane + row * tight_bpr as usize;
                output[destination..destination + tight_bpr as usize]
                    .copy_from_slice(&mapped[source..source + tight_bpr as usize]);
            }
        }
        drop(mapped);
        staging.unmap();
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FORMATS: [TextureFormat; 25] = [
        TextureFormat::Rgba8Unorm,
        TextureFormat::Bgra8Unorm,
        TextureFormat::Rgba8Srgb,
        TextureFormat::Bgra8Srgb,
        TextureFormat::R8Unorm,
        TextureFormat::Rg8Unorm,
        TextureFormat::Rgba16Float,
        TextureFormat::Rgba32Float,
        TextureFormat::R32Float,
        TextureFormat::Depth32Float,
        TextureFormat::Depth24PlusStencil8,
        TextureFormat::Bc1RgbaUnorm,
        TextureFormat::Bc1RgbaSrgb,
        TextureFormat::Bc2RgbaUnorm,
        TextureFormat::Bc2RgbaSrgb,
        TextureFormat::Bc3RgbaUnorm,
        TextureFormat::Bc3RgbaSrgb,
        TextureFormat::Bc4RUnorm,
        TextureFormat::Bc4RSnorm,
        TextureFormat::Bc5RgUnorm,
        TextureFormat::Bc5RgSnorm,
        TextureFormat::Bc6hRgbUfloat,
        TextureFormat::Bc6hRgbFloat,
        TextureFormat::Bc7RgbaUnorm,
        TextureFormat::Bc7RgbaSrgb,
    ];

    #[test]
    fn texture_view_aspects_are_exhaustive_and_format_compatible() {
        for format in FORMATS {
            assert!(view_aspect(format, TextureAspect::All).is_ok());
            assert_eq!(
                view_aspect(format, TextureAspect::DepthOnly).is_ok(),
                matches!(
                    format,
                    TextureFormat::Depth32Float | TextureFormat::Depth24PlusStencil8
                ),
                "{format:?} depth aspect"
            );
            assert_eq!(
                view_aspect(format, TextureAspect::StencilOnly).is_ok(),
                format == TextureFormat::Depth24PlusStencil8,
                "{format:?} stencil aspect"
            );
        }
    }

    #[test]
    fn texture_views_reject_dimensions_incompatible_with_the_source_before_wgpu() {
        let executor = WgpuExecutor::new(crate::DeviceConfig::default())
            .expect("a GPU adapter is required to prove the wgpu executor");
        let mut resources = SessionResources::new();
        let source = executor
            .make_texture(&TextureDesc {
                width: 8,
                height: 8,
                depth: 6,
                mip_levels: 1,
                sample_count: 1,
                dim: TextureDim::D2,
                format: TextureFormat::Rgba8Unorm,
                usage: 0,
                label: "dimension-source".into(),
            })
            .unwrap();
        resources.textures.insert(1, Box::new(source)).unwrap();

        for (dim, layer_count) in [
            (TextureDim::D1, 1),
            (TextureDim::D3, 6),
            (TextureDim::Cube, 6),
        ] {
            let error = match executor.make_texture_view(
                &resources,
                &TextureViewDesc {
                    texture: 1,
                    dim,
                    format: TextureFormat::Rgba8Unorm,
                    aspect: TextureAspect::All,
                    base_mip: 0,
                    mip_count: 1,
                    base_layer: 0,
                    layer_count,
                },
            ) {
                Ok(_) => panic!("incompatible view dimension must be a typed rejection"),
                Err(error) => error,
            };
            assert!(matches!(
                error,
                GpuError::Invalid("texture view dimension is incompatible with its source texture")
            ));
        }
    }
}
