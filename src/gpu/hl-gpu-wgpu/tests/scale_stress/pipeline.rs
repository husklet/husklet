use super::*;

const T2_PIPELINES: usize = 256;

const T2_VS: &str = r#"#version 460
void main() {
    vec2 p[3] = vec2[3](vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    gl_Position = vec4(p[gl_VertexIndex], 0.0, 1.0);
}
"#;

fn t2_color(i: usize) -> [u8; 4] {
    let r = 20 + (i % 19) as u32 * 12; // 20..236
    let g = 20 + ((i / 19) % 17) as u32 * 13; // 20..228
    let b = 30 + (i % 11) as u32 * 20; // 30..230
    [r as u8, g as u8, b as u8, 255]
}

fn t2_fs(i: usize) -> String {
    let c = t2_color(i);
    format!(
        "#version 460\nlayout(location = 0) out vec4 o;\nvoid main() {{ o = vec4({}.0/255.0, {}.0/255.0, {}.0/255.0, 1.0); }}\n",
        c[0], c[1], c[2],
    )
}

#[test]
fn many_pipelines() {
    let mut exec = match try_exec() {
        Some(e) => e,
        None => return,
    };
    let mut s = new_session(&exec);

    // Resident: a tiny target (id 1) + the shared VS (shader id 1). FS ids are 2.. , pipeline ids 1.. .
    hl_gpu::runtime::submit(
        &mut s,
        &mut exec,
        0,
        &[
            Cmd::CreateTexture(
                1,
                tex2d(2, 2, texture_usage::RENDER_TARGET | texture_usage::COPY_SRC),
            ),
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::VERTEX, "vmain", T2_VS),
            },
        ],
    )
    .expect("many-pipelines setup must run cleanly");

    // Create each pipeline in its own submit and time it (shader compile + PSO build).
    let mut create_us: Vec<f64> = Vec::with_capacity(T2_PIPELINES);
    for i in 0..T2_PIPELINES {
        let fs_id = (i as u32) + 2;
        let pid = (i as u32) + 1;
        let topo = if i % 2 == 0 {
            Topology::TriangleList
        } else {
            Topology::TriangleStrip
        };
        let batch = vec![
            Cmd::CreateShader {
                id: fs_id,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::FRAGMENT, "fmain", &t2_fs(i)),
            },
            Cmd::CreateRenderPipeline(
                pid,
                RenderPipelineDesc {
                    vertex: ShaderRef {
                        module: 1,
                        entry: "vmain".into(),
                    },
                    fragment: Some(ShaderRef {
                        module: fs_id,
                        entry: "fmain".into(),
                    }),
                    vertex_buffers: vec![],
                    color_targets: vec![color_target()],
                    depth: None,
                    topology: topo,
                    cull: 0,
                    front_face: 0,
                    sample_count: 1,
                    label: String::new(),
                },
            ),
        ];
        let t = Instant::now();
        hl_gpu::runtime::submit(&mut s, &mut exec, 0, &batch)
            .expect("pipeline create must run cleanly");
        create_us.push(t.elapsed().as_secs_f64() * 1e6);
    }

    // Correctness: draw with EVERY pipeline and assert its exact baked color lands on the target.
    for i in 0..T2_PIPELINES {
        let pid = (i as u32) + 1;
        hl_gpu::runtime::submit(
            &mut s,
            &mut exec,
            0,
            &[Cmd::Submit(CommandBuffer {
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
                    Enc::SetPipeline(pid),
                    Enc::Draw {
                        vertex_count: 3,
                        instance_count: 1,
                        first_vertex: 0,
                        first_instance: 0,
                    },
                    Enc::EndRenderPass,
                ],
                signal: None,
            })],
        )
        .expect("pipeline draw must run cleanly");
        let img = exec.read_texture(&s.resources, 1).unwrap();
        let want = t2_color(i);
        for p in 0..4 {
            let got = px(&img, 2, p % 2, p / 2);
            assert!(
                near_tol(got, want, 2),
                "pipeline {i}: pixel {p} got {got:?} want {want:?}"
            );
        }
    }

    let total_us: f64 = create_us.iter().sum();
    let creates_per_sec = T2_PIPELINES as f64 / (total_us / 1e6);
    // Compare the median create time of the LAST group against the FIRST group. A per-create O(n) cost
    // (some table rebuilt over all resident pipelines) makes the tail climb; a flat backend keeps it steady.
    let grp = (T2_PIPELINES / 4).clamp(1, 32);
    let head = median(&create_us[..grp]);
    let tail = median(&create_us[T2_PIPELINES - grp..]);
    let growth = tail / head.max(1e-9);
    println!(
        "[scale] many_pipelines: {T2_PIPELINES} distinct pipelines, {:.0} creates/s (total {:.1} ms) — \
         head-median {:.1}µs tail-median {:.1}µs => {:.2}x tail/head",
        creates_per_sec,
        total_us / 1e3,
        head,
        tail,
        growth,
    );

    // Property: create time must not balloon with resident count. A 6x factor tolerates real allocator /
    // scheduler jitter on a shared box yet catches a genuine super-linear (O(n²)-total) create path.
    let factor = env_f64("HL_SCALE_PIPELINE_FACTOR", 6.0);
    assert!(
        growth <= factor,
        "pipeline-create time grew {:.2}x (head {:.1}µs → tail {:.1}µs) > {:.1}x — super-linear create path",
        growth,
        head,
        tail,
        factor,
    );
}

// ===================================================================================================
// Test 3 — MANY RESOURCES: hundreds of textures + buffers + bind groups created + USED in one frame.
// ===================================================================================================
//
// N source textures, each seeded (via its own buffer + a copy) with a distinct color and sampled through
// its own bind group into cell `i` of one target (a per-draw scissor clips the fullscreen sample to the
// cell). Exercises hundreds of live textures / buffers / bind groups bound and used in a single pass, then
// reads the target back and asserts every cell equals its texture's seed color.
