//! Adversarial coverage for the CPU reference executor (the semantic ORACLE) + the runtime validation it
//! sits behind. A malformed-but-decodable batch — an out-of-bounds copy/write/read, a use-after-free, a
//! duplicate id, a draw that overruns its vertex buffer, a dispatch with nothing bound, an over-huge grid —
//! must produce a TYPED error (`OutOfBounds` / `UnknownId` / `DuplicateId` / `Invalid`), never memory
//! corruption, a panic, or a hang. Every rejection is atomic: the command-buffer is fully validated before
//! any mutation, so a bad op late in a submit leaves earlier state untouched.
//!
//! These drive `GpuExecutor::execute` directly over a fresh `SessionResources` (isolating the executor's
//! own validation from the runtime residency layer), plus a couple of runtime-pipeline checks.

use hl_gpu::protocol::model::descriptor::*;
use hl_gpu::protocol::model::enums::*;
use hl_gpu::protocol::model::kernel::{glsl_stage, GlslDescriptor};
use hl_gpu::{
    BufferId, Cmd, CommandBuffer, CpuExecutor, Enc, GpuError, GpuExecutor, InProcessCommandSink,
    SessionResources, ShaderPayloadKind, TextureId,
};

fn buf(size: u64, usage: u32) -> BufferDesc {
    BufferDesc { size, usage, label: String::new() }
}

fn tex(w: u32, h: u32, fmt: TextureFormat, usage: u32) -> TextureDesc {
    TextureDesc {
        width: w,
        height: h,
        depth: 1,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D2,
        format: fmt,
        usage,
        label: String::new(),
    }
}

/// A fresh executor + resources with `setup` already applied (asserted clean).
fn primed(setup: &[Cmd]) -> (CpuExecutor, SessionResources) {
    let mut exec = CpuExecutor::new();
    let mut res = SessionResources::new();
    exec.execute(&mut res, setup).expect("setup must run cleanly");
    (exec, res)
}

fn submit(ops: Vec<Enc>) -> Cmd {
    Cmd::Submit(CommandBuffer { encoder: ops, signal: None })
}

// ---------------------------------------------------------------------------------------------------
// resource lifecycle: duplicate create, use-after-free, double-free, empty batch
// ---------------------------------------------------------------------------------------------------

#[test]
fn empty_batch_is_a_clean_noop() {
    let mut exec = CpuExecutor::new();
    let mut res = SessionResources::new();
    assert_eq!(exec.execute(&mut res, &[]).unwrap(), vec![]);
    assert_eq!(res.live_count(), 0);
}

#[test]
fn duplicate_create_is_typed_duplicate_id() {
    let (mut exec, mut res) = primed(&[Cmd::CreateBuffer(1, buf(8, buffer_usage::COPY_DST))]);
    let err = exec.execute(&mut res, &[Cmd::CreateBuffer(1, buf(8, buffer_usage::COPY_DST))]).unwrap_err();
    assert_eq!(err, GpuError::DuplicateId { kind: "buffer", id: 1 });
}

#[test]
fn destroy_unknown_is_typed_unknown_id() {
    let mut exec = CpuExecutor::new();
    let mut res = SessionResources::new();
    assert_eq!(
        exec.execute(&mut res, &[Cmd::DestroyBuffer(99)]).unwrap_err(),
        GpuError::UnknownId { kind: "buffer", id: 99 }
    );
}

#[test]
fn use_after_free_is_typed_unknown_id() {
    let (mut exec, mut res) = primed(&[
        Cmd::CreateBuffer(1, buf(8, buffer_usage::COPY_DST)),
        Cmd::DestroyBuffer(1),
    ]);
    let err = exec
        .execute(&mut res, &[Cmd::WriteBuffer { id: 1, offset: 0, data: vec![1, 2, 3, 4] }])
        .unwrap_err();
    assert_eq!(err, GpuError::UnknownId { kind: "buffer", id: 1 });
    // Double free is likewise typed.
    assert_eq!(
        exec.execute(&mut res, &[Cmd::DestroyBuffer(1)]).unwrap_err(),
        GpuError::UnknownId { kind: "buffer", id: 1 }
    );
}

