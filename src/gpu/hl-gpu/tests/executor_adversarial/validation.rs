use super::*;

#[test]
fn command_buffer_that_ends_inside_a_pass_is_rejected() {
    let (mut exec, mut res) = primed(&[Cmd::CreateTexture(
        1,
        tex(
            2,
            2,
            TextureFormat::Rgba8Unorm,
            texture_usage::RENDER_TARGET,
        ),
    )]);
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
    assert_eq!(
        err,
        GpuError::Invalid("command buffer ends inside an open pass")
    );
}

#[test]
fn bind_group_rejects_a_reused_resource_id() {
    let (mut exec, mut res) = primed(&[
        Cmd::CreateShader {
            id: 1,
            kind: ShaderPayloadKind::SpirV,
            spirv: vec![0x0723_0203],
        },
        Cmd::CreateComputePipeline(
            1,
            ComputePipelineDesc {
                compute: ShaderRef {
                    module: 1,
                    entry: "main".into(),
                },
                label: String::new(),
            },
        ),
        Cmd::CreateBuffer(1, buf(16, buffer_usage::STORAGE)),
        Cmd::CreateBindGroup(
            1,
            BindGroupDesc {
                set: 0,
                entries: vec![BindEntry {
                    binding: 0,
                    resource: BindResource::Buffer {
                        id: 1,
                        offset: 0,
                        size: 16,
                    },
                }],
            },
        ),
    ]);

    exec.execute(
        &mut res,
        &[
            Cmd::DestroyBuffer(1),
            Cmd::CreateBuffer(1, buf(16, buffer_usage::STORAGE)),
        ],
    )
    .expect("reusing a destroyed resource id creates a distinct allocation");

    let error = exec
        .execute(
            &mut res,
            &[submit(vec![
                Enc::BeginComputePass,
                Enc::SetPipeline(1),
                Enc::SetBindGroup { index: 0, group: 1 },
                Enc::Dispatch { x: 1, y: 1, z: 1 },
                Enc::EndComputePass,
            ])],
        )
        .expect_err("the bind group must retain the original allocation generation");

    assert_eq!(
        error,
        GpuError::UnknownId {
            kind: BufferId::KIND,
            id: 1,
        }
    );
}

#[test]
fn nested_render_pass_is_rejected() {
    let (mut exec, mut res) = primed(&[Cmd::CreateTexture(
        1,
        tex(
            2,
            2,
            TextureFormat::Rgba8Unorm,
            texture_usage::RENDER_TARGET,
        ),
    )]);
    let begin = || Enc::BeginRenderPass {
        color: vec![ColorAttachment {
            texture: 1,
            load: LoadOp::Load,
            clear: [0.0; 4],
            store: true,
        }],
        depth: None,
    };
    let err = exec
        .execute(&mut res, &[submit(vec![begin(), begin()])])
        .unwrap_err();
    assert_eq!(err, GpuError::Invalid("nested render pass"));
}

#[test]
fn dispatch_with_no_pipeline_bound_is_rejected() {
    let mut exec = CpuExecutor::new();
    let mut res = SessionResources::new();
    let err = exec
        .execute(
            &mut res,
            &[submit(vec![
                Enc::BeginComputePass,
                Enc::Dispatch { x: 1, y: 1, z: 1 },
                Enc::EndComputePass,
            ])],
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
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::SpirV,
                spirv: vec![0x0723_0203],
            },
            Cmd::CreateTexture(
                1,
                tex(
                    4,
                    4,
                    TextureFormat::Rgba8Unorm,
                    texture_usage::RENDER_TARGET,
                ),
            ),
            Cmd::CreateBuffer(1, buf(16, buffer_usage::VERTEX)), // room for exactly 1 vertex of stride 16
            Cmd::CreateRenderPipeline(
                1,
                RenderPipelineDesc {
                    vertex: ShaderRef {
                        module: 1,
                        entry: "v".into(),
                    },
                    fragment: None,
                    vertex_buffers: vec![VertexLayout {
                        stride: 16,
                        step_mode: 0,
                        attrs: vec![VertexAttr {
                            location: 0,
                            format: 0,
                            offset: 0,
                        }],
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
                    sample_count: 1,
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
                Enc::SetVertexBuffer {
                    slot: 0,
                    buffer: 1,
                    offset: 0,
                },
                // 3 vertices * stride 16 = 48 bytes needed, buffer is 16 -> OOB.
                Enc::Draw {
                    vertex_count: 3,
                    instance_count: 1,
                    first_vertex: 0,
                    first_instance: 0,
                },
                Enc::EndRenderPass,
            ])],
        )
        .unwrap_err();
    assert_eq!(err, GpuError::OutOfBounds);
}
