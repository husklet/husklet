//! Validation-behavior tests for the software backend (the headless oracle). Each test pins a
//! previously-silent gap: malformed IR must be a typed `GpuError`, never a panic, a silent no-op, or
//! a divergence from what a real backend would reject. Grouped by the finding they close in
//! docs/bugs/gpu-display-sentry.md.

use dd_gpu::backend::GpuBackend;
use dd_gpu::id::*;
use dd_gpu::ir::*;
use dd_gpu::software::SoftwareBackend;
use dd_gpu::GpuError;

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

fn buf(size: u64, usage: u32) -> BufferDesc {
    BufferDesc { size, usage, label: String::new() }
}

// ------------------------------------------------------------------------------------------------
// wrapping / overflow — controlled errors, never panics
// ------------------------------------------------------------------------------------------------

#[test]
fn write_buffer_wrapping_offset_is_out_of_bounds_not_panic() {
    let mut be = SoftwareBackend::new();
    be.create_buffer(BufferId(1), &buf(16, buffer_usage::COPY_DST)).unwrap();
    assert_eq!(be.write_buffer(BufferId(1), u64::MAX, &[1, 2, 3, 4]), Err(GpuError::OutOfBounds));
}

#[test]
fn copy_buffer_to_buffer_wrapping_offset_is_out_of_bounds() {
    let mut be = SoftwareBackend::new();
    be.create_buffer(BufferId(1), &buf(16, buffer_usage::COPY_SRC)).unwrap();
    be.create_buffer(BufferId(2), &buf(16, buffer_usage::COPY_DST)).unwrap();
    let cb = CommandBuffer {
        encoder: vec![Enc::CopyBufferToBuffer { src: 1, src_offset: u64::MAX, dst: 2, dst_offset: 0, size: 8 }],
        signal: None,
    };
    assert_eq!(be.submit(&cb), Err(GpuError::OutOfBounds));
}

#[test]
fn oversized_texture_dimensions_do_not_panic() {
    let mut be = SoftwareBackend::new();
    let r = be.create_texture(TextureId(1), &tex(u32::MAX, u32::MAX, TextureFormat::Rgba8Unorm, texture_usage::SAMPLED));
    assert_eq!(r, Err(GpuError::OutOfBounds));
}

// ------------------------------------------------------------------------------------------------
// texture descriptor validation
// ------------------------------------------------------------------------------------------------

#[test]
fn zero_sized_texture_is_rejected() {
    let mut be = SoftwareBackend::new();
    assert_eq!(
        be.create_texture(TextureId(1), &tex(0, 4, TextureFormat::Rgba8Unorm, texture_usage::SAMPLED)),
        Err(GpuError::Invalid("zero-sized texture"))
    );
}

#[test]
fn zero_mip_texture_is_rejected() {
    let mut be = SoftwareBackend::new();
    let mut d = tex(4, 4, TextureFormat::Rgba8Unorm, texture_usage::SAMPLED);
    d.mip_levels = 0;
    assert!(matches!(be.create_texture(TextureId(1), &d), Err(GpuError::Invalid(_))));
}

#[test]
fn multisample_texture_is_rejected_not_downleveled() {
    let mut be = SoftwareBackend::new();
    let mut d = tex(4, 4, TextureFormat::Rgba8Unorm, texture_usage::RENDER_TARGET);
    d.sample_count = 4;
    assert!(matches!(be.create_texture(TextureId(1), &d), Err(GpuError::Unsupported(_))));
}

#[test]
fn non_2d_texture_is_rejected_not_flattened() {
    let mut be = SoftwareBackend::new();
    let mut d = tex(4, 4, TextureFormat::Rgba8Unorm, texture_usage::SAMPLED);
    d.dim = TextureDim::D3;
    d.depth = 2;
    assert!(matches!(be.create_texture(TextureId(1), &d), Err(GpuError::Unsupported(_))));
}

// ------------------------------------------------------------------------------------------------
// shader / pipeline validation
// ------------------------------------------------------------------------------------------------

