use super::*;

const T1_DRAWS: usize = 2000;
const T1_GX: u32 = 50;
const T1_GY: u32 = 40; // 50*40 = 2000 cells
const T1_CELL: u32 = 4;
const T1_W: u32 = T1_GX * T1_CELL; // 200
const T1_H: u32 = T1_GY * T1_CELL; // 160

pub(super) const T1_VS: &str = r#"#version 460
layout(location = 0) in vec2 pos;
layout(location = 1) in vec4 color;
layout(location = 0) flat out vec4 vcol;
void main() { gl_Position = vec4(pos, 0.0, 1.0); vcol = color; }
"#;
pub(super) const T1_FS: &str = r#"#version 460
layout(location = 0) flat in vec4 vcol;
layout(location = 0) out vec4 o;
void main() { o = vcol; }
"#;

/// Deterministic, well-separated per-draw color (as exact bytes) for draw index `i`.
fn t1_color(i: usize) -> [u8; 4] {
    let r = 30 + (i % 20) as u32 * 11; // 30..239
    let g = 30 + ((i / 20) % 10) as u32 * 20; // 30..210
    let b = 40 + (i % 7) as u32 * 30; // 40..220
    [r as u8, g as u8, b as u8, 255]
}

#[test]
fn many_draws_one_frame() {
    let mut exec = match try_exec() {
        Some(e) => e,
        None => return,
    };
    let mut s = new_session(&exec);

    // Build the shared vertex buffer: NUM_DRAWS quads × 4 verts × (vec2 pos + vec4 color) = 24 B/vert.
    let mut verts: Vec<f32> = Vec::with_capacity(T1_DRAWS * 4 * 6);
    for i in 0..T1_DRAWS {
        let gx = (i as u32) % T1_GX;
        let gy = (i as u32) / T1_GX;
        let (px0, px1) = (gx * T1_CELL, (gx + 1) * T1_CELL);
        let (py0, py1) = (gy * T1_CELL, (gy + 1) * T1_CELL);
        let ndc_x = |p: u32| p as f32 / T1_W as f32 * 2.0 - 1.0;
        let ndc_y = |p: u32| 1.0 - p as f32 / T1_H as f32 * 2.0; // y-up NDC → y-down framebuffer
        let (x0, x1) = (ndc_x(px0), ndc_x(px1));
        let (yt, yb) = (ndc_y(py0), ndc_y(py1)); // top / bottom
        let c = t1_color(i);
        let cf = [
            c[0] as f32 / 255.0,
            c[1] as f32 / 255.0,
            c[2] as f32 / 255.0,
            1.0,
        ];
        // Triangle strip: TL, TR, BL, BR.
        for &(x, y) in &[(x0, yt), (x1, yt), (x0, yb), (x1, yb)] {
            verts.extend_from_slice(&[x, y]);
            verts.extend_from_slice(&cf);
        }
    }
    let vbytes: Vec<u8> = verts.iter().flat_map(|f| f.to_le_bytes()).collect();

    // Setup submit: target, vertex buffer, shaders, pipeline. Timed separately from the draw frame.
    hl_gpu::runtime::submit(
        &mut s,
        &mut exec,
        0,
        &[
            Cmd::CreateTexture(
                1,
                tex2d(
                    T1_W,
                    T1_H,
                    texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
                ),
            ),
            Cmd::CreateBuffer(
                1,
                BufferDesc {
                    size: vbytes.len() as u64,
                    usage: buffer_usage::VERTEX,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: vbytes,
            },
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::VERTEX, "vmain", T1_VS),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::FRAGMENT, "fmain", T1_FS),
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
                    vertex_buffers: vec![VertexLayout {
                        stride: 24,
                        step_mode: 0,
                        attrs: vec![
                            VertexAttr {
                                location: 0,
                                format: VFMT_F32X2,
                                offset: 0,
                            },
                            VertexAttr {
                                location: 1,
                                format: VFMT_F32X4,
                                offset: 8,
                            },
                        ],
                    }],
                    color_targets: vec![color_target()],
                    depth: None,
                    topology: Topology::TriangleStrip,
                    cull: 0,
                    front_face: 0,
                    sample_count: 1,
                    label: String::new(),
                },
            ),
        ],
    )
    .expect("many-draws setup must run cleanly");

    // The frame: one render pass, NUM_DRAWS individual Draw ops each pulling its quad at `first_vertex=i*4`.
    let mut encoder: Vec<Enc> = Vec::with_capacity(T1_DRAWS + 4);
    encoder.push(Enc::BeginRenderPass {
        color: vec![ColorAttachment {
            texture: 1,
            load: LoadOp::Clear,
            clear: [0.0, 0.0, 0.0, 1.0],
            store: true,
        }],
        depth: None,
    });
    encoder.push(Enc::SetPipeline(1));
    encoder.push(Enc::SetVertexBuffer {
        slot: 0,
        buffer: 1,
        offset: 0,
    });
    for i in 0..T1_DRAWS {
        encoder.push(Enc::Draw {
            vertex_count: 4,
            instance_count: 1,
            first_vertex: (i as u32) * 4,
            first_instance: 0,
        });
    }
    encoder.push(Enc::EndRenderPass);
    let frame = vec![Cmd::Submit(CommandBuffer {
        encoder,
        signal: None,
    })];

    let t0 = Instant::now();
    hl_gpu::runtime::submit(&mut s, &mut exec, 0, &frame)
        .expect("many-draws frame must run cleanly");
    let submit_s = t0.elapsed().as_secs_f64();

    let t1 = Instant::now();
    let px_buf = exec.read_texture(&s.resources, 1).unwrap();
    let readback_s = t1.elapsed().as_secs_f64();

    // FULL exact-pixel check: every pixel must equal its own cell's draw color.
    let mut bad = 0usize;
    let mut first: Option<(u32, u32, [u8; 4], [u8; 4])> = None;
    for py in 0..T1_H {
        for pxx in 0..T1_W {
            let i = (py / T1_CELL) * T1_GX + (pxx / T1_CELL);
            let want = t1_color(i as usize);
            let got = px(&px_buf, T1_W, pxx, py);
            if !near_tol(got, want, 2) {
                bad += 1;
                if first.is_none() {
                    first = Some((pxx, py, got, want));
                }
            }
        }
    }
    let draws_per_sec = T1_DRAWS as f64 / submit_s;
    println!(
        "[scale] many_draws_one_frame: {T1_DRAWS} draws into {T1_W}x{T1_H} in {:.3}s submit + {:.3}s readback \
         => {:.0} draws/s ({:.3} ms/frame full)",
        submit_s,
        readback_s,
        draws_per_sec,
        (submit_s + readback_s) * 1e3,
    );
    assert_eq!(
        bad, 0,
        "many_draws: {bad} wrong pixels; first {:?} — a draw was dropped or mis-placed",
        first
    );

    // Property: the whole frame (2000 draws + readback) completes well under a generous ceiling. Observed
    // debug time is a fraction of a second on lavapipe; 30 s is ~50x+ headroom yet trips an O(n²) blowup.
    let ceil_s = env_f64("HL_SCALE_FRAME_CEIL_S", 30.0);
    assert!(
        submit_s + readback_s <= ceil_s,
        "many_draws frame took {:.3}s > {:.3}s ceiling",
        submit_s + readback_s,
        ceil_s,
    );
}

// ===================================================================================================
// Test 2 — MANY PIPELINES: hundreds of distinct render pipelines, each correct, no create-time blowup.
// ===================================================================================================
//
// Each pipeline pairs the shared fullscreen-triangle VS with a UNIQUE fragment shader baking a per-index
// color (distinct source → distinct naga compile → distinct PSO), alternating topology to also vary state.
// We TIME each create and assert the create rate does not collapse as the resident pipeline count grows
// (a per-create O(n) scan would show up as a steadily climbing create time → super-linear total). Then we
// draw with EVERY pipeline and assert its exact baked color, proving all N are individually correct.
