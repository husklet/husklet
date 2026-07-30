//! [`Presenter`]: the boundary the compositor policy hands finished frames across. This is the seam
//! between the platform-neutral `scene` and a platform window backend (Cocoa/Metal, DRM/GBM, or a
//! headless PNG proof) — the neutral analogue of `hl-display::present::Presenter`.
//!
//! `present` takes an already-composed [`PresentableImage`] plus its damage and timing and returns a
//! [`PresentationFeedback`] saying what actually happened to the frame — Delivered / Offscreen /
//! failure — which the schedule service maps onto frame pacing. A test supplies a `FakePresenter` that
//! records calls; a real adapter blits to a native window. NO pixels/GPU/Cocoa leak into this trait.

use crate::scene::model::{
    OutputId, PresentableImage, Rect, SurfaceId, WindowInteraction, WindowState,
};
use std::sync::Arc;

/// Input/window intent emitted by a native presenter. Platform key codes are translated before crossing
/// this seam, so the Wayland adapter receives Linux evdev codes and logical surface coordinates.
#[derive(Clone, Debug, PartialEq)]
pub enum PresenterEvent {
    /// An asynchronously submitted frame reached a terminal host-display outcome.
    Presentation(PresentationCompletion),
    PointerMotion {
        window: SurfaceId,
        x: f64,
        y: f64,
    },
    PointerButton {
        window: SurfaceId,
        button: u32,
        pressed: bool,
        click_count: u8,
    },
    PointerAxis {
        horizontal: f64,
        vertical: f64,
    },
    Key {
        keycode: u32,
        pressed: bool,
    },
    GestureSwipeBegin {
        fingers: u32,
    },
    GestureSwipeUpdate {
        dx: f64,
        dy: f64,
    },
    GestureSwipeEnd {
        cancelled: bool,
    },
    GesturePinchBegin {
        fingers: u32,
    },
    GesturePinchUpdate {
        dx: f64,
        dy: f64,
        scale: f64,
        rotation: f64,
    },
    GesturePinchEnd {
        cancelled: bool,
    },
    TabletProximityIn {
        x: f64,
        y: f64,
    },
    TabletMotion {
        x: f64,
        y: f64,
        pressure: f64,
    },
    TabletTipDown,
    TabletTipUp,
    TabletProximityOut,
    Resize {
        surface: SurfaceId,
        width: u32,
        height: u32,
        maximized: bool,
        fullscreen: bool,
        resizing: bool,
    },
    ResizeEnd {
        surface: SurfaceId,
    },
    /// Host presentation properties changed without a guest commit. Re-present retained content.
    Repaint(SurfaceId),
    Focus(SurfaceId),
    Close(SurfaceId),
}

/// Stable identity of one asynchronous host-display submission.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PresentationId(pub u64);

/// Terminal outcome reported after an asynchronous host-display submission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompletionOutcome {
    Delivered {
        serial: u64,
        timing: Option<PresentTiming>,
    },
    TerminalFailure,
}

/// Completion of a previously returned [`PresentOutcome::Pending`] submission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PresentationCompletion {
    pub id: PresentationId,
    pub outcome: CompletionOutcome,
}

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

impl PresentTiming {
    pub fn fallback(present_ns: u64, refresh_ns: u64) -> Self {
        Self {
            present_ns,
            refresh_ns,
            vsync: refresh_ns > 0,
        }
    }
}

/// What a presenter did with a frame. Neutral port of `hl-display::present::PresentOutcome`: it
/// distinguishes "visibly on screen" from "rendered offscreen" from "failed", so the schedule service
/// never advances frame pacing for a frame that did not actually reach the display.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresentOutcome {
    /// Accepted by an asynchronous display backend, but not yet known to be visible.
    Pending { id: PresentationId },
    /// Visibly delivered to the display. `serial` is a monotonic per-frame pacing counter; `timing` is
    /// the optional hardware present-time evidence (`None` ⇒ fall back to the compositor's own clock).
    Delivered {
        serial: u64,
        timing: Option<PresentTiming>,
    },
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
        PresentationFeedback {
            outcome: PresentOutcome::Delivered { serial, timing },
        }
    }
    /// Rendered offscreen but not shown.
    pub fn offscreen() -> PresentationFeedback {
        PresentationFeedback {
            outcome: PresentOutcome::Offscreen,
        }
    }
}

/// Host-window event ingress.
pub trait HostEvents {
    /// Install an event-loop wakeup used by asynchronous platform callbacks.
    fn set_wake(&mut self, _wake: Arc<dyn Wake>) {}
    /// Service platform-window events without presenting a frame. Most backends need no polling;
    /// main-thread window systems can use this hook from the host adapter's event loop.
    fn poll_events(&mut self) {}

    /// Drain native input collected by [`Self::poll_events`].
    fn take_events(&mut self) -> Vec<PresenterEvent> {
        Vec::new()
    }
}

/// Platform-neutral event-loop wakeup capability.
pub trait Wake: Send + Sync {
    fn wake(&self);
}

/// Host clipboard exchange.
pub trait Clipboard {
    /// Publish UTF-8 text copied by a Wayland client to the host clipboard.
    fn set_clipboard_text(&mut self, _text: &str) {}

    /// Return newly changed UTF-8 host clipboard text, if any.
    fn take_clipboard_text(&mut self) -> Option<String> {
        None
    }
}

/// Native window lifecycle and interaction.
pub trait Windows {
    /// Atomically reconcile a native window from authoritative scene state.
    fn reconcile_window(&mut self, _window: &WindowState) {}

    /// Destroy native state for a surface. This is separate from pixel-buffer retirement.
    fn destroy_window(&mut self, _surface: SurfaceId) {}

    /// Begin an interactive window-manager operation.
    fn begin_interaction(&mut self, _surface: SurfaceId, _interaction: WindowInteraction) {}
}

/// Finished-frame presentation.
#[derive(Clone, Debug, PartialEq)]
pub struct PresentLayer {
    pub image: PresentableImage,
    pub x: i32,
    pub y: i32,
    pub damage: Vec<Rect>,
}

/// One native-window role and every Wayland layer composited into it, bottom to top.
#[derive(Clone, Debug, PartialEq)]
pub struct PresentFrame {
    pub output: OutputId,
    pub role: SurfaceId,
    pub layers: Vec<PresentLayer>,
    pub timing: PresentTiming,
}

pub trait Presenter {
    /// Composite every layer into the persistent backing texture for `role`, then submit exactly one
    /// native drawable. A popup is a distinct role; its subsurfaces remain layers of that popup.
    fn present_frame(&mut self, frame: &PresentFrame) -> PresentationFeedback;

    /// Cancel routing for a destroyed/recreated role. Already-submitted host resources may remain alive
    /// until their native completion or timeout, but their completion must no longer settle scene state.
    fn cancel(&mut self, _role: SurfaceId) {}
}

/// Complete host surface capability required by a platform adapter.
pub trait HostSurface: Presenter + HostEvents + Clipboard + Windows {}

impl<T: Presenter + HostEvents + Clipboard + Windows> HostSurface for T {}
