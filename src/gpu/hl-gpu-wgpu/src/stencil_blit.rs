//! GPU-only nearest-neighbour stencil blits.
//!
//! WGSL cannot return a fragment stencil value. Instead, copy the source stencil aspect to a packed
//! byte buffer, then draw the destination rectangle once for each possible 8-bit value. The fragment
//! shader discards pixels whose source byte differs from the draw's instance index; fixed-function
//! `Replace` writes that index through the dynamic stencil reference. One pipeline and one render pass
//! serve all 256 values.

use hl_gpu::protocol::model::descriptor::{Extent3d, Mirror, Origin3d, TextureSubresource};
use hl_gpu::protocol::model::enums::{TextureAspect, TextureFormat};
use hl_gpu::runtime::model::resources::SessionResources;
use hl_gpu::{GpuError, Result};

use crate::{texture, WgpuExecutor};

const STENCIL_BLIT_WGSL: &str = r#"
struct Params {
    dst_origin: vec2<u32>,
    src_extent: vec2<u32>,
    dst_extent: vec2<u32>,
    row_pitch: u32,
    mirror_bits: u32,
};

@group(0) @binding(0) var<storage, read> source: array<u32>;
@group(0) @binding(1) var<uniform> params: Params;

struct Out {
    @builtin(position) position: vec4<f32>,
    @location(0) @interpolate(flat) reference: u32,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex: u32, @builtin(instance_index) instance: u32) -> Out {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );
    var out: Out;
    out.position = vec4<f32>(positions[vertex], 0.0, 1.0);
    out.reference = instance;
    return out;
}

@fragment
fn fs_main(in: Out) {
    let dst = vec2<u32>(in.position.xy) - params.dst_origin;
    var src = vec2<u32>(
        ((2u * dst.x + 1u) * params.src_extent.x) / (2u * params.dst_extent.x),
        ((2u * dst.y + 1u) * params.src_extent.y) / (2u * params.dst_extent.y),
    );
    if ((params.mirror_bits & 1u) != 0u) { src.x = params.src_extent.x - 1u - src.x; }
    if ((params.mirror_bits & 2u) != 0u) { src.y = params.src_extent.y - 1u - src.y; }
    let byte_index = src.y * params.row_pitch + src.x;
    let value = (source[byte_index / 4u] >> (8u * (byte_index & 3u))) & 255u;
    if (value != in.reference) { discard; }
}
"#;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct StencilBlitRegion {
    pub src_origin: Origin3d,
    pub src_extent: Extent3d,
    pub dst_origin: Origin3d,
    pub dst_extent: Extent3d,
    pub mirror: Mirror,
}

pub(crate) struct StencilBlitCache {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
}

impl StencilBlitCache {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("hl-stencil-blit-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("hl-stencil-blit-pl"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("hl-stencil-blit"),
            source: wgpu::ShaderSource::Wgsl(STENCIL_BLIT_WGSL.into()),
        });
        let replace = wgpu::StencilFaceState {
            compare: wgpu::CompareFunction::Always,
            fail_op: wgpu::StencilOperation::Keep,
            depth_fail_op: wgpu::StencilOperation::Keep,
            pass_op: wgpu::StencilOperation::Replace,
        };
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("hl-stencil-blit"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[],
            }),
            primitive: Default::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth24PlusStencil8,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::Always,
                stencil: wgpu::StencilState {
                    front: replace,
                    back: replace,
                    read_mask: 0xff,
                    write_mask: 0xff,
                },
                bias: Default::default(),
            }),
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });
        Self { layout, pipeline }
    }
}

