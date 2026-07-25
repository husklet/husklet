use super::*;

/// One host/test-driven input action delivered through the compositor input channel.
#[derive(Clone, Debug, PartialEq)]
pub enum InputCommand {
    /// Move the pointer to root-local logical `(x, y)`; re-hit-tests focus and emits enter/leave/motion.
    ///
    /// This is ALSO the seam that drives a `wl_data_device` drag-and-drop: once a source client's
    /// `start_drag` is honoured (in response to a [`Self::PointerButton`] press, whose serial anchors the
    /// implicit grab), Smithay replaces the pointer's grab with its DnD grab, and every subsequent
    /// `PointerMotion` routes through it — carrying the drag over whatever surface the point hit-tests to
    /// (`wl_data_device.enter`/`motion`, or `leave` on moving off). A [`Self::PointerButton`] release then
    /// performs the drop. So no bespoke drag command is needed: the ordinary pointer seam IS the drag
    /// pointer path (watch [`Observations::dnd_active`](super::present::Observations) to know the grab is
    /// live). See the `drag_and_drop` demo.
    PointerMotion {
        x: f64,
        y: f64,
    },
    /// Native-window motion constrained to the tree owning `window`.
    PointerMotionOn {
        window: SurfaceId,
        x: f64,
        y: f64,
    },
    /// Press/release a pointer button (Linux `input-event-codes`, e.g. `0x110` = BTN_LEFT).
    PointerButton {
        button: u32,
        pressed: bool,
    },
    /// Native-window button paired with an exact originating window.
    PointerButtonOn {
        window: SurfaceId,
        button: u32,
        pressed: bool,
        click_count: u8,
    },
    ResizeSurface {
        surface: SurfaceId,
        width: u32,
        height: u32,
        maximized: bool,
        fullscreen: bool,
        resizing: bool,
    },
    ResizeSurfaceEnd {
        surface: SurfaceId,
    },
    /// Scroll: `horizontal`/`vertical` are logical scroll amounts (wheel source).
    PointerAxis {
        horizontal: f64,
        vertical: f64,
    },
    /// Scroll with DISCRETE steps — a real mouse WHEEL, which emits both a smooth value and a discrete
    /// notch count. `horizontal`/`vertical` are the smooth logical amounts; `h120`/`v120` the
    /// high-resolution discrete steps (120 units = one wheel detent, the `wl_pointer` v8 convention).
    /// Delivered as `wl_pointer.axis` (smooth) + `axis_source(wheel)` + `axis_value120` (client v8+, or
    /// `axis_discrete` on v5-7), all grouped in ONE `wl_pointer.frame`.
    PointerAxisDiscrete {
        horizontal: f64,
        vertical: f64,
        h120: i32,
        v120: i32,
    },
    /// Press/release a key by EVDEV keycode (Linux `input-event-codes`, e.g. `30` = KEY_A) — the same
    /// value the client receives on `wl_keyboard.key`.
    Key {
        keycode: u32,
        pressed: bool,
    },
    /// Route the toplevel at index `n` (ascending surface-id order, 0 = earliest-mapped) to the output
    /// whose logical rectangle contains global logical point `(x, y)`, emitting the resulting
    /// `wl_surface.leave`/`enter` and refreshing its preferred fractional scale. The host/window-manager
    /// seam a multi-output demo drives to "place" a window on a monitor by position (see
    /// [`HlState::move_toplevel_to_point`]). A point outside every output — or an out-of-range index — is
    /// ignored. Under the default single-output layout every on-screen point resolves to that one output.
    MoveToplevelToPoint {
        index: usize,
        x: i32,
        y: i32,
    },
    /// Give keyboard focus to the topmost toplevel (emits `wl_keyboard.leave`/`enter` + keymap).
    FocusTopmostKeyboard,
    /// Give keyboard focus to the toplevel owning a specific native presenter surface.
    FocusSurface(SurfaceId),
    /// Give keyboard focus to the toplevel at index `n` in ascending surface-id order (0 = the
    /// earliest-mapped toplevel). Lets a host/test target a SPECIFIC window in a multi-window stack —
    /// `FocusTopmostKeyboard` can only reach the highest id. Out-of-range `n` clears focus (no such
    /// window). The neutral scene models no global stacking, so ascending id (== map order for
    /// sequentially-mapped windows) is the stable, inspectable ordering a driver can reason about.
    FocusToplevelIndex(usize),
    /// Clear keyboard focus (emits `wl_keyboard.leave` to the previously focused surface).
    ClearKeyboardFocus,
    /// Deliver an IME `commit_string` to the focused, enabled `zwp_text_input_v3` — the committed text the
    /// client inserts at its cursor (what an input method produces when a composition is accepted, e.g.
    /// typing "hello"). Wrapped in a `done` so the client applies it immediately. A no-op if no text-input
    /// is focused+active. The host IME seam, mirroring [`Self::Key`] for composed text.
    ImeCommitString(String),
    /// Deliver an IME `preedit_string` to the focused, enabled `zwp_text_input_v3` — the COMPOSING
    /// (pre-edit / underlined) text, with `cursor_begin`/`cursor_end` byte offsets into it. Wrapped in a
    /// `done`. This is the transient text shown before a commit; a following [`Self::ImeCommitString`]
    /// (with an empty preedit) replaces it.
    ImePreeditString {
        text: String,
        cursor_begin: i32,
        cursor_end: i32,
    },
    /// Deliver an IME `delete_surrounding_text` to the focused, enabled `zwp_text_input_v3` — delete
    /// `before_length` bytes before and `after_length` bytes after the cursor (what an IME does when a
    /// composition rewrites already-committed text). Wrapped in a `done`.
    ImeDeleteSurrounding {
        before_length: u32,
        after_length: u32,
    },
    /// Ask the topmost mapped toplevel to close (`xdg_toplevel.close`) — the compositor-initiated close
    /// request (e.g. a window-manager close button / `wm_close`). The client receives the event and
    /// typically tears the toplevel down; the compositor sends only the request (a `close` carries no
    /// reply). A no-op if no toplevel is mapped.
    CloseTopmostToplevel,
    /// Ask the toplevel owning a specific native presenter surface to close.
    CloseSurface(SurfaceId),

