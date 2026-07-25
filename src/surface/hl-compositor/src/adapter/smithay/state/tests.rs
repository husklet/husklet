use super::output::Region;
use super::*;
use smithay::utils::{Logical, Rectangle};

pub(super) fn add(x: i32, y: i32, w: i32, h: i32) -> (RectangleKind, Rectangle<i32, Logical>) {
    (
        RectangleKind::Add,
        Rectangle::new((x, y).into(), (w, h).into()),
    )
}
pub(super) fn subtract(x: i32, y: i32, w: i32, h: i32) -> (RectangleKind, Rectangle<i32, Logical>) {
    (
        RectangleKind::Subtract,
        Rectangle::new((x, y).into(), (w, h).into()),
    )
}

#[test]
fn input_region_unset_means_whole_surface() {
    assert_eq!(Region::new(&None).input(), None);
}

#[test]
fn input_region_single_rect_is_exact() {
    // The common case: a client restricts input to a sub-rectangle (e.g. its content minus CSD shadow).
    let region = RegionAttributes {
        rects: vec![add(100, 0, 100, 150)],
    };
    assert_eq!(
        Region::new(&Some(region)).input(),
        Some(Rect::new(100, 0, 100, 150))
    );
}

#[test]
fn input_region_empty_is_click_through() {
    // A region object with NO rects => the surface accepts input NOWHERE (click-through overlay).
    let mapped = Region::new(&Some(RegionAttributes { rects: vec![] }))
        .input()
        .expect("a set region always maps to Some(rect)");
    assert!(
        mapped.is_empty(),
        "empty input region must reject all input"
    );
    assert!(!mapped.contains_point(0, 0));
}

#[test]
fn input_region_multi_rect_is_superset_bounding_box() {
    // Two disjoint add rects reduce to their (safe, over-accepting) bounding box.
    let region = RegionAttributes {
        rects: vec![add(0, 0, 10, 10), add(90, 90, 10, 10)],
    };
    assert_eq!(
        Region::new(&Some(region)).input(),
        Some(Rect::new(0, 0, 100, 100))
    );
}

#[test]
fn opaque_region_unset_is_none() {
    assert_eq!(Region::new(&None).opaque(), None);
}

#[test]
fn opaque_region_single_rect_is_trusted() {
    let region = RegionAttributes {
        rects: vec![add(0, 0, 200, 150)],
    };
    assert_eq!(
        Region::new(&Some(region)).opaque(),
        Some(Rect::new(0, 0, 200, 150))
    );
}

#[test]
fn opaque_region_with_hole_is_dropped_conservatively() {
    // A subtracted hole can't be a single opaque rect without over-claiming => prove nothing opaque.
    let region = RegionAttributes {
        rects: vec![add(0, 0, 200, 150), subtract(10, 10, 20, 20)],
    };
    assert_eq!(Region::new(&Some(region)).opaque(), None);
}

#[test]
fn opaque_region_multi_rect_is_dropped_conservatively() {
    let region = RegionAttributes {
        rects: vec![add(0, 0, 10, 10), add(90, 90, 10, 10)],
    };
    assert_eq!(Region::new(&Some(region)).opaque(), None);
}
