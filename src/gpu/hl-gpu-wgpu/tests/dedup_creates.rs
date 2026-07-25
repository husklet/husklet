//! Content-dedup of `CreateShader` + `CreateRenderPipeline` in the wgpu executor.
//!
//! Chrome/Skia's offscreen frame re-issues the SAME shader source and the SAME render-pipeline descriptor
//! under fresh resource ids on every `glFlush`. Without dedup each create materializes a NEW
//! `wgpu::ShaderModule` / `RenderPipeline` and pins it resident; over hundreds of frames the executor holds
//! N identical backings. These tests prove the executor now aliases identical content onto ONE shared
//! backing (bounded residency), never falsely dedups distinct content, keeps a backing alive while any
//! alias lives, and renders byte-identical pixels through a deduped shader+pipeline.
//!
//! Every test skips cleanly when no wgpu adapter is reachable (no lavapipe / Vulkan ICD), mirroring the
//! crate's other GPU tests.

use hl_gpu::protocol::model::descriptor::{
    ColorAttachment, ColorTargetState, RenderPipelineDesc, ShaderRef, TextureDesc,
};
use hl_gpu::protocol::model::enums::{texture_usage, LoadOp, TextureDim, TextureFormat, Topology};
use hl_gpu::protocol::model::kernel::{glsl_stage, GlslDescriptor};
use hl_gpu::{
    Cmd, CommandBuffer, Enc, FakeClock, GlobalLedger, GpuExecutor, Limits, Session,
    ShaderPayloadKind,
};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

// A fullscreen-triangle vertex shader with no bindings (the conformance-triangle style).
const VS: &str = r#"#version 460
void main() {
    vec2 p[3] = vec2[3](vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    gl_Position = vec4(p[gl_VertexIndex], 0.0, 1.0);
}
"#;

// Two DISTINCT fragment shaders, each emitting a different constant color (exact under Rgba8Unorm).
const FS_RED: &str = r#"#version 460
layout(location = 0) out vec4 color;
void main() { color = vec4(1.0, 0.0, 0.0, 1.0); }
"#;

const FS_GREEN: &str = r#"#version 460
layout(location = 0) out vec4 color;
void main() { color = vec4(0.0, 1.0, 0.0, 1.0); }
"#;

fn glsl(stage: u32, entry: &str, source: &str) -> Vec<u32> {
    GlslDescriptor {
        stage,
        entry: entry.to_string(),
        source: source.to_string(),
    }
    .to_words()
}

fn target_tex(w: u32, h: u32) -> TextureDesc {
    TextureDesc {
        width: w,
        height: h,
        depth: 1,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D2,
        format: TextureFormat::Rgba8Unorm,
        usage: texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
        label: String::new(),
    }
}

fn pipeline_desc(vs_id: u32, fs_id: u32) -> RenderPipelineDesc {
    RenderPipelineDesc {
        vertex: ShaderRef {
            module: vs_id,
            entry: "vmain".into(),
        },
        fragment: Some(ShaderRef {
            module: fs_id,
            entry: "fmain".into(),
        }),
        vertex_buffers: vec![],
        color_targets: vec![ColorTargetState {
            format: TextureFormat::Rgba8Unorm,
            blend: None,
            write_mask: 0xF,
        }],
        depth: None,
        topology: Topology::TriangleList,
        cull: 0,
        front_face: 0,
        sample_count: 1,
        label: String::new(),
    }
}

/// A session with the residency ceilings lifted, so the RUNTIME's per-id charge (which is what the Chrome
/// bug exhausts, and which the executor cannot lower) never rejects a create before it reaches the executor
/// — letting these tests observe the executor-side DEDUP residency in isolation.
fn session(exec: &WgpuExecutor) -> Session {
    let caps = exec.capabilities();
    let mut limits = Limits::from_capabilities(caps);
    limits.copy_alignment = 1;
    limits.max_connection_bytes = u64::MAX;
    limits.max_connection_objects = u64::MAX;
    limits.max_compiled_cache_bytes = u64::MAX;
    Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    )
}

fn new_exec() -> Option<WgpuExecutor> {
    // No adapter (no lavapipe / Vulkan ICD reachable) — skip, mirroring the suite's other GPU tests.
    WgpuExecutor::new(DeviceConfig::default()).ok()
}

