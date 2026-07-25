//! [`Seat`]: the neutral input-focus state — keyboard focus, pointer location, and pointer focus.
//!
//! Ported from `hl-compositor`'s focus bookkeeping (`HlState::focus`, `ptr_loc`, and the
//! `focus_surface` / teardown focus-clearing logic). Neutral: no Smithay `Seat`/`KeyboardHandle`; the
//! `service/focus.rs` use-cases mutate this and report what changed so an adapter can drive the real
//! keyboard/clipboard/text-input focus.

use super::surface::SurfaceId;

/// Keyboard + pointer focus state for the single seat this compositor exposes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Seat {
    /// The surface with keyboard focus (the most recently mapped/raised toplevel), if any. Drives the
    /// data-device / primary-selection / text-input focus in a real adapter.
    pub keyboard_focus: Option<SurfaceId>,
    /// Last pointer location in logical (point) space (`HlState::ptr_loc`).
    pub pointer_location: (f64, f64),
    /// The surface currently under the pointer, if any (result of hit-testing the tree).
    pub pointer_focus: Option<SurfaceId>,
}

impl Seat {
    pub fn new() -> Seat {
        Seat::default()
    }

    /// Whether `surface` currently holds keyboard focus.
    pub fn has_keyboard_focus(&self, surface: SurfaceId) -> bool {
        self.keyboard_focus == Some(surface)
    }
}
