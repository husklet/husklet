//! [`Scene`]: the aggregate neutral scene graph — every live surface, the window/subsurface/popup tree,
//! the outputs, the seat, and the per-surface dirty + visibility state. This is the "brain"'s single
//! owned state; the `service/*` use-cases read and mutate it, and `port/*` is how it reaches the world.
//!
//! Ported from `hl-compositor`'s `HlState` aggregate — specifically the tree-navigation algorithms
//! (`window_root`, `present_root`, `collect_popups_for_root`, `popup_offset_to_toplevel`,
//! `collect_subtree_offsets`, `collect_tree_surfaces`) — with the Smithay/GPU/budget machinery dropped.
//! Tree links live here explicitly (`subsurface_children`, `popups`) instead of being read back out of
//! Smithay's `get_children` / `get_parent` each traversal.

use std::collections::{HashMap, HashSet};

use super::damage::Rect;
use super::output::{Output, OutputId};
use super::seat::Seat;
use super::surface::{Surface, SurfaceId, Visibility};
use super::window::{SubsurfaceState, SurfaceRole};

/// Depth guard for the parent-link walks — defends against a pathological cycle exactly like the
/// `for _ in 0..256` bounds in `hl-compositor`.
const MAX_TREE_DEPTH: usize = 256;

/// The whole neutral scene.
#[derive(Debug, Default)]
pub struct Scene {
    surfaces: HashMap<SurfaceId, Surface>,
    /// Ordered (bottom → top) subsurface children per parent surface — the z-order Smithay keeps in
    /// `get_children`, made explicit.
    subsurface_children: HashMap<SurfaceId, Vec<SurfaceId>>,
    /// Every live popup surface id (order-independent; placement order is derived by depth).
    popups: HashSet<SurfaceId>,
    outputs: Vec<Output>,
    /// Per-surface selected output (new surfaces start on the primary). Keyed by root sid.
    surface_outputs: HashMap<SurfaceId, OutputId>,
    seat: Seat,
    /// Surfaces whose pixels changed since their window tree was last presented (`HlState::dirty`).
    dirty: HashSet<SurfaceId>,
    /// Per-root visibility; absence means [`Visibility::Visible`] (`HlState::visibility`).
    visibility: HashMap<SurfaceId, Visibility>,
    next_id: u32,
}

impl Scene {
    pub fn new() -> Scene {
        Scene { next_id: 1, ..Scene::default() }
    }

    // ---- outputs ----------------------------------------------------------------------------------

    /// Register an output. The first registered output is the primary that new surfaces enter.
    pub fn add_output(&mut self, output: Output) {
        self.outputs.push(output);
    }

    pub fn outputs(&self) -> &[Output] {
        &self.outputs
    }

    /// The primary output (the first registered), if any.
    pub fn primary_output(&self) -> Option<&Output> {
        self.outputs.first()
    }

    pub fn output(&self, id: OutputId) -> Option<&Output> {
        self.outputs.iter().find(|o| o.id == id)
    }

    /// The output a root surface is presented on: its selected output, else the primary.
    pub fn selected_output(&self, root: SurfaceId) -> Option<&Output> {
        match self.surface_outputs.get(&root) {
            Some(id) => self.output(*id),
            None => self.primary_output(),
        }
    }

    pub fn route_surface_to_output(&mut self, root: SurfaceId, output: OutputId) {
        self.surface_outputs.insert(root, output);
    }

    /// Primary output's logical size, or a sane fallback when no output is registered — the target
    /// popup placement constrains against and maximize sizing uses (`output_logical_size`).
    pub fn output_logical_size(&self) -> (i32, i32) {
        self.primary_output().map(Output::logical_size).unwrap_or((1000, 700))
    }

    // ---- seat -------------------------------------------------------------------------------------

    pub fn seat(&self) -> &Seat {
        &self.seat
    }
    pub fn seat_mut(&mut self) -> &mut Seat {
        &mut self.seat
    }

    // ---- surface lifecycle ------------------------------------------------------------------------

