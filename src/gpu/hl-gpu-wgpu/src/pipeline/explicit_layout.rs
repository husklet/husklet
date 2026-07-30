//! The Zed render-pipeline unblock, FAIL-before / PASS-after in one test.
//!
//! Zed's GPUI/wgpu renderer builds a render pipeline whose vertex + fragment stages BOTH declare the
//! same `(group, binding)` UBO but with DIFFERENT block layouts (the fragment reaches a member at a
//! higher offset), so each stage's naga usage derives a different `min_binding_size` for that buffer.
//! wgpu's AUTO layout (`layout: None`) merges the per-stage derived layouts and, seeing two different
//! derived types for one binding, aborts with `InconsistentlyDerivedType` — "Derived bind group layout
//! type is not consistent between stages". That validation error was UNCAPTURED on the executor's wgpu
//! thread, panicked it, and cost Zed its device.
//!
//! `create_render_pipeline` now builds an EXPLICIT layout from the reconciled union of the two stages'
//! used bindings (visibility = VERTEX|FRAGMENT, buffer `min_binding_size: None` so the per-stage size
//! disagreement no longer collides), so the pipeline creates, a bind group of exactly the used entries
//! matches it, a draw runs, and the readback carries the sampled texel — the pixel a mis-built pipeline
//! could never produce.

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

use crate::{DeviceConfig, WgpuExecutor};

// Vertex: declares binding 0 as a ONE-vec4 block (16 bytes) → naga derives a 16-byte min size.
const VS: &str = r#"#version 460
layout(std140, binding = 0) uniform U { vec4 scale; } u;
layout(location = 0) out vec2 uv;
void main() {
    vec2 p[3] = vec2[3](vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    uv = vec2(0.5, 0.5);
    gl_Position = vec4(p[gl_VertexIndex], 0.0, u.scale.w);
}
"#;

// Fragment: declares binding 0 as a TWO-vec4 block (32 bytes) → naga derives a 32-byte min size,
// DIFFERENT from the vertex stage's 16 — the inconsistency wgpu's auto-derive cannot merge. It also
// samples a texture (binding 1) through a sampler (binding 2), the fragment-only part of the union.
const FS: &str = r#"#version 460
layout(std140, binding = 0) uniform U { vec4 scale; vec4 tint; } u;
layout(binding = 1) uniform texture2D t0_tex;
layout(binding = 2) uniform sampler   t0_smp;
layout(location = 0) in vec2 uv;
layout(location = 0) out vec4 color;
void main() {
    color = texture(sampler2D(t0_tex, t0_smp), uv) * u.tint;
}
"#;

fn glsl(stage: u32, entry: &str, source: &str) -> Vec<u32> {
    GlslDescriptor {
        stage,
        entry: entry.to_string(),
        source: source.to_string(),
    }
    .to_words()
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

fn nearest() -> SamplerDesc {
    SamplerDesc {
        min_filter: Filter::Nearest,
        mag_filter: Filter::Nearest,
        mip_filter: Filter::Nearest,
        address_u: AddressMode::ClampToEdge,
        address_v: AddressMode::ClampToEdge,
        address_w: AddressMode::ClampToEdge,
        ..SamplerDesc::default()
    }
}

/// FAIL-before: raw wgpu auto-derive (`layout: None`) over the two stages' translated WGSL rejects the
/// pipeline because binding 0's derived type is inconsistent between the stages. Returns the wgpu error
/// text so the test can assert it is the cross-stage inconsistency (not some unrelated failure).
impl WgpuExecutor {
    fn autoderive_error(&self) -> Option<String> {
        let dev = &self.gpu.device;
        let vs_wgsl =
            crate::wgsl::glsl_to_wgsl(VS, naga::ShaderStage::Vertex, "vmain").expect("vs wgsl");
        let fs_wgsl =
            crate::wgsl::glsl_to_wgsl(FS, naga::ShaderStage::Fragment, "fmain").expect("fs wgsl");
        let vs = dev.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None,
            source: wgpu::ShaderSource::Wgsl(vs_wgsl.into()),
        });
        let fs = dev.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None,
            source: wgpu::ShaderSource::Wgsl(fs_wgsl.into()),
        });
        let targets = [Some(wgpu::ColorTargetState {
            format: wgpu::TextureFormat::Rgba8Unorm,
            blend: None,
            write_mask: wgpu::ColorWrites::ALL,
        })];
        dev.push_error_scope(wgpu::ErrorFilter::Validation);
        let _p = dev.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: None,
            layout: None, // the OLD path — auto-derive
            vertex: wgpu::VertexState {
                module: &vs,
                entry_point: Some("vmain"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &fs,
                entry_point: Some("fmain"),
                targets: &targets,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            multiview: None,
            cache: None,
        });
        pollster::block_on(dev.pop_error_scope()).map(|e| e.to_string())
    }
}

