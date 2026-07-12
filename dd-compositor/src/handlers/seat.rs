//! `wl_seat` input + `wp_cursor_shape` handlers, and the input-injection methods the platform loop
//! (main.rs, driven by NSEvents) calls to synthesize seat events.
//!
//! The pointer/keyboard handles are `Arc`-backed, so each injector clones the handle out first to avoid
//! aliasing `&mut self` while dispatching into the seat.

use std::os::unix::io::OwnedFd;

use smithay::{
    input::{
        keyboard::{FilterResult, Keycode},
        pointer::{
            AxisFrame, ButtonEvent, CursorIcon, CursorImageStatus, CursorImageSurfaceData,
            MotionEvent, PointerHandle, RelativeMotionEvent,
        },
        Seat, SeatHandler, SeatState,
    },
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::SERIAL_COUNTER,
    wayland::{
        compositor::{with_states, SurfaceAttributes},
        pointer_constraints::{
            with_pointer_constraint, PointerConstraint, PointerConstraintsHandler,
        },
        selection::{
            data_device::{
                set_data_device_selection, ClientDndGrabHandler, DataDeviceHandler, DataDeviceState,
                ServerDndGrabHandler,
            },
            primary_selection::{PrimarySelectionHandler, PrimarySelectionState},
            SelectionHandler, SelectionSource, SelectionTarget,
        },
        shm::with_buffer_contents,
        tablet_manager::TabletSeatHandler,
    },
};

use crate::{surface_id, DdState};

impl SeatHandler for DdState {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }
    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        // `wl_pointer.set_cursor` routes here (as does `wp_cursor_shape_device_v1.set_shape`). Three cases:
        match image {
            // A THEMED cursor: map the `CursorIcon` to the host NSCursor via the reused Presenter seam. The
            // Presenter expects the `wp_cursor_shape_device_v1.shape` enum number, so translate — `CursorIcon`
            // has NO stable discriminant, so `icon as u32` would pick the wrong cursor.
            CursorImageStatus::Named(icon) => {
                self.cursor_surface = None;
                self.presenter.set_cursor_hidden(false);
                self.presenter.set_cursor_shape(cursor_icon_to_wp_shape(icon));
            }
            // A CUSTOM bitmap cursor (CSS `cursor: url(...)`, a game crosshair, a custom app cursor): the
            // client committed (or will commit) a buffer to this surface. It is NOT a window — turn its
            // pixels into a host cursor. If it was already shown as a tiny window (its image committed before
            // it was assigned the cursor role), remove that window first; then build the cursor from its
            // current buffer (or, if none yet, on its next commit — see `on_commit` → `update_cursor_surface`).
            CursorImageStatus::Surface(surface) => {
                self.presenter.drop_window(surface_id(&surface));
                self.cursor_surface = Some(surface.clone());
                self.update_cursor_surface(&surface);
            }
            // The client wants NO pointer (it draws its own, or hides it): hide the host cursor.
            CursorImageStatus::Hidden => {
                self.cursor_surface = None;
                self.presenter.set_cursor_hidden(true);
            }
        }
    }
    fn focus_changed(&mut self, _seat: &Seat<Self>, _focused: Option<&WlSurface>) {
        // A pointer constraint deactivates when its surface loses pointer focus (Smithay sends the
        // unlocked/unconfined event itself). Re-show the cursor if we had hidden it for an active lock.
        self.sync_lock_cursor();
    }
}

/// `wp_cursor_shape_manager_v1` routes a tablet tool's themed cursor here. We drive a single pointer
/// seat (no tablet), so the default (ignore) behaviour is correct — but the impl is required so
/// `delegate_cursor_shape!` can dispatch the shared manager global.
impl TabletSeatHandler for DdState {}

