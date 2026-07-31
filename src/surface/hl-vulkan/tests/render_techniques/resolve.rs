use super::harness::*;

// 4. MSAA RESOLVE — a real multisample color pass resolved into a single-sample texture (#240).
// ===================================================================================================

/// A 4x-MSAA pass renders into a multisampled color target, then `vkCmdResolveImage` resolves it into a
/// single-sample texture. The VK create path now threads `VkImageCreateInfo::samples` →
/// `TextureDesc.sample_count` and `VkPipelineMultisampleStateCreateInfo::rasterizationSamples` →
/// `RenderPipelineDesc.sample_count` (#240), so the multisampled source materializes as a real MSAA texture
/// and the resolve lowers to the executor's TRUE `Enc::ResolveTexture` (#179) — averaging the samples — NOT
/// a same-extent copy (which would drop the antialiasing). This test asserts the threaded sample counts AND
/// that the resolve emits `ResolveTexture`.
#[test]
fn msaa_resolve_pass_lowers_to_a_real_resolve() {
    use hl_gpu::protocol::model::descriptor::{
        Extent3d, Origin3d, RenderPipelineDesc, TextureSubresource,
    };

    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();

    // The 4x-MSAA color target (also TRANSFER_SRC so it can be resolved) and the single-sample resolve dest.
    let msaa = create::create_image(
        &mut d,
        &mut sink,
        64,
        64,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::COLOR_ATTACHMENT | vk_image_usage::TRANSFER_SRC,
        4,
    )
    .unwrap();
    let resolve = create::create_image(
        &mut d,
        &mut sink,
        64,
        64,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::TRANSFER_DST | vk_image_usage::SAMPLED,
        1,
    )
    .unwrap();
    let (msaa_ir, resolve_ir) = (img_ir(&d, msaa), img_ir(&d, resolve));

    // The MSAA source materializes at sample_count == 4; the resolve dest stays single-sample.
    let sample_counts: Vec<(u32, u32)> = sink
        .batches
        .iter()
        .flatten()
        .filter_map(|c| match c {
            Cmd::CreateTexture(id, desc) if *id == msaa_ir || *id == resolve_ir => {
                Some((*id, desc.sample_count))
            }
            _ => None,
        })
        .collect();
    assert_eq!(sample_counts.len(), 2);
    assert_eq!(
        sample_counts
            .iter()
            .find(|(id, _)| *id == msaa_ir)
            .unwrap()
            .1,
        4,
        "MSAA src threads sample_count == 4"
    );
    assert_eq!(
        sample_counts
            .iter()
            .find(|(id, _)| *id == resolve_ir)
            .unwrap()
            .1,
        1,
        "resolve dst is single-sample"
    );

    // A 4x-MSAA graphics pipeline: its RenderPipelineDesc must carry sample_count == 4.
    let pipe = pipeline_samples(&mut d, &mut sink, vec![TextureFormat::Rgba8Unorm], None, 4);
    let pipe_ir = d.pipelines.get(&pipe).unwrap().ir_id;
    let pipe_samples = sink
        .batches
        .iter()
        .flatten()
        .find_map(|c| match c {
            Cmd::CreateRenderPipeline(id, desc) if *id == pipe_ir => Some({
                let d: &RenderPipelineDesc = desc;
                d.sample_count
            }),
            _ => None,
        })
        .expect("render pipeline CreateRenderPipeline recorded");
    assert_eq!(
        pipe_samples, 4,
        "MSAA pipeline threads RenderPipelineDesc.sample_count == 4"
    );

    // ---- One command buffer: render into the MSAA target, then resolve it into the single-sample dest.
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    record::cmd_begin_rendering(&mut d, cb, &[color_clear(msaa, [0.2, 0.4, 0.6, 1.0])], None)
        .unwrap();
    record::cmd_bind_pipeline(&mut d, cb, pipe).unwrap();
    record::cmd_draw(&mut d, cb, 3, 1, 0, 0).unwrap();
    d.end_render_pass(cb).unwrap();
    record::cmd_resolve_image(
        &mut d,
        cb,
        msaa,
        resolve,
        SubresourceLayers::base(),
        SubresourceLayers::base(),
        (0, 0),
        (0, 0),
        (64, 64),
    )
    .unwrap();
    d.end_command_buffer(cb).unwrap();
    let enc = submit_encoder(&mut d, &mut sink, cb);

    assert_eq!(
        enc,
        vec![
            Enc::BeginRenderPass {
                color: vec![ColorAttachment {
                    texture: msaa_ir,
                    load: LoadOp::Clear,
                    clear: [0.2, 0.4, 0.6, 1.0],
                    store: true
                }],
                depth: None,
            },
            Enc::SetPipeline(pipe_ir),
            Enc::Draw {
                vertex_count: 3,
                instance_count: 1,
                first_vertex: 0,
                first_instance: 0
            },
            Enc::EndRenderPass,
            // A multisample source resolves for real: average the samples into the single-sample dest.
            Enc::ResolveTexture {
                src: msaa_ir,
                src_sub: TextureSubresource::base(),
                src_origin: Origin3d { x: 0, y: 0, z: 0 },
                dst: resolve_ir,
                dst_sub: TextureSubresource::base(),
                dst_origin: Origin3d { x: 0, y: 0, z: 0 },
                extent: Extent3d {
                    width: 64,
                    height: 64,
                    depth: 1
                },
            },
        ],
        "a 4x-MSAA pass + resolve lowers to the color draw followed by a real ResolveTexture"
    );
    assert!(
        !enc.iter()
            .any(|e| matches!(e, Enc::CopyTextureToTexture { .. })),
        "a multisample resolve is a ResolveTexture, never a copy"
    );
}

