//! End-to-end Vulkan WSI SWAPCHAIN render on REAL Metal, driven through dd-shim-vk's `vk*` API.
//!
//! The milestone this proves for increment 3: an app that goes through the full windowed-Vulkan WSI
//! path dd-shim-vk now exports — `vkCreateWaylandSurfaceKHR` → `vkCreateSwapchainKHR` →
//! `vkGetSwapchainImagesKHR` → `vkAcquireNextImageKHR` → render into the acquired presentable image →
//! `vkQueuePresentKHR` — produces the correct rendered frame in the swapchain image on a live Metal
//! device. This is the same render→present IR a live `vkcube-wayland` emits; the live guest ships it to
//! the host GPU-exec over `$DD_GPU_EXEC` and commits the IOSurface dma-buf to dd-display, whereas this
//! test (playing the host exec service) replays the render on the WgpuBackend and reads the presentable
//! image back. `Cmd::Present` is the live wayland/dd-display step, so it is filtered before the
//! backend replay. Needs a Metal device → macOS only. Run: `cargo test -p dd-gpu-wgpu --test vk_present`.

#![cfg(target_os = "macos")]

use ash::vk;
use ash::vk::Handle;
use core::ffi::c_void;
use dd_gpu::ir::{Cmd, TextureFormat};
use dd_gpu_wgpu::WgpuBackend;
use vk_dd as ddvk;

const W: u32 = 64;
const H: u32 = 64;

fn module_to_spirv(m: &naga::Module) -> Vec<u32> {
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(m)
    .expect("validate");
    naga::back::spv::write_vec(m, &info, &naga::back::spv::Options::default(), None).expect("spv")
}
fn wgsl_spirv(src: &str) -> Vec<u32> {
    module_to_spirv(&naga::front::wgsl::parse_str(src).expect("wgsl"))
}
const VS: &str = "struct VOut { @builtin(position) pos: vec4<f32>, @location(0) col: vec4<f32> };\n\
@vertex fn main(@location(0) pos: vec2<f32>, @location(1) col: vec4<f32>) -> VOut {\n\
  var o: VOut; o.pos = vec4<f32>(pos, 0.0, 1.0); o.col = col; return o; }";
const FS: &str = "@fragment fn main(@location(0) col: vec4<f32>) -> @location(0) vec4<f32> { return col; }";

fn shader(dev: *mut c_void, spv: &[u32]) -> u64 {
    let ci = vk::ShaderModuleCreateInfo::default().code(spv);
    let mut m = 0u64;
    assert_eq!(ddvk::vkCreateShaderModule(dev, &ci, core::ptr::null(), &mut m), 0);
    m
}

