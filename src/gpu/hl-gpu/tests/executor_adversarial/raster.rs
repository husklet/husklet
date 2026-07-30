//! Hostile RASTER-path inputs: the draw arms of the CPU oracle read extents, strides, offsets and index
//! values that all originate in the guest IR stream. Each case here reaches the rasterizer through the
//! executor's own pre-execution validation pass and must come back as a typed `GpuError` — never a slice
//! panic, an arithmetic overflow, or a multi-gigabyte reservation.

use super::*;

/// Stride-28 vertices: `[x, y, z, r, g, b, a]` as little-endian f32 — the layout `read_vertex` decodes.
fn vertex_bytes(verts: &[[f32; 7]]) -> Vec<u8> {
    let mut out = Vec::with_capacity(verts.len() * 28);
    for v in verts {
        for c in v {
            out.extend_from_slice(&c.to_le_bytes());
        }
    }
    out
}

/// A screen-covering triangle at depth 0.5, opaque white.
fn covering_triangle() -> Vec<u8> {
    vertex_bytes(&[
        [-1.0, -1.0, 0.5, 1.0, 1.0, 1.0, 1.0],
        [3.0, -1.0, 0.5, 1.0, 1.0, 1.0, 1.0],
        [-1.0, 3.0, 0.5, 1.0, 1.0, 1.0, 1.0],
    ])
}

