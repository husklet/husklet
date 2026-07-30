use super::*;

fn mat2_std140_bytes(col0: [f32; 2], col1: [f32; 2]) -> Vec<u8> {
    [col0[0], col0[1], 0.0, 0.0, col1[0], col1[1], 0.0, 0.0]
        .iter()
        .flat_map(|f| f.to_le_bytes())
        .collect()
}

// Unit quad in LOCAL space [0,1]² from gl_VertexID (a 4-vertex triangle strip), transformed by a `mat2` in a
// std140 UBO. GLSL-ES (`gl_VertexID`) so it takes the normalize path; the mat2 member is the construct under
// test. `x.m2 * local` = col0*local.x + col1*local.y, so a swapped/mis-padded column visibly mis-maps.
const MAT2_VS: &str = r#"#version 300 es
precision highp float;
layout(std140, binding = 0) uniform Xf { mat2 m2; } x;
void main() {
    vec2 local = vec2(float(gl_VertexID & 1), float((gl_VertexID >> 1) & 1));
    gl_Position = vec4(x.m2 * local, 0.0, 1.0);
}
"#;

const MAT2_FILL_FS: &str = r#"#version 300 es
precision highp float;
layout(location = 0) out vec4 o;
void main() { o = vec4(0.2, 0.8, 0.4, 1.0); }
"#;

/// Draw the unit quad transformed by `mat2(col0, col1)` (std140 UBO at binding 0) over an 8×8 target.
fn draw_mat2_quad(exec: &mut WgpuExecutor, col0: [f32; 2], col1: [f32; 2]) -> Vec<u8> {
    let mut s = new_sess(exec);
    hl_gpu::runtime::submit(
        &mut s,
        exec,
        0,
        &[
            Cmd::CreateTexture(1, rt(W, H)),
            Cmd::CreateBuffer(
                1,
                BufferDesc {
                    size: 32,
                    usage: buffer_usage::UNIFORM | buffer_usage::COPY_DST,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: mat2_std140_bytes(col0, col1),
            },
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::VERTEX, "vmain", MAT2_VS),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::FRAGMENT, "fmain", MAT2_FILL_FS),
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
                    color_targets: vec![ct()],
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
                            size: 32,
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
    .expect("mat2 std140 UBO quad draw must run cleanly");
    exec.read_texture(&s.resources, 1).unwrap()
}

/// The EXACT covered-pixel mask (8×8, readback row order) for a quad occupying the wgpu NDC rect
/// `x∈[nx0,nx1]`, `y∈[ny0,ny1]` (y-up), by the standard center-sample rule. Edges are integer, centers are
/// half-integers, so coverage is unambiguous.
fn ndc_rect_mask(nx0: f32, nx1: f32, ny0: f32, ny1: f32) -> Vec<bool> {
    let fx = |n: f32| (n + 1.0) / 2.0 * W as f32;
    let fy = |n: f32| (1.0 - n) / 2.0 * H as f32; // y-up NDC → y-down framebuffer row
    let (x0, x1) = (fx(nx0.min(nx1)), fx(nx0.max(nx1)));
    let (y0, y1) = (fy(ny0.max(ny1)), fy(ny0.min(ny1)));
    let mut m = vec![false; (W * H) as usize];
    for py in 0..H {
        for px in 0..W {
            let (cx, cy) = (px as f32 + 0.5, py as f32 + 0.5);
            m[(py * W + px) as usize] = cx > x0 && cx < x1 && cy > y0 && cy < y1;
        }
    }
    m
}

fn assert_mat2_coverage(label: &str, px: &[u8], mask: &[bool]) {
    let fill = [51u8, 204, 102, 255]; // vec4(0.2,0.8,0.4,1.0) * 255
    let clear = [0u8, 0, 0, 255];
    for i in 0..(W * H) as usize {
        let p = [px[i * 4], px[i * 4 + 1], px[i * 4 + 2], px[i * 4 + 3]];
        let want = if mask[i] { fill } else { clear };
        assert!(
            approx(p, want, 2),
            "{label}: pixel ({},{}) got {p:?} want {want:?} (mat2 must map the quad EXACTLY)",
            i as u32 % W,
            i as u32 / W,
        );
    }
}

#[test]
fn es_mat2_std140_ubo_transforms_quad_to_exact_pixels() {
    let mut guard = exec();
    let exec = &mut *guard;

    // Case 1 — 90° CCW rotation about origin: (x,y) → (-y,x). Column-major mat2*v = col0*v.x + col1*v.y, so
    // col0=(0,1), col1=(-1,0). The unit quad [0,1]² maps to NDC x∈[-1,0], y∈[0,1] = the TOP-LEFT quadrant.
    // Off-diagonal terms only — a transpose or a mis-padded second column would land it elsewhere.
    let px = draw_mat2_quad(exec, [0.0, 1.0], [-1.0, 0.0]);
    assert_mat2_coverage("rotate90", &px, &ndc_rect_mask(-1.0, 0.0, 0.0, 1.0));

    // Case 2 — non-uniform scale diag(0.5, 0.75): col0=(0.5,0), col1=(0,0.75). The quad maps to NDC
    // x∈[0,0.5], y∈[0,0.75]. Diagonal terms — proves col0.x and col1.y are read at the right std140 offsets.
    let px = draw_mat2_quad(exec, [0.5, 0.0], [0.0, 0.75]);
    assert_mat2_coverage("scale", &px, &ndc_rect_mask(0.0, 0.5, 0.0, 0.75));
}
