//! Exhaustive shared GPU-IR contract tests.
//!
//! Shims produce this wire format and both Metal/wgpu executors consume it. Keep the variant classifiers
//! exhaustive: adding a new `Cmd` or `Enc` must make this test fail to compile until its corpus case is added.

use hl_gpu::ir::*;
use hl_gpu::software::SoftwareBackend;
use hl_gpu::mock::RecordingBackend;
use hl_gpu::wire::Encoder;
use hl_gpu::{replay, GpuError};

fn buffer_desc() -> BufferDesc {
    BufferDesc { size: 256, usage: buffer_usage::VERTEX | buffer_usage::INDEX | buffer_usage::COPY_SRC |
        buffer_usage::COPY_DST, label: "all-cmd-buffer".into() }
}

fn texture_desc() -> TextureDesc {
    TextureDesc { width: 8, height: 4, depth: 1, mip_levels: 1, sample_count: 1, dim: TextureDim::D2,
        format: TextureFormat::Rgba8Unorm, usage: texture_usage::SAMPLED | texture_usage::RENDER_TARGET |
        texture_usage::COPY_SRC | texture_usage::COPY_DST | texture_usage::PRESENT, label: "all-cmd-texture".into() }
}

fn sampler_desc() -> SamplerDesc {
    SamplerDesc { min_filter: Filter::Nearest, mag_filter: Filter::Linear, mip_filter: Filter::Nearest,
        address_u: AddressMode::ClampToEdge, address_v: AddressMode::Repeat,
        address_w: AddressMode::MirrorRepeat }
}

fn render_pipeline_desc() -> RenderPipelineDesc {
    RenderPipelineDesc {
        vertex: ShaderRef { module: 4, entry: "vs_main".into() },
        fragment: Some(ShaderRef { module: 4, entry: "fs_main".into() }),
        vertex_buffers: vec![VertexLayout { stride: 20, step_mode: 1,
            attrs: vec![VertexAttr { location: 3, format: 23, offset: 4 }] }],
        color_targets: vec![ColorTargetState { format: TextureFormat::Rgba8Unorm,
            blend: Some(BlendState { src_color: 1, dst_color: 5, op_color: 0,
                src_alpha: 1, dst_alpha: 0, op_alpha: 2 }), write_mask: 0xb }],
        depth: Some(DepthState { format: TextureFormat::Depth32Float, depth_write: true, depth_compare: 4 }),
        topology: Topology::TriangleStrip, cull: 2, front_face: 1, label: "all-render-pipeline".into(),
    }
}

fn every_encoder_variant() -> Vec<Enc> {
    vec![
        Enc::BeginRenderPass { color: vec![ColorAttachment { texture: 2, load: LoadOp::Clear,
            clear: [0.125, 0.25, 0.5, 1.0], store: true }],
            depth: Some(DepthAttachment { texture: 9, load: LoadOp::DontCare, clear_depth: 0.75 }) },
        Enc::EndRenderPass,
        Enc::SetPipeline(5),
        Enc::SetBindGroup { index: 2, group: 6 },
        Enc::SetVertexBuffer { slot: 1, buffer: 1, offset: 16 },
        Enc::SetIndexBuffer { buffer: 1, offset: 32, format: IndexFormat::U32 },
        Enc::SetViewport { x: 1.5, y: 2.5, w: 7.0, h: 3.0, min_depth: 0.1, max_depth: 0.9 },
        Enc::SetScissor { x: 1, y: 2, w: 6, h: 2 },
        Enc::ClearRect { texture: 2, x: 2, y: 1, w: 3, h: 2, color: [1.0, 0.5, 0.25, 0.0] },
        Enc::Draw { vertex_count: 7, instance_count: 2, first_vertex: 3, first_instance: 1 },
        Enc::DrawIndexed { index_count: 9, instance_count: 3, first_index: 2, base_vertex: -4, first_instance: 1 },
        Enc::BeginComputePass,
        Enc::EndComputePass,
        Enc::Dispatch { x: 2, y: 3, z: 4 },
        Enc::CopyBufferToBuffer { src: 1, src_offset: 8, dst: 8, dst_offset: 24, size: 40 },
        Enc::CopyBufferToTexture { src: 1, src_offset: 12, bytes_per_row: 32, dst: 2, mip: 0, width: 8, height: 4 },
        Enc::CopyTextureToBuffer { src: 2, mip: 0, width: 8, height: 4, dst: 8, dst_offset: 20, bytes_per_row: 32 },
        Enc::CopyTextureToTexture { src: 2,
            src_sub: TextureSubresource { mip: 1, layer: 2, aspect: TextureAspect::All },
            src_origin: Origin3d { x: 1, y: 2, z: 3 }, dst: 12,
            dst_sub: TextureSubresource { mip: 2, layer: 1, aspect: TextureAspect::DepthOnly },
            dst_origin: Origin3d { x: 4, y: 5, z: 6 },
            extent: Extent3d { width: 7, height: 8, depth: 2 } },
        Enc::BlitTexture { src: 2,
            src_sub: TextureSubresource { mip: 1, layer: 3, aspect: TextureAspect::StencilOnly },
            src_origin: Origin3d { x: 2, y: 1, z: 0 },
            src_extent: Extent3d { width: 6, height: 3, depth: 1 }, dst: 13,
            dst_sub: TextureSubresource { mip: 0, layer: 4, aspect: TextureAspect::All },
            dst_origin: Origin3d { x: 1, y: 1, z: 0 },
            dst_extent: Extent3d { width: 12, height: 6, depth: 1 }, filter: Filter::Linear },
        Enc::ResolveTexture { src: 2,
            src_sub: TextureSubresource { mip: 0, layer: 1, aspect: TextureAspect::All },
            src_origin: Origin3d { x: 3, y: 2, z: 1 }, dst: 14,
            dst_sub: TextureSubresource { mip: 1, layer: 0, aspect: TextureAspect::All },
            dst_origin: Origin3d { x: 0, y: 1, z: 2 },
            extent: Extent3d { width: 5, height: 4, depth: 1 } },
    ]
}