// ---------------------------------------------------------------------------------------------------
// bounds checks: write / read / fill / copy OutOfBounds (never corruption or panic)
// ---------------------------------------------------------------------------------------------------

#[test]
fn write_buffer_out_of_bounds_is_rejected() {
    let (mut exec, mut res) = primed(&[Cmd::CreateBuffer(1, buf(4, buffer_usage::COPY_DST))]);
    // offset + len overruns the 4-byte buffer.
    let err = exec
        .execute(&mut res, &[Cmd::WriteBuffer { id: 1, offset: 2, data: vec![0, 0, 0, 0] }])
        .unwrap_err();
    assert_eq!(err, GpuError::OutOfBounds);
    // A write starting past the end also fails, even with empty data past the end.
    assert_eq!(
        exec.execute(&mut res, &[Cmd::WriteBuffer { id: 1, offset: 5, data: vec![] }]).unwrap_err(),
        GpuError::OutOfBounds
    );
}

#[test]
fn read_buffer_out_of_bounds_is_rejected() {
    let (exec, res) = primed(&[Cmd::CreateBuffer(1, buf(4, buffer_usage::COPY_DST))]);
    let mut out = [0u8; 4];
    // Reading 4 bytes at offset 2 runs off the end.
    assert_eq!(
        exec.read_buffer(&res, BufferId(1), 2, &mut out).unwrap_err(),
        GpuError::OutOfBounds
    );
    // Reading a live buffer fully at the boundary is fine (offset 0, len 4).
    assert!(exec.read_buffer(&res, BufferId(1), 0, &mut out).is_ok());
    // Reading a non-existent buffer is a typed error.
    assert_eq!(
        exec.read_buffer(&res, BufferId(2), 0, &mut out).unwrap_err(),
        GpuError::UnknownId { kind: "buffer", id: 2 }
    );
}

#[test]
fn fill_buffer_out_of_bounds_is_rejected_atomically() {
    let (mut exec, mut res) = primed(&[
        Cmd::CreateBuffer(1, buf(8, buffer_usage::COPY_DST | buffer_usage::COPY_SRC)),
        Cmd::WriteBuffer { id: 1, offset: 0, data: vec![0xFF; 8] },
    ]);
    // size overruns [offset, offset+size) past the 8-byte buffer.
    let err = exec
        .execute(&mut res, &[submit(vec![Enc::FillBuffer { buffer: 1, offset: 4, size: 8, value: 0 }])])
        .unwrap_err();
    assert_eq!(err, GpuError::OutOfBounds);
    // Failure atomicity: the buffer is untouched (validation runs before any write).
    let mut out = [0u8; 8];
    exec.read_buffer(&res, BufferId(1), 0, &mut out).unwrap();
    assert_eq!(out, [0xFF; 8], "a rejected fill mutated nothing");
}

#[test]
fn copy_buffer_to_buffer_out_of_bounds_is_rejected() {
    let (mut exec, mut res) = primed(&[
        Cmd::CreateBuffer(1, buf(4, buffer_usage::COPY_SRC | buffer_usage::COPY_DST)),
        Cmd::CreateBuffer(2, buf(4, buffer_usage::COPY_DST)),
    ]);
    // size 8 exceeds the 4-byte source.
    let err = exec
        .execute(
            &mut res,
            &[submit(vec![Enc::CopyBufferToBuffer { src: 1, src_offset: 0, dst: 2, dst_offset: 0, size: 8 }])],
        )
        .unwrap_err();
    assert_eq!(err, GpuError::OutOfBounds);
}