impl DdState {
    /// Absolute pointer motion in logical/point space (top-left origin). Focuses the pointer on the
    /// currently focused toplevel surface. Honours an active `zwp_pointer_constraints` constraint on the
    /// focused surface: a pointer LOCK freezes the absolute position (the motion is dropped — games read
    /// relative deltas instead, via [`Self::relative_motion`]); a CONFINE clamps the pointer to the region.
    pub fn pointer_motion(&mut self, x: f64, y: f64) {
        let (x, y) = match self.constrain_motion(x, y) {
            Some(p) => p,
            None => return, // locked: the absolute pointer stays put
        };
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
        // A button press is the start of an implicit pointer grab: record its serial so a subsequent
        // xdg_toplevel.move/resize carrying it validates as a real user gesture (handlers/xdg.rs).
        if pressed {
            self.note_input_serial(serial);
        }
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

    // ---- relative pointer + pointer constraints (games / 3D / FPS mouselook) ---------------------------

    /// Feed a host relative pointer delta (raw mouse movement in points; the macOS loop reads
    /// `NSEvent.deltaX/deltaY`). Always emits a `zwp_relative_pointer_v1.relative_motion` to the focused
    /// surface — the unaccelerated stream a game/3D app reads for mouselook. When the pointer is NOT locked
    /// the absolute pointer also advances by the delta (ordinary motion, clamped by any confine region);
    /// when a lock is active the absolute pointer stays frozen and only this relative event is delivered.
    pub fn relative_motion(&mut self, dx: f64, dy: f64) {
        let ptr = self.pointer.clone();
        let focus = self.focus.clone().map(|s| (s, (0.0, 0.0).into()));
        let utime = self.now_us();
        // dd has no host pointer-acceleration curve of its own, so accelerated == unaccelerated delta.
        ptr.relative_motion(
            self,
            focus,
            &RelativeMotionEvent {
                delta: (dx, dy).into(),
                delta_unaccel: (dx, dy).into(),
                utime,
            },
        );
        ptr.frame(self);
        if !self.pointer_locked() {
            let (x, y) = self.ptr_loc;
            self.pointer_motion(x + dx, y + dy);
        }
        self.sync_lock_cursor();
    }

    /// Whether the focused surface currently holds an ACTIVE pointer LOCK (`zwp_locked_pointer_v1`). While a
    /// lock is active the absolute pointer is frozen and clients read motion only through
    /// [`Self::relative_motion`]. The macOS loop polls this to decide whether to feed relative deltas.
    pub fn pointer_locked(&self) -> bool {
        let Some(focus) = self.focus.clone() else {
            return false;
        };
        let ptr = self.pointer.clone();
        with_pointer_constraint(&focus, &ptr, |c| {
            matches!(c, Some(c) if c.is_active() && matches!(&*c, PointerConstraint::Locked(_)))
        })
    }

    /// Resolve a proposed absolute pointer location against the focused surface's pointer constraint.
    /// Returns the location to actually move to, or `None` when the pointer is LOCKED (frozen — the caller
    /// drops the motion). An active CONFINE rejects a location outside its region by pinning to the last
    /// in-region location; an unconstrained (or inactive) pointer passes the location through unchanged.
    fn constrain_motion(&self, x: f64, y: f64) -> Option<(f64, f64)> {
        let Some(focus) = self.focus.clone() else {
            return Some((x, y));
        };
        let ptr = self.pointer.clone();
        with_pointer_constraint(&focus, &ptr, |c| {
            let Some(c) = c else { return Some((x, y)) };
            if !c.is_active() {
                return Some((x, y));
            }
            match &*c {
                PointerConstraint::Locked(_) => None,
                PointerConstraint::Confined(_) => match c.region() {
                    // Region is in surface-local logical coords; our pointer is focused at (0,0) offset, so
                    // ptr_loc IS surface-local. Reject a move that leaves the region (stay where we were).
                    Some(region) if !region.contains((x.round() as i32, y.round() as i32)) => {
                        Some(self.ptr_loc)
                    }
                    _ => Some((x, y)),
                },
            }
        })
    }

    /// Keep the host cursor's visibility in step with the pointer-lock state: hide the system cursor while a
    /// lock is active (FPS mouselook), and re-show exactly that cursor when the lock releases or focus
    /// leaves. Only toggles what it hid (via `cursor_hidden_by_lock`), so it never clobbers a cursor a
    /// client hid deliberately with `set_cursor(null)`.
    pub(crate) fn sync_lock_cursor(&mut self) {
        let locked = self.pointer_locked();
        if locked && !self.cursor_hidden_by_lock {
            self.cursor_hidden_by_lock = true;
            self.presenter.set_cursor_hidden(true);
        } else if !locked && self.cursor_hidden_by_lock {
            self.cursor_hidden_by_lock = false;
            self.presenter.set_cursor_hidden(false);
        }
    }

    /// Build (or refresh) the host cursor from a custom cursor surface's committed `wl_shm` buffer. Called
    /// from `cursor_image` (when the surface is assigned the cursor role) and from the commit path (when the
    /// cursor surface re-commits — animated/updated cursors). The hotspot lives in the surface's
    /// `CursorImageSurfaceData` (logical coords); it is scaled to buffer pixels for the Presenter, which
    /// wraps the pixels in an `NSCursor`. No committed buffer yet ⇒ nothing to do (built on first commit).
    pub(crate) fn update_cursor_surface(&mut self, surface: &WlSurface) {
        let sid = surface_id(surface);
        let Some(buffer) = self.buffers.get(&sid).cloned() else {
            return;
        };
        let (hotspot, scale) = with_states(surface, |states| {
            let hotspot = states
                .data_map
                .get::<CursorImageSurfaceData>()
                .map(|m| m.lock().unwrap().hotspot)
                .unwrap_or_default();
            let scale = states
                .cached_state
                .get::<SurfaceAttributes>()
                .current()
                .buffer_scale
                .max(1);
            (hotspot, scale)
        });
        // Repack the shm buffer into tight BGRA (honouring the pool offset + row stride), then hand it to
        // the Presenter as a bitmap cursor. Small (cursor-sized) copy — no fast-path needed.
        let read = with_buffer_contents(&buffer, |ptr, _len, data| {
            let (w, h, stride, off) = (data.width, data.height, data.stride, data.offset);
            if w <= 0 || h <= 0 {
                return (w, h, Vec::new());
            }
            let tight = (w * 4) as usize;
            let mut bgra = vec![0u8; tight * h as usize];
            for row in 0..h as isize {
                let src = unsafe { ptr.offset(off as isize + row * stride as isize) };
                let dstart = row as usize * tight;
                unsafe {
                    std::ptr::copy_nonoverlapping(src, bgra[dstart..].as_mut_ptr(), tight);
                }
            }
            (w, h, bgra)
        });
        if let Ok((w, h, bgra)) = read {
            if !bgra.is_empty() {
                self.presenter.set_cursor_hidden(false);
                self.presenter
                    .set_cursor_buffer(&bgra, w, h, hotspot.x * scale, hotspot.y * scale);
            }
        }
    }

    /// Whether `surface` is the client's current custom cursor surface. The commit path consults this to
    /// route a cursor-surface commit to [`Self::update_cursor_surface`] instead of presenting it as a window.
    pub(crate) fn is_cursor_surface(&self, surface: &WlSurface) -> bool {
        self.cursor_surface.as_ref() == Some(surface)
    }

    /// Keyboard key (raw evdev keycode; we add the +8 XKB offset here). The focused client's own
    /// xkbcommon turns the keycode into a keysym.
    pub fn key(&mut self, evdev: u32, pressed: bool) {
        use smithay::backend::input::KeyState;
        let kbd = self.keyboard.clone();
        let serial = SERIAL_COUNTER.next_serial();
        // A key press is also a valid grab-initiating input event (a keyboard-driven move/resize).
        if pressed {
            self.note_input_serial(serial);
        }
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
        // Only the clipboard is bridged to the host `NSPasteboard`. The PRIMARY selection
        // (`zwp_primary_selection_v1`) is wired guest↔guest only — Smithay routes a reader straight to the
        // source client — so a `Primary` set needs no host export and is ignored here.
        if ty != SelectionTarget::Clipboard {
            return;
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

// ---- zwp_primary_selection_v1: X11-style primary (middle-click) selection ----------------------------
//
// The primary selection is the "select-to-copy, middle-click-to-paste" buffer terminals and GTK/Qt apps
// expect. Smithay drives the whole guest↔guest transfer through the SAME `SelectionHandler` as the
// clipboard (a `Primary` target on `new_selection`/`send_selection`), and routes a reader straight to the
// owning client, so the handler is a one-liner state getter; the compositor only follows keyboard focus
// with it (see `DdState::focus_surface` → `set_primary_focus`). No host-clipboard bridge (kept optional).

impl PrimarySelectionHandler for DdState {
    fn primary_selection_state(&self) -> &PrimarySelectionState {
        &self.primary_selection
    }
}

// ---- zwp_pointer_constraints_v1: pointer lock / confine (games / 3D / FPS mouselook) -----------------
//
// A client (a game, a 3D viewport, a drawing app) creates a lock or confine constraint on its surface +
// this seat's pointer. The compositor decides WHEN to activate; our policy is to activate as soon as the
// constrained surface holds focus (games request the lock right after their window is focused). Once a
// lock is active the injection path freezes the absolute pointer and the macOS loop feeds relative deltas
// through `relative_motion`; a confine clamps absolute motion to the region (see `constrain_motion`).

impl PointerConstraintsHandler for DdState {
    fn new_constraint(&mut self, surface: &WlSurface, pointer: &PointerHandle<Self>) {
        // Activate immediately if the constrained surface currently holds focus; otherwise leave it pending
        // (Smithay activates nothing on its own — the compositor owns the policy).
        if self.focus.as_ref() == Some(surface) {
            with_pointer_constraint(surface, pointer, |c| {
                if let Some(c) = c {
                    c.activate();
                }
            });
            // Hide the system cursor for an active LOCK (FPS mouselook); a confine keeps the cursor visible.
            self.sync_lock_cursor();
        }
    }

    fn cursor_position_hint(
        &mut self,
        _surface: &WlSurface,
        _pointer: &PointerHandle<Self>,
        _location: smithay::utils::Point<f64, smithay::utils::Logical>,
    ) {
        // A locked client tells us where it is drawing its own cursor, for the moment the lock releases. We
        // render no guest cursor surface during a lock (the host cursor is hidden), so nothing to place.
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