#[test]
fn empty_shader_module_is_rejected() {
    let mut be = SoftwareBackend::new();
    assert_eq!(be.create_shader(ShaderId(1), &[]), Err(GpuError::Invalid("empty shader module")));
}

#[test]
fn vertex_attribute_outside_stride_is_rejected() {
    let mut be = SoftwareBackend::new();
    be.create_shader(ShaderId(1), &[1, 2, 3]).unwrap();
    let desc = RenderPipelineDesc {
        vertex: ShaderRef { module: 1, entry: "vs".into() },
        fragment: None,
        vertex_buffers: vec![VertexLayout {
            stride: 8,
            step_mode: 0,
            attrs: vec![VertexAttr { location: 0, format: 23, offset: 16 }],
        }],
        color_targets: vec![],
        depth: None,
        topology: Topology::TriangleList,
        cull: 0,
        front_face: 0,
        label: String::new(),
    };
    assert!(matches!(be.create_render_pipeline(PipelineId(1), &desc), Err(GpuError::Invalid(_))));
}

// ------------------------------------------------------------------------------------------------
// bind group validation
// ------------------------------------------------------------------------------------------------

#[test]
fn bind_group_buffer_range_out_of_bounds_is_rejected() {
    let mut be = SoftwareBackend::new();
    be.create_buffer(BufferId(1), &buf(16, buffer_usage::STORAGE)).unwrap();
    let desc = BindGroupDesc {
        set: 0,
        entries: vec![BindEntry { binding: 0, resource: BindResource::Buffer { id: 1, offset: 12, size: 8 } }],
    };
    assert_eq!(be.create_bind_group(BindGroupId(1), &desc), Err(GpuError::OutOfBounds));
}

#[test]
fn bind_group_wrapping_buffer_offset_is_rejected_not_panic() {
    let mut be = SoftwareBackend::new();
    be.create_buffer(BufferId(1), &buf(16, buffer_usage::STORAGE)).unwrap();
    let desc = BindGroupDesc {
        set: 0,
        entries: vec![BindEntry { binding: 0, resource: BindResource::Buffer { id: 1, offset: u64::MAX, size: 1 } }],
    };
    assert_eq!(be.create_bind_group(BindGroupId(1), &desc), Err(GpuError::OutOfBounds));
}

#[test]
fn storage_only_texture_bound_as_sampled_is_rejected() {
    let mut be = SoftwareBackend::new();
    // storage usage but NOT sampled
    be.create_texture(TextureId(1), &tex(4, 4, TextureFormat::Rgba8Unorm, texture_usage::STORAGE)).unwrap();
    let desc = BindGroupDesc {
        set: 0,
        entries: vec![BindEntry { binding: 0, resource: BindResource::Texture { id: 1 } }],
    };
    assert!(matches!(be.create_bind_group(BindGroupId(1), &desc), Err(GpuError::Invalid(_))));
}

// ------------------------------------------------------------------------------------------------
// copy validation
// ------------------------------------------------------------------------------------------------

#[test]
fn copy_without_copy_usage_is_rejected() {
    let mut be = SoftwareBackend::new();
    be.create_buffer(BufferId(1), &buf(16, buffer_usage::STORAGE)).unwrap(); // no COPY_SRC
    be.create_buffer(BufferId(2), &buf(16, buffer_usage::COPY_DST)).unwrap();
    let cb = CommandBuffer {
        encoder: vec![Enc::CopyBufferToBuffer { src: 1, src_offset: 0, dst: 2, dst_offset: 0, size: 8 }],
        signal: None,
    };
    assert!(matches!(be.submit(&cb), Err(GpuError::Invalid(_))));
}

#[test]
fn copy_buffer_to_texture_extent_wider_than_texture_is_rejected() {
    let mut be = SoftwareBackend::new();
    be.create_texture(TextureId(1), &tex(4, 4, TextureFormat::Rgba8Unorm, texture_usage::COPY_DST)).unwrap();
    be.create_buffer(BufferId(1), &buf(4096, buffer_usage::COPY_SRC)).unwrap();
    let cb = CommandBuffer {
        encoder: vec![Enc::CopyBufferToTexture { src: 1, src_offset: 0, bytes_per_row: 0, dst: 1, mip: 0, width: 8, height: 4 }],
        signal: None,
    };
    assert_eq!(be.submit(&cb), Err(GpuError::OutOfBounds));
}

