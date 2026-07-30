mod tests {
    use super::*;
    use crate::scene::model::{BufferTransform, Format};

    #[test]
    fn pixel_stats_classify_empty_zero_and_mixed_planes() {
        assert_eq!(
            PixelStats::from_bytes(&[]),
            PixelStats {
                min: 0,
                max: 0,
                nonzero: 0
            }
        );
        assert_eq!(
            PixelStats::from_bytes(&[0; 4]),
            PixelStats {
                min: 0,
                max: 0,
                nonzero: 0
            }
        );
        assert_eq!(
            PixelStats::from_bytes(&[0, 3, 255, 7]),
            PixelStats {
                min: 0,
                max: 255,
                nonzero: 3
            }
        );
    }

    #[test]
    fn direct_iosurface_attach_retains_content_after_source_lease_drops() {
        let Some(mut presenter) = MacPresenter::new_offscreen() else {
            eprintln!("SKIP: no Metal device available");
            return;
        };
        let source = IOSurface::new_bgra(4, 3).expect("allocate IOSurface source");
        // IOSurface and Metal rows are top-down. Distinct X/Y sentinels prove composition neither flips Y
        // nor rotates the image while retaining the source lease.
        let mut pixels = vec![[20, 130, 240, 255]; 12];
        pixels[0] = [0, 255, 0, 255]; // top-left GREEN
        pixels[3] = [0, 255, 255, 255]; // top-right YELLOW
        pixels[8] = [0, 0, 255, 255]; // bottom-left RED
        pixels[11] = [255, 0, 0, 255]; // bottom-right BLUE
        source
            .write_bgra(&pixels.into_iter().flatten().collect::<Vec<_>>())
            .expect("fill source");
        let sid = SurfaceId(91);

        presenter.attach_iosurface(sid, source.clone());
        drop(source);

        let (width, height, bgra) = presenter
            .attached_iosurface_bgra(sid)
            .expect("presenter retained and resolved the attached IOSurface");
        assert_eq!((width, height), (4, 3));
        assert_eq!(&bgra[..4], &[0, 255, 0, 255]);
        assert_eq!(&bgra[44..48], &[255, 0, 0, 255]);

        let image = PresentableImage {
            surface: sid,
            width: 4,
            height: 3,
            format: Format::Argb8888,
            gpu: true,
            popup: None,
            present_crop: None,
            transform: BufferTransform::Normal,
        };
        assert_eq!(presenter.compose(&image, &[]), Ok((4, 3)));
        let (width, height, rgba) = presenter
            .last_rgba(sid)
            .expect("direct IOSurface composed through Metal");
        assert_eq!((width, height), (4, 3));
        assert_eq!(&rgba[0..4], &[0, 255, 0, 255]);
        assert_eq!(&rgba[12..16], &[255, 255, 0, 255]);
        assert_eq!(&rgba[32..36], &[255, 0, 0, 255]);
        assert_eq!(&rgba[44..48], &[0, 0, 255, 255]);
    }

    #[test]
    fn iosurface_sampling_applies_every_buffer_transform_once() {
        let Some(mut presenter) = MacPresenter::new_offscreen() else {
            eprintln!("SKIP: no Metal device available");
            return;
        };
        let transforms = [
            BufferTransform::Normal,
            BufferTransform::_90,
            BufferTransform::_180,
            BufferTransform::_270,
            BufferTransform::Flipped,
            BufferTransform::Flipped90,
            BufferTransform::Flipped180,
            BufferTransform::Flipped270,
        ];
        let source_pixels = [
            [1, 11, 101, 255],
            [2, 12, 102, 255],
            [3, 13, 103, 255],
            [4, 14, 104, 255],
            [5, 15, 105, 255],
            [6, 16, 106, 255],
            [7, 17, 107, 255],
            [8, 18, 108, 255],
            [9, 19, 109, 255],
            [10, 20, 110, 255],
            [11, 21, 111, 255],
            [12, 22, 112, 255],
        ];

        for (index, transform) in transforms.into_iter().enumerate() {
            let sid = SurfaceId(100 + index as u32);
            let cropped_sid = SurfaceId(200 + index as u32);
            let source = IOSurface::new_bgra(4, 3).expect("allocate transformed source");
            source
                .write_bgra(&source_pixels.into_iter().flatten().collect::<Vec<_>>())
                .expect("fill transformed source");
            presenter.attach_iosurface(sid, source.clone());
            presenter.attach_iosurface(cropped_sid, source);
            let (width, height) = transform.surface_size(4, 3);
            let image = PresentableImage {
                surface: sid,
                width,
                height,
                format: Format::Argb8888,
                gpu: true,
                popup: None,
                present_crop: None,
                transform,
            };

            assert_eq!(
                presenter.compose(&image, &[]),
                Ok((width as u32, height as u32))
            );
            let (actual_width, actual_height, actual) =
                presenter.last_rgba(sid).expect("read transformed output");
            assert_eq!((actual_width, actual_height), (width as u32, height as u32));

            let mut expected = vec![[0; 4]; (width * height) as usize];
            for by in 0..3 {
                for bx in 0..4 {
                    let (x, y) = transform.map_point(bx, by, 4, 3);
                    let bgra = source_pixels[(by * 4 + bx) as usize];
                    expected[(y * width + x) as usize] = [bgra[2], bgra[1], bgra[0], bgra[3]];
                }
            }
            assert_eq!(
                actual,
                expected.into_iter().flatten().collect::<Vec<_>>(),
                "wrong sampling for {transform:?}"
            );

            let cropped = PresentableImage {
                surface: cropped_sid,
                width: 2,
                height: 1,
                present_crop: Some((1.0, 1.0, 2.0, 1.0)),
                ..image
            };
            assert_eq!(presenter.compose(&cropped, &[]), Ok((2, 1)));
            let (_, _, actual_crop) = presenter
                .last_rgba(cropped_sid)
                .expect("read transformed crop");
            let expected_crop = expected_surface_row(&source_pixels, transform, 1, 1, 2);
            assert_eq!(
                actual_crop, expected_crop,
                "wrong crop sampling for {transform:?}"
            );
        }
    }

    #[test]
    fn iosurface_crop_scales_to_presentable_destination() {
        let Some(mut presenter) = MacPresenter::new_offscreen() else {
            eprintln!("SKIP: no Metal device available");
            return;
        };
        let sid = SurfaceId(300);
        let source = IOSurface::new_bgra(4, 3).expect("allocate crop-scale source");
        let pixels = [
            [1, 11, 101, 255],
            [2, 12, 102, 255],
            [3, 13, 103, 255],
            [4, 14, 104, 255],
            [5, 15, 105, 255],
            [6, 16, 106, 255],
            [7, 17, 107, 255],
            [8, 18, 108, 255],
            [9, 19, 109, 255],
            [10, 20, 110, 255],
            [11, 21, 111, 255],
            [12, 22, 112, 255],
        ];
        source
            .write_bgra(&pixels.into_iter().flatten().collect::<Vec<_>>())
            .expect("fill crop-scale source");
        presenter.attach_iosurface(sid, source);
        let image = PresentableImage {
            surface: sid,
            width: 4,
            height: 2,
            format: Format::Argb8888,
            gpu: true,
            popup: None,
            present_crop: Some((1.0, 1.0, 2.0, 1.0)),
            transform: BufferTransform::Normal,
        };

        assert_eq!(presenter.compose(&image, &[]), Ok((4, 2)));
        let (width, height, actual) = presenter.last_rgba(sid).expect("scaled crop");
        assert_eq!((width, height), (4, 2));
        let left = [106, 16, 6, 255];
        let right = [107, 17, 7, 255];
        assert_eq!(
            actual,
            [left, left, right, right, left, left, right, right]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
        );
    }

    fn expected_surface_row(
        source: &[[u8; 4]; 12],
        transform: BufferTransform,
        x: i32,
        y: i32,
        width: i32,
    ) -> Vec<u8> {
        let (surface_width, surface_height) = transform.surface_size(4, 3);
        let mut transformed = vec![[0; 4]; (surface_width * surface_height) as usize];
        for by in 0..3 {
            for bx in 0..4 {
                let (surface_x, surface_y) = transform.map_point(bx, by, 4, 3);
                let bgra = source[(by * 4 + bx) as usize];
                transformed[(surface_y * surface_width + surface_x) as usize] =
                    [bgra[2], bgra[1], bgra[0], bgra[3]];
            }
        }
        transformed[(y * surface_width + x) as usize..(y * surface_width + x + width) as usize]
            .iter()
            .flatten()
            .copied()
            .collect()
    }

    #[test]
    fn offscreen_composition_does_not_consume_native_drawable_slots() {
        let Some(mut presenter) = MacPresenter::new_offscreen() else {
            eprintln!("SKIP: no Metal device available");
            return;
        };
        let sid = SurfaceId(92);
        presenter.attach_iosurface(
            sid,
            IOSurface::new_bgra(4, 3).expect("allocate IOSurface source"),
        );
        let image = PresentableImage {
            surface: sid,
            width: 4,
            height: 3,
            format: Format::Argb8888,
            gpu: true,
            popup: None,
            present_crop: None,
            transform: BufferTransform::Normal,
        };
        let timing = PresentTiming::fallback(1, 1);
        let frame = |image: PresentableImage| crate::scene::port::PresentFrame {
            output: OutputId(1),
            role: image.surface,
            layers: vec![crate::scene::port::PresentLayer {
                image,
                x: 0,
                y: 0,
                damage: Vec::new(),
            }],
            timing,
        };
        presenter.present_frame(&frame(image.clone()));
        presenter.present_frame(&frame(image.clone()));
        assert!(presenter.surfaces[&sid].native_presents.is_empty());

        let resized = PresentableImage {
            width: 2,
            ..image.clone()
        };
        presenter.present_frame(&frame(resized));
        assert!(presenter.surfaces[&sid].native_presents.is_empty());
        assert_eq!(
            presenter
                .surfaces
                .get(&sid)
                .and_then(|state| state.composite.as_ref())
                .map(|(width, height, _)| (*width, *height)),
            Some((2, 3))
        );
    }

    #[test]
    fn role_composite_rebuild_removes_and_moves_child_without_ghosts() {
        let Some(mut presenter) = MacPresenter::new_offscreen() else {
            return;
        };
        let root = SurfaceId(101);
        let child = SurfaceId(102);
        presenter.attach_bgra(root, [0, 0, 255, 255].repeat(16), 4, 4);
        presenter.attach_bgra(child, [255, 0, 0, 255].repeat(4), 2, 2);
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
        let layer = |surface, width, height, x, y| crate::scene::port::PresentLayer {
            image: image(surface, width, height),
            x,
            y,
            damage: vec![Rect::new(x, y, width, height)],
        };
        let frame = |layers| crate::scene::port::PresentFrame {
            output: OutputId(1),
            role: root,
            layers,
            timing: PresentTiming::fallback(1, 1),
        };

        presenter.present_frame(&frame(vec![
            layer(root, 4, 4, 0, 0),
            layer(child, 2, 2, 0, 0),
        ]));
        let first = presenter.last_rgba(root).unwrap().2;
        assert_eq!(&first[..4], &[0, 0, 255, 255]);

        presenter.present_frame(&frame(vec![layer(root, 4, 4, 0, 0)]));
        let removed = presenter.last_rgba(root).unwrap().2;
        assert_eq!(&removed[..4], &[255, 0, 0, 255]);

        presenter.present_frame(&frame(vec![
            layer(root, 4, 4, 0, 0),
            layer(child, 2, 2, 2, 2),
        ]));
        let moved = presenter.last_rgba(root).unwrap().2;
        assert_eq!(&moved[..4], &[255, 0, 0, 255]);
        let bottom_right = ((3 * 4 + 3) * 4) as usize;
        assert_eq!(&moved[bottom_right..bottom_right + 4], &[0, 0, 255, 255]);
    }

    include!("tests/composition.rs");

    #[test]
    fn destination_scale_changes_are_bounded_and_reversible() {
        assert_eq!(destination_pixels((640, 480), 1.0), Ok((640, 480)));
        assert_eq!(destination_pixels((640, 480), 2.0), Ok((1280, 960)));
        assert_eq!(destination_pixels((640, 480), 1.0), Ok((640, 480)));
        assert!(destination_pixels((MAX_PRESENT_DIM as i32, 1), 2.0).is_err());
        assert!(destination_pixels((MAX_PRESENT_DIM as i32, MAX_PRESENT_DIM as i32), 1.0).is_err());
        assert!(destination_pixels((0, 480), 1.0).is_err());
        assert!(destination_pixels((640, 480), f64::NAN).is_err());
    }

    #[test]
    fn backing_scale_transition_invalidates_static_native_submission() {
        let mut state = SurfState::new();
        state.native_submission = Some(PresentationKey {
            transform: BufferTransform::Normal,
            crop: None,
            width: 640,
            height: 480,
            format: Format::Argb8888,
        });

        assert!(!state.observe_backing_scale(1.0));
        assert!(state.native_submission.is_some());
        assert!(state.observe_backing_scale(2.0));
        assert!(state.native_submission.is_none());
        assert!(!state.observe_backing_scale(2.0));
        assert!(state.observe_backing_scale(1.0));
    }

    #[test]
    fn hostile_geometry_arithmetic_is_widened_and_rejected_before_allocation() {
        let Some(mut presenter) = MacPresenter::new_offscreen() else {
            eprintln!("SKIP: no Metal device available");
            return;
        };
        let sid = SurfaceId(301);
        presenter.attach_iosurface(
            sid,
            IOSurface::new_bgra(1, 1).expect("allocate hostile geometry source"),
        );
        presenter.reconcile_window(&WindowState {
            surface: sid,
            kind: WindowKind::Toplevel { parent: None },
            title: "hostile geometry".into(),
            logical_size: Some((1, 1)),
            min_size: (None, None),
            max_size: (None, None),
            maximized: false,
            fullscreen: false,
            geometry: Some(Rect::new(i32::MAX, i32::MAX, i32::MAX, i32::MAX)),
            visibility: Visibility::Visible,
        });
        let image = PresentableImage {
            surface: sid,
            width: 1,
            height: 1,
            format: Format::Argb8888,
            gpu: true,
            popup: None,
            present_crop: None,
            transform: BufferTransform::Normal,
        };

        let error = presenter
            .compose(&image, &[])
            .expect_err("hostile geometry must fail");
        assert!(error.contains("exceeds"), "{error}");
    }
}