    /// Allocate a fresh surface id and insert a roleless surface — the neutral analogue of
    /// `register_surface` (which mints a collision-free monotonic host sid). New surfaces enter the
    /// primary output.
    pub fn create_surface(&mut self) -> SurfaceId {
        let id = SurfaceId(self.next_id);
        self.next_id = self.next_id.checked_add(1).expect("surface id space exhausted");
        self.surfaces.insert(id, Surface::new(id));
        if let Some(primary) = self.primary_output() {
            self.surface_outputs.insert(id, primary.id);
        }
        id
    }

    /// Insert a surface with a caller-chosen id (test convenience / adapter that owns id allocation).
    pub fn insert_surface(&mut self, surface: Surface) {
        self.next_id = self.next_id.max(surface.id.0 + 1);
        let id = surface.id;
        self.surfaces.insert(id, surface);
        let primary = self.primary_output().map(|o| o.id);
        if let Some(primary) = primary {
            self.surface_outputs.entry(id).or_insert(primary);
        }
    }

    pub fn get(&self, id: SurfaceId) -> Option<&Surface> {
        self.surfaces.get(&id)
    }
    pub fn get_mut(&mut self, id: SurfaceId) -> Option<&mut Surface> {
        self.surfaces.get_mut(&id)
    }
    pub fn contains(&self, id: SurfaceId) -> bool {
        self.surfaces.contains_key(&id)
    }

    /// Assign a role, registering the tree links it implies (a subsurface joins its parent's ordered
    /// child list at the top; a popup joins the popup registry).
    pub fn set_role(&mut self, id: SurfaceId, role: SurfaceRole) {
        match &role {
            SurfaceRole::Subsurface(SubsurfaceState { parent, .. }) => {
                let kids = self.subsurface_children.entry(*parent).or_default();
                if !kids.contains(&id) {
                    kids.push(id);
                }
            }
            SurfaceRole::Popup(_) => {
                self.popups.insert(id);
            }
            _ => {}
        }
        if let Some(s) = self.surfaces.get_mut(&id) {
            s.role = role;
        }
    }

    /// Remove a surface and every reference to it (child lists, popup registry, dirty/visibility,
    /// focus). Idempotent — mirrors `teardown_surface`'s reclaim breadth (minus GPU/budget state).
    pub fn remove_surface(&mut self, id: SurfaceId) {
        let removed = self.surfaces.remove(&id);
        self.dirty.remove(&id);
        self.visibility.remove(&id);
        self.surface_outputs.remove(&id);
        self.popups.remove(&id);
        self.subsurface_children.remove(&id);
        for kids in self.subsurface_children.values_mut() {
            kids.retain(|k| *k != id);
        }
        if let Some(Surface { role, .. }) = removed {
            if let Some(parent) = role.parent() {
                if let Some(kids) = self.subsurface_children.get_mut(&parent) {
                    kids.retain(|k| *k != id);
                }
            }
        }
        if self.seat.keyboard_focus == Some(id) {
            self.seat.keyboard_focus = None;
        }
        if self.seat.pointer_focus == Some(id) {
            self.seat.pointer_focus = None;
        }
    }

    // ---- dirty tracking ---------------------------------------------------------------------------

    pub fn mark_dirty(&mut self, id: SurfaceId) {
        self.dirty.insert(id);
    }
    pub fn clear_dirty(&mut self, id: SurfaceId) {
        self.dirty.remove(&id);
    }
    pub fn is_dirty(&self, id: SurfaceId) -> bool {
        self.dirty.contains(&id)
    }
    pub fn any_dirty(&self) -> bool {
        !self.dirty.is_empty()
    }

    // ---- visibility -------------------------------------------------------------------------------

    pub fn set_visibility(&mut self, id: SurfaceId, visibility: Visibility) {
        self.visibility.insert(id, visibility);
    }
    pub fn visibility(&self, id: SurfaceId) -> Visibility {
        self.visibility.get(&id).copied().unwrap_or(Visibility::Visible)
    }

    // ---- tree navigation --------------------------------------------------------------------------