/// A single-sample source resolves as a same-extent content-MOVING copy — the resolve degenerates to
/// `CopyTextureToTexture` (a legit no-op/move), NOT `Enc::ResolveTexture`. This pins that a non-MSAA app's
/// `vkCmdResolveImage` is byte-identical to the pre-#240 copy lowering.
#[test]
fn single_sample_resolve_still_lowers_to_a_copy() {
    use hl_gpu::protocol::model::descriptor::{Extent3d, Origin3d, TextureSubresource};

    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();

    let src = create::create_image(
        &mut d,
        &mut sink,
        32,
        32,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::COLOR_ATTACHMENT | vk_image_usage::TRANSFER_SRC,
        1,
    )
    .unwrap();
    let dst = create::create_image(
        &mut d,
        &mut sink,
        32,
        32,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::TRANSFER_DST | vk_image_usage::SAMPLED,
        1,
    )
    .unwrap();
    let (src_ir, dst_ir) = (img_ir(&d, src), img_ir(&d, dst));

    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    record::cmd_resolve_image(
        &mut d,
        cb,
        src,
        dst,
        SubresourceLayers::base(),
        SubresourceLayers::base(),
        (0, 0),
        (0, 0),
        (32, 32),
    )
    .unwrap();
    d.end_command_buffer(cb).unwrap();
    let enc = submit_encoder(&mut d, &mut sink, cb);

    assert_eq!(
        enc,
        vec![Enc::CopyTextureToTexture {
            src: src_ir,
            src_sub: TextureSubresource::base(),
            src_origin: Origin3d { x: 0, y: 0, z: 0 },
            dst: dst_ir,
            dst_sub: TextureSubresource::base(),
            dst_origin: Origin3d { x: 0, y: 0, z: 0 },
            extent: Extent3d {
                width: 32,
                height: 32,
                depth: 1
            },
        }],
        "a single-sample resolve is a content-moving copy"
    );
    assert!(
        !enc.iter().any(|e| matches!(e, Enc::ResolveTexture { .. })),
        "single-sample source emits a copy, not a ResolveTexture"
    );
}

// ===================================================================================================