#[test]
fn copy_missing_usage_flag_is_typed_invalid() {
    // A copy source without the COPY_SRC usage bit is rejected as Invalid (not OOB / not a silent copy).
    let (mut exec, mut res) = primed(&[
        Cmd::CreateBuffer(1, buf(4, buffer_usage::COPY_DST)), // no COPY_SRC
        Cmd::CreateBuffer(2, buf(4, buffer_usage::COPY_DST)),
    ]);
    let err = exec
        .execute(
            &mut res,
            &[submit(vec![Enc::CopyBufferToBuffer { src: 1, src_offset: 0, dst: 2, dst_offset: 0, size: 4 }])],
        )
        .unwrap_err();
    assert_eq!(err, GpuError::Invalid("copy src lacks COPY_SRC"));
}

#[test]
fn copy_from_unknown_buffer_is_typed_unknown_id() {
    let (mut exec, mut res) = primed(&[Cmd::CreateBuffer(2, buf(4, buffer_usage::COPY_DST))]);
    let err = exec
        .execute(
            &mut res,
            &[submit(vec![Enc::CopyBufferToBuffer { src: 77, src_offset: 0, dst: 2, dst_offset: 0, size: 4 }])],
        )
        .unwrap_err();
    assert_eq!(err, GpuError::UnknownId { kind: "buffer", id: 77 });
}

#[test]
fn copy_texture_to_texture_region_out_of_bounds_is_rejected() {
    let (mut exec, mut res) = primed(&[
        Cmd::CreateTexture(1, tex(4, 4, TextureFormat::Rgba8Unorm, texture_usage::COPY_SRC)),
        Cmd::CreateTexture(2, tex(4, 4, TextureFormat::Rgba8Unorm, texture_usage::COPY_DST)),
    ]);
    let err = exec
        .execute(
            &mut res,
            &[submit(vec![Enc::CopyTextureToTexture {
                src: 1,
                src_sub: TextureSubresource::base(),
                src_origin: Origin3d { x: 2, y: 2, z: 0 }, // origin + extent runs past the 4x4 plane
                dst: 2,
                dst_sub: TextureSubresource::base(),
                dst_origin: Origin3d::default(),
                extent: Extent3d { width: 4, height: 4, depth: 1 },
            }])],
        )
        .unwrap_err();
    assert_eq!(err, GpuError::OutOfBounds);
}

// ---------------------------------------------------------------------------------------------------
// encoder-state validation: open passes, nesting, unbound draw/dispatch
// ---------------------------------------------------------------------------------------------------

#[test]
fn command_buffer_that_ends_inside_a_pass_is_rejected() {
    let (mut exec, mut res) =
        primed(&[Cmd::CreateTexture(1, tex(2, 2, TextureFormat::Rgba8Unorm, texture_usage::RENDER_TARGET))]);
    let err = exec
        .execute(
            &mut res,
            &[submit(vec![Enc::BeginRenderPass {
                color: vec![ColorAttachment {
                    texture: 1,
                    load: LoadOp::Clear,
                    clear: [0.0; 4],
                    store: true,
                }],
                depth: None,
            }])], // no EndRenderPass
        )
        .unwrap_err();
    assert_eq!(err, GpuError::Invalid("command buffer ends inside an open pass"));
}

#[test]
fn nested_render_pass_is_rejected() {
    let (mut exec, mut res) =
        primed(&[Cmd::CreateTexture(1, tex(2, 2, TextureFormat::Rgba8Unorm, texture_usage::RENDER_TARGET))]);
    let begin = || Enc::BeginRenderPass {
        color: vec![ColorAttachment { texture: 1, load: LoadOp::Load, clear: [0.0; 4], store: true }],
        depth: None,
    };
    let err = exec.execute(&mut res, &[submit(vec![begin(), begin()])]).unwrap_err();
    assert_eq!(err, GpuError::Invalid("nested render pass"));
}

#[test]
fn dispatch_with_no_pipeline_bound_is_rejected() {
    let mut exec = CpuExecutor::new();
    let mut res = SessionResources::new();
    let err = exec
        .execute(
            &mut res,
            &[submit(vec![Enc::BeginComputePass, Enc::Dispatch { x: 1, y: 1, z: 1 }, Enc::EndComputePass])],
        )
        .unwrap_err();
    assert_eq!(err, GpuError::Invalid("Dispatch with no pipeline bound"));
}

