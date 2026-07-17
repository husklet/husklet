//! Multi-pass render-technique lowering tests (task #239).
//!
//! Task #222's `lowering.rs` proves every *single* `vkCmd*` lowers to the right `Enc`. THIS file proves the
//! multi-pass *compositions* real engines build out of those commands lower to a correct `Enc` stream: a
//! G-buffer MRT pass feeding a fullscreen lighting pass, a depth-only shadow pass feeding a shadow-sampling
//! pass, a ping-pong post-process chain, an MSAA resolve, and render-to-layer/mip. Each test mints the
//! `VkCmd` sequence in-process (driving the same `record::cmd_*` / `create::*` / `submit::*` seam the
//! shipping ICD marshals into) against a `RecordingSink`, and asserts the exact `Enc` IR — the
//! `BeginRenderPass` color/depth attachments, the pipeline/draw ops, and the cross-pass sampler bindings.
//!
//! MSAA is now threaded end to end (#240): `vkCreateImage` maps `VkImageCreateInfo::samples` →
//! `TextureDesc.sample_count` and `vkCreateGraphicsPipelines` maps
//! `VkPipelineMultisampleStateCreateInfo::rasterizationSamples` → `RenderPipelineDesc.sample_count`, so a
//! multisample `vkCmdResolveImage` lowers to the executor's real `Enc::ResolveTexture` (#179) — averaging
//! the samples — while a single-sample resolve stays a same-extent content-moving COPY. See
//! `msaa_resolve_pass_lowers_to_a_real_resolve` and `single_sample_resolve_still_lowers_to_a_copy`.
//!
//! One technique remains a DOCUMENTED LIMIT of the VK→IR lowering (not a bug, and not fixable here):
//!   * Render-to-layer / render-to-mip — the IR `ColorAttachment`/`DepthAttachment` carry only a whole
//!     `texture` id (no mip/layer subresource selector), and `vkCreateImage` models single-mip
//!     (`mip_levels: 1`) single-layer (`depth: 1`) 2D images. Selecting a layer/mip as a render target is
//!     therefore not expressible; the attachment always names the whole texture. The subresource-carrying
//!     `ColorAttachment` lives in the protocol crate (concurrent-agent-owned), so this cannot be fixed in
//!     `hl-vulkan`. See `render_to_layer_and_mip_is_a_documented_whole_texture_limit`.

use hl_vulkan::model::descriptor::{vk_descriptor_type, LayoutBinding};
use hl_vulkan::model::memory::{vk_format, vk_image_usage};
use hl_vulkan::result;
use hl_vulkan::service::record::{RenderingColorAttachment, RenderingDepthAttachment};
use hl_vulkan::service::{create, record, submit};
use hl_vulkan::Device;

use hl_gpu::protocol::model::command::Enc;
use hl_gpu::protocol::model::descriptor::{
    BindResource, ColorAttachment, DepthAttachment, DepthState,
};
use hl_gpu::protocol::model::enums::{compare, LoadOp, TextureFormat, Topology};
use hl_gpu::{Cmd, RecordingSink};

// ---------------------------------------------------------------------------------------------------
// harness
// ---------------------------------------------------------------------------------------------------

fn dev() -> Device {
    let inst = create::create_instance(result::HL_API_VERSION);
    create::create_device(&inst)
}

/// The ir texture id behind a `VkImage` handle (the id an emitted `ColorAttachment`/`BindResource` names).
fn img_ir(d: &Device, h: u64) -> u32 {
    d.images.get(&h).unwrap().ir_id
}
/// The ir sampler id behind a `VkSampler` handle.
fn samp_ir(d: &Device, h: u64) -> u32 {
    d.samplers.get(&h).unwrap().ir_id
}