fn every_command_variant() -> Vec<Cmd> {
    vec![
        Cmd::CreateBuffer(1, buffer_desc()),
        Cmd::DestroyBuffer(1),
        Cmd::WriteBuffer { id: 1, offset: 17, data: vec![0, 1, 2, 127, 128, 255] },
        Cmd::CreateTexture(2, texture_desc()),
        Cmd::DestroyTexture(2),
        Cmd::CreateSampler(3, sampler_desc()),
        Cmd::DestroySampler(3),
        Cmd::CreateShader { id: 4, kind: ShaderPayloadKind::SpirV, spirv: vec![0x0723_0203, 0x0001_0000, 0, 4, 0] },
        Cmd::DestroyShader(4),
        Cmd::CreateRenderPipeline(5, render_pipeline_desc()),
        Cmd::CreateComputePipeline(7, ComputePipelineDesc {
            compute: ShaderRef { module: 4, entry: "compute_main".into() }, label: "all-compute-pipeline".into() }),
        Cmd::DestroyPipeline(5),
        Cmd::CreateBindGroup(6, BindGroupDesc { set: 2, entries: vec![
            BindEntry { binding: 0, resource: BindResource::Buffer { id: 1, offset: 4, size: 64 } },
            BindEntry { binding: 1, resource: BindResource::Texture { id: 2 } },
            BindEntry { binding: 2, resource: BindResource::Sampler { id: 3 } },
        ]}),
        Cmd::DestroyBindGroup(6),
        Cmd::CreateSurface(10, SurfaceDesc { width: 8, height: 4, format: TextureFormat::Rgba8Unorm, ddp_surface: 77 }),
        Cmd::DestroySurface(10),
        Cmd::CreateFence(11),
        Cmd::DestroyFence(11),
        Cmd::Submit(CommandBuffer { encoder: every_encoder_variant(), signal: Some((11, 0x1122_3344_5566_7788)) }),
        Cmd::WaitFence { id: 11, value: 0x8877_6655_4433_2211 },
        Cmd::Present { surface: 10, texture: 2 },
    ]
}

fn cmd_kind(cmd: &Cmd) -> &'static str {
    match cmd {
        Cmd::CreateBuffer(..) => "CreateBuffer", Cmd::DestroyBuffer(..) => "DestroyBuffer",
        Cmd::WriteBuffer { .. } => "WriteBuffer", Cmd::CreateTexture(..) => "CreateTexture",
        Cmd::DestroyTexture(..) => "DestroyTexture", Cmd::CreateSampler(..) => "CreateSampler",
        Cmd::DestroySampler(..) => "DestroySampler", Cmd::CreateShader { .. } => "CreateShader",
        Cmd::DestroyShader(..) => "DestroyShader", Cmd::CreateRenderPipeline(..) => "CreateRenderPipeline",
        Cmd::CreateComputePipeline(..) => "CreateComputePipeline", Cmd::DestroyPipeline(..) => "DestroyPipeline",
        Cmd::CreateBindGroup(..) => "CreateBindGroup", Cmd::DestroyBindGroup(..) => "DestroyBindGroup",
        Cmd::CreateSurface(..) => "CreateSurface", Cmd::DestroySurface(..) => "DestroySurface",
        Cmd::CreateFence(..) => "CreateFence", Cmd::DestroyFence(..) => "DestroyFence",
        Cmd::Submit(..) => "Submit", Cmd::WaitFence { .. } => "WaitFence", Cmd::Present { .. } => "Present",
    }
}

