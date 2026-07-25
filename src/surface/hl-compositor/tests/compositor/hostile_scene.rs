use super::*;

// Rect edges, translation, union, containment, intersection, and damage bounds.

#[test]
fn rect_edges_survive_extreme_coordinates() {
    // right()/bottom() = x + w. Both near i32::MAX must not overflow (was a raw `+`).
    let r = Rect::new(i32::MAX - 1, i32::MAX - 1, 1_000_000, 1_000_000);
    let _ = r.right();
    let _ = r.bottom();
    let _ = r.is_empty();
    let _ = r.contains_point(0, 0);
    // A rect with i32::MIN origin and large extent.
    let lo = Rect::new(i32::MIN, i32::MIN, i32::MAX, i32::MAX);
    let _ = lo.right();
    let _ = lo.bottom();
    let _ = lo.contains_point(0, 0);
}

#[test]
fn rect_translate_saturates_instead_of_overflowing() {
    // Lifting a surface-local rect into root space by a hostile subsurface offset.
    let r = Rect::new(i32::MAX - 5, i32::MAX - 5, 10, 10);
    let t = r.translate(i32::MAX, i32::MAX);
    // No panic; the coordinate saturates rather than wrapping negative.
    assert!(t.x >= i32::MAX - 5);
    let t2 = Rect::new(i32::MIN + 5, i32::MIN + 5, 10, 10).translate(i32::MIN, i32::MIN);
    assert!(t2.x <= i32::MIN + 5);
}

#[test]
fn rect_union_contains_intersects_survive_overflow() {
    let a = Rect::new(i32::MIN, i32::MIN, i32::MAX, i32::MAX);
    let b = Rect::new(0, 0, i32::MAX, i32::MAX);
    // union computes right()/bottom() and (right - x): all must be overflow-safe.
    let u = a.union(&b);
    assert!(!u.is_empty());
    let _ = a.contains(&b);
    let _ = b.contains(&a);
    let _ = a.intersects(&b);
    // A normal union is still exact (guard only bites the extremes).
    assert_eq!(
        Rect::new(10, 10, 20, 20).union(&Rect::new(50, 5, 10, 10)),
        Rect::new(10, 5, 50, 25)
    );
}

// =================================================================================================
// 2. DamageRegion accumulator with overflowing origin + extent
// =================================================================================================

#[test]
fn damage_region_bounding_box_survives_overflowing_rects() {
    let mut d = DamageRegion::new();
    d.add(Rect::new(i32::MAX - 10, i32::MAX - 10, 1000, 1000));
    d.add(Rect::new(i32::MIN, i32::MIN, 2000, 2000));
    d.add(Rect::new(0, 0, i32::MAX, i32::MAX));
    // bounding_box folds union() over all of them — must not overflow.
    let bb = d.bounding_box();
    assert!(bb.is_some());
    assert!(!bb.unwrap().is_empty());
}
