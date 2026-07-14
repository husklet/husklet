//! [`Presenter`]: the boundary the compositor policy hands finished frames across. This is the seam
//! between the platform-neutral `scene` and a platform window backend (Cocoa/Metal, DRM/GBM, or a
//! headless PNG proof) — the neutral analogue of `hl-display::present::Presenter`.
//!
//! `present` takes an already-composed [`PresentableImage`] plus its damage and timing and returns a
//! [`PresentationFeedback`] saying what actually happened to the frame — Delivered / Offscreen /
//! failure — which the schedule service maps onto frame pacing. A test supplies a `FakePresenter` that
//! records calls; a real adapter blits to a native window. NO pixels/GPU/Cocoa leak into this trait.

use crate::scene::model::{OutputId, PresentableImage, Rect, SurfaceId, Visibility};

/// Presentation timing evidence for a delivered frame — host monotonic present time + the output's
/// refresh interval, both nanoseconds. Neutral port of `hl-display::present::PresentTiming`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PresentTiming {
    /// Host monotonic time the frame became (or is expected to become) visible.
    pub present_ns: u64,
    /// Output refresh interval in nanoseconds (`0` = unknown / variable).
    pub refresh_ns: u64,
    /// Whether the backend observed a vertical-blank-synchronized presentation.
    pub vsync: bool,
}

/// What a presenter did with a frame. Neutral port of `hl-display::present::PresentOutcome`: it
/// distinguishes "visibly on screen" from "rendered offscreen" from "failed", so the schedule service
/// never advances frame pacing for a frame that did not actually reach the display.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresentOutcome {
    /// Visibly delivered to the display. `serial` is a monotonic per-frame pacing counter; `timing` is
    /// the optional hardware present-time evidence (`None` ⇒ fall back to the compositor's own clock).
    Delivered { serial: u64, timing: Option<PresentTiming> },
    /// Rendered into an offscreen/backing target but NOT visibly presented this cycle — not an error,
    /// but pacing must not advance as if it shipped.
    Offscreen,
    /// Delivery failed transiently; retain the frame + pacing for retry.
    RetryableFailure,
    /// Delivery cannot succeed without a new frame/device; terminate this pacing attempt.
    TerminalFailure,
}

/// The structured result of a [`Presenter::present`] call — what the port returns and the schedule
/// service turns into frame pacing (see `service/schedule.rs`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PresentationFeedback {
    pub outcome: PresentOutcome,
}

impl PresentationFeedback {
    /// A visibly delivered frame with the given pacing serial and optional hardware timing.
    pub fn delivered(serial: u64, timing: Option<PresentTiming>) -> PresentationFeedback {
        PresentationFeedback { outcome: PresentOutcome::Delivered { serial, timing } }
    }
    /// Rendered offscreen but not shown.
    pub fn offscreen() -> PresentationFeedback {
        PresentationFeedback { outcome: PresentOutcome::Offscreen }
    }
}

/// The platform window backend, behind which all Cocoa/Metal/DRM/PNG specifics live.
pub trait Presenter {
    /// Present one composed image to the named output, with its `damage` (root-space upload hint) and
    /// `timing`. Returns a [`PresentationFeedback`] describing the fate of the frame. The scene calls
    /// this in composite order (bottom → top) for each layer of a present root; the schedule service
    /// advances frame pacing only for a `Delivered` outcome.
    fn present(
        &mut self,
        output: OutputId,
        image: &PresentableImage,
        damage: &[Rect],
        timing: PresentTiming,
    ) -> PresentationFeedback;

    /// Apply compositor-requested visibility to a native window (minimize / occlude / reveal). Headless
    /// presenters keep the default no-op. Mirrors `Presenter::set_surface_visibility`.
    fn set_visibility(&mut self, _surface: SurfaceId, _visibility: Visibility) {}
}
