#[cfg(target_os = "macos")]
mod macos {

use dd_display::metal::MetalCtx;
use dd_display::metal_backend::MetalBackend;
use dd_gpu::backend::GpuBackend;
use dd_gpu::id::{BindGroupId, BufferId, PipelineId, SamplerId, ShaderId, TextureId};
use dd_gpu::ir::{
    self, texture_usage, AddressMode, BindEntry, BindGroupDesc, BindResource, BufferDesc,
    ColorAttachment, ColorTargetState, CommandBuffer, Enc, Filter, IndexFormat, LoadOp,
    RenderPipelineDesc, SamplerDesc, ShaderRef, TextureDesc, TextureDim, TextureFormat, Topology,
    VertexAttr, VertexLayout,
};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{
    MTLDevice, MTLPixelFormat, MTLStorageMode, MTLTexture, MTLTextureDescriptor, MTLTextureUsage,
};

const TARGET_W: u32 = 8;
const TARGET_H: u32 = 8;
const ATLAS_W: u32 = 4;
const ATLAS_H: u32 = 4;

const CHROME_TEXTURED_MSL: &str = r#"
#include <metal_stdlib>
using namespace metal;

struct Uniforms {
    float4 sk_RTAdjust;
    float2 invAtlasSize;
};

struct VIn {
    float2 position [[attribute(0)]];
    float4 color [[attribute(1)]];
    ushort2 texcoord [[attribute(2)]];
};

struct VOut {
    float4 position [[position]];
    float4 color [[user(v0)]];
    float2 uv [[user(v1)]];
};

vertex VOut vmain(VIn in [[stage_in]], constant Uniforms& u [[buffer(1)]]) {
    VOut out;
    float4 devicePosition = float4(in.position, 0.0, 1.0);
    out.position = float4(
        devicePosition.xy * u.sk_RTAdjust.xz + devicePosition.ww * u.sk_RTAdjust.yw,
        0.0,
        devicePosition.w
    );
    out.color = in.color;
    out.uv = float2(in.texcoord.x, in.texcoord.y) * u.invAtlasSize;
    return out;
}

fragment float4 fmain(
    VOut in [[stage_in]],
    texture2d<float> atlas [[texture(0)]],
    sampler atlasSampler [[sampler(0)]]
) {
    return atlas.sample(atlasSampler, in.uv) * in.color;
}
"#;

fn pack_msl(src: &str) -> Vec<u32> {
    let mut words = vec![src.len() as u32];
    for chunk in src.as_bytes().chunks(4) {
        let mut word = [0u8; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        words.push(u32::from_le_bytes(word));
    }
    words
}

fn push_f32(out: &mut Vec<u8>, value: f32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn chrome_stride16_vertices() -> Vec<u8> {
    let tint = [255u8, 128, 64, 255];
    let verts: [([f32; 2], [u8; 4], [u16; 2]); 4] = [
        ([0.0, 0.0], tint, [0, 0]),
        ([0.0, TARGET_H as f32], tint, [0, ATLAS_H as u16]),
        ([TARGET_W as f32, 0.0], tint, [ATLAS_W as u16, 0]),
        (
            [TARGET_W as f32, TARGET_H as f32],
            tint,
            [ATLAS_W as u16, ATLAS_H as u16],
        ),
    ];

    let mut out = Vec::with_capacity(verts.len() * 16);
    for (position, color, texcoord) in verts {
        push_f32(&mut out, position[0]);
        push_f32(&mut out, position[1]);
        out.extend_from_slice(&color);
        push_u16(&mut out, texcoord[0]);
        push_u16(&mut out, texcoord[1]);
    }
    out
}

fn index_bytes(indices: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(indices.len() * 2);
    for &index in indices {
        push_u16(&mut out, index);
    }
    out
}

fn chrome_uniforms() -> Vec<u8> {
    let values = [
        2.0f32 / TARGET_W as f32,
        -1.0,
        2.0 / TARGET_H as f32,
        -1.0,
        1.0 / ATLAS_W as f32,
        1.0 / ATLAS_H as f32,
        0.0,
        0.0,
    ];
    let mut out = Vec::with_capacity(values.len() * std::mem::size_of::<f32>());
    for value in values {
        push_f32(&mut out, value);
    }
    out
}

fn rgba_atlas() -> Vec<u8> {
    let mut out = Vec::with_capacity((ATLAS_W * ATLAS_H * 4) as usize);
    for y in 0..ATLAS_H {
        for x in 0..ATLAS_W {
            let rgba = match (x < 2, y < 2) {
                (true, true) => [255, 255, 255, 255],
                (false, true) => [255, 0, 0, 255],
                (true, false) => [0, 255, 0, 255],
                (false, false) => [0, 0, 255, 255],
            };
            out.extend_from_slice(&rgba);
        }
    }
    out
}

fn new_rgba_texture(
    ctx: &MetalCtx,
    w: u32,
    h: u32,
) -> Retained<ProtocolObject<dyn MTLTexture>> {
    let desc = unsafe {
        MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
            MTLPixelFormat::RGBA8Unorm,
            w as usize,
            h as usize,
            false,
        )
    };
    desc.setUsage(MTLTextureUsage::ShaderRead | MTLTextureUsage::RenderTarget);
    desc.setStorageMode(MTLStorageMode::Shared);
    ctx.device
        .newTextureWithDescriptor(&desc)
        .expect("newTextureWithDescriptor RGBA failed")
}

fn read_rgba(tex: &ProtocolObject<dyn MTLTexture>, w: u32, h: u32) -> Vec<u8> {
    let mut out = vec![0u8; (w * h * 4) as usize];
    let region = objc2_metal::MTLRegion {
        origin: objc2_metal::MTLOrigin { x: 0, y: 0, z: 0 },
        size: objc2_metal::MTLSize {
            width: w as usize,
            height: h as usize,
            depth: 1,
        },
    };
    unsafe {
        tex.getBytes_bytesPerRow_fromRegion_mipmapLevel(
            std::ptr::NonNull::new(out.as_mut_ptr() as *mut _).unwrap(),
            (w * 4) as usize,
            region,
            0,
        );
    }
    out
}

fn assert_rgba_near(rgba: &[u8], x: u32, y: u32, want: [u8; 4]) {
    let i = ((y * TARGET_W + x) * 4) as usize;
    let got = [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]];
    for (g, w) in got.into_iter().zip(want) {
        assert!(
            (g as i16 - w as i16).abs() <= 3,
            "pixel ({x},{y}) got RGBA={got:?}, want approximately {want:?}"
        );
    }
}

fn create_chrome_pipeline(be: &mut MetalBackend) {
    be.create_shader(ShaderId(20), dd_gpu::ir::ShaderPayloadKind::LegacyMsl, &pack_msl(CHROME_TEXTURED_MSL))
        .unwrap();
    be.create_render_pipeline(
        PipelineId(30),
        &RenderPipelineDesc {
            vertex: ShaderRef {
                module: 20,
                entry: "vmain".into(),
            },
            fragment: Some(ShaderRef {
                module: 20,
                entry: "fmain".into(),
            }),
            vertex_buffers: vec![VertexLayout {
                stride: 16,
                step_mode: 0,
                attrs: vec![
                    VertexAttr {
                        location: 0,
                        format: 2,
                        offset: 0,
                    },
                    VertexAttr {
                        location: 1,
                        format: 0x10104,
                        offset: 8,
                    },
                    VertexAttr {
                        location: 2,
                        format: 0x0302,
                        offset: 12,
                    },
                ],
            }],
            color_targets: vec![ColorTargetState {
                format: TextureFormat::Rgba8Unorm,
                blend: None,
                write_mask: 0xf,
            }],
            depth: None,
            topology: Topology::TriangleList,
            cull: 0,
            front_face: 0,
            label: "chrome-indexed-stride16-textured-rgba".into(),
        },
    )
    .unwrap();
}

fn render_chrome_textured_quad(indices: &[u16], first_index: u32) -> Option<Vec<u8>> {
    let Some(ctx) = MetalCtx::new() else {
        eprintln!("skipping Chrome Metal IR regression test: no Metal device");
        return None;
    };

    let target = new_rgba_texture(&ctx, TARGET_W, TARGET_H);
    let vertices = chrome_stride16_vertices();
    let indices = index_bytes(indices);
    let uniforms = chrome_uniforms();
    let atlas = rgba_atlas();
    let mut be = MetalBackend::new(&ctx);
    be.set_render_target(1, target.clone());

    be.create_buffer(
        BufferId(10),
        &BufferDesc {
            size: vertices.len() as u64,
            usage: ir::buffer_usage::VERTEX,
            label: "chrome-packed-vertices".into(),
        },
    )
    .unwrap();
    be.write_buffer(BufferId(10), 0, &vertices).unwrap();
    be.create_buffer(
        BufferId(11),
        &BufferDesc {
            size: indices.len() as u64,
            usage: ir::buffer_usage::INDEX,
            label: "chrome-u16-indices".into(),
        },
    )
    .unwrap();
    be.write_buffer(BufferId(11), 0, &indices).unwrap();
    be.create_buffer(
        BufferId(12),
        &BufferDesc {
            size: uniforms.len() as u64,
            usage: ir::buffer_usage::UNIFORM,
            label: "chrome-rtadjust-atlas-uniforms".into(),
        },
    )
    .unwrap();
    be.write_buffer(BufferId(12), 0, &uniforms).unwrap();
    be.create_buffer(
        BufferId(13),
        &BufferDesc {
            size: atlas.len() as u64,
            usage: ir::buffer_usage::COPY_SRC,
            label: "chrome-rgba-atlas-staging".into(),
        },
    )
    .unwrap();
    be.write_buffer(BufferId(13), 0, &atlas).unwrap();
    be.create_texture(
        TextureId(2),
        &TextureDesc {
            width: ATLAS_W,
            height: ATLAS_H,
            depth: 1,
            mip_levels: 1,
            sample_count: 1,
            dim: TextureDim::D2,
            format: TextureFormat::Rgba8Unorm,
            usage: texture_usage::SAMPLED | texture_usage::COPY_DST,
            label: "chrome-rgba-atlas".into(),
        },
    )
    .unwrap();
    be.create_sampler(
        SamplerId(3),
        &SamplerDesc {
            min_filter: Filter::Nearest,
            mag_filter: Filter::Nearest,
            mip_filter: Filter::Nearest,
            address_u: AddressMode::ClampToEdge,
            address_v: AddressMode::ClampToEdge,
            address_w: AddressMode::ClampToEdge,
        },
    )
    .unwrap();
    be.create_bind_group(
        BindGroupId(40),
        &BindGroupDesc {
            set: 0,
            entries: vec![
                BindEntry {
                    binding: 1,
                    resource: BindResource::Buffer {
                        id: 12,
                        offset: 0,
                        size: uniforms.len() as u64,
                    },
                },
                BindEntry {
                    binding: 0,
                    resource: BindResource::Texture { id: 2 },
                },
                BindEntry {
                    binding: 0,
                    resource: BindResource::Sampler { id: 3 },
                },
            ],
        },
    )
    .unwrap();
    create_chrome_pipeline(&mut be);

    be.submit(&CommandBuffer {
        encoder: vec![
            Enc::CopyBufferToTexture {
                src: 13,
                src_offset: 0,
                bytes_per_row: ATLAS_W * 4,
                dst: 2,
                mip: 0,
                width: ATLAS_W,
                height: ATLAS_H,
            },
            Enc::BeginRenderPass {
                color: vec![ColorAttachment {
                    texture: 1,
                    load: LoadOp::Clear,
                    clear: [0.0, 0.0, 0.0, 1.0],
                    store: true,
                }],
                depth: None,
            },
            Enc::SetPipeline(30),
            Enc::SetBindGroup {
                index: 0,
                group: 40,
            },
            Enc::SetViewport {
                x: 0.0,
                y: 0.0,
                w: TARGET_W as f32,
                h: TARGET_H as f32,
                min_depth: 0.0,
                max_depth: 1.0,
            },
            Enc::SetScissor {
                x: 0,
                y: 0,
                w: TARGET_W,
                h: TARGET_H,
            },
            Enc::SetVertexBuffer {
                slot: 0,
                buffer: 10,
                offset: 0,
            },
            Enc::SetIndexBuffer {
                buffer: 11,
                offset: 0,
                format: IndexFormat::U16,
            },
            Enc::DrawIndexed {
                index_count: 6,
                instance_count: 1,
                first_index,
                base_vertex: 0,
                first_instance: 0,
            },
            Enc::EndRenderPass,
        ],
        signal: None,
    })
    .unwrap();

    Some(read_rgba(&target, TARGET_W, TARGET_H))
}

#[test]
fn indexed_triangle_list_stride16_samples_rgba_atlas_into_rgba_target() {
    let Some(rgba) = render_chrome_textured_quad(&[0, 1, 2, 2, 1, 3], 0) else {
        return;
    };

    // Golden reflects the backend's correct surface-target orientation (live Chrome is upright, see
    // a8e5df30 / ab820089): the four atlas quadrants read back V-mirrored vs. the earlier stale golden.
    assert_rgba_near(&rgba, 1, 1, [0, 128, 0, 255]);
    assert_rgba_near(&rgba, 6, 1, [0, 0, 64, 255]);
    assert_rgba_near(&rgba, 1, 6, [255, 128, 64, 255]);
    assert_rgba_near(&rgba, 6, 6, [255, 0, 0, 255]);
    assert_rgba_near(&rgba, 3, 3, [0, 128, 0, 255]);
    assert_rgba_near(&rgba, 4, 4, [255, 0, 0, 255]);
}

#[test]
fn indexed_triangle_list_first_index_selects_chrome_quad_from_batch() {
    let Some(rgba) = render_chrome_textured_quad(&[3, 3, 0, 1, 2, 2, 1, 3], 2) else {
        return;
    };

    // Golden reflects the backend's correct surface-target orientation (live Chrome is upright, see
    // a8e5df30 / ab820089): the four atlas quadrants read back V-mirrored vs. the earlier stale golden.
    assert_rgba_near(&rgba, 1, 1, [0, 128, 0, 255]);
    assert_rgba_near(&rgba, 6, 1, [0, 0, 64, 255]);
    assert_rgba_near(&rgba, 1, 6, [255, 128, 64, 255]);
    assert_rgba_near(&rgba, 6, 6, [255, 0, 0, 255]);
    assert_rgba_near(&rgba, 3, 3, [0, 128, 0, 255]);
    assert_rgba_near(&rgba, 4, 4, [255, 0, 0, 255]);
}
}

#[cfg(not(target_os = "macos"))]
#[test]
fn metal_chrome_ir_regression_tests_require_macos() {
    eprintln!("Metal Chrome IR regression tests are macOS-only");
}