    /// Ordered (bottom → top) subsurface children of `surface`.
    pub fn subsurface_children(&self, surface: SurfaceId) -> &[SurfaceId] {
        self.subsurface_children.get(&surface).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Every live popup id (unordered).
    pub fn popup_ids(&self) -> impl Iterator<Item = SurfaceId> + '_ {
        self.popups.iter().copied()
    }

    fn role(&self, surface: SurfaceId) -> Option<&SurfaceRole> {
        self.surfaces.get(&surface).map(|s| &s.role)
    }

    /// The popup's parent surface, if `surface` carries the popup role (`popup_parent`).
    pub fn popup_parent(&self, surface: SurfaceId) -> Option<SurfaceId> {
        match self.role(surface)? {
            SurfaceRole::Popup(p) => Some(p.parent),
            _ => None,
        }
    }

    /// The popup's resolved geometry `(x, y, w, h)` relative to its parent's window geometry origin
    /// (`popup_geometry`). `None` if `surface` is not a popup.
    pub fn popup_geometry(&self, surface: SurfaceId) -> Option<Rect> {
        match self.role(surface)? {
            SurfaceRole::Popup(p) => Some(p.geometry),
            _ => None,
        }
    }

    /// The toplevel that owns `surface`'s composite tree: climb subsurface parents, then popup parents,
    /// to the surface that is neither a subsurface nor a popup. Exact port of `window_root`.
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

    /// The present root when native popup windows are enabled: the nearest ancestor that is NOT a
    /// subsurface — a popup (its own window) or the owning toplevel. Exact port of `present_root`:
    /// STOPS at a popup instead of climbing through it.
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

    /// `surface` and every subsurface descendant, depth-first (`collect_tree_surfaces`).
    pub fn collect_tree_surfaces(&self, surface: SurfaceId, out: &mut Vec<SurfaceId>) {
        out.push(surface);
        for &child in self.subsurface_children(surface) {
            if child == surface {
                continue;
            }
            self.collect_tree_surfaces(child, out);
        }
    }

    /// `surface` and its subsurface descendants with accumulated root-relative offsets, bottom → top
    /// (`collect_subtree_offsets`).
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
            self.collect_subtree_offsets(child, x + cx, y + cy, out);
        }
    }

    /// Walk a popup's parent chain to its owning toplevel, summing each popup's geometry origin.
    /// Returns `(toplevel, x, y, depth)` where `(x, y)` is the popup's top-left relative to the
    /// toplevel and `depth` is the number of popups traversed. Exact port of `popup_offset_to_toplevel`.
    pub fn popup_offset_to_toplevel(&self, popup: SurfaceId) -> Option<(SurfaceId, i32, i32, usize)> {
        let mut cur = popup;
        let (mut x, mut y, mut depth) = (0i32, 0i32, 0usize);
        for _ in 0..MAX_TREE_DEPTH {
            let geo = self.popup_geometry(cur)?;
            let parent = self.popup_parent(cur)?;
            x += geo.x;
            y += geo.y;
            depth += 1;
            if self.popup_parent(parent).is_some() {
                cur = parent; // parent is itself a popup — keep climbing the submenu chain
                continue;
            }
            // Parent is not a popup: resolve it to its window root (handles a popup anchored on a
            // subsurface) and stop.
            return self.window_root(parent).map(|tl| (tl, x, y, depth));
        }
        None
    }

    /// Every popup that ultimately belongs to `root`, each with its screen offset within `root`,
    /// ordered parents-before-children (by depth) so a submenu composites on top of its menu. Exact
    /// port of `collect_popups_for_root` (default composite-into-toplevel mode).
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

    /// Mutate a popup's resolved geometry in place (used by `place_popup` after constraint solving).
    pub fn set_popup_geometry(&mut self, surface: SurfaceId, geometry: Rect) {
        if let Some(Surface { role: SurfaceRole::Popup(p), .. }) = self.surfaces.get_mut(&surface) {
            p.geometry = geometry;
        }
    }
}
