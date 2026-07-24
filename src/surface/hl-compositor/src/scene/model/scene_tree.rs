use super::*;

impl Scene {
    /// The toplevel that owns `surface`'s composite tree.
    pub fn window_root(&self, surface: SurfaceId) -> Option<SurfaceId> {
        let mut cur = surface;
        for _ in 0..MAX_TREE_DEPTH {
            let role = self.role(cur)?;
            match role {
                SurfaceRole::Subsurface(s) => cur = s.parent,
                SurfaceRole::Popup(p) => cur = p.parent,
                _ => return Some(cur),
            }
        }
        Some(cur)
    }

    /// The nearest present root that is not a subsurface.
    pub fn present_root(&self, surface: SurfaceId) -> Option<SurfaceId> {
        let mut cur = surface;
        for _ in 0..MAX_TREE_DEPTH {
            match self.role(cur)? {
                SurfaceRole::Subsurface(s) => cur = s.parent,
                _ => return Some(cur),
            }
        }
        Some(cur)
    }

    /// `surface` and every subsurface descendant, depth-first.
    pub fn collect_tree_surfaces(&self, surface: SurfaceId, out: &mut Vec<SurfaceId>) {
        out.push(surface);
        for &child in self.subsurface_children(surface) {
            if child != surface {
                self.collect_tree_surfaces(child, out);
            }
        }
    }

    /// `surface` and descendants with accumulated root-relative offsets.
    pub fn collect_subtree_offsets(
        &self,
        surface: SurfaceId,
        x: i32,
        y: i32,
        out: &mut Vec<(SurfaceId, i32, i32)>,
    ) {
        out.push((surface, x, y));
        for &child in self.subsurface_children(surface) {
            if child == surface {
                continue;
            }
            let (cx, cy) = match self.role(child) {
                Some(SurfaceRole::Subsurface(s)) => (s.x, s.y),
                _ => (0, 0),
            };
            self.collect_subtree_offsets(child, x.saturating_add(cx), y.saturating_add(cy), out);
        }
    }

    /// Resolve a popup to its toplevel and accumulated offset.
    pub fn popup_offset_to_toplevel(
        &self,
        popup: SurfaceId,
    ) -> Option<(SurfaceId, i32, i32, usize)> {
        let mut cur = popup;
        let (mut x, mut y, mut depth) = (0i32, 0i32, 0usize);
        for _ in 0..MAX_TREE_DEPTH {
            let geo = self.popup_geometry(cur)?;
            let parent = self.popup_parent(cur)?;
            x = x.saturating_add(geo.x);
            y = y.saturating_add(geo.y);
            depth += 1;
            if self.popup_parent(parent).is_some() {
                cur = parent;
                continue;
            }
            return self.window_root(parent).map(|tl| (tl, x, y, depth));
        }
        None
    }

    /// Popups belonging to `root`, ordered parents before children.
    pub fn collect_popups_for_root(&self, root: SurfaceId) -> Vec<(SurfaceId, i32, i32)> {
        let mut out: Vec<(SurfaceId, i32, i32, usize)> = Vec::new();
        for popup in self.popup_ids() {
            if let Some((tl, x, y, depth)) = self.popup_offset_to_toplevel(popup) {
                if tl == root {
                    out.push((popup, x, y, depth));
                }
            }
        }
        out.sort_by_key(|(_, _, _, depth)| *depth);
        out.into_iter().map(|(s, x, y, _)| (s, x, y)).collect()
    }

    /// Reorder `parent`'s subsurface children to match `order` (bottom to top).
    pub fn set_subsurface_order(&mut self, parent: SurfaceId, order: &[SurfaceId]) {
        let Some(kids) = self.subsurface_children.get_mut(&parent) else {
            return;
        };
        let mut reordered: Vec<SurfaceId> = order
            .iter()
            .copied()
            .filter(|id| kids.contains(id))
            .collect();
        for &kid in kids.iter() {
            if !reordered.contains(&kid) {
                reordered.push(kid);
            }
        }
        *kids = reordered;
    }

    /// Mutate a popup's resolved geometry in place.
    pub fn set_popup_geometry(&mut self, surface: SurfaceId, geometry: Rect) {
        if let Some(Surface {
            role: SurfaceRole::Popup(p),
            ..
        }) = self.surfaces.get_mut(&surface)
        {
            p.geometry = geometry;
        }
    }
}
