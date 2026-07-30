use super::*;

/// The WSI swapchain full loop, end-to-end at the DRIVER level — the Vulkan analog of the
/// weston-simple-egl GL milestone, proving a presented swapchain image is a REAL, readable-back render
/// target through our IR + the reference `CpuExecutor`:
///
///   vkCreateSurfaceKHR → vkCreateSwapchainKHR (emits a real `CreateTexture(RENDER_TARGET | COPY_SRC)`
///   per presentable image) → vkGetSwapchainImagesKHR (the images' `VkImage` handles) →
///   vkAcquireNextImageKHR (an image index) → record a render pass that CLEARS the acquired image to a
///   known color → vkQueueSubmit (the executor clears the real texture) → vkQueuePresentKHR (`Cmd::Present`
///   naming that texture) → read the PRESENTED image back (`CopyTextureToBuffer` + `read_buffer`) and
///   assert the known pixels.
///
/// This proves the swapchain image is genuinely allocated + rendered into + presented + read back (not a
/// reserved host-owned alias): the app rendered into it and the exact bytes come back. The clear color's
/// channels are all distinct so the readback also proves the surface's BGRA texel order. What remains for
/// a LIVE vkcube on-screen is the wayland present-marshalling (attaching the presented image to a
/// compositor buffer) — a separate follow-up, deliberately NOT exercised here.
#[test]
fn swapchain_present_loop_reads_back_the_presented_image_end_to_end() {
    const W: u32 = 8;
    const H: u32 = 8;
    let exec = CpuExecutor::new();
    let session = Session::new(
        Limits::from_capabilities(Capabilities::permissive_fixture("hl-cpu-swapchain")),
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );
    let mut sink = InProcessCommandSink::with_session(session, exec);

    let inst = Instance::new(HL_API_VERSION);
    let mut d = inst.create_device();

    // vkCreateSurfaceKHR + vkCreateSwapchainKHR (2 presentable images, real render-target textures).
    let surface =
        present::create_surface(&mut d, &mut sink, W, H, vk_format::B8G8R8A8_UNORM, None).unwrap();
    let swapchain = present::create_swapchain(&mut d, &mut sink, surface, 2).unwrap();

    // vkGetSwapchainImagesKHR → the presentable images' VkImage handles.
    let images = d.swapchain_images(swapchain).unwrap();
    assert_eq!(
        images.len(),
        2,
        "the swapchain has its two presentable images"
    );

    // Run a CONTINUOUS present loop for MORE iterations than there are images (a real app's acquire →
    // render → present, repeat). This proves acquisition genuinely round-robins: the returned index must
    // cycle 0,1,0,1,... (NOT stay pinned at image 0, which aborted vkcube's demo_draw after one frame), and
    // each frame's readback must return exactly what THAT frame cleared into THAT acquired image.
    const FRAMES: usize = 5; // > the 2 images, so the cycle wraps twice
    let mut acquired_indices = Vec::new();
    for frame in 0..FRAMES {
        // A per-frame clear color with 4 distinct channels; frame index rides in the blue channel so each
        // frame's presented pixels are provably the ones this frame rendered into the acquired image.
        let clear = [
            51.0 / 255.0,
            102.0 / 255.0,
            (10 + frame as u32) as f32 / 255.0,
            1.0,
        ];
        let expected = [(10 + frame) as u8, 102, 51, 255]; // BGRA readback of the above

        // vkAcquireNextImageKHR — the next image in round-robin order.
        let idx = d.acquire_next_image(swapchain).unwrap();
        acquired_indices.push(idx);
        let acquired = images[idx as usize];

        // Record a render pass that CLEARS the acquired image to this frame's color, then submit — the
        // executor clears the image's REAL backing texture (fails if the image were not a real texture).
        let cb = d.allocate_command_buffer();
        d.begin_command_buffer(cb, false).unwrap();
        record::cmd_begin_render_pass(&mut d, cb, acquired, clear, true, None).unwrap();
        d.end_render_pass(cb).unwrap();
        d.end_command_buffer(cb).unwrap();
        submit::queue_submit(&mut d, &mut sink, &[cb], None).unwrap();

        // vkQueuePresentKHR — presents the rendered image (Cmd::Present names its real texture id) and
        // returns it to the pool so the next acquire can advance.
        present::queue_present(&mut d, &mut sink, swapchain, idx, None).unwrap();

        // Read the PRESENTED image back and assert it is exactly what THIS frame cleared into THIS image.
        let pixels = present::read_presented_image(&mut d, &mut sink, swapchain, idx).unwrap();
        assert_eq!(
            pixels.len(),
            (W * H * 4) as usize,
            "the whole presented image plane came back"
        );
        let texel = |x: u32, y: u32| -> [u8; 4] {
            let o = ((y * W + x) * 4) as usize;
            [pixels[o], pixels[o + 1], pixels[o + 2], pixels[o + 3]]
        };
        for y in 0..H {
            for x in 0..W {
                assert_eq!(
                    texel(x, y),
                    expected,
                    "frame {frame} presented image (idx {idx}) texel ({x},{y})"
                );
            }
        }
    }

    // The acquire indices genuinely cycled through the 2 images (0,1,0,1,0) — NOT pinned at 0.
    assert_eq!(
        acquired_indices,
        vec![0, 1, 0, 1, 0],
        "acquire round-robins across the swapchain images"
    );
    assert!(
        acquired_indices.contains(&1),
        "more than image 0 was acquired (the round-robin fix)"
    );
}

