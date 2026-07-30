//! Hostile-GEOMETRY sweep of the compositor's SCENE / COMPOSE / PRESENT pixel & rect math (task #205).
//!
//! `tests/wayland_serve_adversarial.rs` / `#173` covered the PROTOCOL surface — smithay's guarded wire
//! handling. This file targets OUR pixel and geometry arithmetic DIRECTLY: the same class of bug the
//! driver sweeps found everywhere (attacker size/offset → unchecked add/mul → panic, OOB slice, or a
//! multi-GiB allocation). Every case feeds hostile geometry through `compose_frame` / `is_tree_dirty` /
//! the `Rect` primitives / `BufferState::logical_size` / the `PngPresenter` rasterizer and asserts the
//! math SURVIVES: no panic, no add/mul overflow (these run in debug with overflow-checks on), no
//! out-of-bounds slice, no unbounded allocation — the frame is clamped or skipped cleanly — and a
//! following VALID frame still composes correctly (proving the guard did not wedge the path).

use hl_compositor::scene::model::{
    BufferState, BufferTransform, DamageRegion, Format, Output, OutputId, Rect, Scene,
    SubsurfaceState, SurfaceRole, Viewport,
};
use hl_compositor::scene::service::{commit_surface, BufferChange, Commit};

#[path = "compositor/hostile_scene.rs"]
mod geometry;

// ---- helpers -----------------------------------------------------------------------------------

fn shm(w: i32, h: i32) -> BufferState {
    BufferState {
        tex_w: w,
        tex_h: h,
        format: Format::Argb8888,
        buffer_scale: 1,
        gpu: false,
    }
}

fn scene_with_output() -> Scene {
    let mut scene = Scene::new();
    scene.add_output(Output::new(OutputId(1), "hl-0", 2560, 1440, 60_000));
    scene
}

fn map_toplevel(scene: &mut Scene, w: i32, h: i32) -> hl_compositor::scene::model::SurfaceId {
    let id = scene.create_surface();
    scene.set_role(id, SurfaceRole::Toplevel);
    commit_surface(scene, id, Commit::attach(shm(w, h)));
    id
}

fn sub(parent: hl_compositor::scene::model::SurfaceId, x: i32, y: i32) -> SurfaceRole {
    SurfaceRole::Subsurface(SubsurfaceState {
        parent,
        x,
        y,
        sync: false,
    })
}

// Hostile subsurface offsets, damage translation, logical sizing, and presentation crop.
#[test]
fn compose_survives_absurd_subsurface_offset_chain() {
    // A stack of subsurfaces each placed at an absurd offset — offset accumulation (`x + cx`) in
    // collect_subtree_offsets and the damage translate must not overflow.
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 100, 100);
    let mut parent = top;
    for _ in 0..8 {
        let child = scene.create_surface();
        scene.set_role(child, sub(parent, i32::MAX, i32::MAX));
        commit_surface(&mut scene, child, Commit::attach(shm(50, 50)));
        parent = child;
    }
    // Compose must not panic and must still yield a frame (the root has content).
    let frame = scene.compose_frame(top).expect("root composes");
    assert!(!frame.items.is_empty());
    let _ = frame.damage(); // unions every layer's (translated) damage
    let _ = scene.is_tree_dirty(top);
}

#[test]
fn compose_survives_min_offset_subsurface() {
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 200, 200);
    let child = scene.create_surface();
    scene.set_role(child, sub(top, i32::MIN, i32::MIN));
    commit_surface(&mut scene, child, Commit::attach(shm(64, 64)));
    let frame = scene.compose_frame(top).expect("root composes");
    let _ = frame.damage();
    let _ = scene.is_tree_dirty(top);
    // A valid follow-up compose still works.
    assert!(scene.compose_frame(top).is_some());
}

#[test]
fn compose_survives_out_of_bounds_damage_rect() {
    // A commit that damages a rect with an overflowing origin+extent, then compose translates it.
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 100, 100);
    let child = scene.create_surface();
    scene.set_role(child, sub(top, i32::MAX - 3, 0));
    commit_surface(&mut scene, child, Commit::attach(shm(80, 80)));
    scene.get_mut(child).unwrap().damage.clear();
    commit_surface(
        &mut scene,
        child,
        Commit {
            buffer: BufferChange::Keep,
            ..Commit::default()
        }
        .with_damage(Rect::new(i32::MAX - 2, i32::MAX - 2, i32::MAX, i32::MAX)),
    );
    let frame = scene.compose_frame(top).expect("composes");
    let _ = frame.damage();
    let _ = scene.is_tree_dirty(top);
}

