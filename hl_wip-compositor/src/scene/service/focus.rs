//! Focus: keyboard focus + window activation, and pointer hit-testing.
//!
//! Ported from `hl-compositor`'s `focus_surface` / `activate_surface` (raise → focus), the focus-
//! clearing in `teardown_surface` / `toplevel_destroyed` / `minimize_request`, and the pointer hit-test
//! `input_surface_at` / `input_surface_at_offset`. Neutral: the Smithay `KeyboardHandle` / data-device /
//! text-input side effects are dropped; the service mutates the [`Seat`] and REPORTS the change so an
//! adapter can drive the real keyboard/clipboard/IME focus.

use crate::scene::model::{Scene, SurfaceId};

/// The result of a focus operation: what held keyboard focus before and after. `changed()` is the
/// signal an adapter uses to move the real keyboard/selection/text-input focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FocusChange {
    pub previous: Option<SurfaceId>,
    pub current: Option<SurfaceId>,
}

impl FocusChange {
    pub fn changed(&self) -> bool {
        self.previous != self.current
    }
}

/// Give `sid` keyboard focus (a toplevel mapped or was activated/raised). Records the previous focus so
/// the caller can diff. Mirrors `focus_surface` (minus the Smithay keyboard/selection wiring).
pub fn focus_surface(scene: &mut Scene, sid: SurfaceId) -> FocusChange {
    let previous = scene.seat().keyboard_focus;
    scene.seat_mut().keyboard_focus = Some(sid);
    FocusChange { previous, current: Some(sid) }
}

/// Activate a window: focus + raise (the effect of `xdg_activation_v1` / a compositor-driven raise).
/// Neutral equivalent of `activate_surface` — the raise is the presenter's job, so this only moves
/// focus here.
pub fn activate(scene: &mut Scene, sid: SurfaceId) -> FocusChange {
    focus_surface(scene, sid)
}

/// Clear keyboard focus (nothing focused).
pub fn clear_focus(scene: &mut Scene) -> FocusChange {
    let previous = scene.seat().keyboard_focus;
    scene.seat_mut().keyboard_focus = None;
    FocusChange { previous, current: None }
}

/// A window closed / was destroyed / was minimized: drop keyboard focus if it held it, otherwise leave
/// focus untouched. Mirrors the focus-clearing in `toplevel_destroyed` / `set_surface_visibility`.
/// (There is no auto-refocus policy — focus goes to `None`, exactly as the ported code does.)
pub fn on_window_gone(scene: &mut Scene, sid: SurfaceId) -> FocusChange {
    let previous = scene.seat().keyboard_focus;
    if previous == Some(sid) {
        scene.seat_mut().keyboard_focus = None;
        FocusChange { previous, current: None }
    } else {
        FocusChange { previous, current: previous }
    }
}

/// The topmost input-sensitive surface at root-local logical coordinates `(x, y)`, with its root-space
/// offset. Walks the tree top → bottom (subsurface children reversed, deepest-first), honouring each
/// surface's input region. Exact port of `input_surface_at` / `input_surface_at_offset`.
pub fn surface_at(scene: &Scene, root: SurfaceId, x: i32, y: i32) -> Option<(SurfaceId, i32, i32)> {
    surface_at_offset(scene, root, x, y, 0, 0)
}

fn surface_at_offset(
    scene: &Scene,
    surface: SurfaceId,
    x: i32,
    y: i32,
    ox: i32,
    oy: i32,
) -> Option<(SurfaceId, i32, i32)> {
    // Children topmost-first (reverse of the bottom → top z-order).
    for &child in scene.subsurface_children(surface).iter().rev() {
        if child == surface {
            continue;
        }
        let (cx, cy) = subsurface_offset(scene, child);
        if let Some(hit) = surface_at_offset(scene, child, x, y, ox + cx, oy + cy) {
            return Some(hit);
        }
    }
    let (lx, ly) = (x - ox, y - oy);
    let surface_ref = scene.get(surface)?;
    surface_ref.accepts_input_at(lx, ly).then_some((surface, ox, oy))
}

fn subsurface_offset(scene: &Scene, sid: SurfaceId) -> (i32, i32) {
    match scene.get(sid).map(|s| &s.role) {
        Some(crate::scene::model::SurfaceRole::Subsurface(s)) => (s.x, s.y),
        _ => (0, 0),
    }
}

/// Move the pointer to `(x, y)` (root-local logical) and recompute pointer focus by hit-testing the
/// tree rooted at `root`. Returns the surface now under the pointer. Updates the [`Seat`].
pub fn update_pointer(scene: &mut Scene, root: SurfaceId, x: f64, y: f64) -> Option<SurfaceId> {
    scene.seat_mut().pointer_location = (x, y);
    let hit = surface_at(scene, root, x.floor() as i32, y.floor() as i32).map(|(s, _, _)| s);
    scene.seat_mut().pointer_focus = hit;
    hit
}
