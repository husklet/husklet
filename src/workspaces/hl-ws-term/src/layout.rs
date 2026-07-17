//! Split-pane layout: a binary tree of H/V splits with leaf panes (iTerm2 / tmux style).
//!
//! One terminal window can hold many panes. The tree is a set of nested [`Dir`] splits whose
//! leaves are [`PaneId`]s. Geometry is computed on demand from an outer [`Rect`] — the tree only
//! stores structure, ratios, and which pane is focused, so it is pure and trivially testable.

/// A pane's stable identity (assigned by the caller, e.g. one per shell/terminal).
pub type PaneId = u64;

/// Split orientation. `Vertical` divides the area with a vertical line → panes side by side (left|right);
/// `Horizontal` divides with a horizontal line → panes stacked (top/bottom). (Matches iTerm2 wording:
/// "split vertically" puts panes left/right.)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dir {
    Vertical,
    Horizontal,
}

/// A rectangle in pixels (or any unit); origin top-left.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Internal tree node: either a single pane or a split of two subtrees.
enum Node {
    Leaf(PaneId),
    Split {
        dir: Dir,
        /// Fraction (0.0..1.0) of the split axis given to the FIRST child `a`.
        ratio: f32,
        a: Box<Node>,
        b: Box<Node>,
    },
}

impl Node {
    /// In-order (a before b) collection of leaf ids → left-to-right / top-to-bottom order.
    fn collect(&self, out: &mut Vec<PaneId>) {
        match self {
            Node::Leaf(id) => out.push(*id),
            Node::Split { a, b, .. } => {
                a.collect(out);
                b.collect(out);
            }
        }
    }

    /// Split the leaf equal to `target` into `Split{ a: target, b: new }`. Returns true if found.
    fn split(&mut self, target: PaneId, dir: Dir, new: PaneId, ratio: f32) -> bool {
        match self {
            Node::Leaf(id) if *id == target => {
                let old = *id;
                *self = Node::Split {
                    dir,
                    ratio,
                    a: Box::new(Node::Leaf(old)),
                    b: Box::new(Node::Leaf(new)),
                };
                true
            }
            Node::Leaf(_) => false,
            Node::Split { a, b, .. } => {
                a.split(target, dir, new, ratio) || b.split(target, dir, new, ratio)
            }
        }
    }

    /// Remove leaf `pane`, collapsing its parent split so the sibling subtree replaces it.
    /// Returns true if removed. Never call on a root that is itself `Leaf(pane)` (the caller
    /// guards the last-pane case).
    fn remove(&mut self, pane: PaneId) -> bool {
        // Inspect direct children without holding a borrow across the structural replace below.
        let (a_match, b_match) = match self {
            Node::Split { a, b, .. } => (
                matches!(**a, Node::Leaf(id) if id == pane),
                matches!(**b, Node::Leaf(id) if id == pane),
            ),
            Node::Leaf(_) => return false,
        };
        if a_match {
            let taken = std::mem::replace(self, Node::Leaf(pane));
            if let Node::Split { b, .. } = taken {
                *self = *b;
            }
            return true;
        }
        if b_match {
            let taken = std::mem::replace(self, Node::Leaf(pane));
            if let Node::Split { a, .. } = taken {
                *self = *a;
            }
            return true;
        }
        match self {
            Node::Split { a, b, .. } => a.remove(pane) || b.remove(pane),
            Node::Leaf(_) => false,
        }
    }

    /// Append `(pane, rect)` for every leaf, dividing `rect` by ratio along each split axis and
    /// leaving `gap` px between siblings.
    fn rects(&self, rect: Rect, gap: f32, out: &mut Vec<(PaneId, Rect)>) {
        match self {
            Node::Leaf(id) => out.push((*id, rect)),
            Node::Split { dir, ratio, a, b } => {
                let r = ratio.clamp(0.0, 1.0);
                match dir {
                    Dir::Vertical => {
                        // Side by side; divide the width, first pane on the left.
                        let avail = (rect.w - gap).max(0.0);
                        let wa = avail * r;
                        let wb = avail - wa;
                        let ra = Rect {
                            x: rect.x,
                            y: rect.y,
                            w: wa,
                            h: rect.h,
                        };
                        let rb = Rect {
                            x: rect.x + wa + gap,
                            y: rect.y,
                            w: wb,
                            h: rect.h,
                        };
                        a.rects(ra, gap, out);
                        b.rects(rb, gap, out);
                    }
                    Dir::Horizontal => {
                        // Stacked; divide the height, first pane on top.
                        let avail = (rect.h - gap).max(0.0);
                        let ha = avail * r;
                        let hb = avail - ha;
                        let ra = Rect {
                            x: rect.x,
                            y: rect.y,
                            w: rect.w,
                            h: ha,
                        };
                        let rb = Rect {
                            x: rect.x,
                            y: rect.y + ha + gap,
                            w: rect.w,
                            h: hb,
                        };
                        a.rects(ra, gap, out);
                        b.rects(rb, gap, out);
                    }
                }
            }
        }
    }
}

