//! DEMO 4 — mipmap LOD selection: a 2-mip texture sampled at an explicit LOD must select that mip's color.
//!
//! The texture has a base level (2×2, color M0) and a mip-1 level (1×1, color M1), each uploaded to its own
//! mip via `CopyBufferToTexture { mip }`. A fullscreen triangle samples it with `textureLod(..., lod)` and a
//! NEAREST mip filter into a 1×1 target:
//!   * lod = 0.0 → base mip → exactly M0.
//!   * lod = 1.0 → mip 1     → exactly M1.
//!
//! M0 and M1 are distinct, so reading M1 at lod=1 proves the sampler descended to the correct mip level
//! (and that the executor materialized more than one mip and uploaded to the non-base level).
//!
//! Materializing multiple mips + uploading to a non-base mip required backend changes (see `texture.rs`
//! `make_texture`/`write_region` and `submit.rs`'s `CopyBufferToTexture` mip handling).

mod gpu_harness;
use gpu_harness::*;

use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BufferDesc, ColorAttachment, RenderPipelineDesc,
    SamplerDesc, ShaderRef,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, texture_usage, AddressMode, Filter, LoadOp, Topology,
};
use hl_gpu::protocol::model::kernel::glsl_stage;
use hl_gpu::{Cmd, CommandBuffer, Enc, ShaderPayloadKind};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

const M0: [u8; 4] = [210, 50, 60, 255]; // base mip
const M1: [u8; 4] = [50, 200, 90, 255]; // mip 1

const VS: &str = r#"#version 460
void main() {
    vec2 p[3] = vec2[3](vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    gl_Position = vec4(p[gl_VertexIndex], 0.0, 1.0);
}
"#;
const FS: &str = r#"#version 460
layout(std140, set = 0, binding = 0) uniform U { vec4 p; } u; // p.xy = uv, p.z = lod
layout(set = 0, binding = 1) uniform texture2D t;
layout(set = 0, binding = 2) uniform sampler   s;
layout(location = 0) out vec4 o;
void main() { o = textureLod(sampler2D(t, s), u.p.xy, u.p.z); }
"#;

fn nearest_mip() -> SamplerDesc {
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

/// Sample the mipmapped texture at center uv with explicit `lod`; return the single readback pixel.
fn sample_lod(exec: &mut WgpuExecutor, lod: f32) -> [u8; 4] {
    let mut s = new_session(exec);
    let mip0: Vec<u8> = M0.iter().cycle().take(16).copied().collect(); // 2×2 of M0

    hl_gpu::runtime::submit(
        &mut s,
        exec,
        0,
        &[
            Cmd::CreateTexture(
                1,
                tex2d(1, 1, texture_usage::RENDER_TARGET | texture_usage::COPY_SRC),
            ),
            // A 2×2 texture with 2 mip levels.
            Cmd::CreateTexture(
                2,
                tex2d_mips(2, 2, 2, texture_usage::SAMPLED | texture_usage::COPY_DST),
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
                data: le_f32(&[0.5, 0.5, lod, 0.0]),
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
                data: mip0,
            },
            Cmd::CreateBuffer(
                3,
                BufferDesc {
                    size: 4,
                    usage: buffer_usage::COPY_SRC,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 3,
                offset: 0,
                data: M1.to_vec(),
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
            Cmd::CreateSampler(1, nearest_mip()),
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
                    // Upload each mip level to its own `mip` slot.
                    Enc::CopyBufferToTexture {
                        src: 2,
                        src_offset: 0,
                        bytes_per_row: 8,
                        dst: 2,
                        mip: 0,
                        width: 2,
                        height: 2,
                    },
                    Enc::CopyBufferToTexture {
                        src: 3,
                        src_offset: 0,
                        bytes_per_row: 4,
                        dst: 2,
                        mip: 1,
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
    .expect("the mipmapped LOD-sample draw must run cleanly");

    let img = exec.read_texture(&s.resources, 1).unwrap();
    [img[0], img[1], img[2], img[3]]
}

#[test]
fn explicit_lod_selects_the_matching_mip_level() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");

    let base = sample_lod(&mut exec, 0.0);
    let mip1 = sample_lod(&mut exec, 1.0);
    write_png("mipmap_lod0", 1, 1, &base);
    write_png("mipmap_lod1", 1, 1, &mip1);

    assert!(
        near(base, M0),
        "lod=0 must sample base mip {M0:?}, got {base:?}"
    );
    assert!(
        near(mip1, M1),
        "lod=1 must sample mip 1 {M1:?}, got {mip1:?}"
    );
    assert!(
        !near(base, mip1),
        "the two mip levels must differ for the demo to prove LOD selection"
    );
    eprintln!("demo `mipmap_lod`: lod0={base:?} (M0)  lod1={mip1:?} (M1) exact");
}
