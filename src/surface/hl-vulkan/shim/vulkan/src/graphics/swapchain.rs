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
        let surface_status = application_surface_status(&s.surfaces, ci.surface);
        if surface_status != VK_SUCCESS {
            return surface_status;
        }
        let window = s.wayland_surfaces.get(&ci.surface).copied();
        let presenter_id = window
            .map(|window| PresenterId::Wayland(window.surface))
            .unwrap_or(PresenterId::Surface(ci.surface));
        let presentation_target = match presenter_id {
            PresenterId::Surface(surface) => {
                hl_vulkan::model::queue::PresentationTarget::Surface(surface)
            }
            PresenterId::Wayland(window) => {
                hl_vulkan::model::queue::PresentationTarget::Window(window as u64)
            }
        };
        if ci.old_swapchain == 0 {
            let Some((dev, _)) = s.device_and_sink() else {
                return VK_ERROR_INITIALIZATION_FAILED;
            };
            let status = active_target_status(
                ci.old_swapchain,
                present::has_active_swapchain(dev, presentation_target),
            );
            if status != VK_SUCCESS {
                return status;
            }
        }
        let queue_family_indices_valid = if ci.image_sharing_mode == 1 {
            if ci.queue_family_index_count <= 1 || ci.p_queue_family_indices.is_null() {
                false
            } else {
                let indices = unsafe {
                    std::slice::from_raw_parts(
                        ci.p_queue_family_indices,
                        ci.queue_family_index_count as usize,
                    )
                };
                indices.iter().enumerate().all(|(position, &family)| {
                    present::QueueFamily(family).supports_present()
                        && !indices[..position].contains(&family)
                })
            }
        } else {
            true
        };
        let config = present::SwapchainConfig {
            application_surface: ci.surface,
            application_surface_live: s.surfaces.contains(&ci.surface),
            presentation_target,
            flags: ci.flags,
            min_image_count: ci.min_image_count,
            image_format: ci.image_format as u32,
            image_color_space: ci.image_color_space,
            image_extent: (ci.image_extent.width, ci.image_extent.height),
            image_array_layers: ci.image_array_layers,
            image_usage: ci.image_usage,
            image_sharing_mode: ci.image_sharing_mode,
            queue_family_index_count: ci.queue_family_index_count,
            queue_family_indices_valid,
            pre_transform: ci.pre_transform,
            composite_alpha: ci.composite_alpha,
            present_mode: ci.present_mode,
            old_swapchain: ci.old_swapchain,
        };
        {
            let Some((dev, _)) = s.device_and_sink() else {
                return VK_ERROR_INITIALIZATION_FAILED;
            };
            if let Err(error) = present::validate_swapchain(dev, config) {
                return Status::from_error(&error);
            }
        }
        if !old_presenter_matches(&s.presenters, ci.old_swapchain, presenter_id) {
            return VK_ERROR_INITIALIZATION_FAILED;
        }
        {
            let Some((dev, _)) = s.device_and_sink() else {
                return VK_ERROR_INITIALIZATION_FAILED;
            };
            if let Err(error) = present::retire_swapchain(dev, ci.old_swapchain) {
                return Status::from_error(&error);
            }
        }
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
        let Some((dev, sink)) = s.device_and_sink() else {
            return VK_ERROR_INITIALIZATION_FAILED;
        };
        let r = (|| {
            let surface = Swapchain::create_surface(dev, sink, ci, token)?;
            present::create_swapchain_for_target(
                dev,
                sink,
                surface,
                ci.surface,
                presentation_target,
                ci.min_image_count,
                0,
            )
        })();
        match r {
            Ok(h) => {
                s.presenters.bind(h, presenter_id);
                if !p_swapchain.is_null() {
                    unsafe { *p_swapchain = h };
                }
                VK_SUCCESS
            }
            Err(e) => {
                s.presenters.discard_unbound(presenter_id);
                Status::from_error(&e)
            }
        }
    })
}

fn application_surface_status(live: &std::collections::HashSet<u64>, surface: u64) -> VkResult {
    if live.contains(&surface) {
        VK_SUCCESS
    } else {
        VK_ERROR_SURFACE_LOST_KHR
    }
}

fn old_presenter_matches(
    presenters: &crate::state::Presenters,
    old_swapchain: u64,
    target: PresenterId,
) -> bool {
    old_swapchain == 0 || presenters.surface(old_swapchain) == Some(target)
}

