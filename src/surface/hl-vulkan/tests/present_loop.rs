//! Swapchain + multi-frame present-loop LIFECYCLE tests: drive the WSI service
//! ([`hl_vulkan::service::present`]) across a real acquire → render → present loop and a swapchain
//! recreation, asserting the driver-level lifecycle is correct — the presentable images are tracked as
//! real render targets, acquisition round-robins and the present sources the ACQUIRED image, per-frame
//! transient resources are reused (no id leak), and a recreated swapchain retires the old set (no leak).
//!
//! These complement the exact-stream `tests/lowering.rs` present tests and the end-to-end readback loops
//! in `tests/e2e.rs`: here the focus is object LIFETIME across many frames + a resize.

use hl_vulkan::model::memory::{vk_buffer_usage, vk_format};
use hl_vulkan::model::queue::ImageState;
use hl_vulkan::result;
use hl_vulkan::service::{create, present, record, submit, sync};
use hl_vulkan::{Device, Instance};

use hl_gpu::protocol::model::command::Enc;
use hl_gpu::protocol::model::enums::{texture_usage, TextureFormat};
use hl_gpu::{Cmd, RecordingSink};

fn dev() -> Device {
    let inst = Instance::new(result::HL_API_VERSION);
    inst.create_device()
}

/// The backing hl-GPU texture id behind a swapchain image's `VkImage` handle.
fn tex_of(d: &Device, image: u64) -> u32 {
    d.images
        .get(&image)
        .expect("presentable image tracked in dev.images")
        .ir_id
}

// ---------------------------------------------------------------------------------------------------
// 1) vkCreateSwapchainKHR + vkGetSwapchainImagesKHR — the images are tracked real render targets
// ---------------------------------------------------------------------------------------------------

