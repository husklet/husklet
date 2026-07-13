//! `zwp_pointer_gestures_v1` — touchpad swipe / pinch / hold gestures (GTK two-finger swipe, Chrome
//! pinch-zoom, long-press). Composed from the vendored Smithay `pointer_gestures` module.
//!
//! ## Host policy (no dedicated gesture device)
//! dd drives ONE pointer seat from a single Cocoa window and does not (yet) forward the macOS trackpad
//! `NSEventTypeMagnify` / swipe phases into the seat, so in production no gesture stream is synthesized.
//! Advertising the manager is still the correct, spec-faithful behaviour: a client binds it, creates
//! swipe/pinch/hold gesture objects bound to its `wl_pointer`, and simply receives no events — exactly
//! what a machine with a mouse but no touchpad exposes. The moment a gesture source IS wired, the
//! [`smithay::input::pointer::PointerHandle`] gesture injectors (see [`DdState::inject_swipe_gesture`])
//! deliver begin/update/end through this same delegate with no further glue, which the roundtrip test
//! drives to prove the wiring end to end.

use smithay::{
    input::pointer::{GestureSwipeBeginEvent, GestureSwipeEndEvent},
    utils::SERIAL_COUNTER,
};

use crate::DdState;

impl DdState {
    /// Synthesize a one-shot touchpad swipe (begin → end, `fingers` fingers) on the focused surface —
    /// the seam a future macOS trackpad bridge would drive. Delivered through the seat's pointer to
    /// every swipe-gesture object of the focused client, so the delegate wiring is provable without any
    /// real gesture hardware. No-op when nothing holds pointer focus.
    pub fn inject_swipe_gesture(&mut self, fingers: u32) {
        let ptr = self.pointer.clone();
        let time = self.now_ms();
        ptr.gesture_swipe_begin(
            self,
            &GestureSwipeBeginEvent {
                serial: SERIAL_COUNTER.next_serial(),
                time,
                fingers,
            },
        );
        ptr.gesture_swipe_end(
            self,
            &GestureSwipeEndEvent {
                serial: SERIAL_COUNTER.next_serial(),
                time,
                cancelled: false,
            },
        );
    }
}
