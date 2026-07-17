//! Window-tree roles + the `xdg_positioner` value type.
//!
//! A [`SurfaceRole`] is what a surface is in the tree — a toplevel window, an `xdg_popup`
//! (menu/dropdown/tooltip), a `wl_subsurface` child, a cursor image, or roleless. The [`Positioner`]
//! (with [`Anchor`] / [`Gravity`] / [`ConstraintAdjustment`]) is the neutral port of the
//! `xdg_positioner` request state that `service/popup.rs` resolves to a placement — replacing Smithay's
//! `PositionerState::get_unconstrained_geometry`, which the neutral core cannot call.
//!
//! Ported from `hl-compositor`'s `handlers/xdg.rs` (`new_popup`/`constrain_popup`, popup parent/geometry
//! walks) and the subsurface offset reads in `handlers/compositor.rs`.

use super::damage::Rect;
use super::surface::SurfaceId;
use super::surface::Visibility;

/// Platform-neutral native-window classification. Unlike [`SurfaceRole`], this describes only surfaces
/// that receive a host window and includes transient toplevel ownership.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowKind {
    Toplevel {
        parent: Option<SurfaceId>,
    },
    Popup {
        parent: SurfaceId,
        position: (i32, i32),
    },
}

/// Complete desired host-window state. Backends reconcile this snapshot atomically; pixel presentation
/// never guesses ownership or lifecycle from a frame.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowState {
    pub surface: SurfaceId,
    pub kind: WindowKind,
    pub title: String,
    pub logical_size: Option<(i32, i32)>,
    /// Client-declared xdg toplevel bounds. `None` on an axis means unconstrained.
    pub min_size: (Option<i32>, Option<i32>),
    pub max_size: (Option<i32>, Option<i32>),
    pub maximized: bool,
    pub fullscreen: bool,
    /// `xdg_surface.set_window_geometry`, in surface-local logical coordinates.
    pub geometry: Option<Rect>,
    pub visibility: Visibility,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowInteraction {
    Move,
}

/// What a surface is in the window tree.
#[derive(Clone, Debug, PartialEq)]
pub enum SurfaceRole {
    /// No role yet (a freshly created `wl_surface`), or a roleless window (an adopted X11 window
    /// presents as its own root).
    None,
    /// An `xdg_toplevel` — a window presented to the screen, the root of its own composite tree.
    Toplevel,
    /// A `wl_subsurface` child, positioned at `(x, y)` relative to its parent; `sync` = a synchronized
    /// subsurface (applied atomically with the parent, never presented on its own).
    Subsurface(SubsurfaceState),
    /// An `xdg_popup` anchored to `parent` (another popup or the owning toplevel) with a resolved
    /// on-screen `geometry` (relative to the parent's window-geometry origin) and its `positioner`.
    Popup(PopupState),
    /// A `wl_pointer.set_cursor` image — turned into a host cursor, never presented as a window.
    Cursor,
}

impl SurfaceRole {
    pub fn is_subsurface(&self) -> bool {
        matches!(self, SurfaceRole::Subsurface(_))
    }
    pub fn is_popup(&self) -> bool {
        matches!(self, SurfaceRole::Popup(_))
    }
    /// The direct parent surface, for subsurfaces and popups (the link `window_root` climbs).
    pub fn parent(&self) -> Option<SurfaceId> {
        match self {
            SurfaceRole::Subsurface(s) => Some(s.parent),
            SurfaceRole::Popup(p) => Some(p.parent),
            _ => None,
        }
    }
}

/// `wl_subsurface` placement + sync mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubsurfaceState {
    pub parent: SurfaceId,
    /// `wl_subsurface.set_position` offset from the parent's origin, logical coordinates.
    pub x: i32,
    pub y: i32,
    /// A synchronized subsurface commits atomically with its parent (`is_sync_subsurface`).
    pub sync: bool,
}

/// `xdg_popup` state: its parent, the positioner it was created with, its resolved geometry, and
/// whether it holds an explicit `xdg_popup.grab` (menus/context-menus grab; tooltips do not).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PopupState {
    pub parent: SurfaceId,
    pub positioner: Positioner,
    /// The last constraint-resolved geometry (origin relative to the parent's window geometry).
    pub geometry: Rect,
    pub grabbed: bool,
}

/// Which point of the anchor rectangle a popup hangs off — the `xdg_positioner.set_anchor` value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchor {
    None,
    Top,
    Bottom,
    Left,
    Right,
    TopLeft,
    BottomLeft,
    TopRight,
    BottomRight,
}

/// Which direction a popup grows from its anchor point — the `xdg_positioner.set_gravity` value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gravity {
    None,
    Top,
    Bottom,
    Left,
    Right,
    TopLeft,
    BottomLeft,
    TopRight,
    BottomRight,
}

/// `xdg_positioner.set_constraint_adjustment`: how a popup may be adjusted to stay on-screen, per axis.
/// Applied in order flip → slide → resize by `service/popup.rs` (the order xdg-shell mandates).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConstraintAdjustment {
    pub flip_x: bool,
    pub flip_y: bool,
    pub slide_x: bool,
    pub slide_y: bool,
    pub resize_x: bool,
    pub resize_y: bool,
}

impl ConstraintAdjustment {
    /// No adjustment permitted — the popup is placed exactly where anchor+gravity+offset put it.
    pub const NONE: ConstraintAdjustment = ConstraintAdjustment {
        flip_x: false,
        flip_y: false,
        slide_x: false,
        slide_y: false,
        resize_x: false,
        resize_y: false,
    };
}

/// Where a popup's native window should open: the parent surface it hangs off plus the
/// positioner-resolved `(x, y)` offset from that parent's window-geometry top-left (logical points,
/// y-down). A windowed presenter opens the popup at parent-content-top-left + `(x, y)`. Neutral port of
/// `hl-display::present::PopupPlacement`. `None` on a `PresentableImage` for toplevels/subsurfaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PopupPlacement {
    pub parent: SurfaceId,
    pub x: i32,
    pub y: i32,
}

/// The full `xdg_positioner` state resolved into a placement by `service/popup.rs::place_popup`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Positioner {
    /// Anchor rectangle, relative to the parent's window-geometry origin.
    pub anchor_rect: Rect,
    /// Requested popup size `(w, h)`.
    pub size: (i32, i32),
    pub anchor: Anchor,
    pub gravity: Gravity,
    pub constraint_adjustment: ConstraintAdjustment,
    /// Additional `(x, y)` offset applied after anchor+gravity.
    pub offset: (i32, i32),
}
