use super::*;
use crate::state::PresenterId;
use hl_vulkan::result::VK_ERROR_OUT_OF_DATE_KHR;

// ==================================================================================================
// WSI: swapchain + present
// ==================================================================================================

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
        let window = s.wayland_surfaces.get(&ci.surface).copied();
        let presenter_id = window
            .map(|window| PresenterId::Wayland(window.surface))
            .unwrap_or(PresenterId::Surface(ci.surface));
        let mut hard_error = None;
        s.presenters.ensure(presenter_id, || {
            let Some(window) = window else {
                return None;
            };
            // SAFETY: the application owns this live wl_surface for the VkSurfaceKHR lifetime.
            match unsafe { WaylandAppPresenter::new(window.surface) } {
                Ok(presenter) => Some(presenter),
                Err(error) if error.is_unavailable() => None,
                Err(error) => {
                    hard_error = Some(error);
                    None
                }
            }
        });
        if let Some(error) = hard_error {
            s.presenters.discard_unbound(presenter_id);
            return error.to_vk_result();
        }
        let token = s
            .native_present
            .then(|| {
                s.presenters
                    .ensure(presenter_id, || None)
                    .as_ref()
                    .and_then(WaylandAppPresenter::native_token)
            })
            .flatten();
        let sink = &mut s.sink;
        let Some(dev) = s.device.as_mut() else {
            return VK_ERROR_INITIALIZATION_FAILED;
        };
        let r = (|| {
            let surface = Swapchain::create_surface(dev, sink, ci, token)?;
            present::create_swapchain(dev, sink, surface, ci.min_image_count)
        })();
        match r {
            Ok(h) => {
                s.presenters.bind(h, presenter_id);
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
        token: Option<hl_gpu::protocol::model::descriptor::SurfaceToken>,
    ) -> hl_gpu::Result<u64> {
        present::create_surface(
            dev,
            sink,
            ci.image_extent.width,
            ci.image_extent.height,
            ci.image_format as u32,
            token,
        )
    }
}

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
        s.presenters.unbind(swapchain);
    });
}

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
            let native = if s.native_present {
                s.presenters
                    .get_mut(sc)
                    .and_then(Option::as_mut)
                    .and_then(WaylandAppPresenter::reserve_native_frame)
            } else {
                None
            };
            // Native presentation submits the real GPU present first. Fallback emits no fake Present.
            if let Err(e) =
                present::queue_present(dev, sink, sc, idx, native.map(|frame| frame.serial))
            {
                res = Status::from_error(&e);
                continue;
            }
            if let Some(frame) = native {
                let Some((w, h)) = dev.swapchains.get(&sc).map(|sc| (sc.width, sc.height)) else {
                    res = VK_ERROR_OUT_OF_DATE_KHR;
                    continue;
                };
                let commit = s
                    .presenters
                    .get_mut(sc)
                    .and_then(Option::as_mut)
                    .expect("native frame requires its presenter")
                    .commit_native(frame, w, h);
                if let Err(error) = commit {
                    let owner = s.presenters.surface(sc);
                    if let Some(surface) = owner {
                        for swapchain in s.presenters.swapchains(surface) {
                            let _ = present::demote_swapchain(dev, sink, swapchain);
                        }
                    }
                    if let Some(Some(presenter)) = s.presenters.get_mut(sc) {
                        presenter.retire_native();
                    }
                    res = error.to_vk_result();
                }
                continue;
            }
            // Compatibility path: synchronous readback + wl_shm.
            let plane = match present::read_presented_xrgb(dev, sink, sc, idx) {
                Ok(p) => p,
                Err(e) => {
                    res = Status::from_error(&e);
                    continue;
                }
            };
            // 3) Marshal that plane onto the app's OWN `wl_surface` (soft-unavailable ⇒ readback-only,
            //    still VK_SUCCESS; a hard marshal/flush failure ⇒ VK_ERROR_OUT_OF_DATE/SURFACE_LOST).
            let vk = Presentation::frame_to_app_surface(&mut s.presenters, sc, plane);
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
        presenters: &mut crate::state::Presenters,
        swapchain: u64,
        plane: (Vec<u8>, u32, u32),
    ) -> VkResult {
        let (xrgb, w, h) = plane;
        match presenters.get_mut(swapchain) {
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

    #[test]
    fn no_wayland_window_is_readback_only_vk_success() {
        let mut presenters = crate::state::Presenters::new();
        let plane = (vec![0xFFu8; 16], 2, 2);

        assert_eq!(
            Presentation::frame_to_app_surface(&mut presenters, 0xABC, plane),
            VK_SUCCESS
        );
    }

    #[test]
    fn soft_unavailable_bringup_caches_none_and_returns_vk_success() {
        let swapchain = 0xBEEF;
        let surface = PresenterId::Wayland(0x515);
        let mut presenters = crate::state::Presenters::new();
        presenters.ensure(surface, || None);
        presenters.bind(swapchain, surface);

        let first =
            Presentation::frame_to_app_surface(&mut presenters, swapchain, (vec![0xFF; 16], 2, 2));
        assert_eq!(first, VK_SUCCESS);

        let second =
            Presentation::frame_to_app_surface(&mut presenters, swapchain, (vec![0xFF; 16], 2, 2));
        assert_eq!(second, VK_SUCCESS);
    }

    #[test]
    fn cached_soft_unavailable_is_vk_success() {
        let swapchain = 0x1234;
        let surface = PresenterId::Wayland(0x515);
        let mut presenters = crate::state::Presenters::new();
        presenters.ensure(surface, || None);
        presenters.bind(swapchain, surface);

        assert_eq!(
            Presentation::frame_to_app_surface(&mut presenters, swapchain, (vec![0xFF; 16], 2, 2),),
            VK_SUCCESS
        );
    }
}
