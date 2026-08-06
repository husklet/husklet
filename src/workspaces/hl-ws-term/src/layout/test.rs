use super::*;

const AREA: Rect = Rect {
    x: 0.0,
    y: 0.0,
    w: 100.0,
    h: 80.0,
};

fn rect_of(layout: &Layout, area: Rect, gap: f32, pane: PaneId) -> Rect {
    layout
        .rects(area, gap)
        .into_iter()
        .find(|(id, _)| *id == pane)
        .expect("pane should exist")
        .1
}

fn approx(a: f32, b: f32) {
    assert!((a - b).abs() < 1e-3, "expected {a} ≈ {b}");
}

#[test]
fn single_pane_fills_area() {
    let l = Layout::new(1);
    let rects = l.rects(AREA, 0.0);
    assert_eq!(rects.len(), 1);
    assert_eq!(rects[0].0, 1);
    assert_eq!(rects[0].1, AREA);
    assert_eq!(l.panes(), vec![1]);
    assert_eq!(l.focused(), 1);
}

#[test]
fn vertical_split_is_side_by_side() {
    let mut l = Layout::new(1);
    l.split(1, Dir::Vertical, 2, 0.5);
    let ra = rect_of(&l, AREA, 0.0, 1);
    let rb = rect_of(&l, AREA, 0.0, 2);
    approx(ra.h, 80.0);
    approx(rb.h, 80.0);
    approx(ra.y, 0.0);
    approx(rb.y, 0.0);
    approx(ra.w + rb.w, 100.0);
    approx(ra.w, 50.0);
    approx(rb.w, 50.0);
    approx(rb.x, ra.x + ra.w);
    assert!(rb.x > ra.x);
}

#[test]
fn horizontal_split_stacks() {
    let mut l = Layout::new(1);
    l.split(1, Dir::Horizontal, 2, 0.5);
    let ra = rect_of(&l, AREA, 0.0, 1);
    let rb = rect_of(&l, AREA, 0.0, 2);
    approx(ra.w, 100.0);
    approx(rb.w, 100.0);
    approx(ra.h + rb.h, 80.0);
    approx(ra.h, 40.0);
    approx(rb.h, 40.0);
    approx(rb.y, ra.y + ra.h);
    assert!(rb.y > ra.y);
}

#[test]
fn ratio_quarter_gives_first_pane_a_quarter() {
    let mut l = Layout::new(1);
    l.split(1, Dir::Vertical, 2, 0.25);
    let ra = rect_of(&l, AREA, 0.0, 1);
    let rb = rect_of(&l, AREA, 0.0, 2);
    approx(ra.w, 25.0);
    approx(rb.w, 75.0);
}

#[test]
fn nested_split_produces_three_rects() {
    let mut l = Layout::new(1);
    l.split(1, Dir::Vertical, 2, 0.5);
    l.split(2, Dir::Horizontal, 3, 0.5);
    let rects = l.rects(AREA, 0.0);
    assert_eq!(rects.len(), 3);
    let r1 = rect_of(&l, AREA, 0.0, 1);
    let r2 = rect_of(&l, AREA, 0.0, 2);
    let r3 = rect_of(&l, AREA, 0.0, 3);
    approx(r1.w, 50.0);
    approx(r1.h, 80.0);
    approx(r2.x, 50.0);
    approx(r3.x, 50.0);
    approx(r2.w, 50.0);
    approx(r3.w, 50.0);
    approx(r2.h, 40.0);
    approx(r3.h, 40.0);
    approx(r2.y, 0.0);
    approx(r3.y, 40.0);
    assert_eq!(l.panes(), vec![1, 2, 3]);
}

#[test]
fn split_moves_focus_to_new() {
    let mut l = Layout::new(1);
    assert_eq!(l.focused(), 1);
    l.split(1, Dir::Vertical, 2, 0.5);
    assert_eq!(l.focused(), 2);
    l.split(2, Dir::Horizontal, 3, 0.5);
    assert_eq!(l.focused(), 3);
}

#[test]
fn split_on_missing_target_is_noop() {
    let mut l = Layout::new(1);
    l.split(99, Dir::Vertical, 2, 0.5);
    assert_eq!(l.panes(), vec![1]);
    assert_eq!(l.focused(), 1);
}

#[test]
fn close_lets_sibling_fill_parent_rect() {
    let mut l = Layout::new(1);
    l.split(1, Dir::Vertical, 2, 0.5);
    assert!(l.close(2));
    assert_eq!(l.panes(), vec![1]);
    assert_eq!(rect_of(&l, AREA, 0.0, 1), AREA);
}

#[test]
fn close_collapses_nested_split_to_sibling_subtree() {
    let mut l = Layout::new(1);
    l.split(1, Dir::Vertical, 2, 0.5);
    l.split(2, Dir::Horizontal, 3, 0.5);
    assert!(l.close(2));
    assert_eq!(l.panes(), vec![1, 3]);
    let r3 = rect_of(&l, AREA, 0.0, 3);
    approx(r3.x, 50.0);
    approx(r3.w, 50.0);
    approx(r3.h, 80.0);
}

