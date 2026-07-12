//! `wl_seat` input + `wp_cursor_shape` handlers, and the input-injection methods the platform loop
//! (main.rs, driven by NSEvents) calls to synthesize seat events.
//!
//! The pointer/keyboard handles are `Arc`-backed, so each injector clones the handle out first to avoid
//! aliasing `&mut self` while dispatching into the seat.

use smithay::{
    input::{
        keyboard::{FilterResult, Keycode},
        pointer::{AxisFrame, ButtonEvent, CursorIcon, CursorImageStatus, MotionEvent},
        Seat, SeatHandler, SeatState,
    },
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::SERIAL_COUNTER,
    wayland::tablet_manager::TabletSeatHandler,
};

use crate::DdState;

impl SeatHandler for DdState {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }
    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        // Map the requested themed cursor to a host NSCursor via the reused Presenter seam. Smithay hands
        // us a `CursorIcon` (from wp_cursor_shape_device_v1.set_shape, or a client's own set_cursor); the
        // Presenter expects the `wp_cursor_shape_device_v1.shape` enum number, so translate — the
        // `CursorIcon` enum has NO stable discriminant, so `icon as u32` would pick the wrong cursor.
        if let CursorImageStatus::Named(icon) = image {
            self.presenter.set_cursor_shape(cursor_icon_to_wp_shape(icon));
        }
    }
    fn focus_changed(&mut self, _seat: &Seat<Self>, _focused: Option<&WlSurface>) {}
}

/// `wp_cursor_shape_manager_v1` routes a tablet tool's themed cursor here. We drive a single pointer
/// seat (no tablet), so the default (ignore) behaviour is correct — but the impl is required so
/// `delegate_cursor_shape!` can dispatch the shared manager global.
impl TabletSeatHandler for DdState {}

impl DdState {
    /// Absolute pointer motion in logical/point space (top-left origin). Focuses the pointer on the
    /// currently focused toplevel surface.
    pub fn pointer_motion(&mut self, x: f64, y: f64) {
        self.ptr_loc = (x, y);
        let ptr = self.pointer.clone();
        let focus = self.focus.clone().map(|s| (s, (0.0, 0.0).into()));
        let serial = SERIAL_COUNTER.next_serial();
        let time = self.now_ms();
        ptr.motion(
            self,
            focus,
            &MotionEvent {
                location: (x, y).into(),
                serial,
                time,
            },
        );
        ptr.frame(self);
    }

    /// Pointer button (evdev code, e.g. `BTN_LEFT = 0x110`).
    pub fn pointer_button(&mut self, button: u32, pressed: bool) {
        use smithay::backend::input::ButtonState;
        let ptr = self.pointer.clone();
        let serial = SERIAL_COUNTER.next_serial();
        let time = self.now_ms();
        ptr.button(
            self,
            &ButtonEvent {
                serial,
                time,
                button,
                state: if pressed {
                    ButtonState::Pressed
                } else {
                    ButtonState::Released
                },
            },
        );
        ptr.frame(self);
    }

    /// Vertical/horizontal scroll. `precise` marks a trackpad (continuous) vs a stepped mouse wheel.
    pub fn pointer_axis(&mut self, vx: f64, vy: f64, precise: bool) {
        use smithay::backend::input::{Axis, AxisSource};
        let ptr = self.pointer.clone();
        let time = self.now_ms();
        let mut frame = AxisFrame::new(time).source(if precise {
            AxisSource::Continuous
        } else {
            AxisSource::Wheel
        });
        if vy != 0.0 {
            frame = frame.value(Axis::Vertical, vy);
            if !precise {
                frame = frame.v120(Axis::Vertical, (vy.signum() as i32) * 120);
            }
        }
        if vx != 0.0 {
            frame = frame.value(Axis::Horizontal, vx);
            if !precise {
                frame = frame.v120(Axis::Horizontal, (vx.signum() as i32) * 120);
            }
        }
        ptr.axis(self, frame);
        ptr.frame(self);
    }

    /// Keyboard key (raw evdev keycode; we add the +8 XKB offset here). The focused client's own
    /// xkbcommon turns the keycode into a keysym.
    pub fn key(&mut self, evdev: u32, pressed: bool) {
        use smithay::backend::input::KeyState;
        let kbd = self.keyboard.clone();
        let serial = SERIAL_COUNTER.next_serial();
        let time = self.now_ms();
        let keycode = Keycode::new(evdev + 8);
        kbd.input::<(), _>(
            self,
            keycode,
            if pressed {
                KeyState::Pressed
            } else {
                KeyState::Released
            },
            serial,
            time,
            |_, _, _| FilterResult::Forward,
        );
    }
}

/// Map Smithay's `CursorIcon` to the `wp_cursor_shape_device_v1.shape` enum number the Presenter
/// (`apply_cursor_shape` → `NSCursor`) understands. This is the inverse of Smithay's internal
/// `shape_to_cursor_icon`; `CursorIcon` is a plain fieldless enum with no `#[repr]`, so it must be
/// matched explicitly rather than cast. Unmapped icons fall back to `default` (1 = arrow).
fn cursor_icon_to_wp_shape(icon: CursorIcon) -> u32 {
    match icon {
        CursorIcon::Default => 1,
        CursorIcon::ContextMenu => 2,
        CursorIcon::Help => 3,
        CursorIcon::Pointer => 4,
        CursorIcon::Progress => 5,
        CursorIcon::Wait => 6,
        CursorIcon::Cell => 7,
        CursorIcon::Crosshair => 8,
        CursorIcon::Text => 9,
        CursorIcon::VerticalText => 10,
        CursorIcon::Alias => 11,
        CursorIcon::Copy => 12,
        CursorIcon::Move => 13,
        CursorIcon::NoDrop => 14,
        CursorIcon::NotAllowed => 15,
        CursorIcon::Grab => 16,
        CursorIcon::Grabbing => 17,
        CursorIcon::EResize => 18,
        CursorIcon::NResize => 19,
        CursorIcon::NeResize => 20,
        CursorIcon::NwResize => 21,
        CursorIcon::SResize => 22,
        CursorIcon::SeResize => 23,
        CursorIcon::SwResize => 24,
        CursorIcon::WResize => 25,
        CursorIcon::EwResize => 26,
        CursorIcon::NsResize => 27,
        CursorIcon::NeswResize => 28,
        CursorIcon::NwseResize => 29,
        CursorIcon::ColResize => 30,
        CursorIcon::RowResize => 31,
        CursorIcon::AllScroll => 32,
        CursorIcon::ZoomIn => 33,
        CursorIcon::ZoomOut => 34,
        _ => 1,
    }
}