/// A `SAMPLED | COLOR_ATTACHMENT` render-then-sample color texture (the G-buffer / ping-pong workhorse).
fn sampled_color(d: &mut Device, sink: &mut RecordingSink, w: u32, h: u32) -> u64 {
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
fn pipeline(
    d: &mut Device,
    sink: &mut RecordingSink,
    color_formats: Vec<TextureFormat>,
    depth: Option<DepthState>,
) -> u64 {
    pipeline_samples(d, sink, color_formats, depth, 1)
}

/// A minimal graphics pipeline over `color_formats` (+ optional depth) at `sample_count`, reusing the
/// passthrough SPIR-V vs/fs. `sample_count > 1` yields a multisample pipeline (`RenderPipelineDesc.sample_count`).
fn pipeline_samples(
    d: &mut Device,
    sink: &mut RecordingSink,
    color_formats: Vec<TextureFormat>,
    depth: Option<DepthState>,
    sample_count: u32,
) -> u64 {
    use hl_vulkan::adapter::spirv;
    let vs =
        create::create_shader_module_words(d, sink, spirv::sample_compute_spirv("vsmain")).unwrap();
    let fs =
        create::create_shader_module_words(d, sink, spirv::sample_compute_spirv("fsmain")).unwrap();
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
fn combined_sampler_set(
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
    let layout = create::create_descriptor_set_layout(d, bindings);
    let pool = create::create_descriptor_pool(d, 1);
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
fn submit_encoder(d: &mut Device, sink: &mut RecordingSink, cb: u64) -> Vec<Enc> {
    submit::queue_submit(d, sink, &[cb], None).unwrap();
    match sink.batches.last().unwrap().as_slice() {
        [Cmd::Submit(cbuf)] => cbuf.encoder.clone(),
        other => panic!("expected a single Submit, got {other:?}"),
    }
}

fn color_clear(image: u64, clear: [f32; 4]) -> RenderingColorAttachment {
    RenderingColorAttachment {
        image,
        clear,
        load_clear: true,
        store: true,
    }
}

// ===================================================================================================
// 1. DEFERRED SHADING — an MRT G-buffer pass feeding a fullscreen lighting pass that samples it.
// ===================================================================================================

/// Pass 1 writes a 3-attachment G-buffer (albedo/normal/position) with a geometry draw; pass 2's fullscreen
/// lighting draw SAMPLES all three G-buffer textures. Asserts pass 1 lowers to a `BeginRenderPass` with 3
/// color attachments + the geometry draw, and pass 2 binds the 3 textures (+ their samplers) and draws.
#[test]
fn deferred_shading_mrt_gbuffer_then_lighting_samples_three_targets() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();

    // The three G-buffer render targets, each also SAMPLED so the lighting pass can read them back.
    let albedo = sampled_color(&mut d, &mut sink, 128, 128);
    let normal = sampled_color(&mut d, &mut sink, 128, 128);
    let position = sampled_color(&mut d, &mut sink, 128, 128);
    let (a_ir, n_ir, p_ir) = (img_ir(&d, albedo), img_ir(&d, normal), img_ir(&d, position));

    // Geometry pipeline: 3 color targets (MRT). Lighting pipeline: 1 color target + no depth.
    let geo = pipeline(&mut d, &mut sink, vec![TextureFormat::Rgba8Unorm; 3], None);
    let light = pipeline(&mut d, &mut sink, vec![TextureFormat::Rgba8Unorm], None);
    let out = sampled_color(&mut d, &mut sink, 128, 128);
    let out_ir = img_ir(&d, out);
    let sampler = create::create_sampler(&mut d, &mut sink, 1, 1, 1, [0, 0, 0]);

    // ---- Pass 1: fill the G-buffer with a geometry draw (MRT: 3 color attachments, all CLEAR+STORE).
    let cb1 = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb1, false).unwrap();
    record::cmd_begin_rendering(
        &mut d,
        cb1,
        &[
            color_clear(albedo, [0.0; 4]),
            color_clear(normal, [0.0; 4]),
            color_clear(position, [0.0; 4]),
        ],
        None,
    )
    .unwrap();
    record::cmd_bind_pipeline(&mut d, cb1, geo).unwrap();
    record::cmd_draw(&mut d, cb1, 36, 1, 0, 0).unwrap();
    record::cmd_end_render_pass(&mut d, cb1).unwrap();
    record::end(&mut d, cb1).unwrap();
    let enc1 = submit_encoder(&mut d, &mut sink, cb1);

    assert_eq!(
        enc1,
        vec![
            Enc::BeginRenderPass {
                color: vec![
                    ColorAttachment {
                        texture: a_ir,
                        load: LoadOp::Clear,
                        clear: [0.0; 4],
                        store: true
                    },
                    ColorAttachment {
                        texture: n_ir,
                        load: LoadOp::Clear,
                        clear: [0.0; 4],
                        store: true
                    },
                    ColorAttachment {
                        texture: p_ir,
                        load: LoadOp::Clear,
                        clear: [0.0; 4],
                        store: true
                    },
                ],
                depth: None,
            },
            Enc::SetPipeline(d.pipelines.get(&geo).unwrap().ir_id),
            Enc::Draw {
                vertex_count: 36,
                instance_count: 1,
                first_vertex: 0,
                first_instance: 0
            },
            Enc::EndRenderPass,
        ],
        "G-buffer pass 1 must lower to a 3-color-attachment BeginRenderPass + the geometry draw"
    );

    // ---- Pass 2: bind the 3 G-buffer textures as samplers, then a fullscreen lighting draw reads them.
    let cb2 = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb2, false).unwrap();
    let entries =
        combined_sampler_set(&mut d, &mut sink, cb2, &[albedo, normal, position], sampler);

    // The lighting pass samples EXACTLY the three G-buffer textures: a Texture at each binding B + its
    // Sampler at B + 16 (the combined-image-sampler split the wgpu executor performs).
    let s = samp_ir(&d, sampler);
    assert_eq!(
        entries,
        vec![
            hl_gpu::protocol::model::descriptor::BindEntry {
                binding: 0,
                resource: BindResource::Texture { id: a_ir }
            },
            hl_gpu::protocol::model::descriptor::BindEntry {
                binding: 16,
                resource: BindResource::Sampler { id: s }
            },
            hl_gpu::protocol::model::descriptor::BindEntry {
                binding: 1,
                resource: BindResource::Texture { id: n_ir }
            },
            hl_gpu::protocol::model::descriptor::BindEntry {
                binding: 17,
                resource: BindResource::Sampler { id: s }
            },
            hl_gpu::protocol::model::descriptor::BindEntry {
                binding: 2,
                resource: BindResource::Texture { id: p_ir }
            },
            hl_gpu::protocol::model::descriptor::BindEntry {
                binding: 18,
                resource: BindResource::Sampler { id: s }
            },
        ],
        "lighting pass must sample all three G-buffer textures (image at B, sampler at B+16)"
    );

    record::cmd_begin_rendering(&mut d, cb2, &[color_clear(out, [0.0; 4])], None).unwrap();
    record::cmd_bind_pipeline(&mut d, cb2, light).unwrap();
    record::cmd_draw(&mut d, cb2, 3, 1, 0, 0).unwrap();
    record::cmd_end_render_pass(&mut d, cb2).unwrap();
    record::end(&mut d, cb2).unwrap();
    let enc2 = submit_encoder(&mut d, &mut sink, cb2);

    // The lighting pass targets the output and replays the sampled bind group into the fullscreen draw.
    let light_ir = d.pipelines.get(&light).unwrap().ir_id;
    let bind_group = match enc2.iter().find_map(|e| match e {
        Enc::SetBindGroup { index: 0, group } => Some(*group),
        _ => None,
    }) {
        Some(g) => g,
        None => panic!("lighting draw did not replay the G-buffer bind group: {enc2:?}"),
    };
    assert_eq!(
        enc2,
        vec![
            Enc::BeginRenderPass {
                color: vec![ColorAttachment {
                    texture: out_ir,
                    load: LoadOp::Clear,
                    clear: [0.0; 4],
                    store: true
                }],
                depth: None,
            },
            Enc::SetPipeline(light_ir),
            Enc::SetBindGroup {
                index: 0,
                group: bind_group
            },
            Enc::Draw {
                vertex_count: 3,
                instance_count: 1,
                first_vertex: 0,
                first_instance: 0
            },
            Enc::EndRenderPass,
        ],
        "lighting pass must render into the output with the G-buffer bound + a fullscreen draw"
    );
}

