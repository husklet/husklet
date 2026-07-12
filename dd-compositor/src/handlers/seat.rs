//! `wl_seat` input + `wp_cursor_shape` handlers, and the input-injection methods the platform loop
//! (main.rs, driven by NSEvents) calls to synthesize seat events.
//!
//! The pointer/keyboard handles are `Arc`-backed, so each injector clones the handle out first to avoid
//! aliasing `&mut self` while dispatching into the seat.

use std::os::unix::io::OwnedFd;

use smithay::{
    input::{
        keyboard::{FilterResult, Keycode},
        pointer::{AxisFrame, ButtonEvent, CursorIcon, CursorImageStatus, MotionEvent},
        Seat, SeatHandler, SeatState,
    },
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::SERIAL_COUNTER,
    wayland::{
        selection::{
            data_device::{
                set_data_device_selection, ClientDndGrabHandler, DataDeviceHandler, DataDeviceState,
                ServerDndGrabHandler,
            },
            SelectionHandler, SelectionSource, SelectionTarget,
        },
        tablet_manager::TabletSeatHandler,
    },
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

    /// Apply a host modifier bitmask (`bit0` Shift, `bit1` Ctrl, `bit2` Alt, `bit3` Super/Cmd, `bit4`
    /// CapsLock) — the macOS `NSEvent.FlagsChanged` path calls this. Rather than poking the XKB modifier
    /// mask directly (Smithay exposes no clean "set mask" seam), we drive the change through the ordinary
    /// key path with the modifier keys' evdev codes: `kbd.input` folds them into the XKB state and emits
    /// `wl_keyboard.modifiers` (depressed/latched/locked/group) for free — so Shift/Ctrl/Alt/Cmd chords and
    /// CapsLock reach the client exactly as a real keyboard would deliver them.
    pub fn update_modifiers(&mut self, mask: u32) {
        let old = self.mod_mask;
        if old == mask {
            return;
        }
        self.mod_mask = mask;
        // Momentary modifiers: press on a rising edge, release on a falling edge. Left-hand evdev codes
        // (KEY_LEFTSHIFT/CTRL/ALT/META) — the modifier *state* is what matters, not which physical side.
        for &(bit, code) in &[
            (0b0_0001u32, 42u32), // Shift → KEY_LEFTSHIFT
            (0b0_0010, 29),       // Ctrl  → KEY_LEFTCTRL
            (0b0_0100, 56),       // Alt   → KEY_LEFTALT
            (0b0_1000, 125),      // Super → KEY_LEFTMETA (Command)
        ] {
            let was = old & bit != 0;
            let now = mask & bit != 0;
            if now != was {
                self.key(code, now);
            }
        }
        // CapsLock is an XKB *lock*, and macOS reports its LED level (on/off), not a keypress. Emit a full
        // tap (press+release) each time the level flips so the XKB Lock toggles in lockstep with the host.
        const CAPS_BIT: u32 = 0b1_0000;
        if (old ^ mask) & CAPS_BIT != 0 {
            self.key(58, true); // KEY_CAPSLOCK
            self.key(58, false);
        }
    }

    /// Host clipboard → guest paste. When the host clipboard changes (its `changeCount`/generation moves),
    /// advertise its contents to the focused guest as a compositor-owned `wl_data_device` selection, so a
    /// guest paste (`wl_data_offer.receive`) is answered from the host clipboard via [`Self::send_selection`].
    /// Cheap and idempotent: it no-ops unless the host generation actually advanced past what we last
    /// mirrored (which also prevents re-offering our own guest→host push back to the guest).
    pub fn offer_host_clipboard(&mut self) {
        let generation = self.presenter.clipboard_host_generation();
        if generation == 0 || generation == self.host_clip_gen {
            return;
        }
        self.host_clip_gen = generation;
        let mimes = self.presenter.clipboard_host_mimes();
        if mimes.is_empty() {
            return;
        }
        let dh = self.dh.clone();
        let seat = self.seat.clone();
        set_data_device_selection(&dh, &seat, mimes, ());
    }

    /// The mime types a guest just offered on its clipboard selection (a copy) that still need exporting to
    /// the host clipboard, or `None` when nothing is pending. The runtime loop reads this, pulls the guest
    /// source bytes, and pushes them to the host clipboard via `Presenter::clipboard_set_host`.
    pub fn pending_host_copy(&self) -> Option<&[String]> {
        self.pending_host_copy.as_deref()
    }

    /// Take (and clear) the pending guest→host copy's mime types, for the runtime loop to export to the
    /// host clipboard. `None` when nothing is pending.
    pub fn take_pending_host_copy(&mut self) -> Option<Vec<String>> {
        self.pending_host_copy.take()
    }

    /// Record the host clipboard's current generation as already mirrored to guests, so a guest→host push
    /// the runtime just performed is not bounced straight back to the guest as a fresh host selection.
    pub fn mark_host_clipboard_synced(&mut self) {
        self.host_clip_gen = self.presenter.clipboard_host_generation();
    }
}

