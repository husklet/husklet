use super::*;

// ==================================================================================================
// WSI: swapchain + present
// ==================================================================================================

#[no_mangle]
pub extern "C" fn vkCreateSwapchainKHR(
    _device: *mut c_void,
    p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_swapchain: *mut u64,
) -> VkResult {
    let Some(ci) = (unsafe { (p_create_info as *const VkSwapchainCreateInfoKHR).as_ref() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if !p_swapchain.is_null() {
        unsafe { *p_swapchain = 0 };
    }
    // Bring-up: materialize the presentation surface from the swapchain extent/format (hlp surface 0),
    // then register the swapchain's presentable images against it. On success, carry the app's wayland
    // window (captured at `vkCreateWaylandSurfaceKHR` under `ci.surface`) onto the swapchain so a present
    // can marshal the readback onto the app's own `wl_surface`.
    StateStore::with(|s| {
        let sink = &mut s.sink;
        let Some(dev) = s.device.as_mut() else {
            return VK_ERROR_INITIALIZATION_FAILED;
        };
        let r = (|| {
            let surface = Swapchain::create_surface(dev, sink, ci)?;
            present::create_swapchain(dev, sink, surface, ci.min_image_count)
        })();
        match r {
            Ok(h) => {
                if let Some(win) = s.wayland_surfaces.get(&ci.surface).copied() {
                    s.swapchain_windows.insert(h, win);
                }
                if !p_swapchain.is_null() {
                    unsafe { *p_swapchain = h };
                }
                VK_SUCCESS
            }
            Err(e) => Status::from_error(&e),
        }
    })
}

/// Create the GPU surface a swapchain presents through (extent/format from the swapchain create info).
struct Swapchain;
impl Swapchain {
    fn create_surface(
        dev: &mut Device,
        sink: &mut dyn CommandSink,
        ci: &VkSwapchainCreateInfoKHR,
    ) -> hl_gpu::Result<u64> {
        present::create_surface(
            dev,
            sink,
            ci.image_extent.width,
            ci.image_extent.height,
            ci.image_format as u32,
            0,
        )
    }
}

#[no_mangle]
pub extern "C" fn vkDestroySwapchainKHR(
    _device: *mut c_void,
    swapchain: u64,
    _p_allocator: *const c_void,
) {
    StateStore::with(|s| {
        let sink = &mut s.sink;
        if let Some(dev) = s.device.as_mut() {
            // Retire the swapchain AND its presentable images + presentation surface (dropping their
            // `dev.images`/`dev.surfaces` bookkeeping + freeing the host textures/surface). Removing only the
            // `SwapchainRec` would orphan the images in `dev.images` forever — a per-resize handle leak.
            let _ = dev.destroy_swapchain(sink, swapchain);
        }
        // Tear down the app-surface presenter + its window binding (drops the private queue wrappers +
        // the bound `wl_shm`, releasing the app's connection).
        s.swapchain_windows.remove(&swapchain);
        s.presenters.remove(&swapchain);
    });
}

#[no_mangle]
pub extern "C" fn vkGetSwapchainImagesKHR(
    _device: *mut c_void,
    swapchain: u64,
    p_swapchain_image_count: *mut u32,
    p_swapchain_images: *mut u64,
) -> VkResult {
    if p_swapchain_image_count.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    ShimState::with_device(|dev| {
        // The swapchain's presentable images (real render-target textures + their VkImage handles) were
        // created with the swapchain; return the SAME handles here (identical on every call).
        let Ok(handles) = dev.swapchain_images(swapchain) else {
            return VK_ERROR_INITIALIZATION_FAILED;
        };
        let count = handles.len() as u32;
        if p_swapchain_images.is_null() {
            unsafe { *p_swapchain_image_count = count };
            return VK_SUCCESS;
        }
        let cap = unsafe { *p_swapchain_image_count };
        let n = cap.min(count);
        let out = unsafe { std::slice::from_raw_parts_mut(p_swapchain_images, n as usize) };
        for (slot, &handle) in out.iter_mut().zip(handles.iter()) {
            *slot = handle;
        }
        unsafe { *p_swapchain_image_count = n };
        if n < count {
            VK_INCOMPLETE
        } else {
            VK_SUCCESS
        }
    })
    .unwrap_or(VK_ERROR_INITIALIZATION_FAILED)
}

#[no_mangle]
pub extern "C" fn vkAcquireNextImageKHR(
    _device: *mut c_void,
    swapchain: u64,
    _timeout: u64,
    _semaphore: u64,
    _fence: u64,
    p_image_index: *mut u32,
) -> VkResult {
    ShimState::with_device(|dev| match dev.acquire_next_image(swapchain) {
        Ok(idx) => {
            if !p_image_index.is_null() {
                unsafe { *p_image_index = idx };
            }
            VK_SUCCESS
        }
        Err(e) => {
            hl_log::hl_warn!(
                hl_log::tag::SHIM,
                "vkAcquireNextImageKHR sc={swapchain:#x} -> {:?}",
                Status::from_error(&e)
            );
            Status::from_error(&e)
        }
    })
    .unwrap_or(VK_ERROR_INITIALIZATION_FAILED)
}

#[no_mangle]
pub extern "C" fn vkQueuePresentKHR(
    _queue: *mut c_void,
    p_present_info: *const c_void,
) -> VkResult {
    let Some(pi) = (unsafe { (p_present_info as *const VkPresentInfoKHR).as_ref() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if pi.p_swapchains.is_null() || pi.p_image_indices.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    let swapchains =
        unsafe { std::slice::from_raw_parts(pi.p_swapchains, pi.swapchain_count as usize) };
    let indices =
        unsafe { std::slice::from_raw_parts(pi.p_image_indices, pi.swapchain_count as usize) };
    StateStore::with(|s| {
        let sink = &mut s.sink;
        let Some(dev) = s.device.as_mut() else {
            return VK_ERROR_INITIALIZATION_FAILED;
        };
        let mut res = VK_SUCCESS;
        for (&sc, &idx) in swapchains.iter().zip(indices) {
            // 1) The present lowering (`Cmd::Present` names the surface + presented image).
            if let Err(e) = present::queue_present(dev, sink, sc, idx) {
                res = Status::from_error(&e);
                continue;
            }
            // 2) Read the presented image back + convert to the XRGB plane a `wl_shm` buffer wants.
            let plane = match present::read_presented_xrgb(dev, sink, sc, idx) {
                Ok(p) => p,
                Err(e) => {
                    res = Status::from_error(&e);
                    continue;
                }
            };
            // 3) Marshal that plane onto the app's OWN `wl_surface` (soft-unavailable ⇒ readback-only,
            //    still VK_SUCCESS; a hard marshal/flush failure ⇒ VK_ERROR_OUT_OF_DATE/SURFACE_LOST).
            let vk = Presentation::frame_to_app_surface(
                &mut s.presenters,
                &s.swapchain_windows,
                sc,
                plane,
            );
            if vk != VK_SUCCESS {
                hl_log::hl_warn!(hl_log::tag::PRESENT, "commit failed sc={sc:#x} -> {:?}", vk);
                res = vk;
            }
        }
        res
    })
}

/// Marshal one presented frame's XRGB plane onto the app's OWN `wl_surface` via a cached
/// [`WaylandAppPresenter`]. If the swapchain has no captured wayland window (a headless/offscreen or
/// non-wayland surface), the readback already ran and the on-surface attach is skipped — `VK_SUCCESS`. A
/// *soft* bring-up error (libwayland/global absent) caches `None` (so it is not re-probed each frame) and
/// is likewise `VK_SUCCESS`. A *hard* per-frame marshal/flush/size failure maps to
/// `VK_ERROR_OUT_OF_DATE_KHR` / `VK_ERROR_SURFACE_LOST_KHR` — never a faked present.
struct Presentation;
impl Presentation {
    fn frame_to_app_surface(
        presenters: &mut std::collections::HashMap<u64, Option<WaylandAppPresenter>>,
        windows: &std::collections::HashMap<u64, WaylandWindow>,
        swapchain: u64,
        plane: (Vec<u8>, u32, u32),
    ) -> VkResult {
        let (xrgb, w, h) = plane;
        let Some(win) = windows.get(&swapchain) else {
            return VK_SUCCESS; // no captured wl_surface: readback-only present
        };
        // Bring the presenter up once, caching a soft-unavailable outcome as `None`.
        if !presenters.contains_key(&swapchain) {
            // SAFETY: win.surface is the live wl_surface captured from VkWaylandSurfaceCreateInfoKHR;
            // the presenter is owned by this swapchain state and dropped before the surface record.
            match unsafe { WaylandAppPresenter::new(win.surface) } {
                Ok(p) => {
                    presenters.insert(swapchain, Some(p));
                }
                Err(e) if e.is_unavailable() => {
                    presenters.insert(swapchain, None);
                }
                Err(e) => return e.to_vk_result(),
            }
        }
        match presenters.get_mut(&swapchain) {
            Some(Some(p)) => match p.present(&xrgb, w, h) {
                Ok(()) => VK_SUCCESS,
                Err(e) => e.to_vk_result(),
            },
            _ => VK_SUCCESS, // soft-unavailable: readback-only present
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn no_wayland_window_is_readback_only_vk_success() {
        let mut presenters: HashMap<u64, Option<WaylandAppPresenter>> = HashMap::new();
        let windows = HashMap::new();
        let plane = (vec![0xFFu8; 16], 2, 2);

        assert_eq!(
            Presentation::frame_to_app_surface(&mut presenters, &windows, 0xABC, plane),
            VK_SUCCESS
        );
        assert!(presenters.is_empty());
    }

    #[test]
    fn soft_unavailable_bringup_caches_none_and_returns_vk_success() {
        let swapchain = 0xBEEF;
        let mut presenters = HashMap::new();
        let mut windows = HashMap::new();
        windows.insert(
            swapchain,
            WaylandWindow {
                display: 0xD15,
                surface: 0,
            },
        );

        let first = Presentation::frame_to_app_surface(
            &mut presenters,
            &windows,
            swapchain,
            (vec![0xFF; 16], 2, 2),
        );
        assert_eq!(first, VK_SUCCESS);
        assert!(matches!(presenters.get(&swapchain), Some(None)));

        let second = Presentation::frame_to_app_surface(
            &mut presenters,
            &windows,
            swapchain,
            (vec![0xFF; 16], 2, 2),
        );
        assert_eq!(second, VK_SUCCESS);
    }

    #[test]
    fn cached_soft_unavailable_is_vk_success() {
        let swapchain = 0x1234;
        let mut presenters = HashMap::from([(swapchain, None)]);
        let windows = HashMap::from([(
            swapchain,
            WaylandWindow {
                display: 0xD15,
                surface: 0xF00,
            },
        )]);

        assert_eq!(
            Presentation::frame_to_app_surface(
                &mut presenters,
                &windows,
                swapchain,
                (vec![0xFF; 16], 2, 2),
            ),
            VK_SUCCESS
        );
    }
}