#[test]
fn copy_buffer_to_texture_short_row_pitch_is_rejected() {
    let mut be = SoftwareBackend::new();
    be.create_texture(TextureId(1), &tex(2, 2, TextureFormat::Rgba8Unorm, texture_usage::COPY_DST)).unwrap();
    be.create_buffer(BufferId(1), &buf(4096, buffer_usage::COPY_SRC)).unwrap();
    // 2x2 RGBA needs >= 8 bytes/row; bytes_per_row=4 is too small.
    let cb = CommandBuffer {
        encoder: vec![Enc::CopyBufferToTexture { src: 1, src_offset: 0, bytes_per_row: 4, dst: 1, mip: 0, width: 2, height: 2 }],
        signal: None,
    };
    assert_eq!(be.submit(&cb), Err(GpuError::OutOfBounds));
}

#[test]
fn nonzero_mip_copy_is_rejected_not_aliased_to_base() {
    let mut be = SoftwareBackend::new();
    be.create_texture(TextureId(1), &tex(2, 2, TextureFormat::Rgba8Unorm, texture_usage::COPY_DST)).unwrap();
    be.create_buffer(BufferId(1), &buf(4096, buffer_usage::COPY_SRC)).unwrap();
    let cb = CommandBuffer {
        encoder: vec![Enc::CopyBufferToTexture { src: 1, src_offset: 0, bytes_per_row: 0, dst: 1, mip: 1, width: 2, height: 2 }],
        signal: None,
    };
    assert!(matches!(be.submit(&cb), Err(GpuError::Unsupported(_))));
    // base level untouched
    let mut px = [0u8; 16];
    be.read_texture(TextureId(1), &mut px).unwrap();
    assert_eq!(px, [0u8; 16], "base mip must not be aliased by a mip-1 copy");
}

#[test]
fn copy_texture_to_buffer_honors_bytes_per_row() {
    let mut be = SoftwareBackend::new();
    // 2x1 RGBA texture (8 tight bytes) read into a buffer with 12-byte row stride.
    be.create_texture(TextureId(1), &tex(2, 1, TextureFormat::Rgba8Unorm, texture_usage::COPY_SRC | texture_usage::COPY_DST)).unwrap();
    be.create_buffer(BufferId(1), &buf(4096, buffer_usage::COPY_SRC)).unwrap();
    be.create_buffer(BufferId(2), &buf(64, buffer_usage::COPY_DST)).unwrap();
    // fill texture via a tight upload
    be.write_buffer(BufferId(1), 0, &[1, 2, 3, 4, 5, 6, 7, 8]).unwrap();
    be.submit(&CommandBuffer {
        encoder: vec![Enc::CopyBufferToTexture { src: 1, src_offset: 0, bytes_per_row: 0, dst: 1, mip: 0, width: 2, height: 1 }],
        signal: None,
    }).unwrap();
    // Now read back a 2x2 (two rows) with padded stride to exercise inter-row padding.
    be.create_texture(TextureId(2), &tex(2, 2, TextureFormat::Rgba8Unorm, texture_usage::COPY_SRC | texture_usage::COPY_DST)).unwrap();
    be.write_buffer(BufferId(1), 0, &[10, 11, 12, 13, 14, 15, 16, 17, 20, 21, 22, 23, 24, 25, 26, 27]).unwrap();
    be.submit(&CommandBuffer {
        encoder: vec![Enc::CopyBufferToTexture { src: 1, src_offset: 0, bytes_per_row: 0, dst: 2, mip: 0, width: 2, height: 2 }],
        signal: None,
    }).unwrap();
    // fill dst buffer with a sentinel so we can see padding is preserved
    be.write_buffer(BufferId(2), 0, &[0xEE; 40]).unwrap();
    be.submit(&CommandBuffer {
        encoder: vec![Enc::CopyTextureToBuffer { src: 2, mip: 0, width: 2, height: 2, dst: 2, dst_offset: 0, bytes_per_row: 12 }],
        signal: None,
    }).unwrap();
    let mut out = [0u8; 40];
    be.read_buffer(BufferId(2), 0, &mut out).unwrap();
    // row 0 at [0..8], padding [8..12] preserved as 0xEE, row 1 at [12..20]
    assert_eq!(&out[0..8], &[10, 11, 12, 13, 14, 15, 16, 17]);
    assert_eq!(&out[8..12], &[0xEE; 4], "inter-row padding must be preserved, not packed");
    assert_eq!(&out[12..20], &[20, 21, 22, 23, 24, 25, 26, 27]);
}