// ===================================================================================================
// 2. SHADOW MAPPING — a depth-only pass rendering occluders feeding a pass that samples the depth map.
// ===================================================================================================

/// Pass 1 is a DEPTH-ONLY render pass (no color attachment, one depth attachment) rendering occluders;
/// pass 2's draw samples that depth texture as a shadow map. Asserts pass 1 lowers to `BeginRenderPass {
/// color: [], depth: Some(..) }` (the depth-only shape — NO color attachment) + the occluder draw, and
/// pass 2's bind group resolves the depth texture as a sampled shadow map.
#[test]
fn shadow_mapping_depth_only_pass_then_samples_depth_map() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();

    // The shadow map: a D32 depth image, DEPTH_STENCIL_ATTACHMENT to render into + SAMPLED to read back.
    let shadow = create::create_image(
        &mut d,
        &mut sink,
        1024,
        1024,
        vk_format::D32_SFLOAT,
        vk_image_usage::DEPTH_STENCIL_ATTACHMENT | vk_image_usage::SAMPLED,
        1,
    )
    .unwrap();
    let shadow_ir = img_ir(&d, shadow);

    // Occluder pipeline: NO color targets, depth-write on (a depth-only pipeline). Main pipeline: color.
    let occluder = pipeline(
        &mut d,
        &mut sink,
        vec![],
        Some(DepthState::depth_only(
            TextureFormat::Depth32Float,
            true,
            compare::LESS,
        )),
    );
    let main = pipeline(&mut d, &mut sink, vec![TextureFormat::Rgba8Unorm], None);
    let scene = sampled_color(&mut d, &mut sink, 800, 600);
    let scene_ir = img_ir(&d, scene);
    let sampler = create::create_sampler(&mut d, &mut sink, 1, 1, 1, [0, 0, 0]);

    // ---- Pass 1: depth-only shadow pass. No color attachment, one CLEAR depth attachment.
    let cb1 = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb1, false).unwrap();
    record::cmd_begin_rendering(
        &mut d,
        cb1,
        &[], // <-- ZERO color attachments: the depth-only pass must NOT emit a color attachment.
        Some(RenderingDepthAttachment {
            image: shadow,
            clear_depth: 1.0,
            load_clear: true,
        }),
    )
    .unwrap();
    record::cmd_bind_pipeline(&mut d, cb1, occluder).unwrap();
    record::cmd_draw(&mut d, cb1, 24, 1, 0, 0).unwrap();
    record::cmd_end_render_pass(&mut d, cb1).unwrap();
    record::end(&mut d, cb1).unwrap();
    let enc1 = submit_encoder(&mut d, &mut sink, cb1);

    assert_eq!(
        enc1,
        vec![
            Enc::BeginRenderPass {
                color: vec![],
                depth: Some(DepthAttachment { texture: shadow_ir, load: LoadOp::Clear, clear_depth: 1.0, clear_stencil: 0 }),
            },
            Enc::SetPipeline(d.pipelines.get(&occluder).unwrap().ir_id),
            Enc::Draw { vertex_count: 24, instance_count: 1, first_vertex: 0, first_instance: 0 },
            Enc::EndRenderPass,
        ],
        "shadow pass must lower to a depth-only BeginRenderPass (color: [], depth: Some) + the occluder draw"
    );

    // ---- Pass 2: sample the depth texture as a shadow map, then the main scene draw reads it.
    let cb2 = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb2, false).unwrap();
    let entries = combined_sampler_set(&mut d, &mut sink, cb2, &[shadow], sampler);
    let s = samp_ir(&d, sampler);
    assert_eq!(
        entries,
        vec![
            hl_gpu::protocol::model::descriptor::BindEntry {
                binding: 0,
                resource: BindResource::Texture { id: shadow_ir }
            },
            hl_gpu::protocol::model::descriptor::BindEntry {
                binding: 16,
                resource: BindResource::Sampler { id: s }
            },
        ],
        "the main pass must bind the DEPTH texture as a sampled shadow map"
    );

    record::cmd_begin_rendering(
        &mut d,
        cb2,
        &[color_clear(scene, [0.1, 0.1, 0.1, 1.0])],
        None,
    )
    .unwrap();
    record::cmd_bind_pipeline(&mut d, cb2, main).unwrap();
    record::cmd_draw(&mut d, cb2, 3, 1, 0, 0).unwrap();
    record::cmd_end_render_pass(&mut d, cb2).unwrap();
    record::end(&mut d, cb2).unwrap();
    let enc2 = submit_encoder(&mut d, &mut sink, cb2);
    assert!(
        enc2.contains(&Enc::BeginRenderPass {
            color: vec![ColorAttachment {
                texture: scene_ir,
                load: LoadOp::Clear,
                clear: [0.1, 0.1, 0.1, 1.0],
                store: true
            }],
            depth: None,
        }),
        "the main pass renders into the color scene target: {enc2:?}"
    );
}