// =================================================================================================
// 4. BufferState::logical_size + compose present_crop under hostile viewport / transform / size
// =================================================================================================

#[test]
fn logical_size_survives_hostile_viewport_and_transform() {
    // Huge tex, huge buffer_scale, huge/degenerate viewport src+dst, all transforms.
    let cases = [
        (
            BufferState {
                buffer_scale: i32::MAX,
                ..shm(i32::MAX, i32::MAX)
            },
            Viewport::default(),
        ),
        (
            shm(i32::MAX, i32::MAX),
            Viewport {
                dst: Some((i32::MAX, i32::MAX)),
                src: None,
            },
        ),
        (
            shm(4, 4),
            Viewport {
                dst: Some((-5, -5)),
                src: None,
            },
        ),
        (
            shm(4, 4),
            Viewport {
                dst: None,
                src: Some((0.0, 0.0, 1e30, 1e30)),
            },
        ),
        (
            shm(4, 4),
            Viewport {
                dst: None,
                src: Some((0.0, 0.0, -1.0, -1.0)),
            },
        ),
        (shm(-8, -8), Viewport::default()),
    ];
    for t in [
        BufferTransform::Normal,
        BufferTransform::_90,
        BufferTransform::_180,
        BufferTransform::_270,
        BufferTransform::Flipped90,
    ] {
        for (b, vp) in &cases {
            let (w, h) = b.logical_size(vp, t);
            assert!(w >= 1 && h >= 1, "logical size never collapses ({w}x{h})");
        }
    }
}

#[test]
fn compose_present_crop_survives_huge_viewport_dst() {
    // A wp_viewport dst larger than any real display drives the composed logical size — compose must
    // still produce an image (present-time rasterization is where the alloc is bounded, tested below).
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 64, 64);
    commit_surface(
        &mut scene,
        top,
        Commit {
            viewport: Some(Viewport {
                dst: Some((i32::MAX, i32::MAX)),
                src: Some((0.0, 0.0, 1e18, 1e18)),
            }),
            buffer_transform: Some(BufferTransform::_270),
            ..Commit::default()
        },
    );
    let frame = scene.compose_frame(top).expect("composes");
    // present_crop is computed in f64 (src * buffer_scale) — no panic, and it is present.
    assert!(frame.items[0].image.present_crop.is_some());
    let _ = frame.items[0].image.width;
}

// =================================================================================================
// 5. PRESENT rasterizer (PngPresenter): transform_buffer + resample_nearest hostile dims
//    (feature-gated: PngPresenter lives behind `smithay-adapter`)
// =================================================================================================

#[cfg(feature = "smithay-adapter")]
mod present_path {
    use hl_compositor::adapter::smithay::{PngPresenter, StoredBuffer};
    use hl_compositor::scene::model::{
        BufferTransform, Format, OutputId, PresentableImage, Rect, SurfaceId,
    };
    use hl_compositor::scene::port::{
        PresentFrame, PresentLayer, PresentOutcome, PresentTiming, Presenter,
    };

    /// Build the one-role, one-layer frame the neutral presenter takes.
    fn single_layer(
        output: OutputId,
        image: PresentableImage,
        damage: &[Rect],
        timing: PresentTiming,
    ) -> PresentFrame {
        PresentFrame {
            output,
            role: image.surface,
            origin: (0, 0),
            layers: vec![PresentLayer {
                image,
                x: 0,
                y: 0,
                damage: damage.to_vec(),
            }],
            timing,
        }
    }

    fn img(
        surface: SurfaceId,
        width: i32,
        height: i32,
        transform: BufferTransform,
        crop: Option<(f64, f64, f64, f64)>,
    ) -> PresentableImage {
        PresentableImage {
            surface,
            width,
            height,
            format: Format::Argb8888,
            gpu: false,
            popup: None,
            present_crop: crop,
            transform,
        }
    }