    // ----- wl_touch (multi-touch) -----
    /// A new touch point `id` appeared at root-local logical `(x, y)`. Hit-tests the surface under the point
    /// and delivers `wl_touch.down` (with the surface-local coordinate) to the client that owns it. Each
    /// live `id` is an independent finger; distinct ids coexist so a multi-touch gesture is expressed by
    /// interleaving several. Delivered on the SAME touch frame until [`Self::TouchFrame`] closes it.
    TouchDown {
        id: i32,
        x: f64,
        y: f64,
    },
    /// Touch point `id` moved to root-local logical `(x, y)` — `wl_touch.motion` at the surface-local
    /// coordinate. A no-op if `id` is not a live down point.
    TouchMotion {
        id: i32,
        x: f64,
        y: f64,
    },
    /// Touch point `id` lifted — `wl_touch.up`. The id is released and may be reused by a later down.
    TouchUp {
        id: i32,
    },
    /// Close the current touch frame — `wl_touch.frame`. Groups all the down/motion/up delivered since the
    /// last frame into one atomic update the client applies together (the touch-protocol contract).
    TouchFrame,
    /// Cancel the whole active touch sequence — `wl_touch.cancel` (the compositor took the gesture over,
    /// e.g. an edge swipe). The client discards every in-progress touch point.
    TouchCancel,

    // ----- zwp_pointer_gestures_v1 (trackpad pinch/swipe) -----
    /// Begin a multi-finger SWIPE gesture with `fingers` fingers — `zwp_pointer_gesture_swipe_v1.begin` to
    /// the pointer-focused surface (set the focus first with a [`Self::PointerMotion`]). A no-op if no
    /// surface is focused or the client bound no swipe-gesture object.
    GestureSwipeBegin {
        fingers: u32,
    },
    /// Update the active swipe by logical center delta `(dx, dy)` — `zwp_pointer_gesture_swipe_v1.update`.
    GestureSwipeUpdate {
        dx: f64,
        dy: f64,
    },
    /// End the active swipe — `zwp_pointer_gesture_swipe_v1.end` (`cancelled` = the gesture was aborted, not
    /// completed).
    GestureSwipeEnd {
        cancelled: bool,
    },
    /// Begin a multi-finger PINCH gesture with `fingers` fingers — `zwp_pointer_gesture_pinch_v1.begin`
    /// (pinch-to-zoom). Targets the pointer-focused surface. A no-op if no surface is focused or the client
    /// bound no pinch-gesture object.
    GesturePinchBegin {
        fingers: u32,
    },
    /// Update the active pinch by logical center delta `(dx, dy)`, absolute `scale` (relative to begin, 1.0
    /// = unchanged), and `rotation` degrees clockwise since the previous update —
    /// `zwp_pointer_gesture_pinch_v1.update`.
    GesturePinchUpdate {
        dx: f64,
        dy: f64,
        scale: f64,
        rotation: f64,
    },
    /// End the active pinch — `zwp_pointer_gesture_pinch_v1.end` (`cancelled` = aborted).
    GesturePinchEnd {
        cancelled: bool,
    },

    // ----- zwp_tablet_tool_v2 (stylus) -----
    /// The pen entered proximity of the surface under root-local logical `(x, y)` —
    /// `zwp_tablet_tool_v2.proximity_in(tablet, surface)` + a first `motion` + `frame`. The tool is now
    /// hovering over that client. A no-op if no surface is under the point.
    TabletToolProximityIn {
        x: f64,
        y: f64,
    },
    /// The pen moved (while in proximity) to root-local logical `(x, y)`, reporting absolute `pressure`
    /// (0.0–1.0; queued and sent with the motion) — `zwp_tablet_tool_v2.motion` (+ `pressure` + `frame`).
    TabletToolMotion {
        x: f64,
        y: f64,
        pressure: f64,
    },
    /// The pen tip made contact — `zwp_tablet_tool_v2.down` (+ `frame`). The stylus is now "drawing".
    TabletToolTipDown,
    /// The pen tip lifted — `zwp_tablet_tool_v2.up` (+ `frame`).
    TabletToolTipUp,
    /// The pen left proximity — `zwp_tablet_tool_v2.proximity_out` (+ `frame`). Hovering ends.
    TabletToolProximityOut,

    // ----- ext_session_lock_manager_v1 (screen lock) -----
    /// Lock the session AS THE COMPOSITOR would on an incoming client `lock` — hide every normal toplevel
    /// and mark the session locked. In practice the CLIENT drives the lock over the wire
    /// (`ext_session_lock_manager_v1.lock`), so this host seam is mainly for a host-initiated lock; the demo
    /// drives it through the real protocol. (Kept for symmetry / host control.)
    SessionLock,
    /// Unlock the session — restore every normal toplevel to visible. Mirrors [`Self::SessionLock`].
    SessionUnlock,
}
