use super::*;
#[test]
fn rect_empty_and_negative_sizes_cover_nothing() {
    assert!(Rect::new(0, 0, 0, 10).is_empty(), "zero width is empty");
    assert!(Rect::new(0, 0, 10, 0).is_empty(), "zero height is empty");
    assert!(Rect::new(0, 0, -5, 5).is_empty(), "negative width is empty");
    // An empty rect contains no point and no rect, and is contained by none.
    let empty = Rect::new(0, 0, 0, 0);
    assert!(!empty.contains_point(0, 0));
    assert!(
        !Rect::new(0, 0, 100, 100).contains(&empty),
        "empty target is never 'contained'"
    );
    assert!(!empty.contains(&Rect::new(0, 0, 1, 1)));
    assert!(!empty.intersects(&Rect::new(-5, -5, 100, 100)));
}

#[test]
fn rect_contains_point_is_half_open() {
    let r = Rect::new(10, 20, 30, 40); // x:[10,40), y:[20,60)
    assert!(r.contains_point(10, 20), "top-left inclusive");
    assert!(r.contains_point(39, 59), "just inside the far edge");
    assert!(!r.contains_point(40, 59), "right edge exclusive");
    assert!(!r.contains_point(39, 60), "bottom edge exclusive");
    assert!(!r.contains_point(9, 20), "left of the rect");
    assert!(!r.contains_point(10, 19), "above the rect");
}

#[test]
fn rect_contains_rect_requires_full_containment() {
    let outer = Rect::new(0, 0, 100, 100);
    assert!(
        outer.contains(&Rect::new(0, 0, 100, 100)),
        "equal rect is contained"
    );
    assert!(outer.contains(&Rect::new(10, 10, 80, 80)));
    assert!(
        !outer.contains(&Rect::new(50, 50, 60, 10)),
        "spills past the right edge"
    );
    assert!(
        !outer.contains(&Rect::new(-1, 0, 10, 10)),
        "starts left of the outer"
    );
}

#[test]
fn rect_intersects_touching_edges_do_not_overlap() {
    let a = Rect::new(0, 0, 10, 10);
    assert!(
        !a.intersects(&Rect::new(10, 0, 10, 10)),
        "edge-adjacent = no positive-area overlap"
    );
    assert!(!a.intersects(&Rect::new(0, 10, 10, 10)));
    assert!(a.intersects(&Rect::new(9, 9, 10, 10)), "1px overlap counts");
}

#[test]
fn rect_union_ignores_empties_and_bounds_both() {
    let a = Rect::new(10, 10, 20, 20);
    let empty = Rect::new(0, 0, 0, 0);
    assert_eq!(
        a.union(&empty),
        a,
        "union with empty returns the non-empty side"
    );
    assert_eq!(empty.union(&a), a);
    assert_eq!(empty.union(&empty), empty);
    let b = Rect::new(50, 5, 10, 10);
    let u = a.union(&b);
    assert_eq!(
        u,
        Rect::new(10, 5, 50, 25),
        "union spans both extents (x:10..60, y:5..30)"
    );
}

#[test]
fn rect_translate_moves_origin_keeps_size() {
    let r = Rect::new(5, 6, 7, 8).translate(-10, 100);
    assert_eq!(r, Rect::new(-5, 106, 7, 8));
}

// =================================================================================================
// 2. DamageRegion accumulator
// =================================================================================================

#[test]
fn damage_region_drops_empties_and_bounds_the_rest() {
    let mut d = DamageRegion::new();
    assert!(d.is_empty());
    assert_eq!(d.bounding_box(), None, "no damage => no bounding box");
    d.add(Rect::new(0, 0, 0, 0)); // empty ignored
    d.add(Rect::new(0, 0, -3, 5)); // negative ignored
    assert!(
        d.is_empty(),
        "only empty/negative rects added => still empty"
    );
    d.add(Rect::new(10, 10, 5, 5));
    d.add(Rect::new(100, 0, 5, 5));
    assert_eq!(d.bounding_box(), Some(Rect::new(10, 0, 95, 15)));
    d.clear();
    assert!(d.is_empty() && d.bounding_box().is_none());
}