fn row_pitch(width: u32) -> Result<u32> {
    width
        .checked_add(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT - 1)
        .map(|n| n / wgpu::COPY_BYTES_PER_ROW_ALIGNMENT * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        .ok_or(GpuError::OutOfBounds)
}

fn staging_size(width: u32, height: u32, limits: &wgpu::Limits) -> Result<u64> {
    let size = u64::from(row_pitch(width)?)
        .checked_mul(u64::from(height))
        .ok_or(GpuError::OutOfBounds)?;
    if size > limits.max_buffer_size || size > u64::from(limits.max_storage_buffer_binding_size) {
        // The exact maximum height for this width is
        // `min(max_buffer_size, max_storage_buffer_binding_size) / row_pitch(width)`.
        // Do not tile this operation naively: Vulkan permits source/destination aliasing, and rendering
        // one tile could overwrite source texels needed by a later tile. Preserving those semantics needs
        // either one complete immutable snapshot (this path) or an alias-aware multi-buffer tiler.
        return Err(GpuError::Unsupported(
            "wgpu: stencil blit source exceeds storage-buffer limits",
        ));
    }
    Ok(size)
}

#[cfg(test)]
fn source_coordinate(dst: u32, src_extent: u32, dst_extent: u32, mirrored: bool) -> u32 {
    let value =
        ((2 * u64::from(dst) + 1) * u64::from(src_extent) / (2 * u64::from(dst_extent))) as u32;
    if mirrored {
        src_extent - 1 - value
    } else {
        value
    }
}

impl WgpuExecutor {
    /// Resample one layer of an 8-bit stencil aspect with nearest filtering. Work is entirely queue-ordered:
    /// no mapping, host wait, or CPU-visible readback occurs between the aspect copy and render pass.
    pub(crate) fn blit_stencil_nearest(
        &mut self,
        res: &SessionResources,
        src: u32,
        src_sub: &TextureSubresource,
        dst: u32,
        dst_sub: &TextureSubresource,
        region: StencilBlitRegion,
    ) -> Result<()> {
        if src_sub.aspect != TextureAspect::StencilOnly
            || dst_sub.aspect != TextureAspect::StencilOnly
        {
            return Err(GpuError::Unsupported("wgpu: stencil blit subresource"));
        }
        if region.mirror.z
            || region.src_extent.depth != 1
            || region.dst_extent.depth != 1
            || region.src_extent.width == 0
            || region.src_extent.height == 0
            || region.dst_extent.width == 0
            || region.dst_extent.height == 0
        {
            return Err(GpuError::Unsupported("wgpu: layered or empty stencil blit"));
        }
        let source = texture::WgpuTexture::get(res, src)?;
        let destination = texture::WgpuTexture::get(res, dst)?;
        for texture in [source, destination] {
            if texture.format != TextureFormat::Depth24PlusStencil8
                || texture.sample_count != 1
                || texture.dim != hl_gpu::protocol::model::enums::TextureDim::D2
            {
                return Err(GpuError::Unsupported(
                    "wgpu: stencil blit texture format or samples",
                ));
            }
        }
        if source.usage & hl_gpu::protocol::model::enums::texture_usage::COPY_SRC == 0
            || destination.usage & hl_gpu::protocol::model::enums::texture_usage::RENDER_TARGET == 0
        {
            return Err(GpuError::Invalid("wgpu: stencil blit texture usage"));
        }
        let in_bounds = |sub: &TextureSubresource,
                         origin: &Origin3d,
                         extent: &Extent3d,
                         texture: &texture::WgpuTexture| {
            if sub.mip >= texture.mip_levels {
                return false;
            }
            let width = (texture.width >> sub.mip).max(1);
            let height = (texture.height >> sub.mip).max(1);
            origin
                .x
                .checked_add(extent.width)
                .is_some_and(|x| x <= width)
                && origin
                    .y
                    .checked_add(extent.height)
                    .is_some_and(|y| y <= height)
                && sub
                    .layer
                    .checked_add(origin.z)
                    .and_then(|base| base.checked_add(extent.depth))
                    .is_some_and(|end| end <= texture.depth)
        };
        if !in_bounds(src_sub, &region.src_origin, &region.src_extent, source)
            || !in_bounds(dst_sub, &region.dst_origin, &region.dst_extent, destination)
        {
            return Err(GpuError::OutOfBounds);
        }
        let source_layer = src_sub
            .layer
            .checked_add(region.src_origin.z)
            .ok_or(GpuError::OutOfBounds)?;
        let destination_layer = dst_sub
            .layer
            .checked_add(region.dst_origin.z)
            .ok_or(GpuError::OutOfBounds)?;

        let pitch = row_pitch(region.src_extent.width)?;
        let device = &self.gpu.device;
        let staging_size = staging_size(
            region.src_extent.width,
            region.src_extent.height,
            &device.limits(),
        )?;
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("hl-stencil-blit-source"),
            size: staging_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let words = [
            region.dst_origin.x,
            region.dst_origin.y,
            region.src_extent.width,
            region.src_extent.height,
            region.dst_extent.width,
            region.dst_extent.height,
            pitch,
            u32::from(region.mirror.x) | (u32::from(region.mirror.y) << 1),
        ];
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("hl-stencil-blit-params"),
            size: std::mem::size_of_val(&words) as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        // SAFETY: `words` is contiguous initialized POD and the byte view cannot outlive it.
        let bytes = unsafe {
            std::slice::from_raw_parts(words.as_ptr().cast::<u8>(), std::mem::size_of_val(&words))
        };
        self.gpu.queue.write_buffer(&uniform, 0, bytes);

        if self.stencil_blit.is_none() {
            self.stencil_blit = Some(StencilBlitCache::new(device));
        }
        let cache = self.stencil_blit.as_ref().expect("initialized above");
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("hl-stencil-blit-bg"),
            layout: &cache.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: staging.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: uniform.as_entire_binding(),
                },
            ],
        });
        let dst_view = destination
            .texture
            .create_view(&wgpu::TextureViewDescriptor {
                label: Some("hl-stencil-blit-destination"),
                dimension: Some(wgpu::TextureViewDimension::D2),
                aspect: wgpu::TextureAspect::All,
                base_mip_level: dst_sub.mip,
                mip_level_count: Some(1),
                base_array_layer: destination_layer,
                array_layer_count: Some(1),
                ..Default::default()
            });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("hl-stencil-blit"),
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &source.texture,
                mip_level: src_sub.mip,
                origin: wgpu::Origin3d {
                    x: region.src_origin.x,
                    y: region.src_origin.y,
                    z: source_layer,
                },
                aspect: wgpu::TextureAspect::StencilOnly,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(pitch),
                    rows_per_image: Some(region.src_extent.height),
                },
            },
            wgpu::Extent3d {
                width: region.src_extent.width,
                height: region.src_extent.height,
                depth_or_array_layers: 1,
            },
        );
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("hl-stencil-blit-pass"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &dst_view,
                    depth_ops: None,
                    stencil_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    }),
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&cache.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.set_viewport(
                region.dst_origin.x as f32,
                region.dst_origin.y as f32,
                region.dst_extent.width as f32,
                region.dst_extent.height as f32,
                0.0,
                1.0,
            );
            pass.set_scissor_rect(
                region.dst_origin.x,
                region.dst_origin.y,
                region.dst_extent.width,
                region.dst_extent.height,
            );
            for reference in 0..=255 {
                pass.set_stencil_reference(reference);
                pass.draw(0..3, reference..reference + 1);
            }
        }
        self.gpu.queue.submit(Some(encoder.finish()));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hl_gpu::protocol::model::descriptor::TextureDesc;
    use hl_gpu::protocol::model::enums::{texture_usage, TextureDim};
    use hl_gpu::{Cmd, FakeClock, GlobalLedger, GpuExecutor, Limits, Session};

    #[test]
    fn shader_is_valid_wgsl() {
        let module =
            naga::front::wgsl::parse_str(STENCIL_BLIT_WGSL).expect("parse stencil blit WGSL");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("validate stencil blit WGSL");
    }

    #[test]
    fn nearest_mapping_matches_pixel_center_rule_and_mirroring() {
        assert_eq!(
            (0..4)
                .map(|d| source_coordinate(d, 2, 4, false))
                .collect::<Vec<_>>(),
            [0, 0, 1, 1]
        );
        assert_eq!(
            (0..4)
                .map(|d| source_coordinate(d, 2, 4, true))
                .collect::<Vec<_>>(),
            [1, 1, 0, 0]
        );
        assert_eq!(
            (0..2)
                .map(|d| source_coordinate(d, 4, 2, false))
                .collect::<Vec<_>>(),
            [1, 3]
        );
        assert_eq!(
            (0..2)
                .map(|d| source_coordinate(d, 4, 2, true))
                .collect::<Vec<_>>(),
            [2, 0]
        );
    }

    #[test]
    fn source_rows_are_256_aligned_without_truncation() {
        assert_eq!(row_pitch(1), Ok(256));
        assert_eq!(row_pitch(255), Ok(256));
        assert_eq!(row_pitch(256), Ok(256));
        assert_eq!(row_pitch(257), Ok(512));
    }

    #[test]
    fn staging_size_respects_buffer_and_storage_binding_limits() {
        let limits = wgpu::Limits {
            max_buffer_size: 4096,
            max_storage_buffer_binding_size: 2048,
            ..wgpu::Limits::default()
        };
        assert_eq!(staging_size(257, 4, &limits), Ok(2048));
        assert!(matches!(
            staging_size(257, 5, &limits),
            Err(GpuError::Unsupported(_))
        ));

        let limits = wgpu::Limits {
            max_buffer_size: 1024,
            max_storage_buffer_binding_size: 4096,
            ..wgpu::Limits::default()
        };
        assert!(matches!(
            staging_size(257, 3, &limits),
            Err(GpuError::Unsupported(_))
        ));
    }

    fn copy_aspect_in(
        exec: &WgpuExecutor,
        texture: &wgpu::Texture,
        mip: u32,
        layer: u32,
        aspect: wgpu::TextureAspect,
        width: u32,
        height: u32,
        bytes: &[u8],
    ) {
        let pitch = row_pitch(width).unwrap();
        let mut padded = vec![0; (pitch * height) as usize];
        for y in 0..height as usize {
            padded[y * pitch as usize..y * pitch as usize + width as usize]
                .copy_from_slice(&bytes[y * width as usize..(y + 1) * width as usize]);
        }
        let buffer = exec.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("hl-stencil-test-upload"),
            size: padded.len() as u64,
            usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        exec.gpu.queue.write_buffer(&buffer, 0, &padded);
        let mut encoder = exec.gpu.device.create_command_encoder(&Default::default());
        encoder.copy_buffer_to_texture(
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(pitch),
                    rows_per_image: Some(height),
                },
            },
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: mip,
                origin: wgpu::Origin3d { x: 0, y: 0, z: layer },
                aspect,
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        exec.gpu.queue.submit(Some(encoder.finish()));
    }

    fn copy_stencil_out(
        exec: &WgpuExecutor,
        texture: &wgpu::Texture,
        mip: u32,
        layer: u32,
        width: u32,
        height: u32,
    ) -> Vec<u8> {
        let pitch = row_pitch(width).unwrap();
        let buffer = exec.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("hl-stencil-test-readback"),
            size: u64::from(pitch * height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = exec.gpu.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: mip,
                origin: wgpu::Origin3d { x: 0, y: 0, z: layer },
                aspect: wgpu::TextureAspect::StencilOnly,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(pitch),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        exec.gpu.queue.submit(Some(encoder.finish()));
        let slice = buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| tx.send(result).unwrap());
        exec.gpu.device.poll(wgpu::Maintain::Wait);
        rx.recv().unwrap().unwrap();
        let mapped = slice.get_mapped_range();
        let mut tight = Vec::with_capacity((width * height) as usize);
        for y in 0..height as usize {
            tight.extend_from_slice(&mapped[y * pitch as usize..y * pitch as usize + width as usize]);
        }
        tight
    }

    #[test]
    fn native_stencil_blit_scales_mirrors_layers_origins_and_aliases() {
        let mut exec = WgpuExecutor::new(crate::DeviceConfig::default()).expect("wgpu adapter");
        let mut session = Session::new(
            Limits::from_capabilities(exec.capabilities()),
            GlobalLedger::unbounded(),
            Box::new(FakeClock::new(0)),
        );
        let desc = TextureDesc {
            width: 16,
            height: 16,
            depth: 2,
            mip_levels: 2,
            sample_count: 1,
            dim: TextureDim::D2,
            format: TextureFormat::Depth24PlusStencil8,
            usage: texture_usage::COPY_SRC
                | texture_usage::COPY_DST
                | texture_usage::RENDER_TARGET,
            label: String::new(),
        };
        hl_gpu::runtime::submit(
            &mut session,
            &mut exec,
            0,
            &[Cmd::CreateTexture(1, desc.clone()), Cmd::CreateTexture(2, desc)],
        )
        .unwrap();
        let src = texture::WgpuTexture::get(&session.resources, 1).unwrap();
        let dst = texture::WgpuTexture::get(&session.resources, 2).unwrap();
        let source = (0..64).map(|i| (i + 1) as u8).collect::<Vec<_>>();
        copy_aspect_in(&exec, &src.texture, 1, 1, wgpu::TextureAspect::StencilOnly, 8, 8, &source);
        copy_aspect_in(&exec, &dst.texture, 1, 1, wgpu::TextureAspect::StencilOnly, 8, 8, &[77; 64]);

        exec.blit_stencil_nearest(
            &session.resources,
            1,
            &TextureSubresource { mip: 1, layer: 1, aspect: TextureAspect::StencilOnly },
            2,
            &TextureSubresource { mip: 1, layer: 1, aspect: TextureAspect::StencilOnly },
            StencilBlitRegion {
                src_origin: Origin3d { x: 2, y: 1, z: 0 },
                src_extent: Extent3d { width: 2, height: 2, depth: 1 },
                dst_origin: Origin3d { x: 1, y: 2, z: 0 },
                dst_extent: Extent3d { width: 4, height: 4, depth: 1 },
                mirror: Mirror { x: true, y: false, z: false },
            },
        )
        .unwrap();
        let got = copy_stencil_out(&exec, &dst.texture, 1, 1, 8, 8);
        let mut want = vec![77; 64];
        for y in 0..4 {
            for x in 0..4 {
                let sx = 2 + source_coordinate(x, 2, 4, true);
                let sy = 1 + source_coordinate(y, 2, 4, false);
                want[((y + 2) * 8 + x + 1) as usize] = source[(sy * 8 + sx) as usize];
            }
        }
        assert_eq!(got, want, "scale/mirror must preserve every outside stencil byte");

        // Same-texture overlap must read the complete source before modifying its destination.
        exec.blit_stencil_nearest(
            &session.resources,
            2,
            &TextureSubresource { mip: 1, layer: 1, aspect: TextureAspect::StencilOnly },
            2,
            &TextureSubresource { mip: 1, layer: 1, aspect: TextureAspect::StencilOnly },
            StencilBlitRegion {
                src_origin: Origin3d { x: 1, y: 2, z: 0 },
                src_extent: Extent3d { width: 4, height: 4, depth: 1 },
                dst_origin: Origin3d { x: 3, y: 3, z: 0 },
                dst_extent: Extent3d { width: 4, height: 4, depth: 1 },
                mirror: Mirror::NONE,
            },
        )
        .unwrap();
        let overlapped = copy_stencil_out(&exec, &dst.texture, 1, 1, 8, 8);
        for y in 0..4 {
            for x in 0..4 {
                assert_eq!(overlapped[((y + 3) * 8 + x + 3) as usize], want[((y + 2) * 8 + x + 1) as usize]);
            }
        }
    }
}
