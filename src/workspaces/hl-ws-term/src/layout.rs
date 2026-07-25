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
            if best.is_none_or(|(_, bd)| dist < bd) {
                best = Some((*id, dist));
            }
        }
        best.map(|(id, _)| id)
    }
}

#[cfg(test)]
#[path = "layout/tests.rs"]
mod tests;