#[test]
fn texture_readback_unaligned_offset_is_rejected() {
    let mut be = SoftwareBackend::new();
    be.create_texture(TextureId(1), &tex(2, 1, TextureFormat::Rgba8Unorm, texture_usage::COPY_SRC)).unwrap();
    be.create_buffer(BufferId(1), &buf(64, buffer_usage::COPY_DST)).unwrap();
    let cb = CommandBuffer {
        encoder: vec![Enc::CopyTextureToBuffer { src: 1, mip: 0, width: 2, height: 1, dst: 1, dst_offset: 1, bytes_per_row: 0 }],
        signal: None,
    };
    assert!(matches!(be.submit(&cb), Err(GpuError::Invalid(_))));
}

// ------------------------------------------------------------------------------------------------
// render-pass / attachment validation
// ------------------------------------------------------------------------------------------------

#[test]
fn render_attachment_without_render_usage_is_rejected() {
    let mut be = SoftwareBackend::new();
    be.create_texture(TextureId(1), &tex(4, 4, TextureFormat::Rgba8Unorm, texture_usage::SAMPLED)).unwrap();
    let cb = CommandBuffer {
        encoder: vec![
            Enc::BeginRenderPass {
                color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [1.0, 0.0, 0.0, 1.0], store: true }],
                depth: None,
            },
            Enc::EndRenderPass,
        ],
        signal: None,
    };
    assert!(matches!(be.submit(&cb), Err(GpuError::Invalid(_))));
    // and no mutation happened (finding: sampled-only texture must not be cleared)
    let mut px = [0u8; 64];
    be.read_texture(TextureId(1), &mut px).unwrap();
    assert_eq!(px, [0u8; 64]);
}

#[test]
fn missing_depth_attachment_texture_is_rejected() {
    let mut be = SoftwareBackend::new();
    be.create_texture(TextureId(1), &tex(4, 4, TextureFormat::Rgba8Unorm, texture_usage::RENDER_TARGET)).unwrap();
    let cb = CommandBuffer {
        encoder: vec![
            Enc::BeginRenderPass {
                color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [0.0, 0.0, 0.0, 1.0], store: true }],
                depth: Some(DepthAttachment { texture: 99, load: LoadOp::Clear, clear_depth: 1.0 }),
            },
            Enc::EndRenderPass,
        ],
        signal: None,
    };
    assert_eq!(be.submit(&cb), Err(GpuError::UnknownId { kind: "texture", id: 99 }));
}

#[test]
fn viewport_invalid_depth_range_is_rejected() {
    let mut be = SoftwareBackend::new();
    be.create_texture(TextureId(1), &tex(4, 4, TextureFormat::Bgra8Unorm, texture_usage::RENDER_TARGET)).unwrap();
    let cb = CommandBuffer {
        encoder: vec![
            Enc::BeginRenderPass {
                color: vec![ColorAttachment { texture: 1, load: LoadOp::Load, clear: [0.0; 4], store: true }],
                depth: None,
            },
            Enc::SetViewport { x: 0.0, y: 0.0, w: 4.0, h: 4.0, min_depth: 1.0, max_depth: 0.0 },
            Enc::EndRenderPass,
        ],
        signal: None,
    };
    assert!(matches!(be.submit(&cb), Err(GpuError::Invalid(_))));
}

