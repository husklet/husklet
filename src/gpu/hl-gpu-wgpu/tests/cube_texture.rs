//! DEMO — cube-map face selection: sample a cube texture by a direction vector and assert the texel of the
//! face that direction selects.
//!
//! The protocol's `TextureDim::Cube` must materialize a **6-layer 2D** wgpu texture whose default view uses
//! `TextureViewDimension::Cube` — anything else and the `samplerCube` bind-group binding wgpu builds from the
//! shader's auto layout (which expects a Cube view) is REJECTED at draw. Before the fix, `make_texture`
//! collapsed `Cube` to a plain single-layer 2D texture, so this draw could not even bind (and, had it bound,
//! only one face's data existed). The six 1×1 faces get six distinct colors, uploaded as one 6-slice volume
//! via `CopyBufferToTexture` (origin.z = face). For each face we sample with a direction pointing straight
//! down that face's major axis; with NEAREST + 1×1 faces the in-face UV is irrelevant, so the readback must
//! equal that face's exact color.
//!
//! Cube layer order is the WebGPU/Vulkan face order: +X, -X, +Y, -Y, +Z, -Z (layers 0..6).

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

/// Six faces in cube layer order (+X, -X, +Y, -Y, +Z, -Z), each a distinct color.
const FACES: [[u8; 4]; 6] = [
    [220, 30, 30, 255],  // +X  red
    [30, 220, 30, 255],  // -X  green
    [30, 30, 220, 255],  // +Y  blue
    [220, 220, 30, 255], // -Y  yellow
    [220, 30, 220, 255], // +Z  magenta
    [30, 220, 220, 255], // -Z  cyan
];

/// A direction pointing straight down each face's major axis (same layer order as `FACES`).
const DIRS: [[f32; 3]; 6] = [
    [1.0, 0.0, 0.0],
    [-1.0, 0.0, 0.0],
    [0.0, 1.0, 0.0],
    [0.0, -1.0, 0.0],
    [0.0, 0.0, 1.0],
    [0.0, 0.0, -1.0],
];

const VS: &str = r#"#version 460
void main() {
    vec2 p[3] = vec2[3](vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    gl_Position = vec4(p[gl_VertexIndex], 0.0, 1.0);
}
"#;
const FS: &str = r#"#version 460
layout(std140, set = 0, binding = 0) uniform U { vec4 dir; } u;
layout(set = 0, binding = 1) uniform textureCube t;
layout(set = 0, binding = 2) uniform sampler     s;
layout(location = 0) out vec4 o;
void main() { o = texture(samplerCube(t, s), u.dir.xyz); }
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

/// A 1×1×6 `Rgba8Unorm` cube texture descriptor (`depth` carries the 6 faces).
fn tex_cube(usage: u32) -> TextureDesc {
    TextureDesc {
        width: 1,
        height: 1,
        depth: 6,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::Cube,
        format: TextureFormat::Rgba8Unorm,
        usage,
        label: String::new(),
    }
}

/// Sample the cube along `dir`; return the single readback pixel.
fn sample_dir(exec: &mut WgpuExecutor, dir: [f32; 3]) -> [u8; 4] {
    let mut s = new_session(exec);
    let faces: Vec<u8> = FACES.iter().flatten().copied().collect(); // 6 stacked texels

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
                tex_cube(texture_usage::SAMPLED | texture_usage::COPY_DST),
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
                data: le_f32(&[dir[0], dir[1], dir[2], 0.0]),
            },
            Cmd::CreateBuffer(
                2,
                BufferDesc {
                    size: 24,
                    usage: buffer_usage::COPY_SRC,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 2,
                offset: 0,
                data: faces,
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
                    // One copy fills all 6 faces (the dst is a 6-layer cube).
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
    .expect("the cube-face sample draw must run cleanly");

    let img = exec.read_texture(&s.resources, 1).unwrap();
    [img[0], img[1], img[2], img[3]]
}

#[test]
fn sampling_a_direction_returns_that_faces_texel() {
    let mut exec = match WgpuExecutor::new(DeviceConfig::default()) {
        Ok(e) => e,
        Err(_) => return,
    };

    for (face, &want) in FACES.iter().enumerate() {
        let dir = DIRS[face];
        let got = sample_dir(&mut exec, dir);
        write_png(&format!("cube_face{face}"), 1, 1, &got);
        assert!(
            near(got, want),
            "face {face} (dir={dir:?}): must sample {want:?}, got {got:?}"
        );
    }
    eprintln!("demo `cube_texture`: all 6 cube faces sampled to their exact texels");
}