// ===================================================================================================
// 3. POST-PROCESS CHAIN — ping-pong: render A, sample A→B, sample B→C.
// ===================================================================================================

/// Render a scene into texture A, then a pass samples A and renders into B, then a pass samples B and
/// renders into C. Asserts the ping-pong: each pass's sampler binding names the PREVIOUS stage's texture,
/// and each pass's `BeginRenderPass` targets the NEXT texture (no aliasing, no stale binding).
#[test]
fn post_process_chain_ping_pongs_sampler_and_target() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();

    let a = sampled_color(&mut d, &mut sink, 256, 256);
    let b = sampled_color(&mut d, &mut sink, 256, 256);
    let c = sampled_color(&mut d, &mut sink, 256, 256);
    let (a_ir, b_ir, c_ir) = (img_ir(&d, a), img_ir(&d, b), img_ir(&d, c));
    let scene = pipeline(&mut d, &mut sink, vec![TextureFormat::Rgba8Unorm], None);
    let blur = pipeline(&mut d, &mut sink, vec![TextureFormat::Rgba8Unorm], None);
    let sampler = create::create_sampler(&mut d, &mut sink, 1, 1, 1, [0, 0, 0]);

    // ---- Stage 0: render the scene into A.
    let cb0 = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb0, false).unwrap();
    record::cmd_begin_rendering(&mut d, cb0, &[color_clear(a, [0.0; 4])], None).unwrap();
    record::cmd_bind_pipeline(&mut d, cb0, scene).unwrap();
    record::cmd_draw(&mut d, cb0, 3, 1, 0, 0).unwrap();
    record::cmd_end_render_pass(&mut d, cb0).unwrap();
    record::end(&mut d, cb0).unwrap();
    let enc0 = submit_encoder(&mut d, &mut sink, cb0);
    assert_eq!(
        enc0[0],
        Enc::BeginRenderPass {
            color: vec![ColorAttachment {
                texture: a_ir,
                load: LoadOp::Clear,
                clear: [0.0; 4],
                store: true
            }],
            depth: None,
        }
    );

    // A closure for one ping-pong stage: SAMPLE `src`, render into `dst`. Returns (sampled ir, target ir).
    let stage = |d: &mut Device, sink: &mut RecordingSink, src: u64, dst: u64| -> (u32, u32) {
        let cb = record::allocate_command_buffer(d);
        record::begin(d, cb, false).unwrap();
        let entries = combined_sampler_set(d, sink, cb, &[src], sampler);
        // The stage samples EXACTLY the previous stage's texture.
        let sampled = match entries[0].resource {
            BindResource::Texture { id } => id,
            ref other => panic!("expected a sampled Texture, got {other:?}"),
        };
        record::cmd_begin_rendering(d, cb, &[color_clear(dst, [0.0; 4])], None).unwrap();
        record::cmd_bind_pipeline(d, cb, blur).unwrap();
        record::cmd_draw(d, cb, 3, 1, 0, 0).unwrap();
        record::cmd_end_render_pass(d, cb).unwrap();
        record::end(d, cb).unwrap();
        let enc = submit_encoder(d, sink, cb);
        let target = match &enc[0] {
            Enc::BeginRenderPass { color, .. } => color[0].texture,
            other => panic!("expected BeginRenderPass, got {other:?}"),
        };
        (sampled, target)
    };

    // ---- Stage 1: sample A → render B. ---- Stage 2: sample B → render C.
    let (s1, t1) = stage(&mut d, &mut sink, a, b);
    let (s2, t2) = stage(&mut d, &mut sink, b, c);

    assert_eq!((s1, t1), (a_ir, b_ir), "stage 1 samples A and targets B");
    assert_eq!(
        (s2, t2),
        (b_ir, c_ir),
        "stage 2 samples B and targets C (ping-pong advanced, no aliasing)"
    );
    assert_ne!(
        s2, t2,
        "a ping-pong stage never samples its own render target"
    );
}

