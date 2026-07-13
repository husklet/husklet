//! End-to-end Vulkan GRAPHICS on the REAL Metal GPU, driven through dd-shim-vk's exported `vk*` API.
//!
//! The milestone: an app that calls ONLY dd-shim-vk's Vulkan entry points — create instance/device,
//! a color-attachment image + view + render pass + framebuffer, a SPIR-V vertex+fragment graphics
//! pipeline, a vertex buffer, then records `vkCmdBeginRenderPass`/`vkCmdBindPipeline`/
//! `vkCmdBindVertexBuffers`/`vkCmdDraw(3)`/`vkCmdEndRenderPass` and `vkQueueSubmit` — produces a dd-gpu
//! IR stream that rasterizes a green triangle on a live Metal device. The Vulkan analogue of
//! `dd-gpu-wgpu`'s `spirv_triangle.rs`, every action through the guest driver's `vk*` surface.
//!
//! The color attachment is host-owned (like a swapchain image): the shim assigns it an IR texture id
//! and references it in `Enc::BeginRenderPass`; the test — playing the host exec service — registers
//! that id as the backend's render target (`create_render_target`), replays the shim-produced IR, and
//! reads the pixels back. Needs a Metal device → macOS only.
//! Run: `cargo test -p dd-shim-vk --test vk_triangle`.

#![cfg(target_os = "macos")]

use ash::vk;
use ash::vk::Handle;
use core::ffi::c_void;
use dd_gpu::ir::TextureFormat;
use dd_gpu_wgpu::WgpuBackend;
use vk_dd as ddvk;

const W: u32 = 64;
const H: u32 = 64;

fn module_to_spirv(module: &naga::Module) -> Result<Vec<u32>, String> {
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(module)
    .map_err(|e| format!("validate: {e:?}"))?;
    naga::back::spv::write_vec(module, &info, &naga::back::spv::Options::default(), None)
        .map_err(|e| format!("spv-out: {e:?}"))
}

const VS_GLSL: &str = "\
#version 450
layout(location = 0) in vec2 pos;
layout(location = 1) in vec4 col;
layout(location = 0) out vec4 vcol;
void main() { gl_Position = vec4(pos, 0.0, 1.0); vcol = col; }
";
const FS_GLSL: &str = "\
#version 450
layout(location = 0) in vec4 vcol;
layout(location = 0) out vec4 frag;
void main() { frag = vcol; }
";
const VS_WGSL: &str = "\
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) col: vec4<f32> };
@vertex fn main(@location(0) pos: vec2<f32>, @location(1) col: vec4<f32>) -> VOut {
    var o: VOut; o.pos = vec4<f32>(pos, 0.0, 1.0); o.col = col; return o;
}
";
const FS_WGSL: &str = "\
@fragment fn main(@location(0) col: vec4<f32>) -> @location(0) vec4<f32> { return col; }
";

fn stage_spirv(glsl: &str, wgsl: &str, stage: naga::ShaderStage, name: &str) -> Vec<u32> {
    let mut fe = naga::front::glsl::Frontend::default();
    match fe
        .parse(&naga::front::glsl::Options::from(stage), glsl)
        .map_err(|e| format!("{e:?}"))
        .and_then(|m| module_to_spirv(&m))
    {
        Ok(w) => {
            eprintln!("vk_triangle: {name} SPIR-V minted from GLSL 450 (Vulkan path)");
            w
        }
        Err(e) => {
            eprintln!("vk_triangle: GLSL front end unavailable for {name} ({e}); using WGSL SPIR-V");
            module_to_spirv(&naga::front::wgsl::parse_str(wgsl).expect("wgsl")).expect("wgsl→spv")
        }
    }
}

fn create_shader(dev: *mut c_void, spirv: &[u32]) -> u64 {
    let ci = vk::ShaderModuleCreateInfo::default().code(spirv);
    let mut m: u64 = 0;
    assert_eq!(ddvk::vkCreateShaderModule(dev, &ci, core::ptr::null(), &mut m), 0);
    m
}