#[test]
fn closing_last_pane_returns_false_and_keeps_it() {
    let mut l = Layout::new(1);
    assert!(!l.close(1));
    assert_eq!(l.panes(), vec![1]);
    assert_eq!(l.focused(), 1);
}

#[test]
fn closing_focused_moves_focus_to_survivor() {
    let mut l = Layout::new(1);
    l.split(1, Dir::Vertical, 2, 0.5);
    assert_eq!(l.focused(), 2);
    assert!(l.close(2));
    let survivors = l.panes();
    assert!(survivors.contains(&l.focused()));
    assert_eq!(l.focused(), 1);
}

#[test]
fn closing_unfocused_keeps_focus() {
    let mut l = Layout::new(1);
    l.split(1, Dir::Vertical, 2, 0.5);
    l.set_focus(1);
    assert!(l.close(2));
    assert_eq!(l.focused(), 1);
}

#[test]
fn close_missing_pane_returns_false() {
    let mut l = Layout::new(1);
    l.split(1, Dir::Vertical, 2, 0.5);
    assert!(!l.close(99));
    assert_eq!(l.panes(), vec![1, 2]);
}

#[test]
fn set_focus_only_accepts_present_panes() {
    let mut l = Layout::new(1);
    l.split(1, Dir::Vertical, 2, 0.5);
    l.set_focus(1);
    assert_eq!(l.focused(), 1);
    l.set_focus(99);
    assert_eq!(l.focused(), 1);
}

#[test]
fn focus_next_wraps() {
    let mut l = Layout::new(1);
    l.split(1, Dir::Vertical, 2, 0.5);
    l.split(2, Dir::Horizontal, 3, 0.5);
    assert_eq!(l.panes(), vec![1, 2, 3]);
    assert_eq!(l.focused(), 3);
    l.focus_next();
    assert_eq!(l.focused(), 1);
    l.focus_next();
    assert_eq!(l.focused(), 2);
    l.focus_next();
    assert_eq!(l.focused(), 3);
}

#[test]
fn gap_shrinks_sibling_rects() {
    let mut l = Layout::new(1);
    l.split(1, Dir::Vertical, 2, 0.5);
    let gap = 10.0;
    let ra = rect_of(&l, AREA, gap, 1);
    let rb = rect_of(&l, AREA, gap, 2);
    approx(ra.w, 45.0);
    approx(rb.w, 45.0);
    approx(ra.w + rb.w, 90.0);
    approx(rb.x, ra.x + ra.w + gap);
}

#[test]
fn neighbor_finds_left_and_right() {
    let mut l = Layout::new(1);
    l.split(1, Dir::Vertical, 2, 0.5);
    assert_eq!(l.neighbor(Dir::Vertical, false, AREA), Some(1));
    assert_eq!(l.neighbor(Dir::Vertical, true, AREA), None);
    l.set_focus(1);
    assert_eq!(l.neighbor(Dir::Vertical, true, AREA), Some(2));
    assert_eq!(l.neighbor(Dir::Vertical, false, AREA), None);
    assert_eq!(l.neighbor(Dir::Horizontal, true, AREA), None);
    assert_eq!(l.neighbor(Dir::Horizontal, false, AREA), None);
}

#[test]
fn neighbor_finds_up_and_down() {
    let mut l = Layout::new(1);
    l.split(1, Dir::Horizontal, 2, 0.5);
    assert_eq!(l.neighbor(Dir::Horizontal, false, AREA), Some(1));
    assert_eq!(l.neighbor(Dir::Horizontal, true, AREA), None);
    l.set_focus(1);
    assert_eq!(l.neighbor(Dir::Horizontal, true, AREA), Some(2));
    assert_eq!(l.neighbor(Dir::Horizontal, false, AREA), None);
    assert_eq!(l.neighbor(Dir::Vertical, true, AREA), None);
    assert_eq!(l.neighbor(Dir::Vertical, false, AREA), None);
}

#[test]
fn neighbor_picks_nearest_in_grid() {
    let mut l = Layout::new(1);
    l.split(1, Dir::Vertical, 2, 0.5);
    l.split(1, Dir::Horizontal, 3, 0.5);
    l.split(2, Dir::Horizontal, 4, 0.5);
    assert_eq!(l.panes().len(), 4);
    l.set_focus(1);
    assert_eq!(l.neighbor(Dir::Vertical, true, AREA), Some(2));
    assert_eq!(l.neighbor(Dir::Horizontal, true, AREA), Some(3));
    l.set_focus(4);
    assert_eq!(l.neighbor(Dir::Vertical, false, AREA), Some(3));
    assert_eq!(l.neighbor(Dir::Horizontal, false, AREA), Some(2));
}