// ===================================================================================================
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
    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb, false).unwrap();
    record::cmd_begin_rendering(&mut d, cb, &[color_clear(msaa, [0.2, 0.4, 0.6, 1.0])], None)
        .unwrap();
    record::cmd_bind_pipeline(&mut d, cb, pipe).unwrap();
    record::cmd_draw(&mut d, cb, 3, 1, 0, 0).unwrap();
    record::cmd_end_render_pass(&mut d, cb).unwrap();
    record::cmd_resolve_image(&mut d, cb, msaa, resolve, (0, 0), (0, 0), (64, 64)).unwrap();
    record::end(&mut d, cb).unwrap();
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

    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb, false).unwrap();
    record::cmd_resolve_image(&mut d, cb, src, dst, (0, 0), (0, 0), (32, 32)).unwrap();
    record::end(&mut d, cb).unwrap();
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
// 5. RENDER-TO-LAYER / RENDER-TO-MIP — a DOCUMENTED whole-texture limit of the VK→IR lowering.
// ===================================================================================================

/// Rendering into a specific array layer / mip of a texture is NOT expressible through this lowering:
///   * `vkCreateImage` models single-mip (`mip_levels == 1`) single-layer (`depth == 1`) 2D images, so
///     there is no layer/mip to select as a render target in the first place; and
///   * the IR `ColorAttachment`/`DepthAttachment` carry only a whole `texture` id — there is NO mip/layer
///     subresource selector on a render attachment (that lives in the protocol crate, which a concurrent
///     agent owns and this task must not touch).
/// So a render-to-layer/mip request collapses to rendering the WHOLE texture. This test pins that truth:
/// the created texture is single-mip/single-layer, and the attachment names the whole texture id.
#[test]
fn render_to_layer_and_mip_is_a_documented_whole_texture_limit() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();

    let tex = sampled_color(&mut d, &mut sink, 512, 512);
    let tex_ir = img_ir(&d, tex);

    // The created texture is single-mip, single-layer (depth 1) — no layer/mip exists to target.
    match sink
        .batches
        .iter()
        .flatten()
        .find(|c| matches!(c, Cmd::CreateTexture(id, _) if *id == tex_ir))
    {
        Some(Cmd::CreateTexture(_, desc)) => {
            assert_eq!(
                desc.mip_levels, 1,
                "vkCreateImage models a single-mip image (no mip to render into)"
            );
            assert_eq!(
                desc.depth, 1,
                "vkCreateImage models a single-layer 2D image (no array layer to render into)"
            );
        }
        other => panic!("expected CreateTexture, got {other:?}"),
    }

    // Rendering into it names the WHOLE texture — the ColorAttachment has no mip/layer field to select one.
    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb, false).unwrap();
    record::cmd_begin_rendering(&mut d, cb, &[color_clear(tex, [0.0; 4])], None).unwrap();
    record::cmd_end_render_pass(&mut d, cb).unwrap();
    record::end(&mut d, cb).unwrap();
    let enc = submit_encoder(&mut d, &mut sink, cb);

    // The attachment is exactly `ColorAttachment { texture, load, clear, store }` — no subresource. The
    // whole-struct equality below is the proof: were a layer/mip selector present it would appear here.
    assert_eq!(
        enc,
        vec![
            Enc::BeginRenderPass {
                color: vec![ColorAttachment {
                    texture: tex_ir,
                    load: LoadOp::Clear,
                    clear: [0.0; 4],
                    store: true
                }],
                depth: None,
            },
            Enc::EndRenderPass,
        ],
        "a render attachment names the whole texture (no layer/mip subresource in the IR)"
    );
}
