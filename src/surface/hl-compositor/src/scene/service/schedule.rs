//! Frame pacing: how a presented tree advances its per-surface frame callbacks + `wp_presentation`
//! feedback, and the vsync throttle decision across time.
//!
//! Ported from `hl-compositor`'s `FramePacing` / `PacingPolicy` state machine (`handlers/compositor.rs`)
//! and the present-timing derivation. [`FramePacing`] classifies what happened to a frame; [`PacingPolicy`]
//! says what to do with the surface's callbacks/feedback; [`FramePacing::from`] maps a presenter's
//! [`PresentOutcome`] onto pacing; [`should_present`] is the neutral vsync throttle a `FakeClock` drives.

use crate::scene::port::PresentOutcome;

/// How a presented tree should advance its per-surface frame pacing. Exact port of the ported enum:
/// `Presented` (a new frame reached the screen), `Skipped` (clean tree — the last frame still stands),
/// `RetryableFailure` (retain callbacks + feedback for retry), `TerminalFailure` (drop them).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FramePacing {
    Pending,
    Presented,
    Skipped,
    RetryableFailure,
    TerminalFailure,
}

/// The concrete actions a [`FramePacing`] implies for one surface's callbacks + feedback. Exact port of
/// `PacingPolicy`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PacingPolicy {
    /// Fire the surface's `wl_surface.frame` callbacks (the client may draw again).
    pub complete_callbacks: bool,
    /// Retain callbacks + feedback across a failed present, to fire on the next accepted present.
    pub retain: bool,
    /// Answer `wp_presentation` feedback with `presented` (vs `discarded`).
    pub present_feedback: bool,
    /// Terminal cleanup: drop retained callbacks/feedback and retire the frame's resources.
    pub terminal_cleanup: bool,
}

impl FramePacing {
    /// The policy for this pacing decision. Exact port of `FramePacing::policy`.
    pub fn policy(self) -> PacingPolicy {
        match self {
            FramePacing::Pending => PacingPolicy {
                complete_callbacks: false,
                retain: true,
                present_feedback: false,
                terminal_cleanup: false,
            },
            FramePacing::Presented => PacingPolicy {
                complete_callbacks: true,
                retain: false,
                present_feedback: true,
                terminal_cleanup: false,
            },
            FramePacing::Skipped => PacingPolicy {
                complete_callbacks: true,
                retain: false,
                present_feedback: false,
                terminal_cleanup: false,
            },
            FramePacing::RetryableFailure => PacingPolicy {
                complete_callbacks: false,
                retain: true,
                present_feedback: false,
                terminal_cleanup: false,
            },
            FramePacing::TerminalFailure => PacingPolicy {
                complete_callbacks: false,
                retain: false,
                present_feedback: false,
                terminal_cleanup: true,
            },
        }
    }
}

/// Map a presenter's structured [`PresentOutcome`] onto frame pacing — the exact classification
/// `present_render_root` performs: only a visibly `Delivered` frame advances callbacks/feedback; an
/// `Offscreen` present is a retryable failure; explicit failures pass through.
impl From<PresentOutcome> for FramePacing {
    fn from(outcome: PresentOutcome) -> Self {
        match outcome {
            PresentOutcome::Pending { .. } => FramePacing::Pending,
            PresentOutcome::Delivered { .. } => FramePacing::Presented,
            PresentOutcome::Offscreen => FramePacing::RetryableFailure,
            PresentOutcome::RetryableFailure => FramePacing::RetryableFailure,
            PresentOutcome::TerminalFailure => FramePacing::TerminalFailure,
        }
    }
}

/// The vsync throttle: whether a root that last presented at `last_present_ns` (or never, `None`) may
/// present again at `now_ns` given the output `refresh_ns`. A frame is due when at least one refresh
/// interval has elapsed since the last present — so a burst of commits within one interval coalesces to
/// a single present, and a static guest that stops committing never presents needlessly. An unknown
/// refresh (`0`) or a first present is always due. Monotonic `now` is assumed; a `now` before `last`
/// (a clock anomaly) is treated as "not yet due".
pub fn should_present(now_ns: u64, last_present_ns: Option<u64>, refresh_ns: u64) -> bool {
    match last_present_ns {
        None => true,
        Some(_) if refresh_ns == 0 => true,
        Some(last) => now_ns.saturating_sub(last) >= refresh_ns,
    }
}