#[test]
fn vk_triangle_renders_on_real_metal() {
    ddvk::reg::reset();

    // --- instance / device / queue / command pool ---
    let mut inst: *mut c_void = core::ptr::null_mut();
    assert_eq!(ddvk::vkCreateInstance(core::ptr::null(), core::ptr::null(), &mut inst), 0);
    let mut n = 1u32;
    let mut phys: *mut c_void = core::ptr::null_mut();
    assert_eq!(ddvk::vkEnumeratePhysicalDevices(inst, &mut n, &mut phys), 0);
    let mut dev: *mut c_void = core::ptr::null_mut();
    assert_eq!(ddvk::vkCreateDevice(phys, &vk::DeviceCreateInfo::default() as *const _, core::ptr::null(), &mut dev), 0);
    let mut queue: *mut c_void = core::ptr::null_mut();
    ddvk::vkGetDeviceQueue(dev, 0, 0, &mut queue);
    let mut pool: u64 = 0;
    assert_eq!(
        ddvk::vkCreateCommandPool(dev, &vk::CommandPoolCreateInfo::default(), core::ptr::null(), &mut pool),
        0
    );

    // --- color attachment: image + view + render pass + framebuffer ---
    let img_ci = vk::ImageCreateInfo::default()
        .image_type(vk::ImageType::TYPE_2D)
        .format(vk::Format::R8G8B8A8_UNORM)
        .extent(vk::Extent3D { width: W, height: H, depth: 1 })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::TYPE_1)
        .usage(vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC);
    let mut image: u64 = 0;
    assert_eq!(ddvk::vkCreateImage(dev, &img_ci, core::ptr::null(), &mut image), 0);
    let attach_ir = ddvk::reg::lock().images.get(&image).map(|i| i.ir_id).unwrap();

    let view_ci = vk::ImageViewCreateInfo::default()
        .image(vk::Image::from_raw(image))
        .view_type(vk::ImageViewType::TYPE_2D)
        .format(vk::Format::R8G8B8A8_UNORM);
    let mut view: u64 = 0;
    assert_eq!(ddvk::vkCreateImageView(dev, &view_ci, core::ptr::null(), &mut view), 0);

    let attachment = vk::AttachmentDescription::default()
        .format(vk::Format::R8G8B8A8_UNORM)
        .samples(vk::SampleCountFlags::TYPE_1)
        .load_op(vk::AttachmentLoadOp::CLEAR)
        .store_op(vk::AttachmentStoreOp::STORE)
        .final_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
    let color_ref = [vk::AttachmentReference::default()
        .attachment(0)
        .layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)];
    let subpass = [vk::SubpassDescription::default()
        .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
        .color_attachments(&color_ref)];
    let atts = [attachment];
    let rp_ci = vk::RenderPassCreateInfo::default().attachments(&atts).subpasses(&subpass);
    let mut render_pass: u64 = 0;
    assert_eq!(ddvk::vkCreateRenderPass(dev, &rp_ci, core::ptr::null(), &mut render_pass), 0);

    let views = [vk::ImageView::from_raw(view)];
    let fb_ci = vk::FramebufferCreateInfo::default()
        .render_pass(vk::RenderPass::from_raw(render_pass))
        .attachments(&views)
        .width(W)
        .height(H)
        .layers(1);
    let mut framebuffer: u64 = 0;
    assert_eq!(ddvk::vkCreateFramebuffer(dev, &fb_ci, core::ptr::null(), &mut framebuffer), 0);

    // --- vertex buffer: a green triangle in clip space (pos float2 + color float4, stride 24) ---
    let tri: [([f32; 2], [f32; 4]); 3] = [
        ([0.0, 0.6], [0.0, 1.0, 0.0, 1.0]),
        ([-0.6, -0.6], [0.0, 1.0, 0.0, 1.0]),
        ([0.6, -0.6], [0.0, 1.0, 0.0, 1.0]),
    ];
    let mut verts = Vec::new();
    for (p, c) in tri {
        for v in p {
            verts.extend_from_slice(&v.to_le_bytes());
        }
        for v in c {
            verts.extend_from_slice(&v.to_le_bytes());
        }
    }
    let vsz = verts.len() as u64;
    let buf_ci = vk::BufferCreateInfo::default().size(vsz).usage(vk::BufferUsageFlags::VERTEX_BUFFER);
    let mut vbuf: u64 = 0;
    assert_eq!(ddvk::vkCreateBuffer(dev, &buf_ci, core::ptr::null(), &mut vbuf), 0);
    let mut vmem: u64 = 0;
    assert_eq!(
        ddvk::vkAllocateMemory(dev, &vk::MemoryAllocateInfo::default().allocation_size(vsz), core::ptr::null(), &mut vmem),
        0
    );
    assert_eq!(ddvk::vkBindBufferMemory(dev, vbuf, vmem, 0), 0);
    let mut p: *mut c_void = core::ptr::null_mut();
    assert_eq!(ddvk::vkMapMemory(dev, vmem, 0, vsz, 0, &mut p), 0);
    unsafe { core::ptr::copy_nonoverlapping(verts.as_ptr(), p as *mut u8, verts.len()) };
    ddvk::vkUnmapMemory(dev, vmem);

    // --- SPIR-V graphics pipeline ---
    let vs = create_shader(dev, &stage_spirv(VS_GLSL, VS_WGSL, naga::ShaderStage::Vertex, "vertex"));
    let fs = create_shader(dev, &stage_spirv(FS_GLSL, FS_WGSL, naga::ShaderStage::Fragment, "fragment"));
    let mut layout: u64 = 0;
    assert_eq!(
        ddvk::vkCreatePipelineLayout(dev, (&vk::PipelineLayoutCreateInfo::default() as *const _) as *const vk::PipelineLayoutCreateInfo, core::ptr::null(), &mut layout),
        0
    );

    let stages = [
        vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::VERTEX).module(vk::ShaderModule::from_raw(vs)).name(c"main"),
        vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::FRAGMENT).module(vk::ShaderModule::from_raw(fs)).name(c"main"),
    ];
    let bindings = [vk::VertexInputBindingDescription::default().binding(0).stride(24).input_rate(vk::VertexInputRate::VERTEX)];
    let attrs = [
        vk::VertexInputAttributeDescription::default().location(0).binding(0).format(vk::Format::R32G32_SFLOAT).offset(0),
        vk::VertexInputAttributeDescription::default().location(1).binding(0).format(vk::Format::R32G32B32A32_SFLOAT).offset(8),
    ];
    let vi = vk::PipelineVertexInputStateCreateInfo::default().vertex_binding_descriptions(&bindings).vertex_attribute_descriptions(&attrs);
    let ia = vk::PipelineInputAssemblyStateCreateInfo::default().topology(vk::PrimitiveTopology::TRIANGLE_LIST);
    let gp_ci = vk::GraphicsPipelineCreateInfo::default()
        .stages(&stages)
        .vertex_input_state(&vi)
        .input_assembly_state(&ia)
        .layout(vk::PipelineLayout::from_raw(layout))
        .render_pass(vk::RenderPass::from_raw(render_pass));
    let mut pipeline: u64 = 0;
    assert_eq!(
        ddvk::vkCreateGraphicsPipelines(dev, 0, 1, &gp_ci, core::ptr::null(), &mut pipeline),
        0
    );

    // --- record + submit the draw ---
    let cb_ai = vk::CommandBufferAllocateInfo::default().command_pool(vk::CommandPool::from_raw(pool)).command_buffer_count(1);
    let mut cb: *mut c_void = core::ptr::null_mut();
    assert_eq!(ddvk::vkAllocateCommandBuffers(dev, &cb_ai, &mut cb), 0);
    assert_eq!(ddvk::vkBeginCommandBuffer(cb, (&vk::CommandBufferBeginInfo::default() as *const _) as *const vk::CommandBufferBeginInfo), 0);

    let clear = [vk::ClearValue { color: vk::ClearColorValue { float32: [0.1, 0.1, 0.1, 1.0] } }];
    let rp_begin = vk::RenderPassBeginInfo::default()
        .render_pass(vk::RenderPass::from_raw(render_pass))
        .framebuffer(vk::Framebuffer::from_raw(framebuffer))
        .render_area(vk::Rect2D { offset: vk::Offset2D { x: 0, y: 0 }, extent: vk::Extent2D { width: W, height: H } })
        .clear_values(&clear);
    ddvk::vkCmdBeginRenderPass(cb, &rp_begin, vk::SubpassContents::INLINE.as_raw());
    ddvk::vkCmdBindPipeline(cb, vk::PipelineBindPoint::GRAPHICS.as_raw(), pipeline);
    let vbufs = [vbuf];
    let offs = [0u64];
    ddvk::vkCmdBindVertexBuffers(cb, 0, 1, vbufs.as_ptr(), offs.as_ptr());
    ddvk::vkCmdDraw(cb, 3, 1, 0, 0);
    ddvk::vkCmdEndRenderPass(cb);
    assert_eq!(ddvk::vkEndCommandBuffer(cb), 0);

    let cbs = [cb];
    let submit = vk::SubmitInfo { command_buffer_count: 1, p_command_buffers: cbs.as_ptr() as *const vk::CommandBuffer, ..Default::default() };
    assert_eq!(ddvk::vkQueueSubmit(queue, 1, &submit, 0), 0);
    assert_eq!(ddvk::vkQueueWaitIdle(queue), 0);

    // --- replay on real Metal, reading back the render target ---
    let ir = ddvk::reg::take_ir();
    eprintln!("vk_triangle: shim produced {} IR commands", ir.len());
    let bytes_ir = dd_gpu::ir::encode_stream(&ir);
    let mut be = WgpuBackend::new().expect("wgpu Metal backend");
    // The host owns the attachment surface: register the shim's attachment texture id as the backend
    // render target (mirrors a swapchain image the host provides), then replay the draw into it.
    be.create_render_target(attach_ir, W, H, TextureFormat::Rgba8Unorm);
    dd_gpu::replay::replay_stream(&mut be, &bytes_ir).expect("replay vk triangle IR on Metal");

    let data = be.read_target(attach_ir).expect("readback");
    let px = |x: u32, y: u32| -> [u8; 4] {
        let i = ((y * W + x) * 4) as usize;
        [data[i], data[i + 1], data[i + 2], data[i + 3]]
    };
    let center = px(32, 32);
    let corner = px(2, 2);
    let mut green = 0u32;
    let mut gray = 0u32;
    for i in (0..data.len()).step_by(4) {
        let p = [data[i], data[i + 1], data[i + 2]];
        if p[0] < 40 && p[1] > 200 && p[2] < 40 {
            green += 1;
        }
        if (20..=40).contains(&p[0]) && (20..=40).contains(&p[1]) && (20..=40).contains(&p[2]) {
            gray += 1;
        }
    }
    eprintln!("[vk-tri] center(32,32)={center:?} corner(2,2)={corner:?} green_px={green} gray_px={gray} ir_cmds={}", ir.len());
    assert!(center[0] < 40 && center[1] > 200 && center[2] < 40, "center should be green, got {center:?}");
    assert!((20..=40).contains(&corner[0]) && (20..=40).contains(&corner[1]), "corner should be gray clear, got {corner:?}");
    assert!(green > 200, "expected a substantial green triangle, got {green} px");
    assert!(gray > 200, "expected the clear background to remain, got {gray} px");
    eprintln!("vk_triangle_renders_on_real_metal: OK (green_px={green}, gray_px={gray})");
}