/// N identical `CreateShader`s share ONE compiled module backing; executor residency stays at one shader's
/// worth, not N.
#[test]
fn identical_shaders_share_one_backing() {
    let Some(mut exec) = new_exec() else { return };
    let mut s = session(&exec);

    const N: u32 = 200;
    let words = glsl(glsl_stage::FRAGMENT, "fmain", FS_RED);
    let one_backing = (words.len() as u64) * 4;

    let cmds: Vec<Cmd> = (1..=N)
        .map(|id| Cmd::CreateShader {
            id,
            kind: ShaderPayloadKind::Glsl,
            spirv: words.clone(),
        })
        .collect();
    hl_gpu::runtime::submit(&mut s, &mut exec, 0, &cmds).expect("200 identical shaders create");

    assert_eq!(
        exec.shader_backing_count(),
        1,
        "200 byte-identical CreateShader must compile ONE module backing, not 200"
    );
    assert_eq!(
        exec.shader_backing_resident_bytes(),
        one_backing,
        "deduped shader residency must be a single module's worth ({one_backing} bytes), not {}× that",
        N
    );
    // The un-deduped executor would have held N × one_backing resident; assert the dedup actually bounds it.
    assert!(
        exec.shader_backing_resident_bytes() * (N as u64) == one_backing * (N as u64),
        "sanity: N aliases would be {} bytes un-deduped",
        one_backing * N as u64
    );
}

/// N identical `CreateRenderPipeline`s share ONE compiled pipeline backing; executor residency stays at one
/// pipeline's worth, not N.
#[test]
fn identical_pipelines_share_one_backing() {
    let Some(mut exec) = new_exec() else { return };
    let mut s = session(&exec);

    const N: u32 = 200;
    // One shared VS + FS, then N identical pipelines built from the SAME descriptor under distinct ids.
    let mut cmds = vec![
        Cmd::CreateShader {
            id: 1,
            kind: ShaderPayloadKind::Glsl,
            spirv: glsl(glsl_stage::VERTEX, "vmain", VS),
        },
        Cmd::CreateShader {
            id: 2,
            kind: ShaderPayloadKind::Glsl,
            spirv: glsl(glsl_stage::FRAGMENT, "fmain", FS_RED),
        },
    ];
    for pid in 1..=N {
        cmds.push(Cmd::CreateRenderPipeline(pid, pipeline_desc(1, 2)));
    }
    hl_gpu::runtime::submit(&mut s, &mut exec, 0, &cmds).expect("200 identical pipelines create");

    assert_eq!(
        exec.pipeline_backing_count(),
        1,
        "200 byte-identical CreateRenderPipeline must compile ONE pipeline backing, not 200"
    );
    assert_eq!(
        exec.pipeline_backing_resident_bytes(),
        hl_gpu_wgpu_pipeline_backing_bytes(),
        "deduped pipeline residency must be a single pipeline's worth, not 200× that"
    );
}

// The executor charges a flat per-pipeline backing footprint (mirrors the runtime's KIND_PIPELINE 4096);
// duplicated here to avoid exposing the constant publicly.
fn hl_gpu_wgpu_pipeline_backing_bytes() -> u64 {
    4096
}

/// Two DIFFERENT shader sources must NOT dedup (no false sharing): two distinct backings, and their render
/// output differs.
#[test]
fn distinct_sources_are_not_deduped() {
    let Some(mut exec) = new_exec() else { return };
    let mut s = session(&exec);

    // Same VS, two DIFFERENT fragment sources → 1 VS backing + 2 FS backings = 3 distinct modules.
    hl_gpu::runtime::submit(
        &mut s,
        &mut exec,
        0,
        &[
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::VERTEX, "vmain", VS),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::FRAGMENT, "fmain", FS_RED),
            },
            Cmd::CreateShader {
                id: 3,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::FRAGMENT, "fmain", FS_GREEN),
            },
        ],
    )
    .expect("three distinct shaders create");

    assert_eq!(
        exec.shader_backing_count(),
        3,
        "two DIFFERENT fragment sources must not falsely dedup — 3 distinct backings (1 VS + 2 FS)"
    );

    // Render each fragment program into its own 2×2 target; the pixels must differ (red vs green).
    let red = render_once(&mut exec, &mut s, 10, 11, 1, 2);
    let green = render_once(&mut exec, &mut s, 13, 14, 1, 3);
    assert_eq!(&red[..4], &[255, 0, 0, 255], "FS_RED must render red");
    assert_eq!(&green[..4], &[0, 255, 0, 255], "FS_GREEN must render green");
    assert_ne!(red, green, "distinct sources must produce distinct output");
}

