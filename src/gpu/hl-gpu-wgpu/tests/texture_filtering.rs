//! DEMO 3 — texture filtering: NEAREST vs LINEAR sampling of a 2×1 texture, asserted to the exact texel.
//!
//! The source texture has two texels, A (left) and B (right), chosen so their per-channel average is an
//! exact 8-bit integer. A fullscreen triangle samples it at a uniform-supplied uv into a 1×1 `Rgba8Unorm`
//! target, and the single pixel is read back:
//!   * NEAREST @ u=0.25 → the LEFT texel center → exactly A.
//!   * NEAREST @ u=0.75 → the RIGHT texel center → exactly B.
//!   * LINEAR  @ u=0.50 → the midpoint between the two texel centers → exactly (A+B)/2.
//!
//! The LINEAR midpoint is neither A nor B, so it proves genuine bilinear interpolation (not a nearest tap
//! that happened to round). All targets are LINEAR `Rgba8Unorm`, so the interpolation is an exact integer.

mod gpu_harness;
use gpu_harness::*;

use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BufferDesc, ColorAttachment, RenderPipelineDesc,
    SamplerDesc, ShaderRef,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, texture_usage, AddressMode, Filter, LoadOp, Topology,
};
use hl_gpu::{Cmd, CommandBuffer, Enc, ShaderPayloadKind};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

const A: [u8; 4] = [200, 60, 40, 255]; // left texel
const B: [u8; 4] = [40, 80, 220, 255]; // right texel

const VS_WGSL: &str = r#"@vertex fn vmain(@builtin(vertex_index) i:u32)->@builtin(position) vec4<f32>{let p=array<vec2<f32>,3>(vec2(-1.,-1.),vec2(3.,-1.),vec2(-1.,3.));return vec4(p[i],0.,1.);}"#;
const FS_WGSL: &str = r#"struct U{uv:vec2<f32>}; @group(0) @binding(0) var<uniform> u:U; @group(0) @binding(1) var t:texture_2d<f32>; @group(0) @binding(2) var s:sampler; @fragment fn fmain()->@location(0) vec4<f32>{return textureSample(t,s,u.uv);}"#;

fn wgsl_to_spirv(src:&str)->Vec<u32>{
    let module=naga::front::wgsl::parse_str(src).unwrap();
    let info=naga::valid::Validator::new(naga::valid::ValidationFlags::all(),naga::valid::Capabilities::all()).validate(&module).unwrap();
    naga::back::spv::write_vec(&module,&info,&naga::back::spv::Options::default(),None).unwrap()
}

fn sampler(f: Filter, address_u: AddressMode) -> SamplerDesc {
    SamplerDesc {
        min_filter: f,
        mag_filter: f,
        mip_filter: Filter::Nearest,
        address_u,
        address_v: AddressMode::ClampToEdge,
        address_w: AddressMode::ClampToEdge,
        ..SamplerDesc::default()
    }
}

