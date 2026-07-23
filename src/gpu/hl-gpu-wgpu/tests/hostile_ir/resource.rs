use super::*;

// =================================================================================================
// (1) DANGLING / never-created ids
// =================================================================================================

#[test]
fn dangling_buffer_in_copy_is_unknown_id() {
    let Some(mut g) = exec() else { return };
    hostile(
        &mut g,
        "dangling_buffer_copy",
        &[
            Cmd::CreateBuffer(1, buf(64, buffer_usage::COPY_SRC | buffer_usage::COPY_DST)),
            // src 999 was never created.
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::CopyBufferToBuffer {
                    src: 999,
                    src_offset: 0,
                    dst: 1,
                    dst_offset: 0,
                    size: 16,
                }],
                signal: None,
            }),
        ],
        is_unknown,
    );
}

#[test]
fn dangling_texture_in_copy_is_unknown_id() {
    let Some(mut g) = exec() else { return };
    hostile(
        &mut g,
        "dangling_texture_copy",
        &[
            Cmd::CreateTexture(1, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::CopyTextureToTexture {
                    src: 777,
                    src_sub: sub(),
                    src_origin: Origin3d::default(),
                    dst: 1,
                    dst_sub: sub(),
                    dst_origin: Origin3d::default(),
                    extent: Extent3d {
                        width: 2,
                        height: 2,
                        depth: 1,
                    },
                }],
                signal: None,
            }),
        ],
        is_unknown,
    );
}

#[test]
fn dangling_pipeline_in_draw_is_unknown_id() {
    let Some(mut g) = exec() else { return };
    hostile(
        &mut g,
        "dangling_pipeline_draw",
        &[
            Cmd::CreateTexture(1, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
            Cmd::Submit(CommandBuffer {
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
                    Enc::SetPipeline(555), // never created
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
        is_unknown,
    );
}

#[test]
fn dangling_vertex_buffer_is_unknown_id() {
    let Some(mut g) = exec() else { return };
    let mut cmds = vec![Cmd::CreateTexture(
        1,
        tex(4, 4, TextureFormat::Rgba8Unorm, RT),
    )];
    cmds.extend(white_triangle_pipeline(1, 1, 2));
    cmds.push(Cmd::Submit(CommandBuffer {
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
            Enc::SetPipeline(1),
            Enc::SetVertexBuffer {
                slot: 0,
                buffer: 404,
                offset: 0,
            }, // never created
            Enc::Draw {
                vertex_count: 3,
                instance_count: 1,
                first_vertex: 0,
                first_instance: 0,
            },
            Enc::EndRenderPass,
        ],
        signal: None,
    }));
    hostile(&mut g, "dangling_vertex_buffer", &cmds, is_unknown);
}

#[test]
fn dangling_pipeline_in_dispatch_is_unknown_id() {
    let Some(mut g) = exec() else { return };
    hostile(
        &mut g,
        "dangling_dispatch_pipeline",
        &[
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
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginComputePass,
                    Enc::SetPipeline(321), // never created
                    Enc::SetBindGroup { index: 0, group: 1 },
                    Enc::Dispatch { x: 1, y: 1, z: 1 },
                    Enc::EndComputePass,
                ],
                signal: None,
            }),
        ],
        is_unknown,
    );
}