/// A depth-tested render pipeline over one `Rgba8Unorm` target with a stride-28 slot-0 vertex layout.
fn depth_pipeline(id: u32, step_mode: u32, depth: Option<DepthState>) -> Cmd {
    Cmd::CreateRenderPipeline(
        id,
        RenderPipelineDesc {
            vertex: ShaderRef {
                module: 1,
                entry: "v".into(),
            },
            fragment: None,
            vertex_buffers: vec![VertexLayout {
                stride: 28,
                step_mode,
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
            depth,
            topology: Topology::TriangleList,
            cull: 0,
            front_face: 0,
            sample_count: 1,
            label: String::new(),
        },
    )
}

fn spirv_module() -> Cmd {
    Cmd::CreateShader {
        id: 1,
        kind: ShaderPayloadKind::SpirV,
        spirv: vec![0x0723_0203],
    }
}

// ---------------------------------------------------------------------------------------------------
// attachment extents must agree
// ---------------------------------------------------------------------------------------------------

/// The depth-tested raster path sizes its per-pixel coverage window from the DEPTH attachment and then
/// composites those pixels into each color attachment, so a render pass whose attachments disagree on
/// extent would index a small color plane with a large depth index. WebGPU and Vulkan both require every
/// attachment of a pass to share an extent; the oracle must reject a mismatch up front, not read past the
/// end of the color plane.
#[test]
fn a_render_pass_whose_attachments_disagree_on_extent_is_rejected() {
    let (mut exec, mut res) = primed(&[
        spirv_module(),
        Cmd::CreateTexture(
            1,
            tex(
                4,
                4,
                TextureFormat::Rgba8Unorm,
                texture_usage::RENDER_TARGET,
            ),
        ),
        // A depth attachment FOUR TIMES wider and taller than the color attachment.
        Cmd::CreateTexture(
            2,
            tex(
                16,
                16,
                TextureFormat::Depth32Float,
                texture_usage::RENDER_TARGET,
            ),
        ),
        Cmd::CreateBuffer(1, buf(28 * 3, buffer_usage::VERTEX | buffer_usage::COPY_DST)),
        Cmd::WriteBuffer {
            id: 1,
            offset: 0,
            data: covering_triangle(),
        },
        depth_pipeline(
            1,
            0,
            Some(DepthState::depth_only(
                TextureFormat::Depth32Float,
                true,
                compare::LESS,
            )),
        ),
    ]);

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
                    depth: Some(DepthAttachment {
                        texture: 2,
                        load: LoadOp::Clear,
                        clear_depth: 1.0,
                        clear_stencil: 0,
                    }),
                },
                Enc::SetPipeline(1),
                Enc::SetVertexBuffer {
                    slot: 0,
                    buffer: 1,
                    offset: 0,
                },
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
    assert_eq!(
        err,
        GpuError::Invalid("render pass attachments disagree on extent")
    );
}

// ---------------------------------------------------------------------------------------------------
// the vertex fetch is bounded whatever the layout's step mode says
// ---------------------------------------------------------------------------------------------------

/// The rasterizer always fetches `vertex_count` vertices from slot 0, so `vertex_count` must be bounded
/// against that buffer even when slot 0's layout is PER-INSTANCE (`step_mode == 1`) — the branch that
/// otherwise only bounds `first_instance + instance_count`. Unbounded, a maximal `vertex_count` reserves
/// tens of gigabytes for the vertex vector before the per-vertex bounds check can reject anything.
#[test]
fn a_maximal_vertex_count_over_a_per_instance_layout_is_rejected_without_reserving() {
    let (mut exec, mut res) = primed(&[
        spirv_module(),
        Cmd::CreateTexture(
            1,
            tex(
                4,
                4,
                TextureFormat::Rgba8Unorm,
                texture_usage::RENDER_TARGET,
            ),
        ),
        Cmd::CreateBuffer(1, buf(28, buffer_usage::VERTEX)),
        depth_pipeline(1, 1, None),
    ]);

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
                Enc::Draw {
                    vertex_count: u32::MAX,
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

// ---------------------------------------------------------------------------------------------------
// an indexed draw's vertex address arithmetic must not overflow
// ---------------------------------------------------------------------------------------------------

/// `DrawIndexed` computes a vertex byte address from `index * stride + base_vertex`, and all three
/// operands are guest-controlled (the index comes out of buffer CONTENT, which no validation can bound).
/// A maximal index over a maximal stride overflows `usize` before the bounds check runs — it must be
/// rejected as out of bounds instead.
#[test]
fn an_indexed_draw_with_a_maximal_index_and_stride_is_out_of_bounds_not_an_overflow() {
    let stride = u32::MAX;
    let (mut exec, mut res) = primed(&[
        spirv_module(),
        Cmd::CreateTexture(
            1,
            tex(
                4,
                4,
                TextureFormat::Rgba8Unorm,
                texture_usage::RENDER_TARGET,
            ),
        ),
        Cmd::CreateBuffer(1, buf(28, buffer_usage::VERTEX)),
        // One u32 index, set to the maximum.
        Cmd::CreateBuffer(2, buf(4, buffer_usage::INDEX | buffer_usage::COPY_DST)),
        Cmd::WriteBuffer {
            id: 2,
            offset: 0,
            data: u32::MAX.to_le_bytes().to_vec(),
        },
        Cmd::CreateRenderPipeline(
            1,
            RenderPipelineDesc {
                vertex: ShaderRef {
                    module: 1,
                    entry: "v".into(),
                },
                fragment: None,
                vertex_buffers: vec![VertexLayout {
                    stride,
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
    ]);

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
                Enc::SetIndexBuffer {
                    buffer: 2,
                    offset: 0,
                    format: IndexFormat::U32,
                },
                Enc::DrawIndexed {
                    index_count: 1,
                    instance_count: 1,
                    first_index: 0,
                    base_vertex: i32::MAX,
                    first_instance: 0,
                },
                Enc::EndRenderPass,
            ])],
        )
        .unwrap_err();
    assert_eq!(err, GpuError::OutOfBounds);
}

// ---------------------------------------------------------------------------------------------------
// the instance count is bounded
// ---------------------------------------------------------------------------------------------------

/// `instance_count` is only bounded when some vertex layout is per-instance; otherwise the rasterizer
/// repeats the whole draw that many times. A maximal count means ~4 billion full-framebuffer
/// rasterizations — a CPU-time denial of service on the host service, with no memory growth to trip any
/// residency ceiling. It must be rejected before the first pass, the way an over-cap dispatch grid is.
#[test]
fn a_maximal_instance_count_is_rejected_before_rasterizing() {
    let (mut exec, mut res) = primed(&[
        spirv_module(),
        Cmd::CreateTexture(
            1,
            tex(
                4,
                4,
                TextureFormat::Rgba8Unorm,
                texture_usage::RENDER_TARGET,
            ),
        ),
        Cmd::CreateBuffer(1, buf(28 * 3, buffer_usage::VERTEX | buffer_usage::COPY_DST)),
        Cmd::WriteBuffer {
            id: 1,
            offset: 0,
            data: covering_triangle(),
        },
        depth_pipeline(1, 0, None),
    ]);

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
                Enc::Draw {
                    vertex_count: 3,
                    instance_count: u32::MAX,
                    first_vertex: 0,
                    first_instance: 0,
                },
                Enc::EndRenderPass,
            ])],
        )
        .expect_err("a maximal instance count must error, not rasterize 4 billion times");
    assert_eq!(err, GpuError::ResourceLimit("draw instances"));

    // And the attachment is untouched: the rejection happened in the pre-execution validation pass.
    let mut px = vec![0u8; 4 * 4 * 4];
    exec.read_texture(&res, TextureId(1), &mut px).unwrap();
    assert_eq!(px, vec![0u8; 4 * 4 * 4]);
}
