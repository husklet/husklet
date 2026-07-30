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
    let mapped = Region::new(&Some(region))
        .input()
        .expect("a set region maps");
    assert!(mapped.contains_point(100, 0));
    assert!(mapped.contains_point(199, 149));
    assert!(!mapped.contains_point(99, 0));
    assert!(!mapped.contains_point(100, 150));
    assert_eq!(mapped.bounding_box(), Some(Rect::new(100, 0, 100, 150)));
}

#[test]
fn input_region_empty_is_click_through() {
    // A region object with NO rects => the surface accepts input NOWHERE (click-through overlay).
    let mapped = Region::new(&Some(RegionAttributes { rects: vec![] }))
        .input()
        .expect("a set region always maps to Some(region)");
    assert!(
        mapped.is_empty(),
        "empty input region must reject all input"
    );
    assert!(!mapped.contains_point(0, 0));
}

#[test]
fn input_region_of_disjoint_rects_rejects_the_gap_between_them() {
    // Two disjoint add rects must NOT become their bounding box: a press in the gap belongs to whatever
    // is behind this surface, and accepting it steals the click.
    let region = RegionAttributes {
        rects: vec![add(0, 0, 10, 10), add(90, 90, 10, 10)],
    };
    let mapped = Region::new(&Some(region))
        .input()
        .expect("a set region maps");
    assert!(mapped.contains_point(5, 5));
    assert!(mapped.contains_point(95, 95));
    assert!(
        !mapped.contains_point(50, 50),
        "the gap between two disjoint input rects must not accept input"
    );
}

#[test]
fn input_region_hole_rejects_input_inside_the_hole() {
    // A shaped region — a rounded-corner window or an overlay with a cut-out — subtracts from its own
    // rect. Input inside the subtracted hole must fall through, in issue order.
    let region = RegionAttributes {
        rects: vec![add(0, 0, 100, 100), subtract(40, 40, 20, 20)],
    };
    let mapped = Region::new(&Some(region))
        .input()
        .expect("a set region maps");
    assert!(mapped.contains_point(10, 10));
    assert!(mapped.contains_point(39, 39));
    assert!(
        !mapped.contains_point(45, 45),
        "input inside a subtracted hole must not be accepted"
    );
    assert!(mapped.contains_point(60, 60));
}

#[test]
fn input_region_re_adds_over_a_hole_in_issue_order() {
    // add, subtract, then add again over the same pixels: the LAST operation covering a point wins.
    let region = RegionAttributes {
        rects: vec![
            add(0, 0, 100, 100),
            subtract(40, 40, 20, 20),
            add(45, 45, 5, 5),
        ],
    };
    let mapped = Region::new(&Some(region))
        .input()
        .expect("a set region maps");
    assert!(mapped.contains_point(47, 47));
    assert!(!mapped.contains_point(41, 41));
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