fn enc_kind(enc: &Enc) -> &'static str {
    match enc {
        Enc::BeginRenderPass { .. } => "BeginRenderPass", Enc::EndRenderPass => "EndRenderPass",
        Enc::SetPipeline(..) => "SetPipeline", Enc::SetBindGroup { .. } => "SetBindGroup",
        Enc::SetVertexBuffer { .. } => "SetVertexBuffer", Enc::SetIndexBuffer { .. } => "SetIndexBuffer",
        Enc::SetViewport { .. } => "SetViewport", Enc::SetScissor { .. } => "SetScissor",
        Enc::ClearRect { .. } => "ClearRect", Enc::Draw { .. } => "Draw", Enc::DrawIndexed { .. } => "DrawIndexed",
        Enc::BeginComputePass => "BeginComputePass", Enc::EndComputePass => "EndComputePass",
        Enc::Dispatch { .. } => "Dispatch", Enc::CopyBufferToBuffer { .. } => "CopyBufferToBuffer",
        Enc::CopyBufferToTexture { .. } => "CopyBufferToTexture", Enc::CopyTextureToBuffer { .. } => "CopyTextureToBuffer",
        Enc::CopyTextureToTexture { .. } => "CopyTextureToTexture", Enc::BlitTexture { .. } => "BlitTexture",
        Enc::ResolveTexture { .. } => "ResolveTexture",
    }
}

#[test]
fn every_gpu_ir_command_and_encoder_variant_round_trips() {
    let commands = every_command_variant();
    assert_eq!(commands.len(), 21, "update corpus count when Cmd grows");
    let encoders = every_encoder_variant();
    assert_eq!(encoders.len(), 20, "update corpus count when Enc grows");
    let mut cmd_kinds = BTreeSet::new();
    for command in &commands { assert!(cmd_kinds.insert(cmd_kind(command))); }
    let mut enc_kinds = BTreeSet::new();
    for encoder in &encoders { assert!(enc_kinds.insert(enc_kind(encoder))); }
    let bytes = encode_stream(&commands);
    assert_eq!(decode_stream(&bytes).expect("decode exhaustive corpus"), commands);
}

use std::collections::BTreeSet;

#[test]
fn every_truncated_single_command_is_rejected_without_panic() {
    for command in every_command_variant() {
        let encoded = encode_stream(std::slice::from_ref(&command));
        for cut in 1..encoded.len() {
            assert!(decode_stream(&encoded[..cut]).is_err(),
                "{} accepted truncated prefix {cut}/{}", cmd_kind(&command), encoded.len());
        }
    }
}

#[test]
fn unknown_top_level_and_encoder_tags_are_rejected() {
    assert!(matches!(decode_stream(&[0xff]), Err(GpuError::Decode(message)) if message.contains("bad command/encoder tag 255")));
    let mut submit = encode_stream(&[Cmd::Submit(CommandBuffer { encoder: vec![Enc::EndRenderPass], signal: None })]);
    // top-level Submit tag + encoder vector count; the first encoder tag begins at byte 5.
    assert_eq!(submit[0], 19);
    submit[5] = 0xff;
    assert!(matches!(decode_stream(&submit), Err(GpuError::Decode(message)) if message.contains("bad command/encoder tag 255")));
}