#[test]
fn dispatch_outside_compute_pass_is_rejected() {
    let mut be = SoftwareBackend::new();
    let cb = CommandBuffer {
        encoder: vec![Enc::Dispatch { x: 1, y: 1, z: 1 }],
        signal: None,
    };
    assert!(matches!(be.submit(&cb), Err(GpuError::Invalid(_))));
}

// ------------------------------------------------------------------------------------------------
// fence / present
// ------------------------------------------------------------------------------------------------

#[test]
fn wait_on_unsignaled_fence_is_error_not_fabricated() {
    let mut be = SoftwareBackend::new();
    be.create_fence(FenceId(1)).unwrap();
    assert!(matches!(be.wait_fence(FenceId(1), 5), Err(GpuError::Invalid(_))));
    // after a submit signals it, the wait succeeds
    be.submit(&CommandBuffer { encoder: vec![], signal: Some((1, 5)) }).unwrap();
    assert!(be.wait_fence(FenceId(1), 5).is_ok());
}

#[test]
fn present_size_mismatch_is_rejected() {
    let mut be = SoftwareBackend::new();
    be.create_surface(SurfaceId(1), &SurfaceDesc { width: 8, height: 8, format: TextureFormat::Bgra8Unorm, ddp_surface: 1 }).unwrap();
    be.create_texture(TextureId(1), &tex(4, 4, TextureFormat::Bgra8Unorm, texture_usage::PRESENT)).unwrap();
    assert!(matches!(be.present(SurfaceId(1), TextureId(1)), Err(GpuError::Invalid(_))));
}

// ------------------------------------------------------------------------------------------------
// atomic submit — no partial mutation on failure
// ------------------------------------------------------------------------------------------------

#[test]
fn failed_submit_does_not_partially_mutate() {
    let mut be = SoftwareBackend::new();
    be.create_texture(TextureId(1), &tex(2, 2, TextureFormat::Rgba8Unorm, texture_usage::RENDER_TARGET)).unwrap();
    be.create_buffer(BufferId(1), &buf(4, buffer_usage::COPY_SRC)).unwrap();
    be.create_buffer(BufferId(2), &buf(4, buffer_usage::COPY_DST)).unwrap();
    // Clear the texture red, THEN an out-of-bounds copy that must fail the whole submit.
    let cb = CommandBuffer {
        encoder: vec![
            Enc::BeginRenderPass {
                color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [1.0, 0.0, 0.0, 1.0], store: true }],
                depth: None,
            },
            Enc::EndRenderPass,
            Enc::CopyBufferToBuffer { src: 1, src_offset: 0, dst: 2, dst_offset: 0, size: 8 }, // 8 > 4
        ],
        signal: None,
    };
    assert_eq!(be.submit(&cb), Err(GpuError::OutOfBounds));
    let mut px = [0u8; 16];
    be.read_texture(TextureId(1), &mut px).unwrap();
    assert_eq!(px, [0u8; 16], "the clear must not have run: submit failed validation");
}

// ------------------------------------------------------------------------------------------------
// full render draw harness — covers missing-vertex-buffer, range, format, hazard, stale refs
// ------------------------------------------------------------------------------------------------