#[test]
fn draw_overrunning_its_vertex_buffer_is_out_of_bounds() {
    // A pipeline with a per-vertex layout (stride 16) plus a vertex buffer too small for the draw range:
    // validation must reject with OutOfBounds before any rasterization touches memory.
    let mut exec = CpuExecutor::new();
    let mut res = SessionResources::new();
    exec.execute(
        &mut res,
        &[
            Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::SpirV, spirv: vec![0x0723_0203] },
            Cmd::CreateTexture(1, tex(4, 4, TextureFormat::Rgba8Unorm, texture_usage::RENDER_TARGET)),
            Cmd::CreateBuffer(1, buf(16, buffer_usage::VERTEX)), // room for exactly 1 vertex of stride 16
            Cmd::CreateRenderPipeline(
                1,
                RenderPipelineDesc {
                    vertex: ShaderRef { module: 1, entry: "v".into() },
                    fragment: None,
                    vertex_buffers: vec![VertexLayout {
                        stride: 16,
                        step_mode: 0,
                        attrs: vec![VertexAttr { location: 0, format: 0, offset: 0 }],
                    }],
                    color_targets: vec![ColorTargetState {
                        format: TextureFormat::Rgba8Unorm,
                        blend: None,
                        write_mask: 0xF,
                    }],
                    depth: None,
                    topology: Topology::TriangleList,
                    cull: 0,
                    front_face: 0,
                    label: String::new(),
                },
            ),
        ],
    )
    .unwrap();
    let err = exec
        .execute(
            &mut res,
            &[submit(vec![
                Enc::BeginRenderPass {
                    color: vec![ColorAttachment {
                        texture: 1,
                        load: LoadOp::Load,
                        clear: [0.0; 4],
                        store: true,
                    }],
                    depth: None,
                },
                Enc::SetPipeline(1),
                Enc::SetVertexBuffer { slot: 0, buffer: 1, offset: 0 },
                // 3 vertices * stride 16 = 48 bytes needed, buffer is 16 -> OOB.
                Enc::Draw { vertex_count: 3, instance_count: 1, first_vertex: 0, first_instance: 0 },
                Enc::EndRenderPass,
            ])],
        )
        .unwrap_err();
    assert_eq!(err, GpuError::OutOfBounds);
}

#[test]
fn wait_on_an_unsignalled_fence_value_is_rejected() {
    let (mut exec, mut res) = primed(&[Cmd::CreateFence(1)]);
    let err = exec.execute(&mut res, &[Cmd::WaitFence { id: 1, value: 5 }]).unwrap_err();
    assert_eq!(err, GpuError::Invalid("wait on a fence value that was never signalled"));
}

#[test]
fn present_size_mismatch_is_rejected() {
    let (mut exec, mut res) = primed(&[
        Cmd::CreateTexture(1, tex(4, 4, TextureFormat::Bgra8Unorm, texture_usage::PRESENT)),
        Cmd::CreateSurface(1, SurfaceDesc { width: 8, height: 8, format: TextureFormat::Bgra8Unorm, hlp_surface: 1 }),
    ]);
    let err = exec.execute(&mut res, &[Cmd::Present { surface: 1, texture: 1 }]).unwrap_err();
    assert_eq!(err, GpuError::Invalid("present texture size does not match surface"));
}

// ---------------------------------------------------------------------------------------------------
// huge dims must not hang or panic; opaque graphics shaders are accepted
// ---------------------------------------------------------------------------------------------------

