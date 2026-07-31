//! std140 uniform ARRAYS of scalars and 2-component vectors — declared, compiled, and READ AT THE RIGHT
//! BYTES.
//!
//! `uniform float u[4]` (and `int[N]`, `vec2[N]`) is an ordinary GLES declaration the GL driver collects
//! into its anonymous `HlUniforms` block. naga's `glsl-in` types it with the ELEMENT's natural stride —
//! `array<f32, 4>`, stride 4 — but WGSL's uniform address space requires a stride that is a multiple of 16,
//! which is also exactly what std140 mandates and what the driver's own writes use
//! (`esz + 15 & !15`). wgpu therefore refused every such module:
//!
//!     Alignment requirements for address space Uniform are not met by [2]
//!     The array stride 4 is not a multiple of the required alignment 16
//!
//! `glsl_es::pad_std140_arrays` rewrites those members to arrays of 4-component vectors and swizzles the
//! value back at each use, which is a NO-OP on the bytes: element `i` still lives at `16 * i`. This test
//! writes a uniform buffer with the std140 layout the driver produces and asserts the drawn pixel carries
//! the values from the elements the shader indexed — so a rewrite that compiled but read the wrong element
//! (or the wrong component) fails here rather than passing as "it compiles now".

mod gpu_harness;
use gpu_harness::{color_target, glsl, near, new_session, px, tex2d};

use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BufferDesc, ColorAttachment, RenderPipelineDesc,
    ShaderRef,
};
use hl_gpu::protocol::model::enums::{buffer_usage, texture_usage, LoadOp, Topology};
use hl_gpu::protocol::model::kernel::glsl_stage;
use hl_gpu::{Cmd, CommandBuffer, Enc, ShaderPayloadKind};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

const W: u32 = 8;
const H: u32 = 8;

const VS: &str = r#"#version 460
void main() {
    vec2 p[3] = vec2[3](vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    gl_Position = vec4(p[gl_VertexIndex], 0.0, 1.0);
}
"#;

/// The driver's shape: an ANONYMOUS `std140` block whose members are globals, holding a scalar array, an
/// integer array and a `vec2` array. Each output channel names a DIFFERENT element/component, so reading
/// the wrong one changes the pixel.
const FS: &str = r#"#version 460
layout(std140, binding = 0) uniform HlUniforms {
    float u[4];
    int k[2];
    vec2 v[2];
};
layout(location = 0) out vec4 o;
void main() {
    o = vec4(u[1], v[1].x, v[0].y, float(k[1]) / 255.0);
}
"#;

/// The std140 bytes the GL driver uploads for that block: every array element occupies 16 bytes, so the
/// element stride is 16 regardless of the element type.
///
/// `u` at 0 (0, 16, 32, 48), `k` at 64 (64, 80), `v` at 96 (96, 112). 128 bytes total.
fn uniform_bytes() -> Vec<u8> {
    let mut bytes = vec![0u8; 128];
    let mut put_f32 = |offset: usize, value: f32| {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    };
    put_f32(0, 1.0); // u[0] — NOT read; a pass that reads element 0 fails here
    put_f32(16, 0.25); // u[1]
    put_f32(32, 1.0); // u[2]
    put_f32(48, 1.0); // u[3]
    put_f32(96, 1.0); // v[0].x — NOT read
    put_f32(100, 0.75); // v[0].y
    put_f32(112, 0.5); // v[1].x
    put_f32(116, 1.0); // v[1].y — NOT read
    bytes[64..68].copy_from_slice(&7i32.to_le_bytes()); // k[0] — NOT read
    bytes[80..84].copy_from_slice(&128i32.to_le_bytes()); // k[1]
    bytes
}

#[test]
fn std140_scalar_and_vec2_uniform_arrays_read_their_own_elements() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");
    let mut session = new_session(&exec);

    hl_gpu::runtime::submit(
        &mut session,
        &mut exec,
        0,
        &[
            Cmd::CreateTexture(
                1,
                tex2d(W, H, texture_usage::RENDER_TARGET | texture_usage::COPY_SRC),
            ),
            Cmd::CreateBuffer(
                1,
                BufferDesc {
                    size: 128,
                    usage: buffer_usage::UNIFORM,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: uniform_bytes(),
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
                    entries: vec![BindEntry {
                        binding: 0,
                        resource: BindResource::Buffer {
                            id: 1,
                            offset: 0,
                            size: 128,
                        },
                    }],
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
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
    .expect("a std140 scalar/vec2 uniform array must compile and draw");

    let pixels = exec.read_texture(&session.resources, 1).unwrap();
    // u[1]=0.25, v[1].x=0.5, v[0].y=0.75, k[1]/255=128/255.
    let expected = [64, 128, 191, 128];
    for y in 0..H {
        for x in 0..W {
            let got = px(&pixels, W, x, y);
            assert!(
                near(got, expected),
                "pixel ({x},{y}) is {got:?}, expected {expected:?} — a uniform array element or \
                 component was read from the wrong std140 offset"
            );
        }
    }
}
