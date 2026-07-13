//! WSI: `VK_KHR_surface` + `VK_KHR_wayland_surface` + `VK_KHR_swapchain` (present) — the path that
//! lets a real windowed Vulkan app (vkcube) render THROUGH dd-shim-vk onto dd-display.
//!
//! Ported from MoltenVK's WSI object model (`MVKSurface.mm`, `MVKSwapchain.mm`) and mirroring
//! dd-shim-gl's present half (`src/wayland.rs` / `gl_shim.c`):
//!   * `MVKSwapchain` owns N presentable images the app renders into and cycles with
//!     `acquireNextImage` / `queuePresent`. Here each presentable image is a `renderd` IOSurface/
//!     dma-buf (`transport::renderd::alloc` — the rung-2 buffer the host Metal executor renders into),
//!     paired with a `VkImage` (its IR texture id is what the app's render pass targets).
//!   * Present = ship the frame's IR (the recorded render + a terminating `Cmd::Present{ surface,
//!     texture }`) to the host GPU-exec service over `dd_shim_common::transport::ExecConn` — the SAME
//!     channel + `[surface.id,w,h,len][ir]` frame protocol dd-shim-gl's `eglSwapBuffers` uses — then
//!     attach the IOSurface dma-buf to the app's `wl_surface` and commit (the dma-buf `modifier_hi =
//!     DD_DMABUF_MOD_MAGIC`, `modifier_lo = surface.id` convention dd-display keys the GPU render on).
//!
//! Off-guest (no `/dev/dri/renderD128`, e.g. the macOS validation host) `renderd::alloc` fails, so the
//! swapchain falls back to plain offscreen images — the render path still exercises + the Metal
//! validation replays it via the backend. The live guest path uses the real IOSurface + `$DD_GPU_EXEC`.

use crate::reg::{
    self, ImageRec, ImageSubresourceState, SurfaceRec, SwapImage, SwapImageState, SwapchainRec,
    SwapchainState,
};
use crate::types::*;
use ash::vk;
use ash::vk::Handle;
use core::ffi::c_void;
use dd_shim_common::ir::encode_stream;
use dd_shim_common::transport::{self, Surface, DRM_FMT_XRGB8888};

fn tex_format_of(f: vk::Format) -> dd_shim_common::ir::TextureFormat {
    crate::memory::tex_format(f)
}

const SURFACE_FORMAT: vk::SurfaceFormatKHR = vk::SurfaceFormatKHR {
    format: vk::Format::B8G8R8A8_UNORM,
    color_space: vk::ColorSpaceKHR::SRGB_NONLINEAR,
};
const SURFACE_USAGE: vk::ImageUsageFlags = vk::ImageUsageFlags::from_raw(
    vk::ImageUsageFlags::COLOR_ATTACHMENT.as_raw()
        | vk::ImageUsageFlags::TRANSFER_SRC.as_raw()
        | vk::ImageUsageFlags::TRANSFER_DST.as_raw(),
);

fn surface_capabilities() -> vk::SurfaceCapabilitiesKHR {
    vk::SurfaceCapabilitiesKHR {
        min_image_count: 2,
        max_image_count: 3,
        current_extent: vk::Extent2D { width: u32::MAX, height: u32::MAX },
        min_image_extent: vk::Extent2D { width: 1, height: 1 },
        max_image_extent: vk::Extent2D { width: 16384, height: 16384 },
        max_image_array_layers: 1,
        supported_transforms: vk::SurfaceTransformFlagsKHR::IDENTITY,
        current_transform: vk::SurfaceTransformFlagsKHR::IDENTITY,
        supported_composite_alpha: vk::CompositeAlphaFlagsKHR::OPAQUE,
        supported_usage_flags: SURFACE_USAGE,
    }
}

fn valid_surface(state: &reg::VkState, surface: u64) -> bool {
    surface != 0 && state.surfaces.contains_key(&surface)
}

// ---- VK_KHR_surface + VK_KHR_wayland_surface -----------------------------------------------------

