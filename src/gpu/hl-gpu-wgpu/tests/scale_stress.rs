//! SCALE + PERF-STRESS battery for the `WgpuExecutor` (task #245).
//!
//! The small `perf_microbench` proves the neutral render path is non-degenerate on the CPU oracle. THIS
//! suite drives the SAME executor the conformance battery uses — the real wgpu/naga/lavapipe backend — but
//! at LARGE workloads, and every test asserts BOTH correctness (exact readback) AND a scaling / leak /
//! throughput property. The thresholds are deliberately generous: their only job is to trip on a genuine
//! O(n²) cliff, a per-frame leak, or a residency that never returns to baseline — never to flake on a slow
//! shared box. Every measured figure is PRINTED so a human reading the log sees the real numbers.
//!
//! Structure is DETERMINISTIC: every loop count is a fixed constant (never wall-clock-bounded), so the work
//! is identical run-to-run and only the elapsed time varies. All ceilings are ENV-overridable for the rare
//! pathologically-slow box (see [`env_f64`]).
//!
//! Each test acquires its own executor and SKIPS (returns) if no adapter is reachable, mirroring the rest of
//! the wgpu suite so a host with no Vulkan ICD still passes.

mod common;
use common::*;

use std::time::Instant;

use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BufferDesc, ColorAttachment, RenderPipelineDesc,
    SamplerDesc, ShaderRef, VertexAttr, VertexLayout,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, texture_usage, AddressMode, Filter, LoadOp, Topology,
};
use hl_gpu::protocol::model::kernel::glsl_stage;
use hl_gpu::BufferId;
use hl_gpu::{Cmd, CommandBuffer, Enc, GpuExecutor, ShaderPayloadKind};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

// Packed vertex-attribute format wire word (`comps | (kind<<8) | (norm<<16)`): a plain f32 vector is
// `comps`, kind=0 (float), norm=0 — so vec2 f32 → 2, vec4 f32 → 4.
const VFMT_F32X2: u32 = 2;
const VFMT_F32X4: u32 = 4;