// =================================================================================================
// 3. Output: logical size + refresh derivation, boundary scales
// =================================================================================================

#[test]
fn output_logical_size_divides_by_scale_and_clamps() {
    let o = Output::new(OutputId(1), "o", 2560, 1440, 60_000).with_scale(2);
    assert_eq!(o.logical_size(), (1280, 720));
    // A scale larger than the mode clamps each axis to >= 1 (never zero-size).
    let tiny = Output::new(OutputId(2), "o", 1, 1, 60_000).with_scale(4);
    assert_eq!(tiny.logical_size(), (1, 1));
    // with_scale never accepts < 1.
    let z = Output::new(OutputId(3), "o", 800, 600, 60_000).with_scale(0);
    assert_eq!(z.scale, 1, "scale is clamped up to 1");
}

#[test]
fn output_refresh_nanos_handles_unknown_rate() {
    assert_eq!(
        Output::new(OutputId(1), "o", 1, 1, 60_000).refresh_nanos(),
        16_666_666
    );
    assert_eq!(
        Output::new(OutputId(1), "o", 1, 1, 0).refresh_nanos(),
        0,
        "unknown rate => 0"
    );
    assert_eq!(
        Output::new(OutputId(1), "o", 1, 1, -1).refresh_nanos(),
        0,
        "negative rate => 0"
    );
}

#[test]
fn scene_output_logical_size_falls_back_without_output() {
    let empty = Scene::new();
    assert_eq!(
        empty.output_logical_size(),
        (1000, 700),
        "no output => sane fallback"
    );
    let scene = scene_with_output();
    assert_eq!(scene.output_logical_size(), (2560, 1440));
}

// =================================================================================================
// 4. BufferState logical size: viewport, buffer scale, HiDPI, degenerate sizes
// =================================================================================================

#[test]
fn buffer_logical_size_honours_viewport_dst_then_src_then_scale() {
    // dst wins over everything.
    let b = shm(800, 600);
    let vp_dst = Viewport {
        dst: Some((320, 240)),
        src: None,
    };
    assert_eq!(b.logical_size(&vp_dst, BufferTransform::Normal), (320, 240));

    // A src crop's size wins when no dst.
    let vp_src = Viewport {
        dst: None,
        src: Some((0.0, 0.0, 100.4, 50.6)),
    };
    assert_eq!(
        b.logical_size(&vp_src, BufferTransform::Normal),
        (100, 51),
        "src size rounds to nearest, >=1"
    );

    // Neither: tex / buffer_scale (HiDPI), clamped to >= 1.
    let hidpi = BufferState {
        buffer_scale: 2,
        ..shm(800, 600)
    };
    assert_eq!(
        hidpi.logical_size(&Viewport::default(), BufferTransform::Normal),
        (400, 300)
    );

    // A degenerate dst (0 or negative) is ignored and falls through to scale.
    let vp_bad_dst = Viewport {
        dst: Some((0, 240)),
        src: None,
    };
    assert_eq!(
        b.logical_size(&vp_bad_dst, BufferTransform::Normal),
        (800, 600),
        "zero dst dimension ignored"
    );
}

#[test]
fn buffer_logical_size_never_zero_for_tiny_buffers() {
    // A 1×1 buffer at scale 4 must not collapse to 0×0.
    let b = BufferState {
        buffer_scale: 4,
        ..shm(1, 1)
    };
    assert_eq!(
        b.logical_size(&Viewport::default(), BufferTransform::Normal),
        (1, 1)
    );
}

#[test]
fn format_opacity_classification() {
    assert!(Format::Xrgb8888.is_opaque());
    assert!(!Format::Argb8888.is_opaque());
}

// =================================================================================================
// 5. commit_surface: malformed / out-of-order / unknown-surface paths
// =================================================================================================
