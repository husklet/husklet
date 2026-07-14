//! `zwp_idle_inhibit_manager_v1` — a surface asks the compositor not to let the session go idle while
//! it is visible (video players, presentations, Chrome/Firefox `wake-lock`, games). Composed from the
//! vendored Smithay `idle_inhibit` module.
//!
//! ## Host policy (record intent)
//! dd has no compositor-owned screensaver/DPMS timer to suppress — idle/blank is macOS's job. The
//! correct, honest behaviour is therefore to RECORD the inhibition intent (which surfaces currently
//! hold an inhibitor) so the host can, if it chooses, assert a `NSProcessInfo` power assertion or
//! simply report the state. [`HlState::idle_inhibited`] exposes whether any inhibitor is live; the set
//! is keyed by surface id so a client dropping its inhibitor (or the surface dying) clears it.

use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::idle_inhibit::IdleInhibitHandler;

use crate::HlState;

impl IdleInhibitHandler for HlState {
    /// A client created a `zwp_idle_inhibitor_v1` for `surface`: record that the session should not idle
    /// while this surface is up. (dd defers the actual power assertion to the host; the intent is the
    /// compositor-side contract.)
    fn inhibit(&mut self, surface: WlSurface) {
        self.idle_inhibitors.insert(self.surface_id(&surface));
    }

    /// The client destroyed its inhibitor (Smithay calls this on `zwp_idle_inhibitor_v1.destroy`): drop
    /// the recorded intent for that surface.
    fn uninhibit(&mut self, surface: WlSurface) {
        self.idle_inhibitors.remove(&self.surface_id(&surface));
    }
}

impl HlState {
    /// Whether any surface currently holds an idle inhibitor — the host reads this to decide whether to
    /// keep the display awake. Proven by the roundtrip test (false → true on create → false on destroy).
    pub fn idle_inhibited(&self) -> bool {
        self.idle_inhibitors.iter().any(|sid| {
            self.surface_resources
                .get(sid)
                .and_then(|surface| self.window_root(surface))
                .is_some_and(|root| self.root_is_visible(&root))
        })
    }
}