/// vkcube's exact per-frame draw loop: each swapchain image has ONE command buffer recorded ONCE (a
/// reusable buffer, no `ONE_TIME_SUBMIT`) that is re-submitted every frame, and a per-frame fence the
/// submit signals and the next frame waits+resets. This proves the continuous multi-frame present loop:
///
///   vkWaitForFences(per-frame fence) → vkResetFences → vkAcquireNextImageKHR → vkQueueSubmit(re-submit
///   the pre-recorded buffer, signal the fence) → vkWaitForFences(fence is now signaled) → vkQueuePresentKHR
///
/// FAIL-BEFORE / PASS-AFTER: before the fix, `vkQueueSubmit` left a submitted command buffer stuck in
/// `Pending` forever, so the SECOND (and every later) re-submit of a pre-recorded per-image buffer failed
/// with `VK_ERROR_INITIALIZATION_FAILED` — exactly vkcube's `demo_draw` abort at cube.c:1093 after ~1
/// presented frame. The reusable buffer must return to `Executable` once its (synchronous) submit
/// completes so the loop keeps running.
#[test]
fn vkcube_style_multiframe_fence_and_resubmit_loop() {
    const W: u32 = 8;
    const H: u32 = 8;

    let exec = CpuExecutor::new();
    let session = Session::new(
        Limits::from_capabilities(Capabilities::permissive_fixture("hl-cpu-vkcube-loop")),
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );
    let mut sink = InProcessCommandSink::with_session(session, exec);

    let inst = Instance::new(HL_API_VERSION);
    let mut d = inst.create_device();

    let surface =
        present::create_surface(&mut d, &mut sink, W, H, vk_format::B8G8R8A8_UNORM, None).unwrap();
    let swapchain = present::create_swapchain(&mut d, &mut sink, surface, 2).unwrap();
    let images = d.swapchain_images(swapchain).unwrap();

    // Record ONE reusable command buffer PER swapchain image, ONCE, up front — the vkcube setup pattern.
    let clear = [51.0 / 255.0, 102.0 / 255.0, 153.0 / 255.0, 1.0];
    let per_image_cbs: Vec<_> = images
        .iter()
        .map(|&img| {
            let cb = d.allocate_command_buffer();
            d.begin_command_buffer(cb, false).unwrap(); // NOT one-time-submit → re-submittable every frame
            record::cmd_begin_render_pass(&mut d, cb, img, clear, true, None).unwrap();
            d.end_render_pass(cb).unwrap();
            d.end_command_buffer(cb).unwrap();
            cb
        })
        .collect();

    // Two per-frame fences (FRAME_LAG = 2), created SIGNALED so the first wait passes — vkcube's model.
    let fences = [
        create::create_fence(&mut d, &mut sink, true).unwrap(),
        create::create_fence(&mut d, &mut sink, true).unwrap(),
    ];

    // Run more frames than there are images OR fences, so both cycles wrap several times.
    const FRAMES: usize = 8;
    for frame in 0..FRAMES {
        let fence = fences[frame % fences.len()];

        // vkWaitForFences → the per-frame fence (signaled from its prior frame, or created signaled).
        submit::wait_for_fence(&mut d, &mut sink, fence).unwrap();
        assert!(
            d.is_fence_signaled(fence).unwrap(),
            "frame {frame}: fence is signaled after wait"
        );
        // vkResetFences → back to unsignaled before this frame's submit re-arms it.
        d.reset_fence(fence).unwrap();
        assert!(
            !d.is_fence_signaled(fence).unwrap(),
            "frame {frame}: fence unsignaled after reset"
        );

        // vkAcquireNextImageKHR → the next image round-robin.
        let idx = d.acquire_next_image(swapchain).unwrap();
        let cb = per_image_cbs[idx as usize];

        // vkQueueSubmit → RE-SUBMIT that image's pre-recorded buffer, signaling this frame's fence. Before
        // the fix this errored on every frame after the buffer's first submit.
        submit::queue_submit(&mut d, &mut sink, &[cb], Some(fence)).unwrap();

        // vkWaitForFences → the submit's signal is now observable (synchronous executor).
        submit::wait_for_fence(&mut d, &mut sink, fence).unwrap();
        assert!(
            d.is_fence_signaled(fence).unwrap(),
            "frame {frame}: fence signaled by its submit"
        );

        // vkQueuePresentKHR → present the rendered image; returns it to the pool for the next acquire.
        present::queue_present(&mut d, &mut sink, swapchain, idx, None).unwrap();
    }
}
