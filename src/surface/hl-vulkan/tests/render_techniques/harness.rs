pub(crate) use hl_vulkan::model::descriptor::{vk_descriptor_type, LayoutBinding};
pub(crate) use hl_vulkan::model::memory::{vk_format, vk_image_usage, SubresourceLayers};
pub(crate) use hl_vulkan::result;
pub(crate) use hl_vulkan::service::record::{RenderingColorAttachment, RenderingDepthAttachment};
pub(crate) use hl_vulkan::service::{create, record, submit};
pub(crate) use hl_vulkan::{Device, Instance};

pub(crate) use hl_gpu::protocol::model::command::Enc;
pub(crate) use hl_gpu::protocol::model::descriptor::{
    BindResource, ColorAttachment, DepthAttachment, DepthState,
};
pub(crate) use hl_gpu::protocol::model::enums::{compare, LoadOp, TextureFormat, Topology};
pub(crate) use hl_gpu::{Cmd, RecordingSink};

// ---------------------------------------------------------------------------------------------------
// harness
// ---------------------------------------------------------------------------------------------------

pub(crate) fn dev() -> Device {
    let inst = Instance::new(result::HL_API_VERSION);
    inst.create_device()
}

/// The ir texture id behind a `VkImage` handle (the id an emitted `ColorAttachment`/`BindResource` names).
pub(crate) fn img_ir(d: &Device, h: u64) -> u32 {
    d.images.get(&h).unwrap().ir_id
}
/// The ir sampler id behind a `VkSampler` handle.
pub(crate) fn samp_ir(d: &Device, h: u64) -> u32 {
    d.samplers.get(&h).unwrap().ir_id
}

/// A `SAMPLED | COLOR_ATTACHMENT` render-then-sample color texture (the G-buffer / ping-pong workhorse).
pub(crate) fn sampled_color(d: &mut Device, sink: &mut RecordingSink, w: u32, h: u32) -> u64 {
    create::create_image(
        d,
        sink,
        w,
        h,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::COLOR_ATTACHMENT | vk_image_usage::SAMPLED,
        1,
    )
    .unwrap()
}

/// A minimal single-sample graphics pipeline over `color_formats` (+ optional depth).
pub(crate) fn pipeline(
    d: &mut Device,
    sink: &mut RecordingSink,
    color_formats: Vec<TextureFormat>,
    depth: Option<DepthState>,
) -> u64 {
    pipeline_samples(d, sink, color_formats, depth, 1)
}

/// A minimal graphics pipeline over `color_formats` (+ optional depth) at `sample_count`, reusing the
/// passthrough SPIR-V vs/fs. `sample_count > 1` yields a multisample pipeline (`RenderPipelineDesc.sample_count`).
pub(crate) fn pipeline_samples(
    d: &mut Device,
    sink: &mut RecordingSink,
    color_formats: Vec<TextureFormat>,
    depth: Option<DepthState>,
    sample_count: u32,
) -> u64 {
    use hl_vulkan::adapter::spirv;
    let vs = create::create_shader_module_words(d, sink, spirv::Module::sample_compute("vsmain"))
        .unwrap();
    let fs = create::create_shader_module_words(d, sink, spirv::Module::sample_compute("fsmain"))
        .unwrap();
    create::create_graphics_pipeline(
        d,
        sink,
        (vs, "vsmain"),
        Some((fs, "fsmain")),
        vec![],
        color_formats,
        depth,
        None,
        sample_count,
        Topology::TriangleList,
        0,
        0,
        0xf,
    )
    .unwrap()
}

/// Allocate a descriptor set with `n` COMBINED_IMAGE_SAMPLER bindings (0..n), bind each to `(images[i],
/// sampler)`, then record `vkCmdBindDescriptorSets` for it — returning the emitted `CreateBindGroup`'s
/// entries (the cross-pass sampler bindings) so a test can assert the sampled textures were resolved.
pub(crate) fn combined_sampler_set(
    d: &mut Device,
    sink: &mut RecordingSink,
    cb: u64,
    images: &[u64],
    sampler: u64,
) -> Vec<hl_gpu::protocol::model::descriptor::BindEntry> {
    let bindings = (0..images.len() as u32)
        .map(|b| LayoutBinding {
            binding: b,
            descriptor_type: vk_descriptor_type::COMBINED_IMAGE_SAMPLER,
            descriptor_count: 1,
            stage_flags: 0,
        })
        .collect();
    let layout = d.create_descriptor_set_layout(bindings);
    let pool = d.create_descriptor_pool(1);
    let set = create::allocate_descriptor_set(d, pool, layout, 0).unwrap();
    for (b, &img) in images.iter().enumerate() {
        create::update_descriptor_image(d, set, b as u32, Some(img), Some(sampler)).unwrap();
    }
    record::cmd_bind_descriptor_sets(d, sink, cb, 0, &[set], &[]).unwrap();
    match sink.batches.last().unwrap().as_slice() {
        [Cmd::CreateBindGroup(_, desc)] => desc.entries.clone(),
        other => panic!("expected CreateBindGroup, got {other:?}"),
    }
}

/// The single submitted encoder stream for command buffer `cb` (asserts exactly one `Cmd::Submit`).
pub(crate) fn submit_encoder(d: &mut Device, sink: &mut RecordingSink, cb: u64) -> Vec<Enc> {
    submit::queue_submit(d, sink, &[cb], None).unwrap();
    match sink.batches.last().unwrap().as_slice() {
        [Cmd::Submit(cbuf)] => cbuf.encoder.clone(),
        other => panic!("expected a single Submit, got {other:?}"),
    }
}

pub(crate) fn color_clear(image: u64, clear: [f32; 4]) -> RenderingColorAttachment {
    RenderingColorAttachment {
        image,
        clear,
        load_clear: true,
        store: true,
    }
}
