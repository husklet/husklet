use super::*;

const MVP_VS: &str = r#"#version 460
layout(std140, set = 0, binding = 0) uniform MVP { mat4 mvp; } u;
void main() {
    vec2 local = vec2(float(gl_VertexIndex & 1), float((gl_VertexIndex >> 1) & 1));
    gl_Position = u.mvp * vec4(local, 0.0, 1.0);
}
"#;

// Fragment: a constant color supplied per draw via a second set-0 uniform (so the fill color is data, not a
// baked constant — proving the covered pixels are the drawn quad).
const CONST_COLOR_FS: &str = r#"#version 460
layout(std140, set = 0, binding = 1) uniform Tint { vec4 color; } t;
layout(location = 0) out vec4 o;
void main() { o = t.color; }
"#;

fn run_mvp_case(exec: &mut WgpuExecutor, mvp: [f32; 16], color: [u8; 4]) -> Vec<u8> {
    let mut s = new_session(exec);
    let col_f = [
        color[0] as f32 / 255.0,
        color[1] as f32 / 255.0,
        color[2] as f32 / 255.0,
        1.0,
    ];
    hl_gpu::runtime::submit(
        &mut s,
        exec,
        0,
        &[
            Cmd::CreateTexture(
                1,
                tex(W, H, texture_usage::RENDER_TARGET | texture_usage::COPY_SRC),
            ),
            // set 0, binding 0: the mat4 MVP (64 bytes, std140).
            Cmd::CreateBuffer(
                1,
                BufferDesc {
                    size: 64,
                    usage: buffer_usage::UNIFORM,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: le_f32(&mvp),
            },
            // set 0, binding 1: the tint (16 bytes, std140 vec4).
            Cmd::CreateBuffer(
                2,
                BufferDesc {
                    size: 16,
                    usage: buffer_usage::UNIFORM,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 2,
                offset: 0,
                data: le_f32(&col_f),
            },
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::VERTEX, "vmain", MVP_VS),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::FRAGMENT, "fmain", CONST_COLOR_FS),
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
                    entries: vec![
                        BindEntry {
                            binding: 0,
                            resource: BindResource::Buffer {
                                id: 1,
                                offset: 0,
                                size: 64,
                            },
                        },
                        BindEntry {
                            binding: 1,
                            resource: BindResource::Buffer {
                                id: 2,
                                offset: 0,
                                size: 16,
                            },
                        },
                    ],
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
                        vertex_count: 4,
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
    .expect("the MVP-transformed quad draw must run cleanly");
    exec.read_texture(&s.resources, 1).unwrap()
}

#[test]
fn transform_quad_lands_at_known_rect() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");

    // Each case: an affine → the LOCAL [0,1]² quad's NDC rect → the exact expected framebuffer coverage.
    // identity      → NDC x[0,1] y[0,1]      = top-right quadrant
    // scale 0.5     → NDC x[0,0.5] y[0,0.5]  = a quarter block hugging the center, in the top-right
    // translate     → NDC x[-1,0] y[-1,0]    = bottom-left quadrant
    // rotate 90° CCW→ NDC x[-1,0] y[0,1]     = top-left quadrant

    // identity
    let m = run_mvp_case(&mut exec, affine_mat4(1.0, 0.0, 0.0, 1.0, 0.0, 0.0), RED);
    let mask = covered_mask(&[ndc_to_fb(0.0, 1.0, 0.0, 1.0)]);
    assert_exact_and_write("transform_identity", &m, &mask, RED);

    // uniform scale 0.5 about the origin
    let m = run_mvp_case(&mut exec, affine_mat4(0.5, 0.0, 0.0, 0.5, 0.0, 0.0), GREEN);
    let mask = covered_mask(&[ndc_to_fb(0.0, 0.5, 0.0, 0.5)]);
    assert_exact_and_write("transform_scale", &m, &mask, GREEN);

    // translate by (-1,-1): the [0,1]² quad moves to [-1,0]²
    let m = run_mvp_case(&mut exec, affine_mat4(1.0, 0.0, 0.0, 1.0, -1.0, -1.0), BLUE);
    let mask = covered_mask(&[ndc_to_fb(-1.0, 0.0, -1.0, 0.0)]);
    assert_exact_and_write("transform_translate", &m, &mask, BLUE);

    // rotate 90° CCW about origin: (x,y) → (-y, x). The [0,1]² quad maps to x[-1,0], y[0,1].
    let m = run_mvp_case(&mut exec, affine_mat4(0.0, -1.0, 1.0, 0.0, 0.0, 0.0), WHITE);
    let mask = covered_mask(&[ndc_to_fb(-1.0, 0.0, 0.0, 1.0)]);
    assert_exact_and_write("transform_rotate90", &m, &mask, WHITE);
}

// ---------------------------------------------------------------------------------------------------
// the 2×2 grid shared by demos 2 & 3
// ---------------------------------------------------------------------------------------------------

// Per-instance rectangles as (center.x, center.y, half.x, half.y) in NDC. Four cells of a 2×2 grid, each a
// half-extent of 0.25 with centers at (±0.5, ±0.5): distinct, separated blocks (a clear gutter between
// them), so a collapsed/huge/mis-placed instance is unmistakable.