/// `vkCreateSwapchainKHR` mints `image_count` REAL presentable images; `vkGetSwapchainImagesKHR` returns
/// their `VkImage` handles — the correct count, each a distinct, live, `RENDER_TARGET | COPY_SRC` texture
/// sized/formatted from the surface that the shim can genuinely render into (a render pass targeting it
/// lowers to a `BeginRenderPass` naming that image's own texture), stable across repeated queries.
#[test]
fn swapchain_create_and_images_tracks_real_render_targets() {
    const N: u32 = 3;
    const W: u32 = 256;
    const H: u32 = 128;
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let surface =
        present::create_surface(&mut d, &mut sink, W, H, vk_format::B8G8R8A8_UNORM, 0).unwrap();
    let sc = present::create_swapchain(&mut d, &mut sink, surface, N).unwrap();

    // vkGetSwapchainImagesKHR: exactly N handles, all live + distinct + non-null.
    let images = d.swapchain_images(sc).unwrap();
    assert_eq!(
        images.len(),
        N as usize,
        "the swapchain reports its N presentable images"
    );
    let uniq: std::collections::BTreeSet<u64> = images.iter().copied().collect();
    assert_eq!(
        uniq.len(),
        N as usize,
        "each presentable image is a distinct VkImage handle"
    );
    assert!(
        images.iter().all(|&h| h != 0),
        "no VkImage handle is VK_NULL_HANDLE"
    );
    // Real Vulkan returns identical handles on every call — the shim must too.
    assert_eq!(
        d.swapchain_images(sc).unwrap(),
        images,
        "image handles are stable across calls"
    );

    // Each VkImage is a REAL render-target texture the shim can draw into: tracked in dev.images with
    // RENDER_TARGET | COPY_SRC, the surface's extent/format, and its OWN backing texture id (no aliasing).
    let mut texids = std::collections::BTreeSet::new();
    for &h in &images {
        let rec = d
            .images
            .get(&h)
            .expect("presentable image tracked in dev.images");
        assert!(
            rec.is_render_target,
            "a presentable image is a render target"
        );
        assert_ne!(
            rec.usage & texture_usage::RENDER_TARGET,
            0,
            "renderable — the app draws into it"
        );
        assert_ne!(
            rec.usage & texture_usage::COPY_SRC,
            0,
            "copy-source-able — present reads it back"
        );
        assert_eq!((rec.width, rec.height), (W, H), "sized from the surface");
        assert_eq!(
            rec.format,
            TextureFormat::Bgra8Unorm,
            "formatted from the surface"
        );
        texids.insert(rec.ir_id);
    }
    assert_eq!(
        texids.len(),
        N as usize,
        "each image has its own distinct backing texture id"
    );

    // create_swapchain emitted exactly one CreateTexture per presentable image.
    let create_texs = sink
        .batches
        .iter()
        .flatten()
        .filter(|c| matches!(c, Cmd::CreateTexture(..)))
        .count();
    assert_eq!(
        create_texs, N as usize,
        "one CreateTexture emitted per presentable image"
    );

    // Each image is genuinely renderable: a render pass targeting its VkImage handle lowers to a
    // BeginRenderPass naming that image's own texture id.
    for &h in &images {
        let tex = tex_of(&d, h);
        let cb = d.allocate_command_buffer();
        d.begin_command_buffer(cb, false).unwrap();
        record::cmd_begin_render_pass(&mut d, cb, h, [0.0, 0.0, 0.0, 1.0], true, None).unwrap();
        d.end_render_pass(cb).unwrap();
        d.end_command_buffer(cb).unwrap();
        submit::queue_submit(&mut d, &mut sink, &[cb], None).unwrap();
        match sink.batches.last().unwrap().as_slice() {
            [Cmd::Submit(cbuf)] => match cbuf.encoder.as_slice() {
                [Enc::BeginRenderPass { color, .. }, Enc::EndRenderPass] => {
                    assert_eq!(
                        color[0].texture, tex,
                        "a render pass targets the swapchain image's own texture"
                    );
                }
                other => panic!("expected [BeginRenderPass, EndRenderPass], got {other:?}"),
            },
            other => panic!("expected a single Submit, got {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------------------------------
// 2) vkAcquireNextImageKHR round-robin — the present sources the ACQUIRED image every frame
// ---------------------------------------------------------------------------------------------------

/// A multi-frame acquire → render → present loop of MORE than `image_count` iterations: the returned image
/// index genuinely FIFO round-robins (`0,1,..,N-1,0,..`, never pinned to image 0), the acquired image is
/// marked `Acquired` while the app holds it, each frame renders into the acquired image, and the present
/// SOURCES exactly that acquired image's texture (present.source == acquire.result). After a headless
/// present the image returns to the pool.
#[test]
fn acquire_round_robin_present_sources_the_acquired_image() {
    const N: u32 = 3;
    const ITERS: usize = 8; // > N, so the FIFO cycle wraps more than twice
    const W: u32 = 64;
    const H: u32 = 64;
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let surface =
        present::create_surface(&mut d, &mut sink, W, H, vk_format::B8G8R8A8_UNORM, 0).unwrap();
    let sc = present::create_swapchain(&mut d, &mut sink, surface, N).unwrap();
    let images = d.swapchain_images(sc).unwrap();
    let texids: Vec<u32> = images.iter().map(|&h| tex_of(&d, h)).collect();

    let mut acquired = Vec::new();
    for _ in 0..ITERS {
        let idx = d.acquire_next_image(sc).unwrap();
        acquired.push(idx);
        // The acquired image is owned by the app (Acquired) until presented — not re-handed out.
        assert_eq!(
            d.swapchains.get(&sc).unwrap().images[idx as usize].state,
            ImageState::Acquired,
            "the acquired image is marked Acquired"
        );

        // Render THIS frame into the ACQUIRED image.
        let cb = d.allocate_command_buffer();
        d.begin_command_buffer(cb, false).unwrap();
        record::cmd_begin_render_pass(
            &mut d,
            cb,
            images[idx as usize],
            [0.2, 0.4, 0.6, 1.0],
            true,
            None,
        )
        .unwrap();
        d.end_render_pass(cb).unwrap();
        d.end_command_buffer(cb).unwrap();
        submit::queue_submit(&mut d, &mut sink, &[cb], None).unwrap();
        let rendered_tex = match sink.batches.last().unwrap().as_slice() {
            [Cmd::Submit(cbuf)] => match cbuf.encoder.as_slice() {
                [Enc::BeginRenderPass { color, .. }, Enc::EndRenderPass] => color[0].texture,
                other => panic!("expected [BeginRenderPass, EndRenderPass], got {other:?}"),
            },
            other => panic!("expected a single Submit, got {other:?}"),
        };
        assert_eq!(
            rendered_tex, texids[idx as usize],
            "the frame renders into the acquired image"
        );

        // Present it — the present must source the acquired image's texture.
        present::queue_present(&mut d, &mut sink, sc, idx).unwrap();
        match sink.batches.last().unwrap().as_slice() {
            [Cmd::Present { texture, .. }] => {
                assert_eq!(
                    *texture, texids[idx as usize],
                    "the present sources the acquired image's texture"
                )
            }
            other => panic!("expected Present, got {other:?}"),
        }
        // The headless present completes immediately, returning the image to the pool.
        assert_eq!(
            d.swapchains.get(&sc).unwrap().images[idx as usize].state,
            ImageState::Available,
            "the presented image returns to the pool"
        );
    }
    // FIFO round-robin: 0,1,2,0,1,2,0,1 — never stuck at image 0 (the one-frame vkcube abort bug).
    assert_eq!(
        acquired,
        vec![0, 1, 2, 0, 1, 2, 0, 1],
        "acquire cycles through all {N} images in FIFO order"
    );
    assert!(
        acquired.iter().any(|&i| i == N - 1),
        "the whole ring is used, not just image 0"
    );
}

// ---------------------------------------------------------------------------------------------------
// 3) multi-frame resource lifetime — transients reused, no id leak, each frame's stream correct
// ---------------------------------------------------------------------------------------------------

/// A vkcube-style loop where ONE command buffer, ONE fence, and ONE (timeline) semaphore are created up
/// front and REUSED every frame (the cb reset+rerecorded, the fence waited+reset, the semaphore signalled).
/// Asserts: (a) each frame's re-recorded encoder is correct — exactly this frame's acquired image, nothing
/// stale accumulated; (b) the transients are reused, not reallocated (the device tables never grow a
/// per-frame object, the fence keeps its ir id, the swapchain images persist); (c) NO id leak — the whole
/// loop consumes ZERO fresh IR ids (a sentinel buffer minted before/after the loop differ by exactly one).
#[test]
fn multi_frame_resource_lifetime_reuses_transients_without_id_leak() {
    const N: u32 = 2;
    const FRAMES: usize = 6; // > N, so the swapchain ring wraps
    const W: u32 = 32;
    const H: u32 = 32;
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let surface =
        present::create_surface(&mut d, &mut sink, W, H, vk_format::B8G8R8A8_UNORM, 0).unwrap();
    let sc = present::create_swapchain(&mut d, &mut sink, surface, N).unwrap();
    let images = d.swapchain_images(sc).unwrap();
    let texids: Vec<u32> = images.iter().map(|&h| tex_of(&d, h)).collect();

    // ONE reusable command buffer + ONE fence (created signaled, so the first wait passes — vkcube's model)
    // + ONE timeline semaphore, all reused every frame.
    let cb = d.allocate_command_buffer();
    let fence = create::create_fence(&mut d, &mut sink, true).unwrap();
    let sem = sync::create_semaphore(&mut d, true, 0);
    let fence_ir = d.fences.get(&fence).unwrap().ir_id;

    // Sentinel: the ir id the NEXT alloc_ir hands out. The frame loop below must consume ZERO fresh ir ids
    // (no per-frame CreateTexture/CreateBuffer/CreateBindGroup) — begin/record(clear)/end, submit(fence),
    // acquire, present, wait/reset, signal all allocate none. A leak (e.g. re-minting the swap images each
    // frame) would push the after-sentinel past before+1.
    let before =
        create::create_buffer(&mut d, &mut sink, vk_buffer_usage::VERTEX_BUFFER, 16).unwrap();
    let before_ir = d.buffers.get(&before).unwrap().ir_id;

    for frame in 0..FRAMES {
        // Reuse the fence (wait for the prior frame's work, then reset) and reuse the cb (begin resets it).
        submit::wait_for_fence(&mut d, &mut sink, fence).unwrap();
        d.reset_fence(fence).unwrap();

        let idx = d.acquire_next_image(sc).unwrap();
        d.begin_command_buffer(cb, false).unwrap(); // reset_recording clears the prior frame's encoder
        record::cmd_begin_render_pass(
            &mut d,
            cb,
            images[idx as usize],
            [0.1, 0.2, 0.3, 1.0],
            true,
            None,
        )
        .unwrap();
        d.end_render_pass(cb).unwrap();
        d.end_command_buffer(cb).unwrap();
        submit::queue_submit(&mut d, &mut sink, &[cb], Some(fence)).unwrap();

        // The re-recorded encoder is CORRECT: exactly this frame's acquired image, no stale ops appended.
        match sink.batches.last().unwrap().as_slice() {
            [Cmd::Submit(cbuf)] => match cbuf.encoder.as_slice() {
                [Enc::BeginRenderPass { color, .. }, Enc::EndRenderPass] => {
                    assert_eq!(
                        color.len(),
                        1,
                        "frame {frame}: one color target (recording did not accumulate)"
                    );
                    assert_eq!(
                        color[0].texture, texids[idx as usize],
                        "frame {frame}: renders into the acquired image"
                    );
                }
                other => panic!(
                    "frame {frame}: expected [BeginRenderPass, EndRenderPass], got {other:?}"
                ),
            },
            other => panic!("frame {frame}: expected a single Submit, got {other:?}"),
        }

        present::queue_present(&mut d, &mut sink, sc, idx).unwrap();
        match sink.batches.last().unwrap().as_slice() {
            [Cmd::Present { texture, .. }] => {
                assert_eq!(
                    *texture, texids[idx as usize],
                    "frame {frame}: presents the acquired image"
                )
            }
            other => panic!("frame {frame}: expected Present, got {other:?}"),
        }

        // Reuse the timeline semaphore: signal it to a fresh value each frame — its counter advances, but
        // it is the SAME object (never re-created).
        d.signal_semaphore(sem, (frame + 1) as u64).unwrap();
        assert_eq!(
            d.semaphore_counter(sem).unwrap(),
            (frame + 1) as u64,
            "frame {frame}: semaphore reused"
        );

        // The transients are REUSED, not reallocated: the device tables never grow a per-frame object.
        assert_eq!(
            d.command_buffers.len(),
            1,
            "frame {frame}: the one command buffer is reused"
        );
        assert_eq!(d.fences.len(), 1, "frame {frame}: the one fence is reused");
        assert_eq!(
            d.semaphores.len(),
            1,
            "frame {frame}: the one semaphore is reused"
        );
        assert_eq!(
            d.fences.get(&fence).unwrap().ir_id,
            fence_ir,
            "frame {frame}: the fence keeps its backing ir id"
        );
        assert_eq!(
            d.images.len(),
            N as usize,
            "frame {frame}: the swapchain images persist (none minted/dropped)"
        );
    }

    // No id leak: the whole loop consumed ZERO fresh ir ids — the next sentinel is exactly before + 1.
    let after =
        create::create_buffer(&mut d, &mut sink, vk_buffer_usage::VERTEX_BUFFER, 16).unwrap();
    let after_ir = d.buffers.get(&after).unwrap().ir_id;
    assert_eq!(
        after_ir,
        before_ir + 1,
        "the {FRAMES}-frame present loop leaked no ir ids (transients reused)"
    );
    assert_eq!(
        d.images.len(),
        N as usize,
        "exactly the swapchain's own images remain"
    );
}

// ---------------------------------------------------------------------------------------------------
// 4) swapchain recreation (resize) — the old set is retired, the new set tracked, no leak
// ---------------------------------------------------------------------------------------------------

/// Recreating a swapchain on resize retires the OLD swapchain completely: `vkDestroySwapchainKHR` drops its
/// `SwapchainRec`, every presentable image's `ImageRec` (freeing the host texture), and its presentation
/// surface — so `dev.images` holds exactly the LIVE swapchains' images and nothing leaks per resize. The
/// new swapchain's images are tracked at the new extent, and the live swapchain is unaffected. Regression
/// guard: before the retire fix, destroying a swapchain orphaned its images in `dev.images` forever.
#[test]
fn swapchain_recreation_retires_old_images_without_leak() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();

    // Original swapchain A over a 640x480 surface, double-buffered.
    let surf_a =
        present::create_surface(&mut d, &mut sink, 640, 480, vk_format::B8G8R8A8_UNORM, 0).unwrap();
    let sc_a = present::create_swapchain(&mut d, &mut sink, surf_a, 2).unwrap();
    let imgs_a = d.swapchain_images(sc_a).unwrap();
    let texs_a: Vec<u32> = imgs_a.iter().map(|&h| tex_of(&d, h)).collect();
    let surf_a_ir = d.surfaces.get(&surf_a).unwrap().ir_id;
    assert_eq!(d.images.len(), 2, "A's two presentable images are tracked");

    // Resize: build the fresh swapchain B (new 800x600 surface, triple-buffered) BEFORE destroying A — the
    // real app resize order (oldSwapchain kept alive until the new one is ready).
    let surf_b =
        present::create_surface(&mut d, &mut sink, 800, 600, vk_format::B8G8R8A8_UNORM, 0).unwrap();
    let sc_b = present::create_swapchain(&mut d, &mut sink, surf_b, 3).unwrap();
    let imgs_b = d.swapchain_images(sc_b).unwrap();
    assert_eq!(
        d.images.len(),
        5,
        "both swapchains' images coexist momentarily (2 + 3)"
    );
    for &h in &imgs_b {
        let rec = d.images.get(&h).unwrap();
        assert_eq!(
            (rec.width, rec.height),
            (800, 600),
            "B's images carry the NEW extent"
        );
    }

    // Destroy the OLD swapchain A (the resize completes).
    d.destroy_swapchain(&mut sink, sc_a).unwrap();

    // A is FULLY retired — record, every image, and surface gone from the device tables.
    assert!(
        d.swapchains.get(&sc_a).is_none(),
        "A's swapchain record is retired"
    );
    for &h in &imgs_a {
        assert!(
            d.images.get(&h).is_none(),
            "A's presentable image {h:#x} is retired from dev.images (no leak)"
        );
    }
    assert!(
        d.surfaces.get(&surf_a).is_none(),
        "A's presentation surface is retired"
    );
    // The retire emitted the freeing IR: DestroyTexture per image, then DestroySurface.
    let destroyed_texs: Vec<u32> = sink
        .batches
        .iter()
        .flatten()
        .filter_map(|c| match c {
            Cmd::DestroyTexture(id) => Some(*id),
            _ => None,
        })
        .collect();
    assert_eq!(
        destroyed_texs, texs_a,
        "each of A's textures is freed on the host"
    );
    assert!(
        sink.batches
            .iter()
            .flatten()
            .any(|c| matches!(c, Cmd::DestroySurface(id) if *id == surf_a_ir)),
        "A's presentation surface is freed on the host"
    );

    // B — the LIVE swapchain — is untouched: its images all persist and it still presents.
    assert_eq!(
        d.images.len(),
        3,
        "exactly B's three images remain — A leaked nothing"
    );
    for &h in &imgs_b {
        assert!(
            d.images.get(&h).is_some(),
            "B's image {h:#x} survives A's destruction"
        );
    }
    let idx = d.acquire_next_image(sc_b).unwrap();
    assert!(
        present::queue_present(&mut d, &mut sink, sc_b, idx).is_ok(),
        "B still presents after A is gone"
    );

    // The destroyed swapchain is truly dead: a present against it is a truthful error, not a false success.
    assert!(
        present::queue_present(&mut d, &mut sink, sc_a, 0).is_err(),
        "presenting a destroyed swapchain errors"
    );

    // Retiring an already-dead / unknown swapchain is a harmless no-op (VK_NULL_HANDLE) — no double-free.
    d.destroy_swapchain(&mut sink, sc_a).unwrap();
    d.destroy_swapchain(&mut sink, 0).unwrap();
    assert_eq!(d.images.len(), 3, "a no-op retire frees nothing");
}