    fn valid_buf(w: i32, h: i32) -> StoredBuffer {
        StoredBuffer {
            width: w,
            height: h,
            rgba: vec![0x77u8; (w * h * 4) as usize],
            bgra: false,
            damage: None,
        }
    }

    fn drive(p: &mut PngPresenter, buf: StoredBuffer, image: &PresentableImage) -> PresentOutcome {
        p.deposit(image.surface, buf);
        let timing = PresentTiming {
            present_ns: 0,
            refresh_ns: 0,
            vsync: false,
        };
        p.present_frame(&single_layer(OutputId(1), image.clone(), &[], timing))
            .outcome
    }

    /// After ANY hostile frame, a valid frame must still composite to the exact expected pixels — proving
    /// the guard skipped the abuse without wedging the rasterizer.
    fn assert_valid_frame_after(p: &mut PngPresenter) {
        let s = SurfaceId(4242);
        let mut buf = valid_buf(4, 4);
        buf.rgba[0] = 0x11; // mark pixel (0,0) red channel
        let image = img(s, 4, 4, BufferTransform::Normal, None);
        let outcome = drive(p, buf, &image);
        assert!(
            matches!(outcome, PresentOutcome::Delivered { .. }),
            "valid frame delivers after abuse"
        );
        let caps = p.captures();
        let last = caps.lock().unwrap().last().cloned().unwrap();
        assert_eq!((last.width, last.height), (4, 4));
        assert_eq!(
            last.pixel(0, 0).unwrap()[0],
            0x11,
            "valid pixels reach the capture"
        );
    }

    #[test]
    fn resample_survives_huge_viewport_logical_size() {
        // The reachable client attack: wp_viewport dst → a multi-billion logical size flows into
        // resample_nearest's `vec![0u8; dw*dh*4]`. Must be refused (skipped), not allocated / overflowed.
        let mut p = PngPresenter::new();
        let image = img(
            SurfaceId(1),
            2_000_000_000,
            2_000_000_000,
            BufferTransform::Normal,
            Some((0.0, 0.0, 8.0, 8.0)),
        );
        let outcome = drive(&mut p, valid_buf(8, 8), &image);
        assert!(
            matches!(outcome, PresentOutcome::Offscreen),
            "absurd logical size is skipped, not allocated"
        );
        assert_valid_frame_after(&mut p);
    }

    #[test]
    fn resample_survives_dst_near_i32_overflow() {
        let mut p = PngPresenter::new();
        let image = img(
            SurfaceId(2),
            i32::MAX,
            1,
            BufferTransform::Normal,
            Some((0.0, 0.0, 4.0, 4.0)),
        );
        let outcome = drive(&mut p, valid_buf(4, 4), &image);
        assert!(matches!(outcome, PresentOutcome::Offscreen));
        assert_valid_frame_after(&mut p);
    }

    #[test]
    fn resample_survives_zero_dimension_source_buffer() {
        // buf.width == 0 previously drove `clamp(0, buf.width - 1)` == `clamp(0, -1)` which PANICS.
        let mut p = PngPresenter::new();
        for (bw, bh) in [(0, 4), (4, 0), (0, 0)] {
            let buf = StoredBuffer {
                width: bw,
                height: bh,
                rgba: Vec::new(),
                bgra: false,
                damage: None,
            };
            let image = img(
                SurfaceId(3),
                16,
                16,
                BufferTransform::Normal,
                Some((0.0, 0.0, 4.0, 4.0)),
            );
            let outcome = drive(&mut p, buf, &image);
            assert!(
                matches!(outcome, PresentOutcome::Offscreen),
                "degenerate source buffer skipped"
            );
        }
        assert_valid_frame_after(&mut p);
    }

    #[test]
    fn resample_survives_negative_source_dimensions() {
        let mut p = PngPresenter::new();
        let buf = StoredBuffer {
            width: -8,
            height: -8,
            rgba: Vec::new(),
            bgra: false,
            damage: None,
        };
        let image = img(
            SurfaceId(5),
            8,
            8,
            BufferTransform::Normal,
            Some((0.0, 0.0, 4.0, 4.0)),
        );
        assert!(matches!(
            drive(&mut p, buf, &image),
            PresentOutcome::Offscreen
        ));
        assert_valid_frame_after(&mut p);
    }

