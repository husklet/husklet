//! The rest of the `wl_pointer` axis family: a FINGER-source scroll and its terminating `axis_stop`.
//!
//! `wl_pointer` v5 splits scrolling by source. A `wheel` scroll has no end — each detent is complete on its
//! own. A `finger` scroll (trackpad) does: the protocol requires an `axis_stop` when the fingers lift, and a
//! client that implements kinetic/momentum scrolling — GTK, Qt, Chrome all do — keeps scrolling until it
//! arrives. Emitting `finger` without ever emitting `axis_stop` leaves those clients scrolling forever, so
//! the two belong together.

use super::*;

impl HlState {
    /// Scroll from a TRACKPAD: the same smooth values as a wheel scroll but tagged
    /// `axis_source(finger)`, so the client knows a terminating [`Self::inject_pointer_axis_stop`] is coming.
    pub fn inject_pointer_axis_finger(&mut self, horizontal: f64, vertical: f64) {
        hl_debug!(
            tag::WAYLAND,
            "input axis finger h={:.1} v={:.1}",
            horizontal,
            vertical
        );
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let time = self.input_time_ms();
        let mut frame = AxisFrame::new(time).source(AxisSource::Finger);
        if horizontal != 0.0 {
            frame = frame.value(Axis::Horizontal, horizontal);
        }
        if vertical != 0.0 {
            frame = frame.value(Axis::Vertical, vertical);
        }
        pointer.axis(self, frame);
        pointer.frame(self);
    }

    /// `wl_pointer.axis_stop` on the named axes — the fingers left the trackpad. Ends the scroll sequence a
    /// `finger`-source scroll began; a client applies (or declines) momentum from here.
    pub fn inject_pointer_axis_stop(&mut self, horizontal: bool, vertical: bool) {
        hl_debug!(
            tag::WAYLAND,
            "input axis stop h={} v={}",
            horizontal,
            vertical
        );
        if !horizontal && !vertical {
            return;
        }
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let time = self.input_time_ms();
        let mut frame = AxisFrame::new(time).source(AxisSource::Finger);
        if horizontal {
            frame = frame.stop(Axis::Horizontal);
        }
        if vertical {
            frame = frame.stop(Axis::Vertical);
        }
        pointer.axis(self, frame);
        pointer.frame(self);
    }
}