#[test]
fn vk_swapchain_present_renders_on_real_metal() {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;

    ddvk::reg::reset();

    // vkQueuePresentKHR ships the frame to the host GPU-exec socket ($DD_GPU_EXEC) and, unless disabled,
    // to the wayland compositor. Stand up a throwaway acking sink so the present succeeds off-guest; the
    // test itself replays the render IR on Metal below and checks the pixels.
    let dir = std::env::temp_dir().join(format!("dd-vkpresent-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let sock = dir.join("exec.sock");
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock).unwrap();
    std::env::set_var("DD_GPU_EXEC", &sock);
    std::env::set_var("DD_VK_NO_WL_PRESENT", "1");
    let exec = std::thread::spawn(move || {
        if let Ok((mut c, _)) = listener.accept() {
            let mut hdr = [0u8; 16];
            if c.read_exact(&mut hdr).is_ok() {
                let len = u32::from_le_bytes(hdr[12..16].try_into().unwrap()) as usize;
                let mut body = vec![0u8; len];
                let _ = c.read_exact(&mut body);
                let _ = c.write_all(&[1u8]); // ACK_OK
            }
        }
    });

    // instance / device / queue / command pool
    let mut inst: *mut c_void = core::ptr::null_mut();
    assert_eq!(ddvk::vkCreateInstance(core::ptr::null(), core::ptr::null(), &mut inst), 0);
    let mut n = 1u32;
    let mut phys: *mut c_void = core::ptr::null_mut();
    ddvk::vkEnumeratePhysicalDevices(inst, &mut n, &mut phys);
    let mut dev: *mut c_void = core::ptr::null_mut();
    ddvk::vkCreateDevice(phys, &vk::DeviceCreateInfo::default() as *const _, core::ptr::null(), &mut dev);
    let mut queue: *mut c_void = core::ptr::null_mut();
    ddvk::vkGetDeviceQueue(dev, 0, 0, &mut queue);
    let mut pool = 0u64;
    ddvk::vkCreateCommandPool(dev, &vk::CommandPoolCreateInfo::default(), core::ptr::null(), &mut pool);

    // --- WSI: wayland surface + swapchain (fallback offscreen images off-guest) ---
    let wsci = vk::WaylandSurfaceCreateInfoKHR {
        display: 0x1 as *mut _, // opaque app wl_display/wl_surface (unused off-guest)
        surface: 0x2 as *mut _,
        ..Default::default()
    };
    let mut surface = 0u64;
    assert_eq!(ddvk::vkCreateWaylandSurfaceKHR(inst, &wsci, core::ptr::null(), &mut surface), 0);

    let mut support = 0u32;
    assert_eq!(ddvk::vkGetPhysicalDeviceSurfaceSupportKHR(phys, 0, surface, &mut support), 0);
    assert_eq!(support, vk::TRUE);

    let sc_ci = vk::SwapchainCreateInfoKHR::default()
        .surface(vk::SurfaceKHR::from_raw(surface))
        .min_image_count(2)
        .image_format(vk::Format::B8G8R8A8_UNORM)
        .image_extent(vk::Extent2D { width: W, height: H })
        .image_array_layers(1)
        .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
        // The shim validates the swapchain against the reported surface capabilities: identity transform
        // and opaque composite alpha are the only supported values.
        .pre_transform(vk::SurfaceTransformFlagsKHR::IDENTITY)
        .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
        .present_mode(vk::PresentModeKHR::FIFO);
    let mut swapchain = 0u64;
    assert_eq!(ddvk::vkCreateSwapchainKHR(dev, &sc_ci, core::ptr::null(), &mut swapchain), 0);

    let mut img_count = 0u32;
    ddvk::vkGetSwapchainImagesKHR(dev, swapchain, &mut img_count, core::ptr::null_mut());
    assert!(img_count >= 2, "swapchain must expose >=2 presentable images, got {img_count}");
    let mut images = vec![0u64; img_count as usize];
    ddvk::vkGetSwapchainImagesKHR(dev, swapchain, &mut img_count, images.as_mut_ptr());

    // Acquire requires a semaphore or fence to signal (Vulkan spec); use a throwaway fence.
    let mut acquire_fence = 0u64;
    assert_eq!(ddvk::vkCreateFence(dev, &vk::FenceCreateInfo::default() as *const _, core::ptr::null(), &mut acquire_fence), 0);
    let mut image_index = 0u32;
    assert_eq!(
        ddvk::vkAcquireNextImageKHR(dev, swapchain, u64::MAX, 0, acquire_fence, &mut image_index),
        0
    );
    let sc_image = images[image_index as usize];
    // The presentable image's IR texture id (the render target the host renders into).
    let sc_image_ir = ddvk::reg::lock().images.get(&sc_image).map(|i| i.ir_id).unwrap();

    // --- render a green triangle into the acquired swapchain image ---
    let view_ci = vk::ImageViewCreateInfo::default()
        .image(vk::Image::from_raw(sc_image))
        .view_type(vk::ImageViewType::TYPE_2D)
        .format(vk::Format::B8G8R8A8_UNORM)
        .subresource_range(vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        });
    let mut view = 0u64;
    assert_eq!(ddvk::vkCreateImageView(dev, &view_ci, core::ptr::null(), &mut view), 0);

    let att = [vk::AttachmentDescription::default()
        .format(vk::Format::B8G8R8A8_UNORM)
        .samples(vk::SampleCountFlags::TYPE_1)
        .load_op(vk::AttachmentLoadOp::CLEAR)
        .store_op(vk::AttachmentStoreOp::STORE)];
    let cref = [vk::AttachmentReference::default().attachment(0).layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)];
    let sub = [vk::SubpassDescription::default().pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS).color_attachments(&cref)];
    let rp_ci = vk::RenderPassCreateInfo::default().attachments(&att).subpasses(&sub);
    let mut rp = 0u64;
    ddvk::vkCreateRenderPass(dev, &rp_ci, core::ptr::null(), &mut rp);
    let vlist = [vk::ImageView::from_raw(view)];
    let fb_ci = vk::FramebufferCreateInfo::default().render_pass(vk::RenderPass::from_raw(rp)).attachments(&vlist).width(W).height(H).layers(1);
    let mut fb = 0u64;
    ddvk::vkCreateFramebuffer(dev, &fb_ci, core::ptr::null(), &mut fb);

    // vertex buffer (green triangle)
    let tri: [([f32; 2], [f32; 4]); 3] = [
        ([0.0, 0.6], [0.0, 1.0, 0.0, 1.0]),
        ([-0.6, -0.6], [0.0, 1.0, 0.0, 1.0]),
        ([0.6, -0.6], [0.0, 1.0, 0.0, 1.0]),
    ];
    let mut verts = Vec::new();
    for (p, c) in tri {
        for v in p { verts.extend_from_slice(&v.to_le_bytes()); }
        for v in c { verts.extend_from_slice(&v.to_le_bytes()); }
    }
    let vsz = verts.len() as u64;
    let mut vbuf = 0u64;
    ddvk::vkCreateBuffer(dev, &vk::BufferCreateInfo::default().size(vsz).usage(vk::BufferUsageFlags::VERTEX_BUFFER), core::ptr::null(), &mut vbuf);
    let mut vmem = 0u64;
    ddvk::vkAllocateMemory(dev, &vk::MemoryAllocateInfo::default().allocation_size(vsz), core::ptr::null(), &mut vmem);
    ddvk::vkBindBufferMemory(dev, vbuf, vmem, 0);
    let mut p: *mut c_void = core::ptr::null_mut();
    ddvk::vkMapMemory(dev, vmem, 0, vsz, 0, &mut p);
    unsafe { core::ptr::copy_nonoverlapping(verts.as_ptr(), p as *mut u8, verts.len()) };
    ddvk::vkUnmapMemory(dev, vmem);

    // graphics pipeline
    let vs = shader(dev, &wgsl_spirv(VS));
    let fs = shader(dev, &wgsl_spirv(FS));
    let mut layout = 0u64;
    ddvk::vkCreatePipelineLayout(dev, (&vk::PipelineLayoutCreateInfo::default() as *const _) as *const vk::PipelineLayoutCreateInfo, core::ptr::null(), &mut layout);
    let stages = [
        vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::VERTEX).module(vk::ShaderModule::from_raw(vs)).name(c"main"),
        vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::FRAGMENT).module(vk::ShaderModule::from_raw(fs)).name(c"main"),
    ];
    let binds = [vk::VertexInputBindingDescription::default().binding(0).stride(24).input_rate(vk::VertexInputRate::VERTEX)];
    let attrs = [
        vk::VertexInputAttributeDescription::default().location(0).binding(0).format(vk::Format::R32G32_SFLOAT).offset(0),
        vk::VertexInputAttributeDescription::default().location(1).binding(0).format(vk::Format::R32G32B32A32_SFLOAT).offset(8),
    ];
    let vi = vk::PipelineVertexInputStateCreateInfo::default().vertex_binding_descriptions(&binds).vertex_attribute_descriptions(&attrs);
    let ia = vk::PipelineInputAssemblyStateCreateInfo::default().topology(vk::PrimitiveTopology::TRIANGLE_LIST);
    let gp = vk::GraphicsPipelineCreateInfo::default().stages(&stages).vertex_input_state(&vi).input_assembly_state(&ia).layout(vk::PipelineLayout::from_raw(layout)).render_pass(vk::RenderPass::from_raw(rp));
    let mut pipeline = 0u64;
    ddvk::vkCreateGraphicsPipelines(dev, 0, 1, &gp, core::ptr::null(), &mut pipeline);

    // record + submit the draw into the swapchain image
    let cb_ai = vk::CommandBufferAllocateInfo::default().command_pool(vk::CommandPool::from_raw(pool)).command_buffer_count(1);
    let mut cb: *mut c_void = core::ptr::null_mut();
    ddvk::vkAllocateCommandBuffers(dev, &cb_ai, &mut cb);
    ddvk::vkBeginCommandBuffer(cb, (&vk::CommandBufferBeginInfo::default() as *const _) as *const vk::CommandBufferBeginInfo);
    let clear = [vk::ClearValue { color: vk::ClearColorValue { float32: [0.1, 0.1, 0.1, 1.0] } }];
    let rpb = vk::RenderPassBeginInfo::default()
        .render_pass(vk::RenderPass::from_raw(rp))
        .framebuffer(vk::Framebuffer::from_raw(fb))
        .render_area(vk::Rect2D { offset: vk::Offset2D { x: 0, y: 0 }, extent: vk::Extent2D { width: W, height: H } })
        .clear_values(&clear);
    ddvk::vkCmdBeginRenderPass(cb, &rpb, vk::SubpassContents::INLINE.as_raw());
    ddvk::vkCmdBindPipeline(cb, vk::PipelineBindPoint::GRAPHICS.as_raw(), pipeline);
    let vb = [vbuf];
    let offs = [0u64];
    ddvk::vkCmdBindVertexBuffers(cb, 0, 1, vb.as_ptr(), offs.as_ptr());
    ddvk::vkCmdDraw(cb, 3, 1, 0, 0);
    ddvk::vkCmdEndRenderPass(cb);
    ddvk::vkEndCommandBuffer(cb);
    let cbs = [cb];
    let submit = vk::SubmitInfo { command_buffer_count: 1, p_command_buffers: cbs.as_ptr() as *const vk::CommandBuffer, ..Default::default() };
    ddvk::vkQueueSubmit(queue, 1, &submit, 0);

    // present (adds Cmd::Present; on the live guest this ships to $DD_GPU_EXEC + commits the dma-buf)
    let scs = [vk::SwapchainKHR::from_raw(swapchain)];
    let idxs = [image_index];
    let present = vk::PresentInfoKHR::default().swapchains(&scs).image_indices(&idxs);
    assert_eq!(ddvk::vkQueuePresentKHR(queue, &present), 0);

    // --- replay the shim-produced RENDER IR on real Metal (present is the live wayland step) ---
    let ir: Vec<Cmd> = ddvk::reg::take_ir()
        .into_iter()
        .filter(|c| !matches!(c, Cmd::Present { .. }))
        .collect();
    let has_present = ddvk::reg::lock().present_flushed > 0; // present ran + advanced the cursor
    eprintln!("vk_present: {} render IR cmds (present filtered); present_flushed>0 = {has_present}", ir.len());
    let bytes = dd_gpu::ir::encode_stream(&ir);
    let mut be = WgpuBackend::new().expect("wgpu Metal backend");
    be.create_render_target(sc_image_ir, W, H, TextureFormat::Bgra8Unorm);
    dd_gpu::replay::replay_stream(&mut be, &bytes).expect("replay swapchain render on Metal");

    let data = be.read_target(sc_image_ir).expect("readback");
    // Bgra8: byte order is B,G,R,A. Green = G high, B/R low.
    let px = |x: u32, y: u32| { let i = ((y * W + x) * 4) as usize; [data[i], data[i + 1], data[i + 2], data[i + 3]] };
    let center = px(32, 32);
    let mut green = 0u32;
    for i in (0..data.len()).step_by(4) {
        if data[i] < 40 && data[i + 1] > 200 && data[i + 2] < 40 { green += 1; }
    }
    eprintln!("[vk-present] center(32,32)=BGRA{center:?} green_px={green}");
    assert!(center[1] > 200 && center[0] < 40 && center[2] < 40, "swapchain center should be green, got BGRA {center:?}");
    assert!(green > 200, "expected the presented green triangle, got {green} px");
    assert!(has_present, "vkQueuePresentKHR must have run + terminated the frame with Cmd::Present");
    let _ = exec.join();
    let _ = std::fs::remove_dir_all(&dir);
    eprintln!("vk_swapchain_present_renders_on_real_metal: OK (green_px={green})");
}
