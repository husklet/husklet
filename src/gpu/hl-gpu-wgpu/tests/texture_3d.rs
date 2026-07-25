//! DEMO 5 — 3D texture slice selection: sample a specific depth slice of a 3D texture and assert its texel.
//!
//! The protocol's `TextureDim` carries `D3` (a real volume) but not a 2D-array variant, so this demo uses a
//! 1×1×3 3D texture: three stacked slices S0/S1/S2, each a distinct color, uploaded as one volume via
//! `CopyBufferToTexture` (the whole-volume upload the backend now performs for a 3D destination). A
//! fullscreen triangle samples it with `textureLod(sampler3D, vec3(0.5, 0.5, w), 0)` and a NEAREST filter,
//! choosing `w = (slice + 0.5)/3` to land on each slice's center. Each of the three readbacks must equal the
//! corresponding slice's color exactly — a backend that ignored the third texture dimension (allocating a
//! flat 2D texture) could not distinguish the slices.
//!
//! Materializing a `D3` texture + uploading every slice required backend changes (see `texture.rs`
//! `make_texture`/`write_region` and `submit.rs`'s `CopyBufferToTexture` volume handling).

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

const SLICES: [[u8; 4]; 3] = [
    [200, 40, 40, 255], // slice 0
    [40, 200, 40, 255], // slice 1
    [40, 40, 200, 255], // slice 2
];

const VS: &str = r#"#version 460
void main() {
    vec2 p[3] = vec2[3](vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    gl_Position = vec4(p[gl_VertexIndex], 0.0, 1.0);
}
"#;
const FS: &str = r#"#version 460
layout(std140, set = 0, binding = 0) uniform U { vec4 uvw; } u;
layout(set = 0, binding = 1) uniform texture3D t;
layout(set = 0, binding = 2) uniform sampler   s;
layout(location = 0) out vec4 o;
void main() { o = textureLod(sampler3D(t, s), u.uvw.xyz, 0.0); }
"#;

fn nearest() -> SamplerDesc {
    SamplerDesc {
        min_filter: Filter::Nearest,
        mag_filter: Filter::Nearest,
        mip_filter: Filter::Nearest,
        address_u: AddressMode::ClampToEdge,
        address_v: AddressMode::ClampToEdge,
        address_w: AddressMode::ClampToEdge,
    }
}

/// Sample the 1×1×3 volume at depth coordinate `w`; return the single readback pixel.
fn sample_slice(exec: &mut WgpuExecutor, w: f32) -> [u8; 4] {
    let mut s = new_session(exec);
    let volume: Vec<u8> = SLICES.iter().flatten().copied().collect(); // 3 stacked texels

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
                tex3d(1, 1, 3, texture_usage::SAMPLED | texture_usage::COPY_DST),
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
                data: le_f32(&[0.5, 0.5, w, 0.0]),
            },
            Cmd::CreateBuffer(
                2,
                BufferDesc {
                    size: 12,
                    usage: buffer_usage::COPY_SRC,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 2,
                offset: 0,
                data: volume,
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
                    // One copy fills the whole 3-slice volume (the dst is 3D).
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
    .expect("the 3D-slice sample draw must run cleanly");

    let img = exec.read_texture(&s.resources, 1).unwrap();
    [img[0], img[1], img[2], img[3]]
}

#[test]
fn sampling_a_depth_slice_returns_that_slices_texel() {
    let mut exec = match WgpuExecutor::new(DeviceConfig::default()) {
        Ok(e) => e,
        Err(_) => return,
    };

    for (slice, &want) in SLICES.iter().enumerate() {
        let w = (slice as f32 + 0.5) / 3.0;
        let got = sample_slice(&mut exec, w);
        write_png(&format!("texture3d_slice{slice}"), 1, 1, &got);
        assert!(
            near(got, want),
            "slice {slice} (w={w}): must sample {want:?}, got {got:?}"
        );
    }
    eprintln!("demo `texture_3d`: all 3 slices sampled to their exact texels");
}
