use super::*;
use crate::state::PresenterId;
use hl_vulkan::result::{VK_ERROR_OUT_OF_DATE_KHR, VK_ERROR_SURFACE_LOST_KHR};

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
            match unsafe { WaylandAppPresenter::new(window.surface, window.display) } {
                Ok(presenter) => Some(presenter),
                Err(error) if error.is_unavailable() => {
                    // Soft: cache `None` so it is not re-probed each frame. Log the reason once —
                    // without it a total presentation failure was completely silent.
                    crate::stub::Failure::report(
                        "vkCreateSwapchainKHR",
                        &format!(
                            "no app-surface presenter for wl_surface={:#x} ({error:?}); presents \
                             cannot reach a window",
                            window.surface
                        ),
                    );
                    None
                }
                Err(error) => {
                    // Hard: an ABI/version incompatibility with the app's libwayland-client. Report it
                    // loudly — this is a real presentation failure, not a headless swapchain.
                    crate::stub::Failure::report(
                        "vkCreateSwapchainKHR",
                        &format!(
                            "app-surface presenter bring-up failed for wl_surface={:#x} \
                             ({error:?}); the swapchain cannot present",
                            window.surface
                        ),
                    );
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

/// `VkAcquireNextImageInfoKHR` — the struct `vkAcquireNextImage2KHR` takes in place of the positional
/// arguments of `vkAcquireNextImageKHR`. Layout from `vk.xml`.
#[repr(C)]
pub struct VkAcquireNextImageInfoKHR {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub swapchain: u64,
    pub timeout: u64,
    pub semaphore: u64,
    pub fence: u64,
    pub device_mask: u32,
}

/// `vkAcquireNextImage2KHR` — part of `VK_KHR_swapchain`, which this driver DOES advertise, so a stub here
/// was a capability lie inside an advertised extension. It is `vkAcquireNextImageKHR` with its arguments in
/// a struct plus a `deviceMask`; one physical device is presented, so the only valid mask is device 0.
pub extern "C" fn vkAcquireNextImage2KHR(
    device: *mut c_void,
    p_acquire_info: *const c_void,
    p_image_index: *mut u32,
) -> VkResult {
    let Some(info) = (unsafe { (p_acquire_info as *const VkAcquireNextImageInfoKHR).as_ref() })
    else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if info.device_mask & 0x1 == 0 {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    vkAcquireNextImageKHR(
        device,
        info.swapchain,
        info.timeout,
        info.semaphore,
        info.fence,
        p_image_index,
    )
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
            // 3) Marshal that plane onto the app's OWN `wl_surface`. No live presenter for a surface
            //    that names a window ⇒ VK_ERROR_SURFACE_LOST_KHR; a hard marshal/flush failure ⇒
            //    VK_ERROR_OUT_OF_DATE/SURFACE_LOST. Only a deliberately offscreen target is VK_SUCCESS.
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
/// [`WaylandAppPresenter`].
///
/// A present that commits nothing must never report success. When no presenter is live, the outcome
/// depends on what the swapchain's target IS ([`PresenterId::expects_window`]): for an
/// application-owned `wl_surface` this is a total presentation failure and returns
/// `VK_ERROR_SURFACE_LOST_KHR` — the surface cannot be presented to, and unlike
/// `VK_ERROR_OUT_OF_DATE_KHR` it does not invite an endless swapchain-recreate loop. (Vulkan does not
/// permit `VK_ERROR_INITIALIZATION_FAILED` from `vkQueuePresentKHR`.) For a deliberately offscreen
/// target — a headless surface, or a wayland surface the application created with no `wl_surface` — the
/// readback is the whole present and `VK_SUCCESS` is truthful.
///
/// A *hard* per-frame marshal/flush/size failure maps to `VK_ERROR_OUT_OF_DATE_KHR` /
/// `VK_ERROR_SURFACE_LOST_KHR` — never a faked present.
struct Presentation;
impl Presentation {
    fn frame_to_app_surface(
        presenters: &mut crate::state::Presenters,
        swapchain: u64,
        plane: (Vec<u8>, u32, u32),
    ) -> VkResult {
        let (xrgb, w, h) = plane;
        // Resolved before the mutable borrow below: what this swapchain was supposed to present to.
        let expects_window = presenters
            .surface(swapchain)
            .is_some_and(|target| target.expects_window());
        match presenters.get_mut(swapchain) {
            Some(Some(p)) => match p.present(&xrgb, w, h) {
                Ok(()) => VK_SUCCESS,
                Err(e) => e.to_vk_result(),
            },
            _ if expects_window => {
                crate::stub::Failure::report(
                    "vkQueuePresentKHR",
                    &format!(
                        "present committed nothing (sc={swapchain:#x}): the surface names an \
                         application wl_surface but no presenter is live; returning \
                         VK_ERROR_SURFACE_LOST_KHR"
                    ),
                );
                VK_ERROR_SURFACE_LOST_KHR
            }
            // Deliberately offscreen (headless surface, or no `wl_surface` supplied): the readback IS
            // the present.
            _ => VK_SUCCESS,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plane() -> (Vec<u8>, u32, u32) {
        (vec![0xFFu8; 16], 2, 2)
    }

    /// An unbound swapchain has no known target, so nothing was promised — readback-only is truthful.
    #[test]
    fn unbound_swapchain_is_readback_only_vk_success() {
        let mut presenters = crate::state::Presenters::new();

        assert_eq!(
            Presentation::frame_to_app_surface(&mut presenters, 0xABC, plane()),
            VK_SUCCESS
        );
    }

    /// THE DEFECT: a swapchain whose surface names a real application `wl_surface` but whose presenter
    /// never came up committed nothing and still reported `VK_SUCCESS`. `vkcube-wayland` therefore ran
    /// to completion with 60 successful presents and ZERO composited frames, and exited 0.
    #[test]
    fn present_with_no_live_presenter_for_a_real_window_reports_surface_lost() {
        let swapchain = 0xBEEF;
        let target = PresenterId::Wayland(0x515);
        let mut presenters = crate::state::Presenters::new();
        presenters.ensure(target, || None);
        presenters.bind(swapchain, target);

        assert_eq!(
            Presentation::frame_to_app_surface(&mut presenters, swapchain, plane()),
            VK_ERROR_SURFACE_LOST_KHR,
            "a present that commits nothing to a real window must not report success"
        );
    }

    /// The failure must be reported on EVERY present, not just the first: the cached `None` presenter
    /// must not decay back into a silent success.
    #[test]
    fn repeated_presents_keep_reporting_surface_lost() {
        let swapchain = 0x1234;
        let target = PresenterId::Wayland(0x515);
        let mut presenters = crate::state::Presenters::new();
        presenters.ensure(target, || None);
        presenters.bind(swapchain, target);

        for _ in 0..3 {
            assert_eq!(
                Presentation::frame_to_app_surface(&mut presenters, swapchain, plane()),
                VK_ERROR_SURFACE_LOST_KHR
            );
        }
    }

    /// A headless surface never had a window, so the readback IS the present. This is the path the
    /// offscreen/capture flows use and it must keep succeeding.
    #[test]
    fn headless_surface_target_stays_vk_success() {
        let swapchain = 0x77;
        let target = PresenterId::Surface(0x5000_0000_0000_0001);
        let mut presenters = crate::state::Presenters::new();
        presenters.ensure(target, || None);
        presenters.bind(swapchain, target);

        assert_eq!(
            Presentation::frame_to_app_surface(&mut presenters, swapchain, plane()),
            VK_SUCCESS
        );
    }

    /// `vkCreateWaylandSurfaceKHR` with a null `wl_surface` is an application asking for no window.
    #[test]
    fn wayland_surface_without_a_window_stays_vk_success() {
        let swapchain = 0x88;
        let target = PresenterId::Wayland(0);
        let mut presenters = crate::state::Presenters::new();
        presenters.ensure(target, || None);
        presenters.bind(swapchain, target);

        assert_eq!(
            Presentation::frame_to_app_surface(&mut presenters, swapchain, plane()),
            VK_SUCCESS
        );
    }
}
