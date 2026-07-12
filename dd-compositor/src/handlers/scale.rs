//! `wp_fractional_scale_v1` handler + the compositor's fractional-scale policy.
//!
//! Non-integer Retina and "looks-like-1440p-on-4K" modes need a *fractional* buffer scale: a toolkit
//! (GTK/Qt/Chrome) that only sees the legacy integer `wl_output.scale` renders either too small (1x
//! stretched, blurry) or wastefully large (2x downscaled). `wp_fractional_scale_v1` fixes this — the
//! compositor advertises `wp_fractional_scale_manager_v1`, and for each surface that requests a
//! `wp_fractional_scale_v1` it sends `preferred_scale` as a fixed-point value in 120ths (180 == 1.5×,
//! 150 == 1.25×, 240 == 2×). The client then renders a buffer at that fractional scale and pairs it
//! with a `wp_viewporter` `destination` so the composited result lands at the correct logical size.
//! Smithay's [`FractionalScaleManagerState`] owns the wire encoding (`round(scale * 120)`); this module
//! supplies the *value* (the compositor's per-output fractional backing scale) and the handler that
//! pushes it the moment a surface opts in.
//!
//! `wp_viewporter` is already delegated (see lib.rs / handlers/mod.rs), so the destination side of the
//! fractional path needs nothing new here — a client combines the two: fractional buffer + viewport
//! destination == crisp 1.5×/1.25× windows.

use smithay::wayland::{
    compositor::with_states,
    fractional_scale::{with_fractional_scale, FractionalScaleHandler},
};

use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;

use crate::DdState;

/// Environment override for the advertised fractional backing scale (e.g. `1.5`, `1.25`). The macOS
/// backing store reports an *integer* `backingScaleFactor` (2 on Retina), so a non-integer "logical"
/// scale is a compositor *policy* choice rather than something the platform hands us; this override
/// lets that policy — and the headless tests — drive a true fractional value without a live display.
/// Mirrors the existing `DD_DISPLAY_*` knobs (`DD_DISPLAY_HIDPI`, `DD_DISPLAY_WINDOW_DECORATIONS`).
const FRACTIONAL_SCALE_ENV: &str = "DD_DISPLAY_FRACTIONAL_SCALE";

impl DdState {
    /// The fractional buffer scale the compositor prefers for surfaces on the primary output — the value
    /// sent (in 120ths) via `wp_fractional_scale_v1.preferred_scale`. Defaults to the output's own
    /// fractional scale (equal to the integer `wl_output.scale` when that is all we have), overridable to
    /// a true non-integer (1.5, 1.25, …) via [`FRACTIONAL_SCALE_ENV`]. Always clamped to ≥ 1.0 so a
    /// bogus value can never ask a client for a zero/negative buffer.
    pub(crate) fn output_fractional_scale(&self) -> f64 {
        if let Ok(raw) = std::env::var(FRACTIONAL_SCALE_ENV) {
            if let Ok(v) = raw.trim().parse::<f64>() {
                if v.is_finite() && v >= 1.0 {
                    return v;
                }
            }
        }
        self.output.current_scale().fractional_scale().max(1.0)
    }

    /// Push the current preferred fractional scale onto `surface`'s fractional-scale object (if it has
    /// one). Called when a surface first opts in and whenever the output scale changes, so a live
    /// re-scale (window dragged to a differently-scaled output, HiDPI toggled) re-notifies the client.
    pub(crate) fn send_preferred_fractional_scale(&self, surface: &WlSurface) {
        let scale = self.output_fractional_scale();
        with_states(surface, |states| {
            with_fractional_scale(states, |fractional| {
                // set_preferred_scale is idempotent (only re-sends on change) and encodes the 120ths.
                fractional.set_preferred_scale(scale);
            });
        });
    }
}

impl FractionalScaleHandler for DdState {
    /// A surface just created its `wp_fractional_scale_v1`. Seed it with the output's preferred fractional
    /// scale immediately so the client can pick its first buffer size correctly (GTK/Qt query this before
    /// their first paint).
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        self.send_preferred_fractional_scale(&surface);
    }
}
