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
use super::window::{
    PopupPlacement, Positioner, SubsurfaceState, SurfaceRole, WindowKind, WindowState,
};

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
    /// Children ordered below their parent's content in the wl_subsurface stack.
    subsurface_below: HashSet<SurfaceId>,
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
    pub fn focus(&mut self, surface: SurfaceId) -> crate::scene::service::FocusChange {
        let previous = self.seat().keyboard_focus;
        self.seat_mut().keyboard_focus = Some(surface);
        crate::scene::service::FocusChange {
            previous,
            current: Some(surface),
        }
    }

    pub fn activate(&mut self, surface: SurfaceId) -> crate::scene::service::FocusChange {
        self.focus(surface)
    }

    pub fn clear_focus(&mut self) -> crate::scene::service::FocusChange {
        let previous = self.seat().keyboard_focus;
        self.seat_mut().keyboard_focus = None;
        crate::scene::service::FocusChange {
            previous,
            current: None,
        }
    }

    pub fn on_window_gone(&mut self, surface: SurfaceId) -> crate::scene::service::FocusChange {
        let previous = self.seat().keyboard_focus;
        if previous == Some(surface) {
            self.seat_mut().keyboard_focus = None;
        }
        crate::scene::service::FocusChange {
            previous,
            current: self.seat().keyboard_focus,
        }
    }

    pub(crate) fn subsurface_offset(&self, surface: SurfaceId) -> (i32, i32) {
        match self.get(surface).map(|surface| &surface.role) {
            Some(SurfaceRole::Subsurface(subsurface)) => (subsurface.x, subsurface.y),
            _ => (0, 0),
        }
    }
    pub fn new() -> Scene {
        Scene {
            next_id: 1,
            ..Scene::default()
        }
    }

    /// Parent-relative native-window placement for a popup surface.
    pub fn popup_placement(&self, surface: SurfaceId) -> Option<PopupPlacement> {
        let geometry = self.popup_geometry(surface)?;
        Some(PopupPlacement {
            parent: self.popup_parent(surface)?,
            x: geometry.x,
            y: geometry.y,
        })
    }

    pub fn constrain_popup(&self, positioner: &Positioner) -> Rect {
        let (width, height) = self.output_logical_size();
        positioner.place(width, height)
    }

    /// Constrain a popup against its ROOT WINDOW rather than the output.
    ///
    /// Husklet's popups become real child `NSWindow`s, so AppKit already keeps them inside the display's
    /// work area; constraining to the parent instead is what keeps a menu visually attached to the window
    /// it belongs to, which is what a Mac user expects. The target must therefore be stated in the space
    /// the popup's own coordinates use — relative to the root's WINDOW GEOMETRY, not its surface. Those
    /// differ by exactly the client-side shadow margin every GTK/Chromium window carries, so taking the
    /// surface's logical size here lets a menu at the window's edge hang off it by that margin.
    pub fn constrain_popup_for_parent(&self, parent: SurfaceId, positioner: &Positioner) -> Rect {
        let Some(root) = self.window_root(parent) else {
            return self.constrain_popup(positioner);
        };
        let Some(surface) = self.get(root) else {
            return self.constrain_popup(positioner);
        };
        let Some((width, height)) = surface
            .window_geometry
            .map(|geometry| (geometry.w, geometry.h))
            .or_else(|| surface.logical_size())
        else {
            return self.constrain_popup(positioner);
        };
        let (parent_x, parent_y) = if self.popup_parent(parent).is_some() {
            self.popup_offset_to_toplevel(parent)
                .map(|(_, x, y, _)| (x, y))
                .unwrap_or((0, 0))
        } else {
            (0, 0)
        };
        positioner.place_in(Rect::new(-parent_x, -parent_y, width.max(1), height.max(1)))
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
        self.primary_output()
            .map(Output::logical_size)
            .unwrap_or((1000, 700))
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
        self.next_id = self
            .next_id
            .checked_add(1)
            .expect("surface id space exhausted");
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
        if let Some(previous) = self.surfaces.get(&id).map(|surface| surface.role.clone()) {
            match previous {
                SurfaceRole::Subsurface(SubsurfaceState { parent, .. }) => {
                    let keeps_parent = matches!(
                        &role,
                        SurfaceRole::Subsurface(SubsurfaceState {
                            parent: next_parent,
                            ..
                        }) if *next_parent == parent
                    );
                    if !keeps_parent {
                        self.subsurface_below.remove(&id);
                        if let Some(children) = self.subsurface_children.get_mut(&parent) {
                            children.retain(|child| *child != id);
                        }
                    }
                }
                SurfaceRole::Popup(_) if !matches!(&role, SurfaceRole::Popup(_)) => {
                    self.popups.remove(&id);
                }
                _ => {}
            }
        }
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

    pub fn clear_role(&mut self, id: SurfaceId) {
        self.set_role(id, SurfaceRole::None);
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
        self.subsurface_below.remove(&id);
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

    /// Record (or clear) the GPU surface token a zero-copy client presents this surface through.
    ///
    /// The adapter owns the `hl_surface_identity_v1` protocol; the scene owns what a surface IS, and
    /// "has content" is scene state. Called from exactly two places — the mint and the retire — so the
    /// token in the scene cannot outlive the identity that issued it.
    pub fn set_native_token(&mut self, id: SurfaceId, token: Option<u64>) {
        if let Some(surface) = self.get_mut(id) {
            surface.native_token = token;
        }
    }

    pub fn set_visibility(&mut self, id: SurfaceId, visibility: Visibility) {
        self.visibility.insert(id, visibility);
    }
    pub fn visibility(&self, id: SurfaceId) -> Visibility {
        self.visibility
            .get(&id)
            .copied()
            .unwrap_or(Visibility::Visible)
    }

    /// Derive the complete desired native-window snapshot from the authoritative scene state.
    pub fn window_state(&self, id: SurfaceId) -> Option<WindowState> {
        let surface = self.get(id)?;
        let kind = match &surface.role {
            SurfaceRole::Toplevel => WindowKind::Toplevel {
                parent: surface.transient_parent,
            },
            SurfaceRole::Popup(popup) => WindowKind::Popup {
                parent: popup.parent,
                position: (popup.geometry.x, popup.geometry.y),
            },
            _ => return None,
        };
        let requested_visibility = self.visibility(id);
        // A surface with nothing to show must not be shown — but "nothing to show" is a question about
        // CONTENT, not about which route the content took. Asking `buffer.is_none()` here tested for
        // Wayland-attached content specifically, and held every zero-copy client permanently occluded.
        let visibility = if !surface.has_content() && requested_visibility == Visibility::Visible {
            Visibility::Occluded
        } else {
            requested_visibility
        };
        Some(WindowState {
            surface: id,
            kind,
            title: surface.title.clone(),
            logical_size: surface.logical_size(),
            min_size: surface.min_size,
            max_size: surface.max_size,
            maximized: surface.maximized,
            fullscreen: surface.fullscreen,
            geometry: surface.window_geometry,
            visibility,
        })
    }

    // ---- tree navigation --------------------------------------------------------------------------

    /// Every live toplevel surface id (the roots an input hit-test can land in). Order-independent —
    /// the neutral scene tracks no global on-screen window position, so toplevels all root at `(0, 0)`;
    /// an adapter that injects pointer input disambiguates overlap with focus (see `candidate_roots` in
    /// `adapter/smithay`).
    pub fn toplevels(&self) -> impl Iterator<Item = SurfaceId> + '_ {
        self.surfaces
            .iter()
            .filter(|(_, s)| matches!(s.role, SurfaceRole::Toplevel))
            .map(|(id, _)| *id)
    }

    /// Ordered (bottom → top) subsurface children of `surface`.
    pub fn subsurface_children(&self, surface: SurfaceId) -> &[SurfaceId] {
        self.subsurface_children
            .get(&surface)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Every live popup id (unordered).
    pub fn popup_ids(&self) -> impl Iterator<Item = SurfaceId> + '_ {
        self.popups.iter().copied()
    }

    pub(super) fn role(&self, surface: SurfaceId) -> Option<&SurfaceRole> {
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
}

#[path = "scene_tree.rs"]
mod tree;

#[cfg(test)]
mod window_visibility {
    //! What makes a toplevel showable.
    //!
    //! `window_state` refuses to mark a surface visible when it has nothing to show, and used to decide
    //! that by asking `buffer.is_none()`. That tests for Wayland-attached content specifically, so a
    //! client presenting zero-copy through a GPU surface token — every Vulkan client on this stack —
    //! was held `Occluded` forever: no native window was created for it, so no frame could ever be
    //! shown, and the compositor logged nothing at all while the client presented a thousand correct
    //! frames.
    //!
    //! The three cases below are the three content states, and the first two are the controls that make
    //! the third mean something: a surface with NO content must still be occluded (or this check would
    //! be satisfied by removing it), and a surface with a buffer must still be visible (or the fix would
    //! have broken the path that already worked).

    use super::super::surface::{BufferState, Format};
    use super::*;

    fn toplevel(scene: &mut Scene) -> SurfaceId {
        let id = scene.create_surface();
        scene.set_role(id, SurfaceRole::Toplevel);
        id
    }

    fn visibility(scene: &Scene, id: SurfaceId) -> Visibility {
        scene
            .window_state(id)
            .expect("a toplevel must have a window state")
            .visibility
    }

    #[test]
    fn a_toplevel_with_no_content_at_all_is_occluded() {
        let mut scene = Scene::new();
        let id = toplevel(&mut scene);
        assert_eq!(visibility(&scene, id), Visibility::Occluded);
    }

    #[test]
    fn a_toplevel_with_an_attached_buffer_is_visible() {
        let mut scene = Scene::new();
        let id = toplevel(&mut scene);
        scene.get_mut(id).unwrap().buffer = Some(BufferState {
            tex_w: 64,
            tex_h: 64,
            format: Format::Argb8888,
            buffer_scale: 1,
            gpu: false,
        });
        assert_eq!(visibility(&scene, id), Visibility::Visible);
    }

    #[test]
    fn a_toplevel_presenting_through_a_gpu_surface_token_is_visible() {
        let mut scene = Scene::new();
        let id = toplevel(&mut scene);
        // No buffer will ever be attached: this client's pixels reach the host through the GPU service.
        scene.set_native_token(id, Some(0x5EED));
        assert_eq!(visibility(&scene, id), Visibility::Visible);
        // And the token retiring takes the content with it, or the scene would keep claiming content
        // through an identity that no longer exists.
        scene.set_native_token(id, None);
        assert_eq!(visibility(&scene, id), Visibility::Occluded);
    }
}