#[test]
fn huge_dispatch_grid_over_a_spirv_pipeline_short_circuits() {
    // A dispatch with a maximal grid over a SPIR-V (non-kernel) compute pipeline must return promptly: the
    // CPU oracle cannot run SPIR-V, so it records the dispatch and returns Ok without iterating u32::MAX^3
    // threads. This proves an adversarial grid neither hangs nor panics.
    let mut exec = CpuExecutor::new();
    let mut res = SessionResources::new();
    exec.execute(
        &mut res,
        &[
            Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::SpirV, spirv: vec![0x0723_0203] },
            Cmd::CreateComputePipeline(
                1,
                ComputePipelineDesc { compute: ShaderRef { module: 1, entry: "c".into() }, label: String::new() },
            ),
            Cmd::CreateBuffer(1, buf(16, buffer_usage::STORAGE)),
            Cmd::CreateBindGroup(
                1,
                BindGroupDesc {
                    set: 0,
                    entries: vec![BindEntry {
                        binding: 0,
                        resource: BindResource::Buffer { id: 1, offset: 0, size: 16 },
                    }],
                },
            ),
        ],
    )
    .unwrap();
    exec.execute(
        &mut res,
        &[submit(vec![
            Enc::BeginComputePass,
            Enc::SetPipeline(1),
            Enc::SetBindGroup { index: 0, group: 1 },
            Enc::Dispatch { x: u32::MAX, y: u32::MAX, z: u32::MAX },
            Enc::EndComputePass,
        ])],
    )
    .expect("huge grid over a SPIR-V pipeline returns cleanly (no kernel to run)");
}

#[test]
fn glsl_and_legacy_graphics_shaders_are_accepted_opaquely_by_the_executor() {
    // The fixed-function CPU oracle rasterizes from the pipeline + vertex data, not the shader source, so a
    // forwarded GLSL / legacy-MSL graphics module is an accepted opaque handle at the executor boundary.
    let gd = GlslDescriptor {
        stage: glsl_stage::VERTEX,
        entry: "vmain".into(),
        source: "#version 460\nvoid main(){}\n".into(),
    };
    let mut exec = CpuExecutor::new();
    let mut res = SessionResources::new();
    exec.execute(
        &mut res,
        &[
            Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::Glsl, spirv: gd.to_words() },
            Cmd::CreateShader { id: 2, kind: ShaderPayloadKind::LegacyMsl, spirv: vec![0x4141_4141] },
        ],
    )
    .expect("opaque graphics shader modules are accepted by the CPU executor");
    assert_eq!(res.shaders.len(), 2);
}

// ---------------------------------------------------------------------------------------------------
// runtime-pipeline validation (validate.rs) rejects an unsupported shader payload before execution
// ---------------------------------------------------------------------------------------------------

#[test]
fn runtime_rejects_a_shader_payload_the_backend_never_advertised() {
    use hl_gpu::CommandSink;
    // The CpuExecutor advertises only the KERNEL shader payload, so a GLSL CreateShader routed through the
    // full runtime pipeline is rejected at VALIDATE (a typed ResourceLimit), before the executor is touched.
    let gd = GlslDescriptor { stage: glsl_stage::VERTEX, entry: "v".into(), source: "x".into() };
    let mut sink = InProcessCommandSink::new(CpuExecutor::new());
    let err = sink
        .submit(&[Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::Glsl, spirv: gd.to_words() }])
        .unwrap_err();
    assert_eq!(err, GpuError::ResourceLimit("shader payload"));
}

#[test]
fn present_returns_the_presented_pair() {
    // A well-formed present flows a Presented{surface, texture} back out of execute.
    let (mut exec, mut res) = primed(&[
        Cmd::CreateTexture(1, tex(4, 4, TextureFormat::Bgra8Unorm, texture_usage::PRESENT)),
        Cmd::CreateSurface(1, SurfaceDesc { width: 4, height: 4, format: TextureFormat::Bgra8Unorm, hlp_surface: 1 }),
    ]);
    let presents = exec.execute(&mut res, &[Cmd::Present { surface: 1, texture: 1 }]).unwrap();
    assert_eq!(presents.len(), 1);
    assert_eq!((presents[0].surface, presents[0].texture), (hl_gpu::SurfaceId(1), TextureId(1)));
}