/// Build a backend with a valid render pipeline (id 1), a Bgra8 render target (tex 1), a sampled
/// texture (tex 2), a vertex buffer (buf 1), a storage buffer (buf 2), and a bind group (id 1)
/// referencing tex 2 + buf 2. Returns the backend ready to receive a draw submit.
fn render_harness() -> SoftwareBackend {
    let mut be = SoftwareBackend::new();
    be.create_shader(ShaderId(1), &[1, 2, 3]).unwrap();
    be.create_shader(ShaderId(2), &[4, 5, 6]).unwrap();
    let desc = RenderPipelineDesc {
        vertex: ShaderRef { module: 1, entry: "vs".into() },
        fragment: Some(ShaderRef { module: 2, entry: "fs".into() }),
        vertex_buffers: vec![VertexLayout { stride: 16, step_mode: 0, attrs: vec![VertexAttr { location: 0, format: 23, offset: 0 }] }],
        color_targets: vec![ColorTargetState { format: TextureFormat::Bgra8Unorm, blend: None, write_mask: 0xF }],
        depth: None,
        topology: Topology::TriangleList,
        cull: 0,
        front_face: 0,
        label: String::new(),
    };
    be.create_render_pipeline(PipelineId(1), &desc).unwrap();
    be.create_texture(TextureId(1), &tex(4, 4, TextureFormat::Bgra8Unorm, texture_usage::RENDER_TARGET)).unwrap();
    be.create_texture(TextureId(2), &tex(4, 4, TextureFormat::Rgba8Unorm, texture_usage::SAMPLED)).unwrap();
    be.create_buffer(BufferId(1), &buf(64, buffer_usage::VERTEX)).unwrap();
    be.create_buffer(BufferId(2), &buf(64, buffer_usage::STORAGE)).unwrap();
    be.create_bind_group(BindGroupId(1), &BindGroupDesc {
        set: 0,
        entries: vec![
            BindEntry { binding: 0, resource: BindResource::Texture { id: 2 } },
            BindEntry { binding: 1, resource: BindResource::Buffer { id: 2, offset: 0, size: 0 } },
        ],
    }).unwrap();
    be
}

fn draw_pass(extra: Vec<Enc>) -> CommandBuffer {
    let mut encoder = vec![Enc::BeginRenderPass {
        color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [0.0; 4], store: true }],
        depth: None,
    }];
    encoder.extend(extra);
    encoder.push(Enc::EndRenderPass);
    CommandBuffer { encoder, signal: None }
}

#[test]
fn valid_draw_is_accepted() {
    let mut be = render_harness();
    let cb = draw_pass(vec![
        Enc::SetPipeline(1),
        Enc::SetBindGroup { index: 0, group: 1 },
        Enc::SetVertexBuffer { slot: 0, buffer: 1, offset: 0 },
        Enc::Draw { vertex_count: 3, instance_count: 1, first_vertex: 0, first_instance: 0 },
    ]);
    assert!(be.submit(&cb).is_ok());
    assert_eq!(be.draws, 1);
}

#[test]
fn draw_without_vertex_buffer_is_rejected() {
    let mut be = render_harness();
    let cb = draw_pass(vec![
        Enc::SetPipeline(1),
        Enc::SetBindGroup { index: 0, group: 1 },
        Enc::Draw { vertex_count: 3, instance_count: 1, first_vertex: 0, first_instance: 0 },
    ]);
    assert!(matches!(be.submit(&cb), Err(GpuError::Invalid(_))));
}

#[test]
fn draw_vertex_range_beyond_buffer_is_rejected() {
    let mut be = render_harness();
    // stride 16, 8 vertices → 128 bytes > 64-byte buffer
    let cb = draw_pass(vec![
        Enc::SetPipeline(1),
        Enc::SetBindGroup { index: 0, group: 1 },
        Enc::SetVertexBuffer { slot: 0, buffer: 1, offset: 0 },
        Enc::Draw { vertex_count: 8, instance_count: 1, first_vertex: 0, first_instance: 0 },
    ]);
    assert_eq!(be.submit(&cb), Err(GpuError::OutOfBounds));
}

#[test]
fn indexed_draw_index_range_beyond_buffer_is_rejected() {
    let mut be = render_harness();
    be.create_buffer(BufferId(3), &buf(2, buffer_usage::INDEX)).unwrap(); // 2 bytes = one U16 index
    let cb = draw_pass(vec![
        Enc::SetPipeline(1),
        Enc::SetBindGroup { index: 0, group: 1 },
        Enc::SetVertexBuffer { slot: 0, buffer: 1, offset: 0 },
        Enc::SetIndexBuffer { buffer: 3, offset: 0, format: IndexFormat::U16 },
        Enc::DrawIndexed { index_count: 2, instance_count: 1, first_index: 0, base_vertex: 0, first_instance: 0 },
    ]);
    assert_eq!(be.submit(&cb), Err(GpuError::OutOfBounds));
}