#[test]
fn software_backend_rejects_duplicate_use_after_destroy_and_out_of_bounds_resources() {
    let mut backend = SoftwareBackend::new();
    replay::apply(&mut backend, &Cmd::CreateBuffer(1, buffer_desc())).unwrap();
    assert!(matches!(replay::apply(&mut backend, &Cmd::CreateBuffer(1, buffer_desc())),
        Err(GpuError::DuplicateId { kind: "buffer", id: 1 })));
    assert!(matches!(replay::apply(&mut backend, &Cmd::WriteBuffer { id: 1, offset: 250, data: vec![0; 8] }),
        Err(GpuError::OutOfBounds)));
    replay::apply(&mut backend, &Cmd::DestroyBuffer(1)).unwrap();
    assert!(matches!(replay::apply(&mut backend, &Cmd::WriteBuffer { id: 1, offset: 0, data: vec![1] }),
        Err(GpuError::UnknownId { kind: "buffer", id: 1 })));

    replay::apply(&mut backend, &Cmd::CreateTexture(2, texture_desc())).unwrap();
    assert!(matches!(replay::apply(&mut backend, &Cmd::CreateTexture(2, texture_desc())),
        Err(GpuError::DuplicateId { kind: "texture", id: 2 })));
    replay::apply(&mut backend, &Cmd::DestroyTexture(2)).unwrap();
    assert!(matches!(replay::apply(&mut backend, &Cmd::DestroyTexture(2)),
        Err(GpuError::UnknownId { kind: "texture", id: 2 })));
}

fn assert_decode_rejects(label: &str, bytes: Vec<u8>) {
    assert!(decode_stream(&bytes).is_err(), "malformed {label} decoded successfully");
}

#[test]
fn attacker_controlled_lengths_and_counts_fail_without_large_preallocation() {
    // Shader word count.
    let mut e = Encoder::new();
    e.u8(8); e.u32(1); e.u32(u32::MAX);
    assert_decode_rejects("shader word count", e.into_vec());

    // Buffer label byte length.
    let mut e = Encoder::new();
    e.u8(1); e.u32(1); e.u64(16); e.u32(buffer_usage::COPY_DST); e.u32(u32::MAX);
    assert_decode_rejects("label byte length", e.into_vec());

    // Bind group entry count.
    let mut e = Encoder::new();
    e.u8(13); e.u32(1); e.u32(0); e.u32(u32::MAX);
    assert_decode_rejects("bind entry count", e.into_vec());

    // Submit encoder-op count.
    let mut e = Encoder::new();
    e.u8(19); e.u32(u32::MAX);
    assert_decode_rejects("encoder op count", e.into_vec());

    // Nested render-pass attachment count within a one-op Submit.
    let mut e = Encoder::new();
    e.u8(19); e.u32(1); e.u8(1); e.u32(u32::MAX);
    assert_decode_rejects("color attachment count", e.into_vec());

    // Canonical bool enforcement for Submit.signal.
    let mut e = Encoder::new();
    e.u8(19); e.u32(0); e.u8(2);
    assert!(matches!(decode_stream(&e.into_vec()), Err(GpuError::Decode(message)) if message.contains("non-canonical boolean wire byte 2")));
}

#[test]
fn replay_write_buffer_fast_path_rejects_forged_length_before_backend_mutation() {
    let mut backend = RecordingBackend::new();
    replay::apply(&mut backend, &Cmd::CreateBuffer(7, buffer_desc())).unwrap();
    let before = backend.log.clone();
    let mut e = Encoder::new();
    e.u8(3); e.u32(7); e.u64(0); e.u32(u32::MAX); // no claimed payload follows
    let err = replay::replay_stream(&mut backend, &e.into_vec()).unwrap_err();
    assert!(matches!(err, GpuError::Decode(ref message) if message.contains("replay validation command")),
        "forged fast-path length returned an unexpected error: {err:?}");
    assert_eq!(backend.log, before, "fast path called write_buffer before validating the payload length");
}

#[test]
fn malformed_stream_does_not_partially_mutate_the_backend() {
    let valid = Cmd::CreateBuffer(42, buffer_desc());
    let mut bytes = encode_stream(&[valid]);
    bytes.push(0xff); // malformed second command in the same received stream/frame
    let mut backend = RecordingBackend::new();
    assert!(replay::replay_stream(&mut backend, &bytes).is_err());
    assert!(backend.log.is_empty(), "malformed stream partially applied events: {:?}", backend.log);
}

#[test]
fn non_finite_render_state_is_rejected_at_the_wire_boundary() {
    let commands = [Cmd::Submit(CommandBuffer {
        encoder: vec![
            Enc::SetViewport { x: f32::NAN, y: 0.0, w: f32::INFINITY, h: 1.0,
                min_depth: f32::NEG_INFINITY, max_depth: 1.0 },
            Enc::ClearRect { texture: 1, x: 0, y: 0, w: 1, h: 1,
                color: [f32::NAN, 0.0, 0.0, 1.0] },
        ],
        signal: None,
    })];
    assert!(decode_stream(&encode_stream(&commands)).is_err(), "non-finite GPU state crossed the trust boundary");
}