fn active_target_status(old_swapchain: u64, active: bool) -> VkResult {
    if old_swapchain == 0 && active {
        VK_ERROR_NATIVE_WINDOW_IN_USE_KHR
    } else {
        VK_SUCCESS
    }
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
        let mut completed = false;
        if let Some((dev, sink)) = s.device_and_sink() {
            // Retire the swapchain AND its presentable images + presentation surface (dropping their
            // `dev.images`/`dev.surfaces` bookkeeping + freeing the host textures/surface). Removing only the
            // `SwapchainRec` would orphan the images in `dev.images` forever — a per-resize handle leak.
            completed = dev.destroy_swapchain(sink, swapchain).is_ok();
        }
        if completed {
            s.presenters.unbind(swapchain);
        } else {
            hl_log::hl_error!(
                hl_log::tag::PRESENT,
                "swapchain destroy deferred sc={swapchain:#x}; cleanup ownership retained"
            );
            s.enqueue_swapchain_destroy(swapchain);
            // Retry once in the void ABI call; persistent failures remain queued and are retried at the
            // start of every later Vulkan entry point.
            s.retry_swapchain_destroys();
        }
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
    timeout: u64,
    semaphore: u64,
    fence: u64,
    p_image_index: *mut u32,
) -> VkResult {
    if p_image_index.is_null() || (semaphore == 0 && fence == 0) {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    wait_for_available(timeout, || {
        ShimState::with_device(
        |dev| match dev.try_acquire_next_image(swapchain, semaphore, fence) {
            Ok(idx) => {
                unsafe { *p_image_index = idx };
                VK_SUCCESS
            }
            Err(
                error @ (present::AcquireImageError::Retired
                | present::AcquireImageError::NotReady
                | present::AcquireImageError::Timeout),
            ) => acquire_error_status(&error),
            Err(present::AcquireImageError::Invalid(e)) => {
                // Error: the app cannot get an image to render into, so the frame is lost. Latched per
                // swapchain — acquire runs at display rate and a swapchain that cannot acquire keeps
                // failing until it is recreated, so an unlatched line would be sixty a second.
                static REFUSED: crate::logging::Latch = crate::logging::Latch::new();
                if REFUSED.fires(swapchain) {
                    hl_log::hl_error!(
                        hl_log::tag::SHIM,
                        "vkAcquireNextImageKHR sc={swapchain:#x} -> {:?}",
                        Status::from_error(&e)
                    );
                }
                Status::from_error(&e)
            }
        })
        .unwrap_or(VK_ERROR_DEVICE_LOST)
    })
}

fn wait_for_available(timeout_ns: u64, mut probe: impl FnMut() -> VkResult) -> VkResult {
    let started = std::time::Instant::now();
    loop {
        let result = probe();
        if result != VK_NOT_READY {
            return result;
        }
        if timeout_ns == 0 {
            return VK_NOT_READY;
        }
        if timeout_ns != u64::MAX {
            let timeout = std::time::Duration::from_nanos(timeout_ns);
            let elapsed = started.elapsed();
            if elapsed >= timeout {
                return VK_TIMEOUT;
            }
            std::thread::sleep(
                timeout
                    .saturating_sub(elapsed)
                    .min(std::time::Duration::from_millis(1)),
            );
        } else {
            // Release the global state mutex between probes. Presentation, swapchain retirement, and
            // device destruction can therefore make progress and also cancel this indefinite wait.
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
}

fn acquire_error_status(error: &present::AcquireImageError) -> VkResult {
    match error {
        present::AcquireImageError::Retired => VK_ERROR_OUT_OF_DATE_KHR,
        present::AcquireImageError::NotReady => VK_NOT_READY,
        present::AcquireImageError::Timeout => VK_TIMEOUT,
        present::AcquireImageError::Invalid(error) => Status::from_error(error),
    }
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
    if pi.wait_semaphore_count != 0 && pi.p_wait_semaphores.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    let swapchains =
        unsafe { std::slice::from_raw_parts(pi.p_swapchains, pi.swapchain_count as usize) };
    let indices =
        unsafe { std::slice::from_raw_parts(pi.p_image_indices, pi.swapchain_count as usize) };
    let waits = if pi.wait_semaphore_count == 0 {
        &[][..]
    } else {
        unsafe {
            std::slice::from_raw_parts(
                pi.p_wait_semaphores,
                pi.wait_semaphore_count as usize,
            )
        }
    };
    let mut per_swapchain_results = (!pi.p_results.is_null()).then(|| unsafe {
        std::slice::from_raw_parts_mut(pi.p_results, pi.swapchain_count as usize)
    });
    StateStore::with(|s| {
        let Some(parts) = s.present_parts() else {
            return VK_ERROR_INITIALIZATION_FAILED;
        };
        let crate::state::PresentParts {
            device: dev,
            sink,
            presenters,
            native_present,
        } = parts;
        // Queue presentation waits are binary semaphore waits. The executor completes queue submits
        // synchronously, so their guest-side payload is authoritative here. Validate the whole set
        // before consuming any payload; successful consumption is the queue wait operation.
        if let Err(error) = dev.consume_binary_semaphores(waits) {
            return Status::from_error(&error);
        }
        let mut res = VK_SUCCESS;
        // A present that commits nothing must never be silent. Every failure exit below reports at
        // `error` — the one level a release build keeps — and each is latched per swapchain, because
        // present runs at display rate and these failures persist until the swapchain is recreated.
        // One latch per REASON so a second, different failure on the same swapchain still speaks.
        static LOWERING_FAILED: crate::logging::Latch = crate::logging::Latch::new();
        static EXTENT_GONE: crate::logging::Latch = crate::logging::Latch::new();
        static NATIVE_COMMIT_FAILED: crate::logging::Latch = crate::logging::Latch::new();
        static READBACK_FAILED: crate::logging::Latch = crate::logging::Latch::new();
        static SHM_COMMIT_FAILED: crate::logging::Latch = crate::logging::Latch::new();
        for (position, (&sc, &idx)) in swapchains.iter().zip(indices).enumerate() {
            PresentResults::write(&mut per_swapchain_results, position, VK_SUCCESS);
            let native = if native_present {
                presenters
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
                if LOWERING_FAILED.fires(sc) {
                    hl_log::hl_error!(
                        hl_log::tag::PRESENT,
                        "present lowering failed sc={sc:#x} image={idx} -> {:?}",
                        Status::from_error(&e)
                    );
                }
                let result = Status::from_error(&e);
                PresentResults::write(&mut per_swapchain_results, position, result);
                res = result;
                continue;
            }
            if let Some(frame) = native {
                let Some((w, h)) = dev.swapchains.get(&sc).map(|sc| (sc.width, sc.height)) else {
                    if EXTENT_GONE.fires(sc) {
                        hl_log::hl_error!(
                            hl_log::tag::PRESENT,
                            "present reserved a native frame for an unknown swapchain sc={sc:#x}"
                        );
                    }
                    PresentResults::write(
                        &mut per_swapchain_results,
                        position,
                        VK_ERROR_OUT_OF_DATE_KHR,
                    );
                    res = VK_ERROR_OUT_OF_DATE_KHR;
                    continue;
                };
                let commit = presenters
                    .get_mut(sc)
                    .and_then(Option::as_mut)
                    .expect("native frame requires its presenter")
                    .commit_native(frame, w, h);
                if let Err(error) = commit {
                    if NATIVE_COMMIT_FAILED.fires(sc) {
                        hl_log::hl_error!(
                            hl_log::tag::PRESENT,
                            "native commit failed sc={sc:#x} {}x{} -> {:?}; demoting to readback",
                            w,
                            h,
                            error
                        );
                    }
                    let owner = presenters.surface(sc);
                    if let Some(surface) = owner {
                        for swapchain in presenters.swapchains(surface) {
                            let _ = present::demote_swapchain(dev, sink, swapchain);
                        }
                    }
                    if let Some(Some(presenter)) = presenters.get_mut(sc) {
                        presenter.retire_native();
                    }
                    let result = error.to_vk_result();
                    PresentResults::write(&mut per_swapchain_results, position, result);
                    res = result;
                }
                continue;
            }
            // Compatibility path: synchronous readback + wl_shm.
            let plane = match present::read_presented_xrgb(dev, sink, sc, idx) {
                Ok(p) => p,
                Err(e) => {
                    if READBACK_FAILED.fires(sc) {
                        hl_log::hl_error!(
                            hl_log::tag::PRESENT,
                            "present readback failed sc={sc:#x} image={idx} -> {:?}",
                            Status::from_error(&e)
                        );
                    }
                    let result = Status::from_error(&e);
                    PresentResults::write(&mut per_swapchain_results, position, result);
                    res = result;
                    continue;
                }
            };
            // 3) Marshal that plane onto the app's OWN `wl_surface`. No live presenter for a surface
            //    that names a window ⇒ VK_ERROR_SURFACE_LOST_KHR; a hard marshal/flush failure ⇒
            //    VK_ERROR_OUT_OF_DATE/SURFACE_LOST. Only a deliberately offscreen target is VK_SUCCESS.
            let vk = Presentation::frame_to_app_surface(presenters, sc, plane);
            if vk != VK_SUCCESS {
                if SHM_COMMIT_FAILED.fires(sc) {
                    hl_log::hl_error!(
                        hl_log::tag::PRESENT,
                        "present committed nothing sc={sc:#x} -> {:?}",
                        vk
                    );
                }
                PresentResults::write(&mut per_swapchain_results, position, vk);
                res = vk;
            }
        }
        res
    })
}

struct PresentResults;
impl PresentResults {
    fn write(results: &mut Option<&mut [VkResult]>, index: usize, result: VkResult) {
        if let Some(slot) = results
            .as_deref_mut()
            .and_then(|results| results.get_mut(index))
        {
            *slot = result;
        }
    }
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

    #[test]
    fn swapchain_creation_rejects_a_dead_application_surface() {
        let live = std::collections::HashSet::from([0x5000]);
        assert_eq!(application_surface_status(&live, 0x5000), VK_SUCCESS);
        assert_eq!(
            application_surface_status(&live, 0x5001),
            VK_ERROR_SURFACE_LOST_KHR
        );
    }

    #[test]
    fn replacement_requires_the_old_presenter_to_own_the_same_surface() {
        let mut presenters = crate::state::Presenters::new();
        let first = PresenterId::Surface(0x5000);
        let foreign = PresenterId::Surface(0x5001);
        presenters.ensure(first, || None);
        presenters.bind(7, first);

        assert!(old_presenter_matches(&presenters, 0, foreign));
        assert!(old_presenter_matches(&presenters, 7, first));
        assert!(!old_presenter_matches(&presenters, 7, foreign));
        assert!(!old_presenter_matches(&presenters, 8, first));
    }

    #[test]
    fn duplicate_active_target_without_old_swapchain_reports_native_window_in_use() {
        assert_eq!(
            active_target_status(0, true),
            VK_ERROR_NATIVE_WINDOW_IN_USE_KHR
        );
        assert_eq!(active_target_status(0, false), VK_SUCCESS);
        assert_eq!(active_target_status(7, true), VK_SUCCESS);
    }

    #[test]
    fn acquire_pool_outcomes_map_to_exact_wsi_results() {
        assert_eq!(
            acquire_error_status(&present::AcquireImageError::Retired),
            VK_ERROR_OUT_OF_DATE_KHR
        );
        assert_eq!(
            acquire_error_status(&present::AcquireImageError::NotReady),
            VK_NOT_READY
        );
        assert_eq!(
            acquire_error_status(&present::AcquireImageError::Timeout),
            VK_TIMEOUT
        );
    }

    #[test]
    fn finite_acquire_waits_until_its_deadline() {
        let started = std::time::Instant::now();
        assert_eq!(wait_for_available(5_000_000, || VK_NOT_READY), VK_TIMEOUT);
        assert!(started.elapsed() >= std::time::Duration::from_millis(5));
    }

    #[test]
    fn indefinite_acquire_observes_availability_and_cancellation() {
        let started = std::time::Instant::now();
        assert_eq!(
            wait_for_available(u64::MAX, || {
                if started.elapsed() >= std::time::Duration::from_millis(3) {
                    VK_SUCCESS
                } else {
                    VK_NOT_READY
                }
            }),
            VK_SUCCESS
        );
        assert_eq!(
            wait_for_available(u64::MAX, || VK_ERROR_DEVICE_LOST),
            VK_ERROR_DEVICE_LOST
        );
    }

    #[test]
    fn per_swapchain_results_replace_every_caller_sentinel() {
        let mut values = [VK_ERROR_DEVICE_LOST, VK_ERROR_DEVICE_LOST];
        let mut results = Some(values.as_mut_slice());

        PresentResults::write(&mut results, 0, VK_SUCCESS);
        PresentResults::write(&mut results, 1, VK_ERROR_OUT_OF_DATE_KHR);

        assert_eq!(values, [VK_SUCCESS, VK_ERROR_OUT_OF_DATE_KHR]);
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