#[test]
fn pipeline_attachment_format_mismatch_is_rejected() {
    let mut be = render_harness();
    // render target of a different format than the pipeline's Bgra8 color target
    be.create_texture(TextureId(3), &tex(4, 4, TextureFormat::Rgba8Unorm, texture_usage::RENDER_TARGET)).unwrap();
    let cb = CommandBuffer {
        encoder: vec![
            Enc::BeginRenderPass {
                color: vec![ColorAttachment { texture: 3, load: LoadOp::Clear, clear: [0.0; 4], store: true }],
                depth: None,
            },
            Enc::SetPipeline(1),
            Enc::SetBindGroup { index: 0, group: 1 },
            Enc::SetVertexBuffer { slot: 0, buffer: 1, offset: 0 },
            Enc::Draw { vertex_count: 3, instance_count: 1, first_vertex: 0, first_instance: 0 },
            Enc::EndRenderPass,
        ],
        signal: None,
    };
    assert!(matches!(be.submit(&cb), Err(GpuError::Invalid(_))));
}

#[test]
fn sampled_render_target_hazard_is_rejected() {
    let mut be = render_harness();
    // Bind group that samples the render-target texture (id 1) while it is the color attachment.
    be.create_texture(TextureId(5), &tex(4, 4, TextureFormat::Bgra8Unorm, texture_usage::RENDER_TARGET | texture_usage::SAMPLED)).unwrap();
    be.create_bind_group(BindGroupId(2), &BindGroupDesc {
        set: 0,
        entries: vec![BindEntry { binding: 0, resource: BindResource::Texture { id: 5 } }],
    }).unwrap();
    let cb = CommandBuffer {
        encoder: vec![
            Enc::BeginRenderPass {
                color: vec![ColorAttachment { texture: 5, load: LoadOp::Clear, clear: [0.0; 4], store: true }],
                depth: None,
            },
            Enc::SetPipeline(1),
            Enc::SetBindGroup { index: 0, group: 2 },
            Enc::SetVertexBuffer { slot: 0, buffer: 1, offset: 0 },
            Enc::Draw { vertex_count: 3, instance_count: 1, first_vertex: 0, first_instance: 0 },
            Enc::EndRenderPass,
        ],
        signal: None,
    };
    assert!(matches!(be.submit(&cb), Err(GpuError::Invalid(_))));
}

#[test]
fn destroyed_texture_in_bind_group_is_rejected_on_draw() {
    let mut be = render_harness();
    be.destroy_texture(TextureId(2)).unwrap(); // tex 2 is referenced by bind group 1
    let cb = draw_pass(vec![
        Enc::SetPipeline(1),
        Enc::SetBindGroup { index: 0, group: 1 },
        Enc::SetVertexBuffer { slot: 0, buffer: 1, offset: 0 },
        Enc::Draw { vertex_count: 3, instance_count: 1, first_vertex: 0, first_instance: 0 },
    ]);
    assert_eq!(be.submit(&cb), Err(GpuError::UnknownId { kind: "texture", id: 2 }));
}

#[test]
fn stale_bind_group_after_buffer_id_reuse_is_rejected() {
    let mut be = render_harness();
    // Destroy and recreate buffer 2 (referenced by bind group 1) — the reused id is a different
    // allocation, so the stale bind group must be rejected.
    be.destroy_buffer(BufferId(2)).unwrap();
    be.create_buffer(BufferId(2), &buf(64, buffer_usage::STORAGE)).unwrap();
    let cb = draw_pass(vec![
        Enc::SetPipeline(1),
        Enc::SetBindGroup { index: 0, group: 1 },
        Enc::SetVertexBuffer { slot: 0, buffer: 1, offset: 0 },
        Enc::Draw { vertex_count: 3, instance_count: 1, first_vertex: 0, first_instance: 0 },
    ]);
    assert_eq!(be.submit(&cb), Err(GpuError::UnknownId { kind: "buffer", id: 2 }));
}
