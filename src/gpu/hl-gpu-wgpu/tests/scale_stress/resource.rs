use super::*;

const T3_N: usize = 200;
const T3_GX: u32 = 20;
const T3_GY: u32 = 10; // 200 cells
const T3_CELL: u32 = 4;
const T3_W: u32 = T3_GX * T3_CELL; // 80
const T3_H: u32 = T3_GY * T3_CELL; // 40

const T3_VS: &str = r#"#version 460
layout(location = 0) out vec2 vuv;
void main() {
    vec2 p[3] = vec2[3](vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    gl_Position = vec4(p[gl_VertexIndex], 0.0, 1.0);
    vuv = vec2(0.5, 0.5);
}
"#;
const T3_FS: &str = r#"#version 460
layout(location = 0) in vec2 vuv;
layout(set = 0, binding = 0) uniform texture2D t;
layout(set = 0, binding = 1) uniform sampler s;
layout(location = 0) out vec4 o;
void main() { o = texture(sampler2D(t, s), vuv); }
"#;

fn t3_color(i: usize) -> [u8; 4] {
    let r = 20 + (i % 18) as u32 * 12; // 20..224
    let g = 20 + ((i / 18) % 16) as u32 * 13; // 20..215
    let b = 30 + (i % 9) as u32 * 24; // 30..222
    [r as u8, g as u8, b as u8, 255]
}

#[test]
fn many_resources() {
    let mut exec = try_exec();
    let mut s = new_session(&exec);

    // Resident: target (tex id 1), sampler (id 1), VS (id 1), FS (id 2), pipeline (id 1).
    let mut batch: Vec<Cmd> = vec![
        Cmd::CreateTexture(
            1,
            tex2d(
                T3_W,
                T3_H,
                texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
            ),
        ),
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
            spirv: glsl(glsl_stage::VERTEX, "vmain", T3_VS),
        },
        Cmd::CreateShader {
            id: 2,
            kind: ShaderPayloadKind::Glsl,
            spirv: glsl(glsl_stage::FRAGMENT, "fmain", T3_FS),
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
    ];
    // N seed buffers + N source textures + N bind groups. Buffer i (id i+1), texture i (id i+2), bg i (id i+1).
    for i in 0..T3_N {
        let c = t3_color(i);
        let buf = (i as u32) + 1;
        let texid = (i as u32) + 2;
        let bg = (i as u32) + 1;
        batch.push(Cmd::CreateBuffer(
            buf,
            BufferDesc {
                size: 4,
                usage: buffer_usage::COPY_SRC,
                label: String::new(),
            },
        ));
        batch.push(Cmd::WriteBuffer {
            id: buf,
            offset: 0,
            data: c.to_vec(),
        });
        batch.push(Cmd::CreateTexture(
            texid,
            tex2d(1, 1, texture_usage::SAMPLED | texture_usage::COPY_DST),
        ));
        batch.push(Cmd::CreateBindGroup(
            bg,
            BindGroupDesc {
                set: 0,
                entries: vec![
                    BindEntry {
                        binding: 0,
                        resource: BindResource::Texture { id: texid },
                    },
                    BindEntry {
                        binding: 1,
                        resource: BindResource::Sampler { id: 1 },
                    },
                ],
            },
        ));
    }

    // One frame: seed every source texture, then a single pass drawing all N sampled cells.
    let mut encoder: Vec<Enc> = Vec::with_capacity(T3_N * 4 + 3);
    for i in 0..T3_N {
        let buf = (i as u32) + 1;
        let texid = (i as u32) + 2;
        encoder.push(Enc::CopyBufferToTexture {
            src: buf,
            src_offset: 0,
            bytes_per_row: 4,
            dst: texid,
            mip: 0,
            width: 1,
            height: 1,
        });
    }
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
    for i in 0..T3_N {
        let gx = (i as u32) % T3_GX;
        let gy = (i as u32) / T3_GX;
        encoder.push(Enc::SetScissor {
            x: gx * T3_CELL,
            y: gy * T3_CELL,
            w: T3_CELL,
            h: T3_CELL,
        });
        encoder.push(Enc::SetBindGroup {
            index: 0,
            group: (i as u32) + 1,
        });
        encoder.push(Enc::Draw {
            vertex_count: 3,
            instance_count: 1,
            first_vertex: 0,
            first_instance: 0,
        });
    }
    encoder.push(Enc::EndRenderPass);
    batch.push(Cmd::Submit(CommandBuffer {
        encoder,
        signal: None,
    }));

    let t0 = Instant::now();
    hl_gpu::runtime::submit(&mut s, &mut exec, 0, &batch)
        .expect("many-resources frame must run cleanly");
    let elapsed_s = t0.elapsed().as_secs_f64();

    let img = exec.read_texture(&s.resources, 1).unwrap();
    let mut bad = 0usize;
    for i in 0..T3_N {
        let gx = (i as u32) % T3_GX;
        let gy = (i as u32) / T3_GX;
        // Sample the cell interior (avoids any edge ambiguity).
        let got = px(&img, T3_W, gx * T3_CELL + 1, gy * T3_CELL + 1);
        if !near_tol(got, t3_color(i), 2) {
            bad += 1;
        }
    }
    let live = s.resources.live_count();
    println!(
        "[scale] many_resources: {T3_N} textures + {T3_N} buffers + {T3_N} bind groups used in one frame \
         in {:.3}s — {} live objects, {} resident bytes",
        elapsed_s,
        live,
        s.account.ledger().residency_bytes(),
    );
    assert_eq!(
        bad, 0,
        "many_resources: {bad}/{T3_N} cells wrong — a texture/buffer/bind-group was mis-used"
    );
    // Property: every resource is genuinely resident (no silent drop) and the frame is bounded.
    assert!(
        live >= 3 * T3_N,
        "expected >= {} live objects, saw {live}",
        3 * T3_N
    );
    let ceil_s = env_f64("HL_SCALE_RESOURCE_CEIL_S", 30.0);
    assert!(
        elapsed_s <= ceil_s,
        "many-resources frame took {:.3}s > {:.3}s ceiling",
        elapsed_s,
        ceil_s
    );
}

// ===================================================================================================
// Test 4 — DEEP MULTIPASS: 50 sequential ping-pong passes, final == the exactly-composed transform.
// ===================================================================================================
//
// Two 1×1 textures ping-pong: each pass samples the previous pass's output and adds a fixed per-channel
// delta (a multiple of 1/255, so every step is EXACT in Rgba8Unorm — no rounding drift). After N passes the
// single texel must equal start + N*delta EXACTLY. A pass that read the wrong source, or a missed pass,
// changes the composed total.
