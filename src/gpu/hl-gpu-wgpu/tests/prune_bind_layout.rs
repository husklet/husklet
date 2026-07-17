//! Bound-but-unsampled binding end-to-end: the GskGpu/GTK4 shape that NACK'd `create_bind_group`.
//!
//! This mirrors the real GTK4 frame exactly: a fragment shader DECLARES a uniform block + THREE
//! texture/sampler pairs (the driver's UBO@0, texture@`1+2k`, sampler@`2+2k` scheme, seven bindings), the
//! GL driver BINDS the UBO + TWO of those pairs (a 5-entry bind group — the third pair's texture is
//! unbound), and `main()` SAMPLES only the first pair (three used bindings). So `used(3) ⊂ bound(5) ⊂
//! declared(7)`: neither the bound set nor the declared set equals what wgpu's AUTO layout exposes (the
//! three read bindings), and the 5-entry bind group NACK'd with
//! "Number of bindings in bind group descriptor (5) does not match … the bind group layout (3)".
//!
//! The fix filters the bind-group entries down to the pipeline's USED bindings (`reflect` +
//! `pipeline::create_render_pipeline` → `bindgroup::build_bind_group`), so the 5-entry bind group builds as
//! the 3 the shader reads, matches the auto layout, and the draw samples the FIRST texture correctly. FAILS
//! before the fix (5-vs-3 NACK at `Submit`), PASSES after — and the bound-but-unsampled pair 1 (a real
//! magenta texture) must NOT appear on the target, proving the right resource was sampled.

use std::sync::{Mutex, MutexGuard, OnceLock};

use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BufferDesc, ColorAttachment, ColorTargetState,
    RenderPipelineDesc, SamplerDesc, ShaderRef, TextureDesc,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, texture_usage, AddressMode, Filter, LoadOp, TextureDim, TextureFormat, Topology,
};
use hl_gpu::protocol::model::kernel::{glsl_stage, GlslDescriptor};
use hl_gpu::{
    Cmd, CommandBuffer, Enc, FakeClock, GlobalLedger, GpuExecutor, Limits, Session,
    ShaderPayloadKind,
};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

static EXEC: OnceLock<Mutex<WgpuExecutor>> = OnceLock::new();

fn exec() -> MutexGuard<'static, WgpuExecutor> {
    EXEC.get_or_init(|| {
        Mutex::new(
            WgpuExecutor::new(DeviceConfig::default())
                .expect("acquire a wgpu adapter (is a Vulkan ICD / lavapipe reachable?)"),
        )
    })
    .lock()
    .unwrap_or_else(|e| e.into_inner())
}

fn run_batch(exec: &mut WgpuExecutor, cmds: &[Cmd]) -> Session {
    let caps = exec.capabilities();
    let mut limits = Limits::from_capabilities(caps);
    limits.copy_alignment = 1;
    let mut s = Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );
    hl_gpu::runtime::submit(&mut s, exec, 0, cmds).expect(
        "the declared-but-unsampled bind group must create + draw cleanly (explicit layout)",
    );
    s
}

fn tex(w: u32, h: u32, usage: u32) -> TextureDesc {
    TextureDesc {
        width: w,
        height: h,
        depth: 1,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D2,
        format: TextureFormat::Rgba8Unorm,
        usage,
        label: String::new(),
    }
}

fn glsl(stage: u32, entry: &str, source: &str) -> Vec<u32> {
    GlslDescriptor {
        stage,
        entry: entry.to_string(),
        source: source.to_string(),
    }
    .to_words()
}

