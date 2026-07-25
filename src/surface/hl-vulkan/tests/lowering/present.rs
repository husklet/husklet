use super::*;

#[test]
fn present_path_lowers_surface_and_present() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let surface =
        present::create_surface(&mut d, &mut sink, 1920, 1080, vk_format::B8G8R8A8_UNORM, 7)
            .unwrap();
    match &sink.batches[0][0] {
        Cmd::CreateSurface(id, desc) => {
            assert_eq!(*id, 1);
            assert_eq!((desc.width, desc.height), (1920, 1080));
            assert_eq!(desc.format, TextureFormat::Bgra8Unorm);
            assert_eq!(desc.hlp_surface, 7);
        }
        other => panic!("expected CreateSurface, got {other:?}"),
    }

    let sc = present::create_swapchain(&mut d, &mut sink, surface, 2).unwrap();
    // create_swapchain emits one CreateTexture per presentable image (real render-target textures).
    let img0_ir = d.swapchains.get(&sc).unwrap().images[0].ir_texture_id;
    assert!(sink.batches.iter().flatten().any(|c| matches!(
        c,
        Cmd::CreateTexture(id, desc)
            if *id == img0_ir
                && (desc.width, desc.height) == (1920, 1080)
                && desc.usage & hl_gpu::protocol::model::enums::texture_usage::RENDER_TARGET != 0
                && desc.usage & hl_gpu::protocol::model::enums::texture_usage::COPY_SRC != 0
    )));

    let idx = d.acquire_next_image(sc).unwrap();
    present::queue_present(&mut d, &mut sink, sc, idx).unwrap();

    // the present names the surface's ir id + the presented image's REAL backing texture id.
    match sink.batches.last().unwrap().as_slice() {
        [Cmd::Present {
            surface: s,
            texture: t,
        }] => {
            assert_eq!(*s, 1); // the CreateSurface ir id
            assert_eq!(*t, img0_ir); // the presented swapchain image's real render-target texture
        }
        other => panic!("expected Present, got {other:?}"),
    }
}

/// `vkAcquireNextImageKHR` genuinely FIFO round-robins across a swapchain's images instead of pinning
/// image 0: over an acquire→present loop of MORE than `image_count` iterations the returned indices cycle
/// `0,1,..,N-1,0,..`, each present lowers a `Cmd::Present` naming the acquired image's own texture (so the
/// presented image is exactly the one acquired that iteration), and queue_present returns the image to the
/// pool. This is the driver-level proof of the fix for vkcube's one-frame `demo_draw` abort.
#[test]
fn acquire_round_robins_across_swapchain_images() {
    const N: u32 = 3;
    const ITERS: usize = 7; // > N, so the cycle wraps twice + one
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let surface =
        present::create_surface(&mut d, &mut sink, 64, 64, vk_format::B8G8R8A8_UNORM, 0).unwrap();
    let sc = present::create_swapchain(&mut d, &mut sink, surface, N).unwrap();

    // Each image's own backing texture id, so we can prove the present named the acquired image's texture.
    let img_texs: Vec<u32> = d
        .swapchains
        .get(&sc)
        .unwrap()
        .images
        .iter()
        .map(|i| i.ir_texture_id)
        .collect();

    let mut acquired = Vec::new();
    for _ in 0..ITERS {
        let idx = d.acquire_next_image(sc).unwrap();
        acquired.push(idx);
        present::queue_present(&mut d, &mut sink, sc, idx).unwrap();
        // The just-emitted Present names the acquired image's OWN texture (present == the acquired image).
        match sink.batches.last().unwrap().as_slice() {
            [Cmd::Present { texture: t, .. }] => {
                assert_eq!(
                    *t, img_texs[idx as usize],
                    "present targets the acquired image's texture"
                )
            }
            other => panic!("expected Present, got {other:?}"),
        }
    }

    // The indices cycle 0,1,2,0,1,2,0 — not stuck at 0 (the bug), and every image is used.
    assert_eq!(
        acquired,
        vec![0, 1, 2, 0, 1, 2, 0],
        "acquire cycles through all {N} images in FIFO order"
    );
    // Back in the pool after the loop: a fresh acquire continues the cycle rather than failing.
    assert_eq!(
        d.acquire_next_image(sc).unwrap(),
        1,
        "the cursor persists across the loop"
    );
}