    #[test]
    fn resample_survives_huge_source_crop_extent() {
        // A valid small buffer + valid logical size, but a src crop with a 1e18 extent: the float
        // sampling must clamp in-bounds and still composite the frame (no OOB / no panic).
        let mut p = PngPresenter::new();
        let image = img(
            SurfaceId(6),
            16,
            16,
            BufferTransform::Normal,
            Some((0.0, 0.0, 1e18, 1e18)),
        );
        let outcome = drive(&mut p, valid_buf(8, 8), &image);
        assert!(
            matches!(outcome, PresentOutcome::Delivered { .. }),
            "clamped crop still composites"
        );
        let caps = p.captures();
        let g = caps.lock().unwrap();
        let last = g.last().unwrap();
        assert_eq!((last.width, last.height), (16, 16));
    }

    #[test]
    fn transform_buffer_survives_huge_dimensions() {
        // transform_buffer allocates `surface_size(w,h) * 4`. Huge w*h overflows i32 and the buffer is
        // inconsistent with its (tiny) rgba — must be refused, not OOB-sliced / overflow-allocated.
        let mut p = PngPresenter::new();
        let buf = StoredBuffer {
            width: 40_000,
            height: 40_000,
            rgba: vec![0u8; 64],
            bgra: false,
            damage: None,
        };
        let image = img(SurfaceId(7), 40_000, 40_000, BufferTransform::_90, None);
        assert!(matches!(
            drive(&mut p, buf, &image),
            PresentOutcome::Offscreen
        ));
        assert_valid_frame_after(&mut p);
    }

    #[test]
    fn transform_buffer_survives_inconsistent_rgba_length() {
        // The buffer claims 100x100 but carries far fewer bytes than 100*100*4 → OOB source slice risk.
        let mut p = PngPresenter::new();
        for t in [
            BufferTransform::_90,
            BufferTransform::_180,
            BufferTransform::_270,
            BufferTransform::Flipped180,
        ] {
            let buf = StoredBuffer {
                width: 100,
                height: 100,
                rgba: vec![0u8; 16],
                bgra: false,
                damage: None,
            };
            let image = img(SurfaceId(8), 100, 100, t, None);
            assert!(
                matches!(drive(&mut p, buf, &image), PresentOutcome::Offscreen),
                "short rgba skipped ({t:?})"
            );
        }
        assert_valid_frame_after(&mut p);
    }

    #[test]
    fn transform_plus_viewport_survives_hostile_swap() {
        // 90/270 swaps buffer w/h; combine with a hostile huge logical size — the composed transform+crop
        // path must refuse the absurd destination, not allocate it.
        let mut p = PngPresenter::new();
        let image = img(
            SurfaceId(9),
            i32::MAX,
            i32::MAX,
            BufferTransform::_270,
            Some((0.0, 0.0, 8.0, 8.0)),
        );
        assert!(matches!(
            drive(&mut p, valid_buf(8, 8), &image),
            PresentOutcome::Offscreen
        ));
        assert_valid_frame_after(&mut p);
    }

    #[test]
    fn transform_only_valid_case_still_rotates() {
        // Regression guard: a legitimate non-square transform still produces the swapped-size frame.
        let mut p = PngPresenter::new();
        let image = img(SurfaceId(10), 2, 4, BufferTransform::_90, None);
        let outcome = drive(&mut p, valid_buf(4, 2), &image);
        assert!(matches!(outcome, PresentOutcome::Delivered { .. }));
        let caps = p.captures();
        let last = caps.lock().unwrap().last().cloned().unwrap();
        assert_eq!((last.width, last.height), (2, 4), "90deg swaps 4x2 -> 2x4");
    }

    #[test]
    fn negative_logical_size_frame_is_skipped_then_valid_composes() {
        let mut p = PngPresenter::new();
        let image = img(SurfaceId(11), -100, -100, BufferTransform::Normal, None);
        // A raw negative buffer presented verbatim: no crop/transform. Must not blow up downstream.
        let buf = StoredBuffer {
            width: -100,
            height: -100,
            rgba: Vec::new(),
            bgra: false,
            damage: None,
        };
        assert!(matches!(
            drive(&mut p, buf, &image),
            PresentOutcome::Offscreen
        ));
        assert_valid_frame_after(&mut p);
    }
}
