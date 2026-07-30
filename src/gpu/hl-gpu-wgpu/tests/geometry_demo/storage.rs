use super::grid::{assert_grid, FLAT_COLOR_FS, GRID};
use super::*;
// ---------------------------------------------------------------------------------------------------
// demo 2 — instanced unit-quad from vertex_index, per-instance geometry from a set-1 STORAGE buffer
// ---------------------------------------------------------------------------------------------------

// THE GPUI repro. No vertex buffer. The unit quad is synthesized from gl_VertexIndex; each instance's
// rectangle (center.xy, half.zw, NDC) is read from the set-1 read-only STORAGE buffer at gl_InstanceIndex;
// a set-0 uniform (identity viewport scale, the GPUI "globals") is applied; the per-instance color is
// picked from gl_InstanceIndex and forwarded flat. If the storage stride/offset or the instance_index
// wiring is wrong, a quad renders at the origin or huge — the exact degenerate geometry the screenshot bug
// showed.
const STORAGE_VS: &str = r#"#version 460
layout(std140, set = 0, binding = 0) uniform Globals { vec2 scale; } g;
layout(std430, set = 1, binding = 0) readonly buffer Quads { vec4 quads[]; };
layout(location = 0) flat out vec4 vColor;
void main() {
    vec4 q = quads[gl_InstanceIndex];
    vec2 corner = vec2(float(gl_VertexIndex & 1), float((gl_VertexIndex >> 1) & 1)) * 2.0 - 1.0;
    vec2 pos = (q.xy + corner * q.zw) * g.scale;
    vec4 pal[4] = vec4[4](
        vec4(220.0/255.0, 40.0/255.0, 40.0/255.0, 1.0),
        vec4(40.0/255.0, 200.0/255.0, 60.0/255.0, 1.0),
        vec4(50.0/255.0, 90.0/255.0, 230.0/255.0, 1.0),
        vec4(240.0/255.0, 240.0/255.0, 240.0/255.0, 1.0)
    );
    vColor = pal[gl_InstanceIndex];
    gl_Position = vec4(pos, 0.0, 1.0);
}
"#;

#[test]
fn instanced_vertex_index_quads_from_storage() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");
    let mut s = new_session(&exec);

    let quads: Vec<f32> = GRID.iter().flatten().copied().collect(); // 4×vec4
    let scale: [f32; 2] = [1.0, 1.0];

    hl_gpu::runtime::submit(
        &mut s,
        &mut exec,
        0,
        &[
            Cmd::CreateTexture(
                1,
                tex(W, H, texture_usage::RENDER_TARGET | texture_usage::COPY_SRC),
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
                data: le_f32(&scale),
            },
            Cmd::CreateBuffer(
                2,
                BufferDesc {
                    size: 64,
                    usage: buffer_usage::STORAGE,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 2,
                offset: 0,
                data: le_f32(&quads),
            },
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::VERTEX, "vmain", STORAGE_VS),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::FRAGMENT, "fmain", FLAT_COLOR_FS),
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
                    topology: Topology::TriangleStrip,
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
                            size: 8,
                        },
                    }],
                },
            ),
            Cmd::CreateBindGroup(
                2,
                BindGroupDesc {
                    set: 1,
                    entries: vec![BindEntry {
                        binding: 0,
                        resource: BindResource::Buffer {
                            id: 2,
                            offset: 0,
                            size: 64,
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
                    Enc::SetBindGroup { index: 1, group: 2 },
                    Enc::Draw {
                        vertex_count: 4,
                        instance_count: 4,
                        first_vertex: 0,
                        first_instance: 0,
                    },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
        ],
    )
    .expect("the storage-fed instanced grid draw must run cleanly");

    let px = exec.read_texture(&s.resources, 1).unwrap();
    assert_grid("instanced_storage_grid", &px);
}

// ---------------------------------------------------------------------------------------------------
// demo 3 — the same grid, per-instance data from a step_mode=Instance VERTEX BUFFER
// ---------------------------------------------------------------------------------------------------

// Identical geometry, but each instance's rectangle arrives through a per-instance vertex buffer attribute
// (location 0, a vec4) instead of a storage buffer — isolating the storage path from the vertex-buffer
// path. Same corner-from-vertex_index quad, same instance_index→color, so a divergence between this and
// demo 2 would localize the bug to one of the two per-instance data routes.