/// Alias lifetime: create id A + id B from the same source, destroy A, then a draw using B still renders —
/// the shared backing survives until its LAST alias is gone.
#[test]
fn alias_survives_destroy_of_other_alias() {
    let Some(mut exec) = new_exec() else { return };
    let mut s = session(&exec);

    let fs = glsl(glsl_stage::FRAGMENT, "fmain", FS_RED);
    // Shader 1 = VS. Shaders 2 (A) and 3 (B) are the SAME fragment source (aliases of one backing).
    hl_gpu::runtime::submit(
        &mut s,
        &mut exec,
        0,
        &[
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::VERTEX, "vmain", VS),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::Glsl,
                spirv: fs.clone(),
            },
            Cmd::CreateShader {
                id: 3,
                kind: ShaderPayloadKind::Glsl,
                spirv: fs.clone(),
            },
        ],
    )
    .expect("VS + two fragment aliases create");

    // 1 VS + 1 shared FS backing (ids 2 and 3 alias the same FS module).
    assert_eq!(
        exec.shader_backing_count(),
        2,
        "the two FS aliases must share one backing"
    );

    // Destroy alias A (shader id 2). The shared FS backing must survive (id 3 still aliases it).
    hl_gpu::runtime::submit(&mut s, &mut exec, 0, &[Cmd::DestroyShader(2)])
        .expect("destroying one alias must succeed");
    assert_eq!(
        exec.shader_backing_count(),
        2,
        "destroying one alias must NOT free the shared backing while another alias lives"
    );

    // A draw using the SURVIVING alias B (shader id 3) must still render correctly.
    let px = render_once(&mut exec, &mut s, 20, 21, 1, 3);
    assert_eq!(
        &px[..4],
        &[255, 0, 0, 255],
        "a draw through the surviving alias must render red (its backing survived)"
    );
}

/// A full small render through a DEDUPED shader + pipeline (created twice, drawn via the second alias)
/// produces the exact expected pixels — dedup does not corrupt output.
#[test]
fn deduped_pipeline_renders_exact_pixels() {
    let Some(mut exec) = new_exec() else { return };
    let mut s = session(&exec);

    let vs = glsl(glsl_stage::VERTEX, "vmain", VS);
    let fs = glsl(glsl_stage::FRAGMENT, "fmain", FS_GREEN);

    // Create the VS + FS TWICE (aliases) and the SAME pipeline descriptor TWICE (ids 1 and 2). Then draw
    // with the second, deduped pipeline (id 2) — an alias of the first's compiled backing.
    hl_gpu::runtime::submit(
        &mut s,
        &mut exec,
        0,
        &[
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: vs.clone(),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::Glsl,
                spirv: vs,
            },
            Cmd::CreateShader {
                id: 3,
                kind: ShaderPayloadKind::Glsl,
                spirv: fs.clone(),
            },
            Cmd::CreateShader {
                id: 4,
                kind: ShaderPayloadKind::Glsl,
                spirv: fs,
            },
            // Pipeline 1 from shader ids (1,3); pipeline 2 from the ALIAS ids (2,4) — same source content,
            // so pipeline 2 must dedup onto pipeline 1's compiled backing.
            Cmd::CreateRenderPipeline(1, pipeline_desc(1, 3)),
            Cmd::CreateRenderPipeline(2, pipeline_desc(2, 4)),
            Cmd::CreateTexture(1, target_tex(2, 2)),
        ],
    )
    .expect("aliased shaders + deduped pipeline + target create");

    assert_eq!(
        exec.pipeline_backing_count(),
        1,
        "the two identical-content pipelines must share ONE compiled backing"
    );

    // Draw the fullscreen triangle with the DEDUPED pipeline (id 2) into the 2×2 target.
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
                Enc::SetPipeline(2),
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
    .expect("draw through the deduped pipeline must run");

    let px = exec.read_texture(&s.resources, 1).expect("readback");
    for (i, texel) in px.chunks_exact(4).enumerate() {
        assert_eq!(
            texel,
            &[0, 255, 0, 255],
            "pixel {i}: a deduped shader+pipeline must render the exact expected green, uncorrupted"
        );
    }
}

/// Render a fullscreen triangle from `(vs_id, fs_id)` into a fresh 2×2 target created under `tex_id`, using
/// transient pipeline id `pipe_id`. Returns the RGBA readback.
fn render_once(
    exec: &mut WgpuExecutor,
    s: &mut Session,
    tex_id: u32,
    pipe_id: u32,
    vs_id: u32,
    fs_id: u32,
) -> Vec<u8> {
    hl_gpu::runtime::submit(
        s,
        exec,
        0,
        &[
            Cmd::CreateTexture(tex_id, target_tex(2, 2)),
            Cmd::CreateRenderPipeline(pipe_id, pipeline_desc(vs_id, fs_id)),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment {
                            texture: tex_id,
                            load: LoadOp::Clear,
                            clear: [0.0, 0.0, 0.0, 1.0],
                            store: true,
                        }],
                        depth: None,
                    },
                    Enc::SetPipeline(pipe_id),
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
    .expect("render_once submit");
    exec.read_texture(&s.resources, tex_id).expect("readback")
}