// ---- wl_data_device: clipboard (selection) + drag-and-drop -------------------------------------------
//
// Smithay's `DataDeviceState` drives the whole guest↔guest transfer (a guest copies → its `wl_data_source`
// mimes are replayed to the focused guest as a `wl_data_offer`, and a paste `receive(fd)` is forwarded to
// the source). We add two things on top: (1) the host-clipboard bridge — a guest copy is exported to the
// macOS `NSPasteboard`, and the host clipboard is offered back to guests as a compositor selection — via
// the `Presenter` clipboard hooks; (2) drag-and-drop is accepted (Smithay runs the pointer grab / offer
// enter/motion/leave/drop/finish), with the client-initiated `started`/`dropped` callbacks wired below.

impl SelectionHandler for DdState {
    type SelectionUserData = ();

    /// A guest set (or cleared) the clipboard selection. On a set (a copy), queue the offered mime types
    /// for export to the host clipboard — the runtime loop reads the guest source and pushes the payload to
    /// the `NSPasteboard`. A clear leaves the host clipboard untouched (the host owns its own lifetime).
    fn new_selection(
        &mut self,
        ty: SelectionTarget,
        source: Option<SelectionSource>,
        _seat: Seat<Self>,
    ) {
        if ty != SelectionTarget::Clipboard {
            return; // no primary-selection global is advertised; ignore anything but the clipboard.
        }
        if let Some(src) = source {
            self.pending_host_copy = Some(src.mime_types());
        }
    }

    /// A guest is pasting the host-provided (compositor) selection: write the host clipboard payload for
    /// `mime` into the reader's fd. Fully synchronous — the bytes come straight from the `Presenter`.
    fn send_selection(
        &mut self,
        ty: SelectionTarget,
        mime: String,
        fd: OwnedFd,
        _seat: Seat<Self>,
        _user_data: &(),
    ) {
        if ty != SelectionTarget::Clipboard {
            return;
        }
        if let Some(bytes) = self.presenter.clipboard_host_read(&mime) {
            use std::io::Write;
            let mut f = std::fs::File::from(fd);
            let _ = f.write_all(&bytes);
        }
        // Dropping `f`/`fd` closes the write end so the reader sees EOF.
    }
}

impl DataDeviceHandler for DdState {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device
    }
}

/// Client-initiated drag-and-drop. Smithay owns the pointer grab and the `wl_data_offer`
/// enter/motion/leave/drop/finish choreography; these callbacks are the compositor-policy hooks. A DnD
/// cursor icon is left to the host cursor (we drive a single pointer with no surface-backed drag icon).
impl ClientDndGrabHandler for DdState {}

/// Server-initiated drag-and-drop. The compositor never starts a host-driven drag (there is no host drag
/// source in this seat), so the defaults — which simply ignore the negotiation — are correct.
impl ServerDndGrabHandler for DdState {}

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
