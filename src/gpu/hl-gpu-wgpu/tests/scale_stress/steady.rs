use super::draw::{T1_FS, T1_VS};
use super::*;

const T5_FRAMES: usize = 200;
const T5_DIM: u32 = 64;
const T5_DRAWS: usize = 64;
const T5_TEMP_BYTES: u64 = 256 << 10; // 256 KiB transient buffer churned every frame

#[test]
fn steady_state_no_blowup() {
    let mut exec = match try_exec() {
        Some(e) => e,
        None => return,
    };
    let mut s = new_session(&exec);

    // A resident vertex buffer of T5_DRAWS quads tiling the target (reused every frame — no per-frame create
    // of the scene geometry). Layout matches test 1's pos+color vertex.
    let gx_n = 8u32;
    let cell = T5_DIM / gx_n; // 8 px
    let mut verts: Vec<f32> = Vec::new();
    for i in 0..T5_DRAWS {
        let gx = (i as u32) % gx_n;
        let gy = (i as u32) / gx_n;
        let ndc_x = |p: u32| p as f32 / T5_DIM as f32 * 2.0 - 1.0;
        let ndc_y = |p: u32| 1.0 - p as f32 / T5_DIM as f32 * 2.0;
        let (x0, x1) = (ndc_x(gx * cell), ndc_x((gx + 1) * cell));
        let (yt, yb) = (ndc_y(gy * cell), ndc_y((gy + 1) * cell));
        let cf = [
            ((i * 3) % 255) as f32 / 255.0,
            ((i * 5) % 255) as f32 / 255.0,
            ((i * 7) % 255) as f32 / 255.0,
            1.0,
        ];
        for &(x, y) in &[(x0, yt), (x1, yt), (x0, yb), (x1, yb)] {
            verts.extend_from_slice(&[x, y]);
            verts.extend_from_slice(&cf);
        }
    }
    let vbytes: Vec<u8> = verts.iter().flat_map(|f| f.to_le_bytes()).collect();

    hl_gpu::runtime::submit(
        &mut s,
        &mut exec,
        0,
        &[
            Cmd::CreateTexture(
                1,
                tex2d(
                    T5_DIM,
                    T5_DIM,
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
    .expect("steady-state setup must run cleanly");

    let baseline_live = s.resources.live_count();
    let baseline_bytes = s.ledger.residency_bytes();

    // The transient buffer uses a distinct id (99) created + destroyed every frame.
    const TEMP: u32 = 99;
    let build_frame = || -> Vec<Cmd> {
        let mut render: Vec<Enc> = Vec::with_capacity(T5_DRAWS + 4);
        render.push(Enc::BeginRenderPass {
            color: vec![ColorAttachment {
                texture: 1,
                load: LoadOp::Clear,
                clear: [0.0, 0.0, 0.0, 1.0],
                store: true,
            }],
            depth: None,
        });
        render.push(Enc::SetPipeline(1));
        render.push(Enc::SetVertexBuffer {
            slot: 0,
            buffer: 1,
            offset: 0,
        });
        for i in 0..T5_DRAWS {
            render.push(Enc::Draw {
                vertex_count: 4,
                instance_count: 1,
                first_vertex: (i as u32) * 4,
                first_instance: 0,
            });
        }
        render.push(Enc::EndRenderPass);
        vec![
            Cmd::CreateBuffer(
                TEMP,
                BufferDesc {
                    size: T5_TEMP_BYTES,
                    usage: buffer_usage::COPY_SRC | buffer_usage::COPY_DST,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: TEMP,
                offset: 0,
                data: vec![0xA5u8; 4096],
            },
            Cmd::Submit(CommandBuffer {
                encoder: render,
                signal: None,
            }),
            Cmd::DestroyBuffer(TEMP),
        ]
    };

    // Warm up (allocator / first-touch), then measure a fixed number of frames.
    for _ in 0..3 {
        hl_gpu::runtime::submit(&mut s, &mut exec, 0, &build_frame()).expect("warmup frame");
    }

    let mut per_frame_us: Vec<f64> = Vec::with_capacity(T5_FRAMES);
    for _ in 0..T5_FRAMES {
        let t = Instant::now();
        hl_gpu::runtime::submit(&mut s, &mut exec, 0, &build_frame()).expect("steady frame");
        per_frame_us.push(t.elapsed().as_secs_f64() * 1e6);
        // Leak gate: the transient buffer was destroyed in-frame, so residency must be back at baseline.
        assert_eq!(
            s.resources.live_count(),
            baseline_live,
            "live objects drifted from baseline (leak)"
        );
        assert_eq!(
            s.ledger.residency_bytes(),
            baseline_bytes,
            "resident bytes drifted from baseline (leak)"
        );
    }

    let head = median(&per_frame_us[..16]);
    let tail = median(&per_frame_us[T5_FRAMES - 16..]);
    let growth = tail / head.max(1e-9);
    let min = per_frame_us.iter().cloned().fold(f64::MAX, f64::min);
    let max = per_frame_us.iter().cloned().fold(0.0, f64::max);
    println!(
        "[scale] steady_state_no_blowup: {T5_FRAMES} frames ({T5_DRAWS} draws + 256KiB churn each) — \
         head-median {:.1}µs tail-median {:.1}µs (min {:.1} max {:.1}) => {:.2}x tail/head; residency flat at {} B",
        head, tail, min, max, growth, baseline_bytes,
    );

    // Property: the tail must not balloon relative to the head. 8x tolerates real scheduler jitter on a
    // shared CI box while catching a genuine per-frame leak / quadratic growth (a steadily climbing tail).
    let factor = env_f64("HL_SCALE_STEADY_FACTOR", 8.0);
    assert!(
        growth <= factor,
        "steady-state per-frame time grew {:.2}x (head {:.1}µs → tail {:.1}µs) > {:.1}x — per-frame leak / O(n²)",
        growth, head, tail, factor,
    );
}

// ===================================================================================================
// Test 6 — NO RESOURCE LEAK: N cycles of create-many / destroy-many return to baseline every cycle.
// ===================================================================================================
//
// Each cycle creates a batch of buffers + textures, PROVES they work (write + readback one), then destroys
// every one. After each cycle the session's live-object count, resident bytes, AND the process-global
// residency account must all be back EXACTLY at the pre-cycle baseline — a resource that lingered (a native
// the executor forgot to drop on destroy, or an accounting charge never refunded) shows as an accumulation
// that never returns to baseline.
