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
use hl_gpu::protocol::model::descriptor::Mirror;

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

/// A BLIT whose source is an ARRAY texture runs, at the base layer both backends support.
///
/// It did not. The blit bound the source texture's DEFAULT view, which for an array texture is
/// `D2Array`, into a bind-group layout declaring `D2` — so every blit from an array or cube source failed
/// device validation with `InvalidTextureDimension`, regardless of which layer it named, including layer
/// 0, which is the case both backends otherwise support. Nothing caught it because no test blitted from
/// an array texture and the differential's blit programs all use plain 2D sources.
///
/// A blit addresses ONE layer of each side by definition, so the view now names that layer. That is both
/// the fix and the more accurate description of the operation.
///
/// The destination here is a plain 2D texture on purpose, and the reason is worth recording: an array
/// texture is denied `RENDER_ATTACHMENT` at creation, on the stated grounds that its default view is not
/// "a single-layer 2D view a color pass could target". This change makes that premise false — such a view
/// is exactly what the blit now builds — but widening the usage set touches every array and cube texture
/// in the driver, not only blit destinations, so it is left alone and written down rather than done at
/// the end of an audit.
#[test]
fn a_blit_from_an_array_source_runs_at_the_base_layer() {
    use hl_gpu::protocol::model::descriptor::{Extent3d, Origin3d, TextureSubresource};
    use hl_gpu::protocol::model::enums::TextureAspect;

    let mut exec = WgpuExecutor::new(DeviceConfig::default()).expect("adapter");
    let plain = TextureDesc {
        width: 4,
        height: 4,
        depth: 1,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D2,
        format: TextureFormat::Rgba8Unorm,
        usage: texture_usage::RENDER_TARGET | texture_usage::COPY_SRC | texture_usage::COPY_DST,
        label: String::new(),
    };
    let sub = |layer: u32| TextureSubresource {
        mip: 0,
        layer,
        aspect: TextureAspect::All,
    };
    let extent = Extent3d {
        width: 4,
        height: 4,
        depth: 1,
    };
    let blit = |src_sub, dst_sub| Enc::BlitTexture {
        src: 1,
        src_sub,
        src_origin: Origin3d::default(),
        src_extent: extent.clone(),
        dst: 2,
        dst_sub,
        dst_origin: Origin3d::default(),
        dst_extent: extent.clone(),
        filter: Filter::Nearest,
        mirror: Mirror::NONE,
    };

    let run = |exec: &mut WgpuExecutor, src: TextureDesc, enc| {
        let mut s = new_session(exec);
        hl_gpu::runtime::submit(
            &mut s,
            exec,
            0,
            &[
                Cmd::CreateTexture(1, src),
                Cmd::CreateTexture(2, plain.clone()),
                Cmd::Submit(CommandBuffer {
                    encoder: vec![enc],
                    signal: None,
                }),
            ],
        )
        .map(|_| ())
    };

    let array_src = TextureDesc {
        depth: 4,
        usage: texture_usage::SAMPLED | texture_usage::COPY_SRC | texture_usage::COPY_DST,
        ..plain.clone()
    };
    assert!(
        run(&mut exec, array_src.clone(), blit(sub(0), sub(0))).is_ok(),
        "a blit from a four-layer source must run at layer 0"
    );

    // The control: the plain-2D source that always worked must keep working, so the assertion above is
    // about the array shape and not about the blit path having been loosened generally.
    assert!(
        run(&mut exec, plain.clone(), blit(sub(0), sub(0))).is_ok(),
        "a plain 2D source is unaffected"
    );

    // And a non-base layer stays refused, deliberately: the software oracle materializes one plane per
    // texture and has no array-layer concept, so serving layered blits here alone would make the executor
    // perform what the reference refuses.
    assert!(
        matches!(
            run(&mut exec, array_src, blit(sub(1), sub(0))),
            Err(hl_gpu::GpuError::Unsupported(_))
        ),
        "layer 1 is refused, in agreement with the reference"
    );
}
