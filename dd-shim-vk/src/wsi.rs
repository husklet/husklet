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

use crate::reg::{self, ImageRec, SurfaceRec, SwapImage, SwapchainRec};
use crate::types::*;
use ash::vk;
use ash::vk::Handle;
use core::ffi::c_void;
use dd_shim_common::ir::{Cmd, encode_stream};
use dd_shim_common::transport::{self, Surface, DRM_FMT_XRGB8888};

fn tex_format_of(f: vk::Format) -> dd_shim_common::ir::TextureFormat {
    crate::memory::tex_format(f)
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
    let mut s = reg::lock();
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
    reg::lock().surfaces.remove(&surface);
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
    _queue_family_index: u32,
    _surface: u64,
    p_supported: *mut VkBool32,
) -> VkResult {
    if let Some(out) = unsafe { p_supported.as_mut() } {
        *out = vk::TRUE;
    }
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceSurfaceCapabilitiesKHR(
    _physical_device: VkPhysicalDevice,
    _surface: u64,
    p_caps: *mut vk::SurfaceCapabilitiesKHR,
) -> VkResult {
    if let Some(out) = unsafe { p_caps.as_mut() } {
        *out = vk::SurfaceCapabilitiesKHR {
            min_image_count: 2,
            max_image_count: 3,
            // 0xFFFFFFFF == "extent is defined by the swapchain" (the app picks it) — matches the
            // undefined-current-extent case MoltenVK reports for an unsized layer.
            current_extent: vk::Extent2D { width: 0xFFFF_FFFF, height: 0xFFFF_FFFF },
            min_image_extent: vk::Extent2D { width: 1, height: 1 },
            max_image_extent: vk::Extent2D { width: 16384, height: 16384 },
            max_image_array_layers: 1,
            supported_transforms: vk::SurfaceTransformFlagsKHR::IDENTITY,
            current_transform: vk::SurfaceTransformFlagsKHR::IDENTITY,
            supported_composite_alpha: vk::CompositeAlphaFlagsKHR::OPAQUE,
            supported_usage_flags: vk::ImageUsageFlags::COLOR_ATTACHMENT
                | vk::ImageUsageFlags::TRANSFER_SRC
                | vk::ImageUsageFlags::TRANSFER_DST,
        };
    }
    VK_SUCCESS
}

/// Report B8G8R8A8_UNORM / SRGB-nonlinear (the format the dd-display dma-buf present expects,
/// `DRM_FORMAT_XRGB8888`).
#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceSurfaceFormatsKHR(
    _physical_device: VkPhysicalDevice,
    _surface: u64,
    p_count: *mut u32,
    p_formats: *mut vk::SurfaceFormatKHR,
) -> VkResult {
    let formats = [vk::SurfaceFormatKHR {
        format: vk::Format::B8G8R8A8_UNORM,
        color_space: vk::ColorSpaceKHR::SRGB_NONLINEAR,
    }];
    unsafe { write_enum(&formats, p_count, p_formats) }
}

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceSurfacePresentModesKHR(
    _physical_device: VkPhysicalDevice,
    _surface: u64,
    p_count: *mut u32,
    p_modes: *mut vk::PresentModeKHR,
) -> VkResult {
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
    let width = ci.image_extent.width.max(1);
    let height = ci.image_extent.height.max(1);
    let format = tex_format_of(ci.image_format);
    let count = ci.min_image_count.max(2) as usize;

    let mut s = reg::lock();
    let mut images = Vec::with_capacity(count);
    for _ in 0..count {
        let ir_id = s.alloc_ir();
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
        images.push(SwapImage { image: handle, ir_id, surface });
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
        },
    );
    *out = handle;
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkDestroySwapchainKHR(_device: VkDevice, swapchain: u64, _p_allocator: *const c_void) {
    reg::lock().swapchains.remove(&swapchain);
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

/// Round-robin the next presentable image. Present is synchronous downstream, so the acquire
/// semaphore/fence are treated as already signaled (returned by the bring-up sync stubs).
#[no_mangle]
pub extern "C" fn vkAcquireNextImageKHR(
    _device: VkDevice,
    swapchain: u64,
    _timeout: u64,
    _semaphore: VkSemaphore,
    _fence: VkFence,
    p_image_index: *mut u32,
) -> VkResult {
    crate::reg::trace("vkAcquireNextImageKHR");
    let mut s = reg::lock();
    let Some(sc) = s.swapchains.get_mut(&swapchain) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let idx = sc.next % sc.images.len().max(1);
    sc.next = idx + 1;
    if let Some(out) = unsafe { p_image_index.as_mut() } {
        *out = idx as u32;
    }
    VK_SUCCESS
}

/// Present the acquired image: terminate the frame's IR with `Cmd::Present{ surface, texture }` and
/// ship it to the host GPU-exec service (Metal renders into the IOSurface); then the IOSurface dma-buf
/// is committed to the app's `wl_surface` (see the module doc — the live-guest present rendezvous).
#[no_mangle]
pub extern "C" fn vkQueuePresentKHR(_queue: VkQueue, p_present_info: *const vk::PresentInfoKHR) -> VkResult {
    let Some(pi) = (unsafe { p_present_info.as_ref() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if pi.p_swapchains.is_null() || pi.p_image_indices.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    let swaps = unsafe { core::slice::from_raw_parts(pi.p_swapchains, pi.swapchain_count as usize) };
    let idxs = unsafe { core::slice::from_raw_parts(pi.p_image_indices, pi.swapchain_count as usize) };

    let mut s = reg::lock();
    for (sc_handle, &img_idx) in swaps.iter().zip(idxs.iter()) {
        let Some(sc) = s.swapchains.get(&sc_handle.as_raw()) else { continue };
        let Some(img) = sc.images.get(img_idx as usize) else { continue };
        let (surface, tex, sc_surface) = (img.surface, img.ir_id, sc.surface);
        // Terminate this frame with a Present mapping the rendered swapchain texture to the surface.
        s.record(Cmd::Present {
            surface: surface.id,
            texture: tex,
        });
        // Ship the frame's IR (everything recorded since the last present) to the host executor, which
        // renders it on Metal INTO the surface.id IOSurface.
        let from = s.present_flushed;
        let frame = s.ir_log[from..].to_vec();
        s.present_flushed = s.ir_log.len();
        if std::env::var_os("DD_GPU_EXEC").is_some() {
            let bytes = encode_stream(&frame);
            let conn = s.exec.get_or_insert_with(transport::ExecConn::from_env);
            if let Err(e) = conn.submit(&surface, &bytes) {
                if std::env::var_os("DD_SHIM_DEBUG").is_some() {
                    eprintln!("[dd-shim-vk] vkQueuePresentKHR: exec submit failed: {e}");
                }
            }
        }
        let dbg = std::env::var_os("DD_SHIM_DEBUG").is_some();
        if dbg {
            eprintln!("[dd-shim-vk] vkQueuePresentKHR: surface.id={} tex={} frame_ir={} cmds", surface.id, tex, frame.len());
        }
        // App-side rendezvous: commit the host-rendered IOSurface dma-buf to the app's own wl_surface
        // (dd-display keys the IOSurface off modifier_hi=0x6464 / modifier_lo=surface.id). This is what
        // lands the frame on vkcube's window. Off-guest (no wl display / dma-buf fd) it no-ops.
        // DD_VK_NO_WL_PRESENT disables it (diagnostic isolation of the wayland-marshal path).
        if std::env::var_os("DD_VK_NO_WL_PRESENT").is_none() {
            if let Some(rec) = s.surfaces.get(&sc_surface) {
                let committed = crate::wl_present::present(rec.wl_display, rec.wl_surface, &surface);
                if dbg {
                    eprintln!("[dd-shim-vk] vkQueuePresentKHR: wayland commit -> {committed}");
                }
            }
        }
    }
    VK_SUCCESS
}

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
