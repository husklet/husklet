//! DEMO — 2D-array layer selection: sample a specific layer of a 2D-array texture and assert its texel.
//!
//! A 2D-array is a `TextureDim::D2` whose `depth` carries the **array-layer** count (`> 1`). It must
//! materialize as a wgpu 2D texture with N array layers whose default view is `TextureViewDimension::D2Array`
//! — the view the `sampler2DArray` binding wgpu builds from the shader's auto layout requires. Before the
//! fix, `make_texture` forced every non-3D texture to a single-layer 2D image (`depth = 1`), so the other
//! layers never existed and the array-view bind failed device validation. The four 1×1 layers get four
//! distinct colors, uploaded as one 4-slice volume via `CopyBufferToTexture` (origin.z = layer). Sampling
//! layer `k` with NEAREST must return that layer's exact color.

mod gpu_harness;
use gpu_harness::*;

use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BufferDesc, ColorAttachment, RenderPipelineDesc,
    SamplerDesc, ShaderRef, TextureDesc,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, texture_usage, AddressMode, Filter, LoadOp, TextureDim, TextureFormat, Topology,
};
use hl_gpu::protocol::model::kernel::glsl_stage;
use hl_gpu::{Cmd, CommandBuffer, Enc, ShaderPayloadKind};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

const LAYERS: [[u8; 4]; 4] = [
    [210, 20, 20, 255],  // layer 0
    [20, 210, 20, 255],  // layer 1
    [20, 20, 210, 255],  // layer 2
    [210, 210, 20, 255], // layer 3
];

const VS: &str = r#"#version 460
void main() {
    vec2 p[3] = vec2[3](vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    gl_Position = vec4(p[gl_VertexIndex], 0.0, 1.0);
}
"#;
const FS: &str = r#"#version 460
layout(std140, set = 0, binding = 0) uniform U { vec4 layer; } u;
layout(set = 0, binding = 1) uniform texture2DArray t;
layout(set = 0, binding = 2) uniform sampler        s;
layout(location = 0) out vec4 o;
void main() { o = texture(sampler2DArray(t, s), vec3(0.5, 0.5, u.layer.x)); }
"#;

fn nearest() -> SamplerDesc {
    SamplerDesc {
        min_filter: Filter::Nearest,
        mag_filter: Filter::Nearest,
        mip_filter: Filter::Nearest,
        address_u: AddressMode::ClampToEdge,
        address_v: AddressMode::ClampToEdge,
        address_w: AddressMode::ClampToEdge,
        ..SamplerDesc::default()
    }
}

/// A 1×1 `Rgba8Unorm` 2D-array texture with `layers` array layers (`depth` carries the layer count).
fn tex2d_array(layers: u32, usage: u32) -> TextureDesc {
    TextureDesc {
        width: 1,
        height: 1,
        depth: layers,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D2,
        format: TextureFormat::Rgba8Unorm,
        usage,
        label: String::new(),
    }
}

/// Sample array layer `k`; return the single readback pixel.
fn sample_layer(exec: &mut WgpuExecutor, k: u32) -> [u8; 4] {
    let mut s = new_session(exec);
    let all: Vec<u8> = LAYERS.iter().flatten().copied().collect(); // 4 stacked texels

    hl_gpu::runtime::submit(
        &mut s,
        exec,
        0,
        &[
            Cmd::CreateTexture(
                1,
                tex2d(1, 1, texture_usage::RENDER_TARGET | texture_usage::COPY_SRC),
            ),
            Cmd::CreateTexture(
                2,
                tex2d_array(4, texture_usage::SAMPLED | texture_usage::COPY_DST),
            ),
            Cmd::CreateBuffer(
                1,
                BufferDesc {
                    size: 16,
                    usage: buffer_usage::UNIFORM,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: le_f32(&[k as f32, 0.0, 0.0, 0.0]),
            },
            Cmd::CreateBuffer(
                2,
                BufferDesc {
                    size: 16,
                    usage: buffer_usage::COPY_SRC,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 2,
                offset: 0,
                data: all,
            },
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::VERTEX, "vmain", VS),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::FRAGMENT, "fmain", FS),
            },
            Cmd::CreateSampler(1, nearest()),
            Cmd::CreateRenderPipeline(
                1,
                RenderPipelineDesc {
                    vertex: ShaderRef {
                        module: 1,
                        entry: "vmain".into(),
                    },
                    fragment: Some(ShaderRef {
                        module: 2,
                        entry: "fmain".into(),
                    }),
                    vertex_buffers: vec![],
                    color_targets: vec![color_target()],
                    depth: None,
                    topology: Topology::TriangleList,
                    cull: 0,
                    front_face: 0,
                    sample_count: 1,
                    label: String::new(),
                },
            ),
            Cmd::CreateBindGroup(
                1,
                BindGroupDesc {
                    set: 0,
                    entries: vec![
                        BindEntry {
                            binding: 0,
                            resource: BindResource::Buffer {
                                id: 1,
                                offset: 0,
                                size: 16,
                            },
                        },
                        BindEntry {
                            binding: 1,
                            resource: BindResource::Texture { id: 2 },
                        },
                        BindEntry {
                            binding: 2,
                            resource: BindResource::Sampler { id: 1 },
                        },
                    ],
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    // One copy fills all 4 array layers (the dst has 4 layers).
                    Enc::CopyBufferToTexture {
                        src: 2,
                        src_offset: 0,
                        bytes_per_row: 4,
                        dst: 2,
                        mip: 0,
                        width: 1,
                        height: 1,
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
                    Enc::SetPipeline(1),
                    Enc::SetBindGroup { index: 0, group: 1 },
                    Enc::Draw {
                        vertex_count: 3,
                        instance_count: 1,
                        first_vertex: 0,
                        first_instance: 0,
                    },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
        ],
    )
    .expect("the 2D-array layer sample draw must run cleanly");

    let img = exec.read_texture(&s.resources, 1).unwrap();
    [img[0], img[1], img[2], img[3]]
}

#[test]
fn sampling_an_array_layer_returns_that_layers_texel() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");

    for (layer, &want) in LAYERS.iter().enumerate() {
        let got = sample_layer(&mut exec, layer as u32);
        write_png(&format!("array_layer{layer}"), 1, 1, &got);
        assert!(
            near(got, want),
            "layer {layer}: must sample {want:?}, got {got:?}"
        );
    }
    eprintln!("demo `array_texture`: all 4 array layers sampled to their exact texels");
}