/// Sample the 2×1 texture at `u` (v = 0.5) with `filter`; return the single readback pixel.
fn tap_address(
    exec: &mut WgpuExecutor,
    filter: Filter,
    address_u: AddressMode,
    u: f32,
) -> [u8; 4] {
    let mut s = new_session(exec);
    let mut texels = A.to_vec();
    texels.extend_from_slice(&B);

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
                tex2d(2, 1, texture_usage::SAMPLED | texture_usage::COPY_DST),
            ),
            Cmd::CreateBuffer(
                1,
                BufferDesc {
                    size: 8,
                    usage: buffer_usage::UNIFORM,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: le_f32(&[u, 0.5]),
            },
            Cmd::CreateBuffer(
                2,
                BufferDesc {
                    size: 8,
                    usage: buffer_usage::COPY_SRC,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 2,
                offset: 0,
                data: texels,
            },
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::SpirV,
                spirv: wgsl_to_spirv(VS_WGSL),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::SpirV,
                spirv: wgsl_to_spirv(FS_WGSL),
            },
            Cmd::CreateSampler(1, sampler(filter, address_u)),
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
                                size: 8,
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
                    Enc::CopyBufferToTexture {
                        src: 2,
                        src_offset: 0,
                        bytes_per_row: 8,
                        dst: 2,
                        mip: 0,
                        width: 2,
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
    .expect("the filtered-sample draw must run cleanly");

    let img = exec.read_texture(&s.resources, 1).unwrap();
    [img[0], img[1], img[2], img[3]]
}

fn tap(exec: &mut WgpuExecutor, filter: Filter, u: f32) -> [u8; 4] {
    tap_address(exec, filter, AddressMode::ClampToEdge, u)
}

#[test]
fn nearest_taps_exact_texels_and_linear_interpolates_midpoint() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");

    let near_left = tap(&mut exec, Filter::Nearest, 0.25);
    let near_right = tap(&mut exec, Filter::Nearest, 0.75);
    let lin_mid = tap(&mut exec, Filter::Linear, 0.5);
    let cubic_quarter = tap(&mut exec, Filter::Cubic, 0.375);

    let mid = [
        ((A[0] as u16 + B[0] as u16) / 2) as u8,
        ((A[1] as u16 + B[1] as u16) / 2) as u8,
        ((A[2] as u16 + B[2] as u16) / 2) as u8,
        ((A[3] as u16 + B[3] as u16) / 2) as u8,
    ];

    // 1×1 PNGs are trivial but written for completeness / parity with the visual demos.
    write_png("filter_nearest_left", 1, 1, &near_left);
    write_png("filter_nearest_right", 1, 1, &near_right);
    write_png("filter_linear_mid", 1, 1, &lin_mid);

    assert!(
        near(near_left, A),
        "NEAREST @0.25 must tap left texel {A:?}, got {near_left:?}"
    );
    assert!(
        near(near_right, B),
        "NEAREST @0.75 must tap right texel {B:?}, got {near_right:?}"
    );
    assert!(
        near(lin_mid, mid),
        "LINEAR @0.5 must be the exact midpoint {mid:?}, got {lin_mid:?}"
    );
    assert!(near(cubic_quarter, [168, 64, 77, 255]), "CUBIC Catmull-Rom quarter phase must be [168,64,77,255], got {cubic_quarter:?}");
    // The midpoint must genuinely differ from both taps — proving interpolation, not a rounded nearest tap.
    assert!(
        !near(lin_mid, A) && !near(lin_mid, B),
        "LINEAR midpoint {lin_mid:?} must differ from both A {A:?} and B {B:?}"
    );
    eprintln!("demo `texture_filtering`: nearest={near_left:?}/{near_right:?} linear_mid={lin_mid:?} exact");
}

#[test]
fn mirror_clamp_reflects_once_for_native_and_cubic_filters() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove mirror-clamp sampling");
    let nearest = tap_address(
        &mut exec,
        Filter::Nearest,
        AddressMode::MirrorClampToEdge,
        -0.75,
    );
    let cubic_negative = tap_address(
        &mut exec,
        Filter::Cubic,
        AddressMode::MirrorClampToEdge,
        -0.375,
    );
    let cubic_positive = tap_address(
        &mut exec,
        Filter::Cubic,
        AddressMode::MirrorClampToEdge,
        0.375,
    );
    assert!(near(nearest, B), "mirror-clamp -0.75 must reflect to right texel: {nearest:?}");
    assert_eq!(cubic_negative, cubic_positive, "cubic samples must be symmetric across the mirror edge");
}

#[test]
fn clamp_to_border_blends_native_edges_and_cubic_taps() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default()).unwrap();
    assert_eq!(tap_address(&mut exec, Filter::Nearest, AddressMode::ClampToBorder, -1.0), [0; 4]);
    let edge = tap_address(&mut exec, Filter::Linear, AddressMode::ClampToBorder, 0.0);
    for (got, expected) in edge.into_iter().zip([100, 30, 20, 128]) {
        assert!((got as i16 - expected).abs() <= 1, "edge={edge:?}");
    }
    assert_eq!(tap_address(&mut exec, Filter::Cubic, AddressMode::ClampToBorder, -0.25), [0; 4]);
}
