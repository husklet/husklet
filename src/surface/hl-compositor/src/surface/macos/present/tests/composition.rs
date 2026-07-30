// Multi-layer role composition: how a frame's layers land inside the role's window-geometry frame.

    /// A GTK/Chrome toplevel commits a buffer larger than its xdg window geometry: the margin is
    /// client-side shadow the compositor must drop. The role's frame texture therefore holds ONLY the
    /// geometry, so a child layer must land at its offset minus the geometry origin and occupy its own
    /// cropped composite — not be re-stretched over the uncropped logical size.
    #[test]
    fn shadow_cropped_role_places_children_at_geometry_relative_offsets() {
        let Some(mut presenter) = MacPresenter::new_offscreen() else {
            eprintln!("SKIP: no Metal device available");
            return;
        };
        let root = SurfaceId(401);
        let child = SurfaceId(402);
        // 4x4 buffer, 2x2 of visible window inset by a 1px shadow margin on every side. Attached bytes are
        // BGRA; `last_rgba` reports them channel-swapped.
        let root_bgra = [0, 0, 255, 255];
        let child_bgra = [255, 0, 0, 255];
        let root_rgba = vec![255, 0, 0, 255];
        let child_rgba = vec![0, 0, 255, 255];
        presenter.attach_bgra(root, root_bgra.repeat(16), 4, 4);
        presenter.attach_bgra(child, child_bgra.to_vec(), 1, 1);
        let window = |surface, geometry| WindowState {
            surface,
            kind: WindowKind::Toplevel { parent: None },
            title: "shadow cropped".into(),
            logical_size: Some((4, 4)),
            min_size: (None, None),
            max_size: (None, None),
            maximized: false,
            fullscreen: false,
            geometry: Some(geometry),
            visibility: Visibility::Visible,
        };
        presenter.reconcile_window(&window(root, Rect::new(1, 1, 2, 2)));
        let image = |surface, width, height| PresentableImage {
            surface,
            width,
            height,
            format: Format::Argb8888,
            gpu: false,
            popup: None,
            present_crop: None,
            transform: BufferTransform::Normal,
        };
        // The child sits at root-relative logical (2, 2) — one pixel inside the visible geometry.
        assert_eq!(presenter.compose(&image(root, 4, 4), &[]), Ok((2, 2)));
        assert_eq!(presenter.compose(&image(child, 1, 1), &[]), Ok((1, 1)));
        presenter.present_frame(&crate::scene::port::PresentFrame {
            output: OutputId(1),
            role: root,
            layers: vec![
                crate::scene::port::PresentLayer {
                    image: image(root, 4, 4),
                    x: 0,
                    y: 0,
                    damage: Vec::new(),
                },
                crate::scene::port::PresentLayer {
                    image: image(child, 1, 1),
                    x: 2,
                    y: 2,
                    damage: Vec::new(),
                },
            ],
            timing: PresentTiming::fallback(1, 1),
        });

        let (width, height, rgba) = presenter.last_rgba(root).expect("composed role frame");
        // The frame is the geometry, not the shadowed buffer.
        assert_eq!((width, height), (2, 2));
        // Geometry-relative (1, 1) is the child; every other pixel stays root blue.
        let pixel = |x: u32, y: u32| {
            let start = ((y * width + x) * 4) as usize;
            rgba[start..start + 4].to_vec()
        };
        assert_eq!(pixel(1, 1), child_rgba, "child at geometry-relative (1,1)");
        for (x, y) in [(0, 0), (1, 0), (0, 1)] {
            assert_eq!(pixel(x, y), root_rgba, "root pixel at ({x},{y})");
        }
    }