#[test]
fn cross_stage_inconsistent_binding_builds_via_explicit_layout() {
    let mut exec = match WgpuExecutor::new(DeviceConfig::default()) {
        Ok(e) => e,
        // No adapter (no lavapipe/Vulkan ICD reachable) — skip, mirroring the suite's other gpu tests.
        Err(_) => return,
    };

    // FAIL-BEFORE: the old auto-derive path rejects this exact stage pair as inconsistent.
    let err = exec.autoderive_error().expect(
            "auto-derive (layout: None) MUST reject binding 0 as inconsistent between stages — if it did \
             not, this test no longer reproduces the Zed pipeline gap",
        );
    assert!(
        err.contains("consistent") || err.contains("Derived bind group"),
        "the auto-derive failure must be the cross-stage inconsistency, got: {err}"
    );

    // PASS-AFTER: the executor's explicit-layout path creates the pipeline, matches a bind group of the
    // used entries, draws, and reads back the sampled texel.
    let texel: [u8; 4] = [30, 150, 220, 255]; // texture-0's single texel
    let ubo: [f32; 8] = [1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0]; // scale=(…), tint=(1,1,1,1) → passthrough

    let caps = exec.capabilities();
    let mut limits = Limits::from_capabilities(caps);
    limits.copy_alignment = 1;
    let mut s = Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );

    hl_gpu::runtime::submit(
            &mut s,
            &mut exec,
            0,
            &[
                Cmd::CreateTexture(1, tex(4, 4, texture_usage::RENDER_TARGET | texture_usage::COPY_SRC)),
                Cmd::CreateTexture(2, tex(1, 1, texture_usage::SAMPLED | texture_usage::COPY_DST)),
                Cmd::CreateBuffer(1, BufferDesc { size: 32, usage: buffer_usage::UNIFORM, label: String::new() }),
                Cmd::WriteBuffer { id: 1, offset: 0, data: ubo.iter().flat_map(|f| f.to_le_bytes()).collect() },
                Cmd::CreateBuffer(2, BufferDesc { size: 4, usage: buffer_usage::COPY_SRC, label: String::new() }),
                Cmd::WriteBuffer { id: 2, offset: 0, data: texel.to_vec() },
                Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::Glsl, spirv: glsl(glsl_stage::VERTEX, "vmain", VS) },
                Cmd::CreateShader { id: 2, kind: ShaderPayloadKind::Glsl, spirv: glsl(glsl_stage::FRAGMENT, "fmain", FS) },
                Cmd::CreateSampler(1, nearest()),
                Cmd::CreateRenderPipeline(
                    1,
                    RenderPipelineDesc {
                        vertex: ShaderRef { module: 1, entry: "vmain".into() },
                        fragment: Some(ShaderRef { module: 2, entry: "fmain".into() }),
                        vertex_buffers: vec![],
                        color_targets: vec![ColorTargetState { format: TextureFormat::Rgba8Unorm, blend: None, write_mask: 0xF }],
                        depth: None,
                        topology: Topology::TriangleList,
                        cull: 0,
                        front_face: 0,
                        sample_count: 1,
                        label: String::new(),
                    },
                ),
                // The bind group = exactly the used union {0,1,2}: UBO@0 (used by BOTH stages), texture@1
                // and sampler@2 (fragment). This matches the explicit reconciled layout entry-for-entry.
                Cmd::CreateBindGroup(
                    1,
                    BindGroupDesc {
                        set: 0,
                        entries: vec![
                            BindEntry { binding: 0, resource: BindResource::Buffer { id: 1, offset: 0, size: 32 } },
                            BindEntry { binding: 1, resource: BindResource::Texture { id: 2 } },
                            BindEntry { binding: 2, resource: BindResource::Sampler { id: 1 } },
                        ],
                    },
                ),
                Cmd::Submit(CommandBuffer {
                    encoder: vec![
                        Enc::CopyBufferToTexture { src: 2, src_offset: 0, bytes_per_row: 4, dst: 2, mip: 0, width: 1, height: 1 },
                        Enc::BeginRenderPass {
                            color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [0.0, 0.0, 0.0, 1.0], store: true }],
                            depth: None,
                        },
                        Enc::SetPipeline(1),
                        Enc::SetBindGroup { index: 0, group: 1 },
                        Enc::Draw { vertex_count: 3, instance_count: 1, first_vertex: 0, first_instance: 0 },
                        Enc::EndRenderPass,
                    ],
                    signal: None,
                }),
            ],
        )
        .expect(
            "the cross-stage-inconsistent binding must create + draw cleanly through the explicit reconciled \
             layout (the exact pipeline auto-derive rejected above)",
        );

    let px = exec.read_texture(&s.resources, 1).unwrap();
    for (i, out) in px.chunks_exact(4).enumerate() {
        assert_eq!(
                out, texel,
                "pixel {i}: must be the sampled texture-0 texel {texel:?} (tint is white), proving the \
                 explicit-layout pipeline drew and the bind group matched its layout"
            );
    }
}