#[no_mangle]
pub extern "C" fn vkCreateWaylandSurfaceKHR(
    _instance: VkInstance,
    p_create_info: *const vk::WaylandSurfaceCreateInfoKHR,
    _p_allocator: *const c_void,
    p_surface: *mut u64,
) -> VkResult {
    let (Some(ci), Some(out)) = (unsafe { p_create_info.as_ref() }, unsafe { p_surface.as_mut() })
    else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if ci.display.is_null() || ci.surface.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    let mut s = reg::lock();
    if s.surfaces.values().any(|surface| {
        surface.wl_display == ci.display as usize && surface.wl_surface == ci.surface as usize
    }) {
        return VK_ERROR_NATIVE_WINDOW_IN_USE_KHR;
    }
    let handle = s.alloc_handle();
    s.surfaces.insert(
        handle,
        SurfaceRec {
            wl_display: ci.display as usize,
            wl_surface: ci.surface as usize,
        },
    );
    *out = handle;
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkDestroySurfaceKHR(_instance: VkInstance, surface: u64, _p_allocator: *const c_void) {
    let mut s = reg::lock();
    s.surfaces.remove(&surface);
    for swapchain in s.swapchains.values_mut().filter(|swapchain| swapchain.surface == surface) {
        swapchain.state = SwapchainState::Lost;
    }
}

/// Wayland presentation is always supported on our single queue family.
#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceWaylandPresentationSupportKHR(
    _physical_device: VkPhysicalDevice,
    _queue_family_index: u32,
    _display: *mut c_void,
) -> VkBool32 {
    vk::TRUE
}

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceSurfaceSupportKHR(
    _physical_device: VkPhysicalDevice,
    queue_family_index: u32,
    surface: u64,
    p_supported: *mut VkBool32,
) -> VkResult {
    let Some(out) = (unsafe { p_supported.as_mut() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let s = reg::lock();
    if !valid_surface(&s, surface) {
        return VK_ERROR_SURFACE_LOST_KHR;
    }
    *out = if queue_family_index == 0 { vk::TRUE } else { vk::FALSE };
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceSurfaceCapabilitiesKHR(
    _physical_device: VkPhysicalDevice,
    surface: u64,
    p_caps: *mut vk::SurfaceCapabilitiesKHR,
) -> VkResult {
    let Some(out) = (unsafe { p_caps.as_mut() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if !valid_surface(&reg::lock(), surface) {
        return VK_ERROR_SURFACE_LOST_KHR;
    }
    *out = surface_capabilities();
    VK_SUCCESS
}

/// Report B8G8R8A8_UNORM / SRGB-nonlinear (the format the dd-display dma-buf present expects,
/// `DRM_FORMAT_XRGB8888`).
#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceSurfaceFormatsKHR(
    _physical_device: VkPhysicalDevice,
    surface: u64,
    p_count: *mut u32,
    p_formats: *mut vk::SurfaceFormatKHR,
) -> VkResult {
    if !valid_surface(&reg::lock(), surface) {
        return VK_ERROR_SURFACE_LOST_KHR;
    }
    let formats = [SURFACE_FORMAT];
    unsafe { write_enum(&formats, p_count, p_formats) }
}

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceSurfacePresentModesKHR(
    _physical_device: VkPhysicalDevice,
    surface: u64,
    p_count: *mut u32,
    p_modes: *mut vk::PresentModeKHR,
) -> VkResult {
    if !valid_surface(&reg::lock(), surface) {
        return VK_ERROR_SURFACE_LOST_KHR;
    }
    let modes = [vk::PresentModeKHR::FIFO];
    unsafe { write_enum(&modes, p_count, p_modes) }
}

// ---- VK_KHR_swapchain ----------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn vkCreateSwapchainKHR(
    _device: VkDevice,
    p_create_info: *const vk::SwapchainCreateInfoKHR,
    _p_allocator: *const c_void,
    p_swapchain: *mut u64,
) -> VkResult {
    let (Some(ci), Some(out)) = (unsafe { p_create_info.as_ref() }, unsafe { p_swapchain.as_mut() })
    else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let mut s = reg::lock();
    let surface = ci.surface.as_raw();
    if !valid_surface(&s, surface) {
        return VK_ERROR_SURFACE_LOST_KHR;
    }
    let caps = surface_capabilities();
    let extent_ok = ci.image_extent.width >= caps.min_image_extent.width
        && ci.image_extent.width <= caps.max_image_extent.width
        && ci.image_extent.height >= caps.min_image_extent.height
        && ci.image_extent.height <= caps.max_image_extent.height;
    let count_ok = ci.min_image_count >= caps.min_image_count
        && (caps.max_image_count == 0 || ci.min_image_count <= caps.max_image_count);
    let usage_ok = !ci.image_usage.is_empty() && caps.supported_usage_flags.contains(ci.image_usage);
    let active = s
        .swapchains
        .iter()
        .find_map(|(&handle, swapchain)| {
            (swapchain.surface == surface && swapchain.state == SwapchainState::Active).then_some(handle)
        });
    let old = (!ci.old_swapchain.is_null()).then_some(ci.old_swapchain.as_raw());
    let old_ok = old == active;
    if !count_ok
        || !extent_ok
        || !ci.flags.is_empty()
        || ci.image_format != SURFACE_FORMAT.format
        || ci.image_color_space != SURFACE_FORMAT.color_space
        || ci.image_array_layers != 1
        || !usage_ok
        || ci.image_sharing_mode != vk::SharingMode::EXCLUSIVE
        || ci.queue_family_index_count != 0
        || !ci.p_queue_family_indices.is_null()
        || ci.pre_transform != caps.current_transform
        || ci.composite_alpha != vk::CompositeAlphaFlagsKHR::OPAQUE
        || ci.present_mode != vk::PresentModeKHR::FIFO
        || !old_ok
    {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    let width = ci.image_extent.width;
    let height = ci.image_extent.height;
    let format = tex_format_of(ci.image_format);
    let count = ci.min_image_count as usize;
    let mut images = Vec::with_capacity(count);
    for _ in 0..count {
        // Every presentable image renders into the executor's RESERVED present-target texture id (1):
        // dd-display's Metal executor re-points texture id 1 at the current frame's IOSurface each
        // frame (`set_render_target(1, ...)` keyed by the ExecConn header's surface.id). Using a
        // per-image id instead makes the executor reject it ("unknown/freed texture id"). See
        // dd-display/src/metal_backend.rs run_executor.
        let ir_id = crate::reg::PRESENT_IR_ID;
        let handle = s.alloc_handle();
        // A presentable image is a host-owned render target (its IR texture id is referenced by the
        // app's render pass; the shim emits no CreateTexture — matching the render-target contract).
        s.images.insert(
            handle,
            ImageRec {
                ir_id,
                width,
                height,
                format,
                is_render_target: true,
                mip_levels: 1,
                array_layers: 1,
                aspect_mask: vk::ImageAspectFlags::COLOR.as_raw(),
                usage: SURFACE_USAGE.as_raw(),
                sample_count: vk::SampleCountFlags::TYPE_1.as_raw(),
                subresources: [(
                    (vk::ImageAspectFlags::COLOR.as_raw(), 0, 0),
                    ImageSubresourceState {
                        layout: vk::ImageLayout::UNDEFINED.as_raw(),
                        last_access: 0,
                        last_stage: vk::PipelineStageFlags::TOP_OF_PIPE.as_raw(),
                        owner_queue_family: 0,
                    },
                )]
                .into_iter()
                .collect(),
                bound_mem: None, // presentable images are host-owned; app never vkBindImageMemory's them
            },
        );
        // Mint the rung-2 IOSurface/dma-buf the host renders into (falls back to an unbacked surface
        // off-guest, where /dev/dri/renderD128 is absent — the render path still works via the IR).
        let surface = match transport::renderd::alloc(width, height, DRM_FMT_XRGB8888) {
            Ok(a) => Surface::from_alloc(&a),
            Err(_) => Surface {
                id: ir_id,
                width,
                height,
                stride: width * 4,
                fd: -1,
            },
        };
        images.push(SwapImage { image: handle, ir_id, surface, state: SwapImageState::Available });
    }
    if std::env::var_os("DD_SHIM_DEBUG").is_some() {
        let ids: Vec<u32> = images.iter().map(|i| i.surface.id).collect();
        let fds: Vec<i32> = images.iter().map(|i| i.surface.fd).collect();
        eprintln!("[dd-shim-vk] vkCreateSwapchainKHR: {count} imgs {width}x{height} surf_ids={ids:?} fds={fds:?}");
    }
    let handle = s.alloc_handle();
    s.swapchains.insert(
        handle,
        SwapchainRec {
            surface: ci.surface.as_raw(),
            width,
            height,
            format,
            images,
            next: 0,
            state: SwapchainState::Active,
        },
    );
    if let Some(old) = old {
        s.swapchains.get_mut(&old).expect("validated old swapchain").state = SwapchainState::Retired;
    }
    *out = handle;
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkDestroySwapchainKHR(_device: VkDevice, swapchain: u64, _p_allocator: *const c_void) {
    let mut s = reg::lock();
    if let Some(sc) = s.swapchains.remove(&swapchain) {
        for image in sc.images {
            s.images.remove(&image.image);
        }
    }
}

#[no_mangle]
pub extern "C" fn vkGetSwapchainImagesKHR(
    _device: VkDevice,
    swapchain: u64,
    p_count: *mut u32,
    p_images: *mut u64,
) -> VkResult {
    crate::reg::trace("vkGetSwapchainImagesKHR");
    let s = reg::lock();
    let Some(sc) = s.swapchains.get(&swapchain) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let handles: Vec<u64> = sc.images.iter().map(|i| i.image).collect();
    unsafe { write_enum(&handles, p_count, p_images) }
}

/// Acquire ownership of an available presentable image. The transport completes presentation
/// synchronously, so availability changes only at acquire/present API boundaries.
#[no_mangle]
pub extern "C" fn vkAcquireNextImageKHR(
    _device: VkDevice,
    swapchain: u64,
    timeout: u64,
    semaphore: VkSemaphore,
    fence: VkFence,
    p_image_index: *mut u32,
) -> VkResult {
    crate::reg::trace("vkAcquireNextImageKHR");
    let Some(out) = (unsafe { p_image_index.as_mut() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let started = std::time::Instant::now();
    loop {
        let mut s = reg::lock();
        let Some(sc) = s.swapchains.get(&swapchain) else {
            return VK_ERROR_INITIALIZATION_FAILED;
        };
        match sc.state {
            SwapchainState::Retired => return VK_ERROR_OUT_OF_DATE_KHR,
            SwapchainState::Lost => return VK_ERROR_SURFACE_LOST_KHR,
            SwapchainState::Active => {}
        }
        if semaphore == 0 && fence == 0 {
            return VK_ERROR_INITIALIZATION_FAILED;
        }
        if semaphore != 0 && !s.semaphores.get(&semaphore).is_some_and(|sync| !sync.signaled) {
            return VK_ERROR_INITIALIZATION_FAILED;
        }
        if fence != 0 && !s.fences.get(&fence).is_some_and(|sync| !sync.signaled) {
            return VK_ERROR_INITIALIZATION_FAILED;
        }
        let (next, len) = {
            let sc = &s.swapchains[&swapchain];
            (sc.next, sc.images.len())
        };
        let available = (0..len).map(|offset| (next + offset) % len).find(|&index| {
            s.swapchains[&swapchain].images[index].state == SwapImageState::Available
        });
        if let Some(index) = available {
            let sc = s.swapchains.get_mut(&swapchain).expect("validated swapchain");
            sc.images[index].state = SwapImageState::Acquired;
            sc.next = (index + 1) % len;
            if semaphore != 0 {
                s.semaphores.get_mut(&semaphore).expect("validated semaphore").signaled = true;
            }
            if fence != 0 {
                s.fences.get_mut(&fence).expect("validated fence").signaled = true;
            }
            *out = index as u32;
            return VK_SUCCESS;
        }
        drop(s);
        if timeout == 0 {
            return VK_NOT_READY;
        }
        if timeout != u64::MAX && started.elapsed().as_nanos() >= timeout as u128 {
            return VK_TIMEOUT;
        }
        std::thread::yield_now();
    }
}

/// Present the acquired image: terminate the frame's IR with `Cmd::Present{ surface, texture }` and
/// ship it to the host GPU-exec service (Metal renders into the IOSurface); then the IOSurface dma-buf
/// is committed to the app's `wl_surface` (see the module doc — the live-guest present rendezvous).
#[no_mangle]
pub extern "C" fn vkQueuePresentKHR(_queue: VkQueue, p_present_info: *const vk::PresentInfoKHR) -> VkResult {
    let Some(pi) = (unsafe { p_present_info.as_ref() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if pi.swapchain_count == 0 {
        return VK_SUCCESS;
    }
    if pi.p_swapchains.is_null()
        || pi.p_image_indices.is_null()
        || (pi.wait_semaphore_count != 0 && pi.p_wait_semaphores.is_null())
    {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    let swaps = unsafe { core::slice::from_raw_parts(pi.p_swapchains, pi.swapchain_count as usize) };
    let idxs = unsafe { core::slice::from_raw_parts(pi.p_image_indices, pi.swapchain_count as usize) };
    let waits = if pi.wait_semaphore_count == 0 {
        &[]
    } else {
        unsafe { core::slice::from_raw_parts(pi.p_wait_semaphores, pi.wait_semaphore_count as usize) }
    };

    let write_results = |result: VkResult| {
        if !pi.p_results.is_null() {
            let results = unsafe { core::slice::from_raw_parts_mut(pi.p_results, pi.swapchain_count as usize) };
            results.fill(vk::Result::from_raw(result));
        }
    };

    let mut s = reg::lock();
    // Validate the complete batch before delivery or mutation. A binary semaphore may be consumed
    // only once, and all waits must already be signaled in this synchronous execution model.
    let mut wait_handles = std::collections::HashSet::with_capacity(waits.len());
    for wait in waits {
        let handle = wait.as_raw();
        if !wait_handles.insert(handle) || !s.semaphores.get(&handle).is_some_and(|sem| sem.signaled) {
            write_results(VK_ERROR_INITIALIZATION_FAILED);
            return VK_ERROR_INITIALIZATION_FAILED;
        }
    }
    let mut ownerships = std::collections::HashSet::with_capacity(swaps.len());
    let mut prepared = Vec::with_capacity(swaps.len());
    for (sc_handle, &img_idx) in swaps.iter().zip(idxs.iter()) {
        let sc_handle = sc_handle.as_raw();
        if !ownerships.insert((sc_handle, img_idx)) {
            write_results(VK_ERROR_INITIALIZATION_FAILED);
            return VK_ERROR_INITIALIZATION_FAILED;
        }
        let Some(sc) = s.swapchains.get(&sc_handle) else {
            write_results(VK_ERROR_INITIALIZATION_FAILED);
            return VK_ERROR_INITIALIZATION_FAILED;
        };
        if sc.state == SwapchainState::Lost {
            write_results(VK_ERROR_SURFACE_LOST_KHR);
            return VK_ERROR_SURFACE_LOST_KHR;
        }
        let Some(img) = sc.images.get(img_idx as usize) else {
            write_results(VK_ERROR_INITIALIZATION_FAILED);
            return VK_ERROR_INITIALIZATION_FAILED;
        };
        if img.state != SwapImageState::Acquired {
            write_results(VK_ERROR_INITIALIZATION_FAILED);
            return VK_ERROR_INITIALIZATION_FAILED;
        }
        let Some(surface_rec) = s.surfaces.get(&sc.surface) else {
            write_results(VK_ERROR_SURFACE_LOST_KHR);
            return VK_ERROR_SURFACE_LOST_KHR;
        };
        prepared.push((sc_handle, img_idx as usize, img.surface, surface_rec.wl_display, surface_rec.wl_surface));
    }

    let frame_end = s.ir_log.len();
    let bytes = encode_stream(&s.ir_log[s.present_flushed..frame_end]);
    for &(_, _, surface, display, wl_surface) in &prepared {
        let result = deliver_present(&mut s, &surface, &bytes, display, wl_surface);
        if result != VK_SUCCESS {
            write_results(result);
            return result;
        }
    }

    // Transactional commit: only successful delivery consumes waits, advances the IR cursor and
    // returns image ownership to the presentation engine.
    for wait in wait_handles {
        s.semaphores.get_mut(&wait).expect("validated wait semaphore").signaled = false;
    }
    for &(swapchain, index, _, _, _) in &prepared {
        let image = &mut s.swapchains.get_mut(&swapchain).expect("validated swapchain").images[index];
        image.state = SwapImageState::Presenting;
        image.state = SwapImageState::Available;
    }
    s.present_flushed = frame_end;
    write_results(VK_SUCCESS);
    VK_SUCCESS
}

fn deliver_present(
    state: &mut reg::VkState,
    surface: &Surface,
    bytes: &[u8],
    display: usize,
    wl_surface: usize,
) -> VkResult {
    #[cfg(test)]
    {
        let _ = (state, surface, bytes, display, wl_surface);
        match TEST_DELIVERY.load(std::sync::atomic::Ordering::SeqCst) {
            1 => return VK_ERROR_DEVICE_LOST,
            2 => return VK_ERROR_SURFACE_LOST_KHR,
            _ => return VK_SUCCESS,
        }
    }

    #[cfg(not(test))]
    {
        let conn = state.exec.get_or_insert_with(transport::ExecConn::from_env);
        if conn.submit(surface, bytes).is_err() {
            return VK_ERROR_DEVICE_LOST;
        }
        if std::env::var_os("DD_VK_NO_WL_PRESENT").is_none()
            && !crate::wl_present::present(display, wl_surface, surface)
        {
            return VK_ERROR_SURFACE_LOST_KHR;
        }
        VK_SUCCESS
    }
}

#[cfg(test)]
static TEST_DELIVERY: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// The two-call enumeration idiom (shared with instance.rs's copy but local to avoid a cross-module
/// pub); writes up to `*pCount` items, returns `VK_INCOMPLETE` if truncated.
unsafe fn write_enum<T: Copy>(items: &[T], p_count: *mut u32, p_data: *mut T) -> VkResult {
    let Some(count) = p_count.as_mut() else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if p_data.is_null() {
        *count = items.len() as u32;
        return VK_SUCCESS;
    }
    let n = (*count as usize).min(items.len());
    core::ptr::copy_nonoverlapping(items.as_ptr(), p_data, n);
    *count = n as u32;
    if n < items.len() {
        VK_INCOMPLETE
    } else {
        VK_SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static PRESENT_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn create_surface(display: usize, window: usize) -> u64 {
        let ci = vk::WaylandSurfaceCreateInfoKHR::default()
            .display(display as *mut c_void)
            .surface(window as *mut c_void);
        let mut surface = 0;
        assert_eq!(
            vkCreateWaylandSurfaceKHR(core::ptr::null_mut(), &ci, core::ptr::null(), &mut surface),
            VK_SUCCESS
        );
        surface
    }

    fn valid_swapchain_ci(surface: u64) -> vk::SwapchainCreateInfoKHR<'static> {
        vk::SwapchainCreateInfoKHR::default()
            .surface(vk::SurfaceKHR::from_raw(surface))
            .min_image_count(2)
            .image_format(SURFACE_FORMAT.format)
            .image_color_space(SURFACE_FORMAT.color_space)
            .image_extent(vk::Extent2D { width: 640, height: 480 })
            .image_array_layers(1)
            .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
            .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
            .pre_transform(vk::SurfaceTransformFlagsKHR::IDENTITY)
            .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
            .present_mode(vk::PresentModeKHR::FIFO)
    }

    fn create_swapchain(ci: &vk::SwapchainCreateInfoKHR<'_>) -> u64 {
        let mut swapchain = 0;
        assert_eq!(
            vkCreateSwapchainKHR(core::ptr::null_mut(), ci, core::ptr::null(), &mut swapchain),
            VK_SUCCESS
        );
        swapchain
    }

    fn create_fence() -> VkFence {
        let ci = vk::FenceCreateInfo::default();
        let mut fence = 0;
        assert_eq!(
            crate::command::vkCreateFence(core::ptr::null_mut(), &ci, core::ptr::null(), &mut fence),
            VK_SUCCESS
        );
        fence
    }

    fn create_semaphore() -> VkSemaphore {
        let mut semaphore = 0;
        assert_eq!(
            crate::command::vkCreateSemaphore(
                core::ptr::null_mut(),
                core::ptr::null(),
                core::ptr::null(),
                &mut semaphore,
            ),
            VK_SUCCESS
        );
        semaphore
    }

    fn present(swapchain: u64, index: u32) -> VkResult {
        let swapchains = [vk::SwapchainKHR::from_raw(swapchain)];
        let indices = [index];
        let info = vk::PresentInfoKHR::default().swapchains(&swapchains).image_indices(&indices);
        vkQueuePresentKHR(core::ptr::null_mut(), &info)
    }

    #[test]
    fn wsi_validates_surface_handles_and_swapchain_create_info_atomically() {
        let surface = create_surface(0x1110, 0x2220);

        // The same native Wayland window cannot be wrapped twice, and a failed creation preserves
        // the caller's output just like every other negative path in this test.
        let duplicate = vk::WaylandSurfaceCreateInfoKHR::default()
            .display(0x1110usize as *mut c_void)
            .surface(0x2220usize as *mut c_void);
        let mut duplicate_out = 0xfeed_u64;
        assert_eq!(
            vkCreateWaylandSurfaceKHR(
                core::ptr::null_mut(),
                &duplicate,
                core::ptr::null(),
                &mut duplicate_out,
            ),
            VK_ERROR_NATIVE_WINDOW_IN_USE_KHR
        );
        assert_eq!(duplicate_out, 0xfeed);

        let mut supported = 99;
        assert_eq!(
            vkGetPhysicalDeviceSurfaceSupportKHR(core::ptr::null_mut(), 1, surface, &mut supported),
            VK_SUCCESS
        );
        assert_eq!(supported, vk::FALSE, "only queue family zero is advertised");

        let reject = |ci: &vk::SwapchainCreateInfoKHR<'_>| {
            let initial_swapchains = reg::lock().swapchains.len();
            let mut out = 0xfeed_u64;
            assert_eq!(
                vkCreateSwapchainKHR(core::ptr::null_mut(), ci, core::ptr::null(), &mut out),
                VK_ERROR_INITIALIZATION_FAILED
            );
            assert_eq!(out, 0xfeed, "failure must preserve pSwapchain");
            assert_eq!(reg::lock().swapchains.len(), initial_swapchains, "failure must not allocate");
        };

        let base = valid_swapchain_ci(surface);
        reject(&vk::SwapchainCreateInfoKHR { min_image_count: 1, ..base });
        reject(&vk::SwapchainCreateInfoKHR { min_image_count: 4, ..base });
        reject(&vk::SwapchainCreateInfoKHR {
            image_extent: vk::Extent2D { width: 0, height: 480 },
            ..base
        });
        reject(&vk::SwapchainCreateInfoKHR {
            image_format: vk::Format::R8G8B8A8_UNORM,
            ..base
        });
        reject(&vk::SwapchainCreateInfoKHR {
            image_color_space: vk::ColorSpaceKHR::DISPLAY_P3_NONLINEAR_EXT,
            ..base
        });
        reject(&vk::SwapchainCreateInfoKHR { image_array_layers: 2, ..base });
        reject(&vk::SwapchainCreateInfoKHR { image_usage: vk::ImageUsageFlags::STORAGE, ..base });
        reject(&vk::SwapchainCreateInfoKHR {
            image_sharing_mode: vk::SharingMode::CONCURRENT,
            ..base
        });
        reject(&vk::SwapchainCreateInfoKHR {
            pre_transform: vk::SurfaceTransformFlagsKHR::ROTATE_90,
            ..base
        });
        reject(&vk::SwapchainCreateInfoKHR {
            composite_alpha: vk::CompositeAlphaFlagsKHR::PRE_MULTIPLIED,
            ..base
        });
        reject(&vk::SwapchainCreateInfoKHR { present_mode: vk::PresentModeKHR::IMMEDIATE, ..base });

        let mut swapchain = 0;
        assert_eq!(
            vkCreateSwapchainKHR(core::ptr::null_mut(), &base, core::ptr::null(), &mut swapchain),
            VK_SUCCESS
        );
        assert_ne!(swapchain, 0);
        let image_handles = reg::lock().swapchains[&swapchain]
            .images
            .iter()
            .map(|image| image.image)
            .collect::<Vec<_>>();

        // oldSwapchain is accepted only when it is a live swapchain for this surface.
        let other_surface = create_surface(0x3330, 0x4440);
        let wrong_old = vk::SwapchainCreateInfoKHR {
            old_swapchain: vk::SwapchainKHR::from_raw(swapchain),
            ..valid_swapchain_ci(other_surface)
        };
        reject(&wrong_old);

        vkDestroySwapchainKHR(core::ptr::null_mut(), swapchain, core::ptr::null());
        let state = reg::lock();
        assert!(image_handles.iter().all(|image| !state.images.contains_key(image)));
        drop(state);

        vkDestroySurfaceKHR(core::ptr::null_mut(), surface, core::ptr::null());
        let mut caps = vk::SurfaceCapabilitiesKHR { min_image_count: 77, ..Default::default() };
        assert_eq!(
            vkGetPhysicalDeviceSurfaceCapabilitiesKHR(core::ptr::null_mut(), surface, &mut caps),
            VK_ERROR_SURFACE_LOST_KHR
        );
        assert_eq!(caps.min_image_count, 77, "stale-handle query must preserve output");
        let mut count = 91;
        assert_eq!(
            vkGetPhysicalDeviceSurfaceFormatsKHR(core::ptr::null_mut(), surface, &mut count, core::ptr::null_mut()),
            VK_ERROR_SURFACE_LOST_KHR
        );
        assert_eq!(count, 91, "stale-handle enumeration must preserve count");
        let mut stale_out = 0xface_u64;
        assert_eq!(
            vkCreateSwapchainKHR(core::ptr::null_mut(), &base, core::ptr::null(), &mut stale_out),
            VK_ERROR_SURFACE_LOST_KHR
        );
        assert_eq!(stale_out, 0xface);
        vkDestroySurfaceKHR(core::ptr::null_mut(), other_surface, core::ptr::null());
    }

    #[test]
    fn swapchain_tracks_image_ownership_timeouts_and_retirement() {
        let _present_guard = PRESENT_TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        TEST_DELIVERY.store(0, std::sync::atomic::Ordering::SeqCst);
        let surface = create_surface(0x5510, 0x6620);
        let swapchain = create_swapchain(&valid_swapchain_ci(surface));
        let fence0 = create_fence();
        let fence1 = create_fence();

        let mut image0 = u32::MAX;
        assert_eq!(
            vkAcquireNextImageKHR(core::ptr::null_mut(), swapchain, 0, 0, fence0, &mut image0),
            VK_SUCCESS
        );
        assert_eq!(crate::command::vkGetFenceStatus(core::ptr::null_mut(), fence0), VK_SUCCESS);
        let mut image1 = u32::MAX;
        assert_eq!(
            vkAcquireNextImageKHR(core::ptr::null_mut(), swapchain, 0, 0, fence1, &mut image1),
            VK_SUCCESS
        );
        assert_ne!(image0, image1, "an acquired image cannot be acquired twice");

        let timeout_fence = create_fence();
        let mut unavailable = 0xdead_beef;
        assert_eq!(
            vkAcquireNextImageKHR(core::ptr::null_mut(), swapchain, 0, 0, timeout_fence, &mut unavailable),
            VK_NOT_READY
        );
        assert_eq!(unavailable, 0xdead_beef, "NOT_READY must preserve pImageIndex");
        assert_eq!(crate::command::vkGetFenceStatus(core::ptr::null_mut(), timeout_fence), VK_NOT_READY);
        assert_eq!(
            vkAcquireNextImageKHR(core::ptr::null_mut(), swapchain, 1, 0, timeout_fence, &mut unavailable),
            VK_TIMEOUT
        );
        assert_eq!(unavailable, 0xdead_beef, "TIMEOUT must preserve pImageIndex");
        assert_eq!(crate::command::vkGetFenceStatus(core::ptr::null_mut(), timeout_fence), VK_NOT_READY);

        let duplicate_swaps = [
            vk::SwapchainKHR::from_raw(swapchain),
            vk::SwapchainKHR::from_raw(swapchain),
        ];
        let duplicate_indices = [image0, image0];
        let duplicate_present =
            vk::PresentInfoKHR::default().swapchains(&duplicate_swaps).image_indices(&duplicate_indices);
        assert_eq!(
            vkQueuePresentKHR(core::ptr::null_mut(), &duplicate_present),
            VK_ERROR_INITIALIZATION_FAILED
        );
        assert_eq!(present(swapchain, image0), VK_SUCCESS, "rejected batch must preserve ownership");
        assert_eq!(
            present(swapchain, image0),
            VK_ERROR_INITIALIZATION_FAILED,
            "presentation requires ownership from a successful acquire"
        );

        // Replacing an active swapchain retires it for acquisition but preserves ownership of an
        // image acquired before retirement so that image can complete presentation.
        let replacement_ci = vk::SwapchainCreateInfoKHR {
            old_swapchain: vk::SwapchainKHR::from_raw(swapchain),
            ..valid_swapchain_ci(surface)
        };
        let replacement = create_swapchain(&replacement_ci);
        let mut retired_out = 0xcafe_babe;
        assert_eq!(
            vkAcquireNextImageKHR(core::ptr::null_mut(), swapchain, 0, 0, timeout_fence, &mut retired_out),
            VK_ERROR_OUT_OF_DATE_KHR
        );
        assert_eq!(retired_out, 0xcafe_babe);
        assert_eq!(present(swapchain, image1), VK_SUCCESS);

        let replacement_fence = create_fence();
        let acquire_semaphore = create_semaphore();
        let mut replacement_image = u32::MAX;
        assert_eq!(
            vkAcquireNextImageKHR(
                core::ptr::null_mut(),
                replacement,
                0,
                acquire_semaphore,
                replacement_fence,
                &mut replacement_image,
            ),
            VK_SUCCESS
        );
        let semaphore_handle = vk::Semaphore::from_raw(acquire_semaphore);
        let submit = vk::SubmitInfo {
            wait_semaphore_count: 1,
            p_wait_semaphores: &semaphore_handle,
            ..Default::default()
        };
        assert_eq!(
            crate::command::vkQueueSubmit(core::ptr::null_mut(), 1, &submit, 0),
            VK_SUCCESS,
            "successful acquire must signal its binary semaphore"
        );
        assert_eq!(
            crate::command::vkQueueSubmit(core::ptr::null_mut(), 1, &submit, 0),
            VK_ERROR_INITIALIZATION_FAILED,
            "a binary acquire semaphore is consumed by one wait"
        );
        vkDestroySurfaceKHR(core::ptr::null_mut(), surface, core::ptr::null());
        let mut lost_out = 0x1234_5678;
        assert_eq!(
            vkAcquireNextImageKHR(core::ptr::null_mut(), replacement, 0, 0, timeout_fence, &mut lost_out),
            VK_ERROR_SURFACE_LOST_KHR
        );
        assert_eq!(lost_out, 0x1234_5678);

        vkDestroySwapchainKHR(core::ptr::null_mut(), swapchain, core::ptr::null());
        vkDestroySwapchainKHR(core::ptr::null_mut(), replacement, core::ptr::null());
        for fence in [fence0, fence1, timeout_fence, replacement_fence] {
            crate::command::vkDestroyFence(core::ptr::null_mut(), fence, core::ptr::null());
        }
        crate::command::vkDestroySemaphore(core::ptr::null_mut(), acquire_semaphore, core::ptr::null());
    }

    #[test]
    fn present_failures_preserve_ir_ownership_and_waits_until_transactional_commit() {
        let _present_guard = PRESENT_TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let surface = create_surface(0x7710, 0x8820);
        let swapchain = create_swapchain(&valid_swapchain_ci(surface));
        let acquire = create_semaphore();
        let mut image = u32::MAX;
        assert_eq!(
            vkAcquireNextImageKHR(core::ptr::null_mut(), swapchain, 0, acquire, 0, &mut image),
            VK_SUCCESS
        );
        reg::lock().record(dd_shim_common::ir::Cmd::DestroyBuffer(0x1234));
        let initial_cursor = reg::lock().present_flushed;

        let swaps = [vk::SwapchainKHR::from_raw(swapchain)];
        let indices = [image];
        let waits = [vk::Semaphore::from_raw(acquire)];
        let mut results = [vk::Result::SUCCESS];
        let info = vk::PresentInfoKHR {
            wait_semaphore_count: waits.len() as u32,
            p_wait_semaphores: waits.as_ptr(),
            swapchain_count: swaps.len() as u32,
            p_swapchains: swaps.as_ptr(),
            p_image_indices: indices.as_ptr(),
            p_results: results.as_mut_ptr(),
            ..Default::default()
        };

        TEST_DELIVERY.store(1, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(vkQueuePresentKHR(core::ptr::null_mut(), &info), VK_ERROR_DEVICE_LOST);
        assert_eq!(results[0].as_raw(), VK_ERROR_DEVICE_LOST);
        assert_eq!(reg::lock().present_flushed, initial_cursor, "transport failure must preserve IR");

        // The exact same acquired image and binary wait can be retried: both ownership and the
        // semaphore remained untouched by the failed delivery.
        TEST_DELIVERY.store(0, std::sync::atomic::Ordering::SeqCst);
        results[0] = vk::Result::ERROR_UNKNOWN;
        assert_eq!(vkQueuePresentKHR(core::ptr::null_mut(), &info), VK_SUCCESS);
        assert_eq!(results[0], vk::Result::SUCCESS);
        let state = reg::lock();
        assert_eq!(state.present_flushed, state.ir_log.len());
        drop(state);

        let surface_wait = create_semaphore();
        let mut next_image = u32::MAX;
        assert_eq!(
            vkAcquireNextImageKHR(core::ptr::null_mut(), swapchain, 0, surface_wait, 0, &mut next_image),
            VK_SUCCESS
        );
        let next_indices = [next_image];
        let next_waits = [vk::Semaphore::from_raw(surface_wait)];
        let mut next_results = [vk::Result::SUCCESS];
        let next_info = vk::PresentInfoKHR {
            wait_semaphore_count: next_waits.len() as u32,
            p_wait_semaphores: next_waits.as_ptr(),
            swapchain_count: swaps.len() as u32,
            p_swapchains: swaps.as_ptr(),
            p_image_indices: next_indices.as_ptr(),
            p_results: next_results.as_mut_ptr(),
            ..Default::default()
        };
        TEST_DELIVERY.store(2, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(vkQueuePresentKHR(core::ptr::null_mut(), &next_info), VK_ERROR_SURFACE_LOST_KHR);
        assert_eq!(next_results[0].as_raw(), VK_ERROR_SURFACE_LOST_KHR);
        TEST_DELIVERY.store(0, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(vkQueuePresentKHR(core::ptr::null_mut(), &next_info), VK_SUCCESS);

        // A bad wait rejects the whole batch and reports the same per-swapchain result without
        // consuming ownership. The image remains presentable with its valid acquire semaphore.
        let final_wait = create_semaphore();
        let mut final_image = u32::MAX;
        assert_eq!(
            vkAcquireNextImageKHR(core::ptr::null_mut(), swapchain, 0, final_wait, 0, &mut final_image),
            VK_SUCCESS
        );
        let final_indices = [final_image];
        let bad_waits = [vk::Semaphore::from_raw(0xdead_beef)];
        let mut final_results = [vk::Result::SUCCESS];
        let bad_info = vk::PresentInfoKHR {
            wait_semaphore_count: bad_waits.len() as u32,
            p_wait_semaphores: bad_waits.as_ptr(),
            swapchain_count: swaps.len() as u32,
            p_swapchains: swaps.as_ptr(),
            p_image_indices: final_indices.as_ptr(),
            p_results: final_results.as_mut_ptr(),
            ..Default::default()
        };
        assert_eq!(vkQueuePresentKHR(core::ptr::null_mut(), &bad_info), VK_ERROR_INITIALIZATION_FAILED);
        assert_eq!(final_results[0].as_raw(), VK_ERROR_INITIALIZATION_FAILED);
        let good_waits = [vk::Semaphore::from_raw(final_wait)];
        let good_info = vk::PresentInfoKHR {
            wait_semaphore_count: good_waits.len() as u32,
            p_wait_semaphores: good_waits.as_ptr(),
            swapchain_count: swaps.len() as u32,
            p_swapchains: swaps.as_ptr(),
            p_image_indices: final_indices.as_ptr(),
            p_results: final_results.as_mut_ptr(),
            ..Default::default()
        };
        assert_eq!(vkQueuePresentKHR(core::ptr::null_mut(), &good_info), VK_SUCCESS);

        TEST_DELIVERY.store(0, std::sync::atomic::Ordering::SeqCst);
        vkDestroySwapchainKHR(core::ptr::null_mut(), swapchain, core::ptr::null());
        vkDestroySurfaceKHR(core::ptr::null_mut(), surface, core::ptr::null());
        for semaphore in [acquire, surface_wait, final_wait] {
            crate::command::vkDestroySemaphore(core::ptr::null_mut(), semaphore, core::ptr::null());
        }
    }
}