/// Read an `f64` threshold from `var`, falling back to `default` (a slow CI box can relax a tripwire without
/// touching the source).
fn env_f64(var: &str, default: f64) -> f64 {
    std::env::var(var)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Median of a slice (copies + sorts; used only on small timing vectors).
fn median(v: &[f64]) -> f64 {
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    s[s.len() / 2]
}

/// Acquire the wgpu executor, or `None` if no adapter is reachable (the whole test then skips).
fn try_exec() -> Option<WgpuExecutor> {
    WgpuExecutor::new(DeviceConfig::default()).ok()
}

// ===================================================================================================
// Test 1 — MANY DRAWS, ONE FRAME: thousands of individual draws into one target, exact per-cell readback.
// ===================================================================================================
//
// A tiled grid of NUM_DRAWS cells; draw `i` renders a quad EXACTLY covering cell `i` with a per-draw color
// fed from a single shared vertex buffer (`first_vertex = i*4`). Because the cells tile the target with
// pixel-aligned edges, EVERY output pixel belongs to exactly one draw, so the readback is checked in full:
// a dropped draw leaves its cell at the clear color (fail), a mis-placed draw corrupts two cells (fail).
// This stresses the render-pass encoder's per-draw replay path (submit.rs) for an O(n²) cliff.

const T1_DRAWS: usize = 2000;
const T1_GX: u32 = 50;
const T1_GY: u32 = 40; // 50*40 = 2000 cells
const T1_CELL: u32 = 4;
const T1_W: u32 = T1_GX * T1_CELL; // 200
const T1_H: u32 = T1_GY * T1_CELL; // 160

const T1_VS: &str = r#"#version 460
layout(location = 0) in vec2 pos;
layout(location = 1) in vec4 color;
layout(location = 0) flat out vec4 vcol;
void main() { gl_Position = vec4(pos, 0.0, 1.0); vcol = color; }
"#;
const T1_FS: &str = r#"#version 460
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
    let mut exec = match try_exec() {
        Some(e) => e,
        None => return,
    };
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
        s.ledger.residency_bytes(),
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

const T6_CYCLES: usize = 30;
const T6_BUFS: usize = 64;
const T6_TEXS: usize = 64;
const T6_BUF_BYTES: u64 = 256 << 10; // 256 KiB each

#[test]
fn no_resource_leak() {
    let mut exec = match try_exec() {
        Some(e) => e,
        None => return,
    };
    let mut s = new_session(&exec);

    let baseline_live = s.resources.live_count();
    let baseline_bytes = s.ledger.residency_bytes();
    let baseline_global = s.global.residency_bytes();
    let baseline_objs = s.global.object_count();

    let mut peak_live = baseline_live;
    for cycle in 0..T6_CYCLES {
        // --- create many ---
        let mut create: Vec<Cmd> = Vec::with_capacity(T6_BUFS * 2 + T6_TEXS);
        for b in 0..T6_BUFS {
            let id = (b as u32) + 1;
            create.push(Cmd::CreateBuffer(
                id,
                BufferDesc {
                    size: T6_BUF_BYTES,
                    usage: buffer_usage::COPY_SRC | buffer_usage::COPY_DST,
                    label: String::new(),
                },
            ));
        }
        for t in 0..T6_TEXS {
            let id = (t as u32) + 1;
            create.push(Cmd::CreateTexture(
                id,
                tex2d(
                    64,
                    64,
                    texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
                ),
            ));
        }
        // Write a cycle-distinct pattern into buffer 1 (COPY_DST) so a readback proves the fresh resource works.
        let marker = vec![(cycle as u8).wrapping_add(0x11); 32];
        create.push(Cmd::WriteBuffer {
            id: 1,
            offset: 0,
            data: marker.clone(),
        });
        hl_gpu::runtime::submit(&mut s, &mut exec, 0, &create)
            .expect("create-many must run cleanly");

        peak_live = peak_live.max(s.resources.live_count());

        // --- prove it works: readback the marker + clear-render one texture and read a pixel ---
        let back = exec
            .read_buffer(&s.resources, BufferId(1), 0, marker.len())
            .unwrap();
        assert_eq!(
            back, marker,
            "cycle {cycle}: freshly-created buffer must round-trip its bytes"
        );
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
                            clear: [0.1, 0.2, 0.3, 1.0],
                            store: true,
                        }],
                        depth: None,
                    },
                    Enc::EndRenderPass,
                ],
                signal: None,
            })],
        )
        .expect("cycle render must run cleanly");
        let img = exec.read_texture(&s.resources, 1).unwrap();
        assert!(
            near_tol(px(&img, 64, 0, 0), [26, 51, 77, 255], 3),
            "cycle {cycle}: fresh texture must clear"
        );

        // --- destroy many ---
        let mut destroy: Vec<Cmd> = Vec::with_capacity(T6_BUFS + T6_TEXS);
        for b in 0..T6_BUFS {
            destroy.push(Cmd::DestroyBuffer((b as u32) + 1));
        }
        for t in 0..T6_TEXS {
            destroy.push(Cmd::DestroyTexture((t as u32) + 1));
        }
        hl_gpu::runtime::submit(&mut s, &mut exec, 0, &destroy)
            .expect("destroy-many must run cleanly");

        // Leak gate: everything created this cycle is gone — live count, resident bytes, and the process
        // global account must all be back at the pre-cycle baseline. No drift permitted across cycles.
        assert_eq!(
            s.resources.live_count(),
            baseline_live,
            "cycle {cycle}: live objects did not return to baseline (leak)"
        );
        assert_eq!(
            s.ledger.residency_bytes(),
            baseline_bytes,
            "cycle {cycle}: resident bytes did not return to baseline (leak)"
        );
        assert_eq!(
            s.global.residency_bytes(),
            baseline_global,
            "cycle {cycle}: global residency did not return to baseline (leak)"
        );
        assert_eq!(
            s.global.object_count(),
            baseline_objs,
            "cycle {cycle}: global object count did not return to baseline (leak)"
        );
    }

    println!(
        "[scale] no_resource_leak: {T6_CYCLES} cycles × ({T6_BUFS} buffers @256KiB + {T6_TEXS} textures) — \
         peak {peak_live} live objects, returned to baseline ({baseline_live} live / {baseline_bytes} B) every cycle",
    );
    // Sanity: each cycle genuinely allocated the full batch (proving the baseline-return is meaningful).
    assert!(
        peak_live >= baseline_live + T6_BUFS + T6_TEXS,
        "cycles must have allocated the full resource batch"
    );
}