/// A split-pane tree with a focused pane.
pub struct Layout {
    root: Node,
    focused: PaneId,
}

impl Layout {
    /// A layout with a single full-area pane, which is focused.
    pub fn new(root: PaneId) -> Layout {
        Layout {
            root: Node::Leaf(root),
            focused: root,
        }
    }

    /// Split `target` in the given direction, inserting `new` as the second child at split `ratio`
    /// (0.0..1.0, fraction given to the FIRST/original pane). Focus moves to `new`. No-op if `target`
    /// is not present.
    pub fn split(&mut self, target: PaneId, dir: Dir, new: PaneId, ratio: f32) {
        if self.root.split(target, dir, new, ratio) {
            self.focused = new;
        }
    }

    /// Remove `pane`; its sibling takes over the parent's space. Returns false if it was the last pane
    /// (a layout always keeps ≥1 pane). If the focused pane closed, focus moves to a remaining pane.
    pub fn close(&mut self, pane: PaneId) -> bool {
        // Last pane: refuse.
        if matches!(self.root, Node::Leaf(id) if id == pane) {
            return false;
        }
        if !self.root.remove(pane) {
            return false;
        }
        if self.focused == pane {
            // Move focus to a survivor (there is always at least one).
            if let Some(&id) = self.panes().first() {
                self.focused = id;
            }
        }
        true
    }

    /// All pane ids, left-to-right / top-to-bottom order.
    pub fn panes(&self) -> Vec<PaneId> {
        let mut v = Vec::new();
        self.root.collect(&mut v);
        v
    }

    /// Compute each pane's rectangle within `area` (a gap of `gap` px is left between siblings).
    pub fn rects(&self, area: Rect, gap: f32) -> Vec<(PaneId, Rect)> {
        let mut v = Vec::new();
        self.root.rects(area, gap, &mut v);
        v
    }

    pub fn focused(&self) -> PaneId {
        self.focused
    }

    pub fn set_focus(&mut self, pane: PaneId) {
        if self.panes().contains(&pane) {
            self.focused = pane;
        }
    }

    /// Move focus to the next pane in `panes()` order (wraps).
    pub fn focus_next(&mut self) {
        let panes = self.panes();
        if panes.is_empty() {
            return;
        }
        let idx = panes.iter().position(|&p| p == self.focused).unwrap_or(0);
        self.focused = panes[(idx + 1) % panes.len()];
    }

    /// The neighboring pane in a screen direction from the focused pane, if any (for Cmd+Alt+arrow nav).
    ///
    /// `dir` picks the axis (`Vertical` → left/right, `Horizontal` → up/down) and `toward_end` picks
    /// the sign (`true` → right/down, `false` → left/up). Picks the geometrically nearest pane whose
    /// center lies beyond the focused pane's center in that screen direction.
    pub fn neighbor(&self, dir: Dir, toward_end: bool, area: Rect) -> Option<PaneId> {
        let rects = self.rects(area, 0.0);
        let fr = rects.iter().find(|(id, _)| *id == self.focused)?.1;
        let fcx = fr.x + fr.w / 2.0;
        let fcy = fr.y + fr.h / 2.0;
        let mut best: Option<(PaneId, f32)> = None;
        for (id, r) in &rects {
            if *id == self.focused {
                continue;
            }
            let cx = r.x + r.w / 2.0;
            let cy = r.y + r.h / 2.0;
            let ok = match dir {
                Dir::Vertical => {
                    if toward_end {
                        cx > fcx
                    } else {
                        cx < fcx
                    }
                }
                Dir::Horizontal => {
                    if toward_end {
                        cy > fcy
                    } else {
                        cy < fcy
                    }
                }
            };
            if !ok {
                continue;
            }
            let dx = cx - fcx;
            let dy = cy - fcy;
            let dist = dx * dx + dy * dy;
            if best.map_or(true, |(_, bd)| dist < bd) {
                best = Some((*id, dist));
            }
        }
        best.map(|(id, _)| id)
    }
}

#[cfg(test)]
mod tests {
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
}
