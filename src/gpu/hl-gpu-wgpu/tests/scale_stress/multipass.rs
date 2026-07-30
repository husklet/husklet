use super::*;

const T4_PASSES: usize = 50;
// Per-pass delta in 1/255 units → after 50 passes: (250, 150, 100). All < 255 (no clamp).
const T4_DR: u32 = 5;
const T4_DG: u32 = 3;
const T4_DB: u32 = 2;

const T4_VS: &str = r#"#version 460
layout(location = 0) out vec2 vuv;
void main() {
    vec2 p[3] = vec2[3](vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    gl_Position = vec4(p[gl_VertexIndex], 0.0, 1.0);
    vuv = vec2(0.5, 0.5);
}
"#;

fn t4_fs() -> String {
    format!(
        "#version 460\nlayout(location = 0) in vec2 vuv;\n\
         layout(set = 0, binding = 0) uniform texture2D t;\n\
         layout(set = 0, binding = 1) uniform sampler s;\n\
         layout(location = 0) out vec4 o;\n\
         void main() {{ o = texture(sampler2D(t, s), vuv) + vec4({}.0/255.0, {}.0/255.0, {}.0/255.0, 0.0); }}\n",
        T4_DR, T4_DG, T4_DB,
    )
}

#[test]
fn deep_multipass() {
    let mut exec = match try_exec() {
        Some(e) => e,
        None => return,
    };
    let mut s = new_session(&exec);

    let usage = texture_usage::RENDER_TARGET
        | texture_usage::SAMPLED
        | texture_usage::COPY_SRC
        | texture_usage::COPY_DST;

    // texA = id 2, texB = id 3, sampler = id 1, VS = 1, FS = 2, pipeline = 1, bgA (samples A) = 1, bgB = 2.
    let mut encoder: Vec<Enc> = Vec::with_capacity(T4_PASSES * 4 + 2);
    // Seed A = (0,0,0,255) with a clear-only pass.
    encoder.push(Enc::BeginRenderPass {
        color: vec![ColorAttachment {
            texture: 2,
            load: LoadOp::Clear,
            clear: [0.0, 0.0, 0.0, 1.0],
            store: true,
        }],
        depth: None,
    });
    encoder.push(Enc::EndRenderPass);
    for k in 0..T4_PASSES {
        // Even pass: src = A (bg 1), dst = B (id 3). Odd: src = B (bg 2), dst = A (id 2).
        let (bg, dst) = if k % 2 == 0 {
            (1u32, 3u32)
        } else {
            (2u32, 2u32)
        };
        encoder.push(Enc::BeginRenderPass {
            color: vec![ColorAttachment {
                texture: dst,
                load: LoadOp::Clear,
                clear: [0.0, 0.0, 0.0, 1.0],
                store: true,
            }],
            depth: None,
        });
        encoder.push(Enc::SetPipeline(1));
        encoder.push(Enc::SetBindGroup {
            index: 0,
            group: bg,
        });
        encoder.push(Enc::Draw {
            vertex_count: 3,
            instance_count: 1,
            first_vertex: 0,
            first_instance: 0,
        });
        encoder.push(Enc::EndRenderPass);
    }
    // After 50 (even) passes the LAST write (k=49, odd) lands in A (id 2).
    let final_tex = 2u32;

    let t0 = Instant::now();
    hl_gpu::runtime::submit(
        &mut s,
        &mut exec,
        0,
        &[
            Cmd::CreateTexture(2, tex2d(1, 1, usage)),
            Cmd::CreateTexture(3, tex2d(1, 1, usage)),
            Cmd::CreateSampler(
                1,
                SamplerDesc {
                    min_filter: Filter::Nearest,
                    mag_filter: Filter::Nearest,
                    mip_filter: Filter::Nearest,
                    address_u: AddressMode::ClampToEdge,
                    address_v: AddressMode::ClampToEdge,
                    address_w: AddressMode::ClampToEdge,
                    ..SamplerDesc::default()
                },
            ),
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::VERTEX, "vmain", T4_VS),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::FRAGMENT, "fmain", &t4_fs()),
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
                    entries: vec![
                        BindEntry {
                            binding: 0,
                            resource: BindResource::Texture { id: 2 },
                        },
                        BindEntry {
                            binding: 1,
                            resource: BindResource::Sampler { id: 1 },
                        },
                    ],
                },
            ),
            Cmd::CreateBindGroup(
                2,
                BindGroupDesc {
                    set: 0,
                    entries: vec![
                        BindEntry {
                            binding: 0,
                            resource: BindResource::Texture { id: 3 },
                        },
                        BindEntry {
                            binding: 1,
                            resource: BindResource::Sampler { id: 1 },
                        },
                    ],
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder,
                signal: None,
            }),
        ],
    )
    .expect("deep-multipass frame must run cleanly");
    let elapsed_s = t0.elapsed().as_secs_f64();

    let img = exec.read_texture(&s.resources, final_tex).unwrap();
    let got = [img[0], img[1], img[2], img[3]];
    let want = [
        (T4_PASSES as u32 * T4_DR) as u8,
        (T4_PASSES as u32 * T4_DG) as u8,
        (T4_PASSES as u32 * T4_DB) as u8,
        255,
    ];
    println!(
        "[scale] deep_multipass: {T4_PASSES} sequential ping-pong passes in {:.3}s => texel {got:?} (want {want:?})",
        elapsed_s,
    );
    // Property: the final texel is EXACTLY the composed transform (start + N*delta), proving every pass ran
    // and read the prior pass's output. It also differs from both the start (0,0,0) and a single pass.
    assert!(
        near_tol(got, want, 2),
        "deep_multipass: composed texel {got:?} != expected {want:?}"
    );
    assert!(
        !near_tol(got, [0, 0, 0, 255], 2),
        "result must not be the seed (passes did nothing)"
    );
    assert!(
        !near_tol(got, [T4_DR as u8, T4_DG as u8, T4_DB as u8, 255], 2),
        "result must be many passes, not one"
    );
    let ceil_s = env_f64("HL_SCALE_MULTIPASS_CEIL_S", 30.0);
    assert!(
        elapsed_s <= ceil_s,
        "deep-multipass frame took {:.3}s > {:.3}s ceiling",
        elapsed_s,
        ceil_s
    );
}

// ===================================================================================================
// Test 5 — STEADY STATE: K frames of a fixed scene + per-frame resource churn; no time or residency blowup.
// ===================================================================================================
//
// One resident target + pipeline + vertex buffer. Each of K frames renders the SAME scene AND creates +
// writes + destroys a transient buffer (create/destroy churn). If per-frame work is bounded (no leak, no
// quadratic growth in the session tables or the executor), the last frame costs about the same as the first
// AND the live-object count / resident bytes return EXACTLY to baseline after every frame.