// Uses the shared UBO (so binding 0 gets VERTEX visibility too — exercising the stage merge) and emits a
// fullscreen triangle with a constant uv so every fragment samples texture 0's single texel.
const VS: &str = r#"#version 460
layout(std140, binding = 0) uniform HlUniforms { vec4 tint; } u;
layout(location = 0) out vec2 uv;
void main() {
    vec2 p[3] = vec2[3](vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    uv = vec2(0.5, 0.5);
    gl_Position = vec4(p[gl_VertexIndex], 0.0, u.tint.w);
}
"#;

// DECLARES 7 bindings (UBO@0 + three texture/sampler pairs at 1..6) but SAMPLES only pair 0 — so naga
// prunes bindings 3,4,5,6 from the entry point's usage and wgpu's auto layout exposes only 3 (0,1,2). The
// driver binds pairs 0 and 1 (5 entries); pair 2 is declared-but-never-bound-or-sampled.
const FS: &str = r#"#version 460
layout(std140, binding = 0) uniform HlUniforms { vec4 tint; } u;
layout(binding = 1) uniform texture2D t0_tex;
layout(binding = 2) uniform sampler   t0_smp;
layout(binding = 3) uniform texture2D t1_tex;
layout(binding = 4) uniform sampler   t1_smp;
layout(binding = 5) uniform texture2D t2_tex;
layout(binding = 6) uniform sampler   t2_smp;
layout(location = 0) in vec2 uv;
layout(location = 0) out vec4 color;
void main() {
    color = texture(sampler2D(t0_tex, t0_smp), uv) * u.tint;
}
"#;

fn nearest() -> SamplerDesc {
    SamplerDesc {
        min_filter: Filter::Nearest,
        mag_filter: Filter::Nearest,
        mip_filter: Filter::Nearest,
        address_u: AddressMode::ClampToEdge,
        address_v: AddressMode::ClampToEdge,
        address_w: AddressMode::ClampToEdge,
    }
}

#[test]
fn bound_but_unsampled_bindings_filtered_to_used() {
    let sampled: [u8; 4] = [30, 150, 220, 255]; // texture 0 — the texel that must land on the target
    let unread: [u8; 4] = [255, 0, 255, 255]; // texture 3 — declared+bound but never sampled; must NOT show
    let tint: [f32; 4] = [1.0, 1.0, 1.0, 1.0]; // white → passthrough of the sampled texel

    let mut g = exec();
    let s = run_batch(
        &mut g,
        &[
            // Render target (id 1) + two 1×1 source textures (ids 2, 3).
            Cmd::CreateTexture(
                1,
                tex(4, 4, texture_usage::RENDER_TARGET | texture_usage::COPY_SRC),
            ),
            Cmd::CreateTexture(
                2,
                tex(1, 1, texture_usage::SAMPLED | texture_usage::COPY_DST),
            ),
            Cmd::CreateTexture(
                3,
                tex(1, 1, texture_usage::SAMPLED | texture_usage::COPY_DST),
            ),
            // Uniform buffer (tint) + two staging buffers for the source texels.
            Cmd::CreateBuffer(
                1,
                BufferDesc {
                    size: 16,
                    usage: buffer_usage::UNIFORM,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: bytemuck_cast(&tint),
            },
            Cmd::CreateBuffer(
                2,
                BufferDesc {
                    size: 4,
                    usage: buffer_usage::COPY_SRC,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 2,
                offset: 0,
                data: sampled.to_vec(),
            },
            Cmd::CreateBuffer(
                3,
                BufferDesc {
                    size: 4,
                    usage: buffer_usage::COPY_SRC,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 3,
                offset: 0,
                data: unread.to_vec(),
            },
            // GLSL vertex + fragment (the driver's forwarded-source path).
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::VERTEX, "vmain", VS),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::FRAGMENT, "fmain", FS),
            },
            Cmd::CreateSampler(1, nearest()),
            Cmd::CreateSampler(2, nearest()),
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
                },
            ),
            // The driver-style bind group: an entry per BOUND resource — UBO@0, tex0@1, samp0@2, tex1@3,
            // samp1@4. FIVE entries (pair 2 at 5/6 is declared but never bound), exactly what the GL driver
            // emits; the shader samples only pair 0. Before the fix this NACK'd against the 3-binding auto
            // layout; the fix filters these 5 entries down to the 3 used bindings {0,1,2}.
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
                                size: 16,
                            },
                        },
                        BindEntry {
                            binding: 1,
                            resource: BindResource::Texture { id: 2 },
                        },
                        BindEntry {
                            binding: 2,
                            resource: BindResource::Sampler { id: 1 },
                        },
                        BindEntry {
                            binding: 3,
                            resource: BindResource::Texture { id: 3 },
                        },
                        BindEntry {
                            binding: 4,
                            resource: BindResource::Sampler { id: 2 },
                        },
                    ],
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::CopyBufferToTexture {
                        src: 2,
                        src_offset: 0,
                        bytes_per_row: 4,
                        dst: 2,
                        mip: 0,
                        width: 1,
                        height: 1,
                    },
                    Enc::CopyBufferToTexture {
                        src: 3,
                        src_offset: 0,
                        bytes_per_row: 4,
                        dst: 3,
                        mip: 0,
                        width: 1,
                        height: 1,
                    },
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
    );

    let px = g.read_texture(&s.resources, 1).unwrap();
    for (i, texel) in px.chunks_exact(4).enumerate() {
        assert_eq!(
            texel, sampled,
            "pixel {i}: must be the SAMPLED texture-0 texel {sampled:?} (not the declared-but-unsampled \
             texture-3 texel {unread:?}), proving the 5-entry bind group matched the explicit 5-binding \
             layout and the draw sampled the right resource"
        );
    }
}

/// `f32x4` → little-endian bytes without pulling in a dep (the crate's tests avoid `bytemuck` in-test).
fn bytemuck_cast(v: &[f32; 4]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}
