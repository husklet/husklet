//! [`HlState`]: the Smithay dispatch aggregate (OVERVIEW-v2 §7) — an ADAPTER object.
//!
//! It holds Smithay's `wayland_frontend` state cores (`CompositorState` / `ShmState` / `XdgShellState`)
//! and the neutral [`crate::Compositor`] engine (scene + presenter + clock). The `Handler` callbacks
//! decode the wire (Smithay did the hard part) and translate each `wl_*`/`xdg_*` event into a call on
//! the neutral `scene::service` layer via `engine` — NO compositing/pacing policy lives here, only the
//! translation. Ported from `hl-compositor`'s `HlState` (`register_surface` / `on_commit` /
//! `ingest_buffer`), with the GPU/budget/Cocoa machinery dropped and Smithay reads mapped onto the
//! neutral [`crate::scene::model`].

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use hl_log::{hl_count, hl_debug, hl_info, tag};

use smithay::backend::input::{
    Axis, AxisSource, ButtonState, KeyState, TabletToolCapabilities, TabletToolDescriptor,
    TabletToolType, TouchSlot,
};
use smithay::input::{
    keyboard::{FilterResult, Keycode, XkbConfig},
    pointer::{
        AxisFrame, ButtonEvent, GesturePinchBeginEvent, GesturePinchEndEvent,
        GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent,
        GestureSwipeUpdateEvent, MotionEvent, PointerHandle, RelativeMotionEvent,
    },
    touch::{
        DownEvent as TouchDownEvent, MotionEvent as TouchMotionEvent, UpEvent as TouchUpEvent,
    },
    Seat, SeatHandler, SeatState,
};
use smithay::output::{
    Mode as OutputMode, Output as WlOutputHandle, PhysicalProperties, Scale, Subpixel,
};
use smithay::reexports::wayland_server::{
    backend::{ClientData, ClientId, DisconnectReason, GlobalId, ObjectId},
    protocol::{wl_buffer::WlBuffer, wl_callback::WlCallback, wl_shm, wl_surface::WlSurface},
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum, Weak,
};
use smithay::utils::Transform;
use smithay::utils::{Logical, Point, Size, SERIAL_COUNTER};
use smithay::wayland::{
    buffer::BufferHandler,
    compositor::Cacheable,
    compositor::{
        get_children, get_parent, is_sync_subsurface, with_states, BufferAssignment,
        CompositorClientState, CompositorHandler, CompositorState, Damage, RectangleKind,
        RegionAttributes, SubsurfaceCachedState, SurfaceAttributes,
    },
    content_type::{ContentTypeState, ContentTypeSurfaceCachedState},
    cursor_shape::CursorShapeManagerState,
    fractional_scale::{
        with_fractional_scale, FractionalScaleHandler, FractionalScaleManagerState,
    },
    idle_inhibit::{IdleInhibitHandler, IdleInhibitManagerState},
    input_method::{InputMethodHandler, InputMethodManagerState, PopupSurface as ImePopupSurface},
    keyboard_shortcuts_inhibit::{
        KeyboardShortcutsInhibitHandler, KeyboardShortcutsInhibitState, KeyboardShortcutsInhibitor,
    },
    output::{OutputHandler, OutputManagerState},
    pointer_constraints::{
        with_pointer_constraint, PointerConstraint, PointerConstraintsHandler,
        PointerConstraintsState,
    },
    pointer_gestures::PointerGesturesState,
    presentation::{
        PresentationFeedbackCachedState, PresentationFeedbackCallback, PresentationState, Refresh,
    },
    relative_pointer::RelativePointerManagerState,
    selection::{
        data_device::{
            set_data_device_focus, set_data_device_selection, ClientDndGrabHandler,
            DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
        },
        primary_selection::{set_primary_focus, PrimarySelectionHandler, PrimarySelectionState},
        SelectionHandler, SelectionSource, SelectionTarget,
    },
    session_lock::{LockSurface, SessionLockHandler, SessionLockManagerState, SessionLocker},
    shell::xdg::{
        decoration::{XdgDecorationHandler, XdgDecorationState},
        PopupSurface, PositionerState, SurfaceCachedState as XdgSurfaceCachedState,
        ToplevelSurface, XdgShellHandler, XdgShellState, XdgToplevelSurfaceData,
    },
    shm::{with_buffer_contents, ShmHandler, ShmState},
    single_pixel_buffer::{get_single_pixel_buffer, SinglePixelBufferState},
    tablet_manager::{
        TabletDescriptor, TabletHandle, TabletManagerState, TabletSeatHandler, TabletSeatTrait,
        TabletToolHandle,
    },
    text_input::{TextInputManagerState, TextInputSeat},
    viewporter::{ensure_viewport_valid, ViewportCachedState, ViewporterState},
    xdg_activation::{
        XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
    },
};

#[cfg(feature = "macos-surface")]
use smithay::backend::allocator::dmabuf::WeakDmabuf;
/// `zwp_linux_dmabuf_v1` server plumbing — the accelerated present path real toolkits + Chrome use.
/// `DrmFormat`/`Fourcc`/`Modifier` name the DRM format+modifier pairs the global advertises and the
/// importer validates; `Dmabuf` is the imported buffer (its `handles()`/`strides()`/`offsets()` are what
/// [`read_dmabuf_rgba`] `pread`s). `Format` is aliased `DrmFormat` because `crate::scene::model::Format`
/// already owns the `Format` name here. `Buffer as _` brings the `width()`/`height()`/`format()` accessors
/// into scope for `Dmabuf`.
use smithay::backend::allocator::{
    dmabuf::Dmabuf, Buffer as _, Format as DrmFormat, Fourcc, Modifier,
};
use smithay::wayland::dmabuf::{
    get_dmabuf, DmabufDeviceId, DmabufFeedbackBuilder, DmabufGlobal, DmabufHandler, DmabufState,
    ImportNotifier,
};

/// The `wp_presentation_feedback.presented` `flags` bitmask (vsync / hw-clock / …) — the presentation
/// feedback sent when a surface's committed content reaches the screen.
use smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback;

/// The `wp_content_type_v1.set_content_type` hint enum (`none`/`photo`/`video`/`game`) a client attaches to
/// a surface. Read from the committed [`ContentTypeSurfaceCachedState`] and recorded (as its wire value) for
/// the test to assert; the neutral scene carries no content-type policy headless.
use smithay::reexports::wayland_protocols::wp::content_type::v1::server::wp_content_type_v1::Type as ContentType;

/// `wp_tearing_control_v1` (staging) — the Chrome/Ozone "present hint" protocol. Smithay 0.7 ships NO
/// handler for it, so the manager + per-surface object are dispatched by hand below (mirroring the shape of
/// Smithay's own `content_type` handler). A client attaches a `wp_tearing_control_v1` to a `wl_surface` and
/// sets a per-surface presentation hint (`vsync` = the compositor should not tear / `async` = tearing is
/// acceptable for lowest latency). The hint is double-buffered (applied at `wl_surface.commit`), read at
/// commit into [`Observations`] exactly like `content_type`.
use smithay::reexports::wayland_protocols::wp::tearing_control::v1::server::{
    wp_tearing_control_manager_v1::{self, WpTearingControlManagerV1},
    wp_tearing_control_v1::{self, PresentationHint, WpTearingControlV1},
};

use hl_surface_protocol::server::{
    hl_surface_identity_v1::{self, HlSurfaceIdentityV1},
    hl_surface_manager_v1::{self, HlSurfaceManagerV1},
};
/// The `xdg_positioner` anchor/gravity/constraint-adjustment wire enums — mapped onto the neutral
/// [`crate::scene::model`] positioner value types so the scene's placement math (not Smithay's) resolves
/// the popup geometry.
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_positioner::{
    Anchor as WireAnchor, ConstraintAdjustment as WireConstraint, Gravity as WireGravity,
};
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::reexports::wayland_server::protocol::wl_seat::WlSeat;
use smithay::utils::{Rectangle, Serial};

/// The zxdg-decoration mode the wire speaks (`ServerSide` / `ClientSide`).
use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode;
/// The `xdg_toplevel` state enum (`Activated` / `Maximized` / `Fullscreen` / …) sent in a configure.
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::{
    State as XdgToplevelState, WmCapabilities,
};

use crate::scene::model::{
    Anchor, BufferState, BufferTransform, ConstraintAdjustment, Format, Gravity, Output, OutputId,
    PopupState, Positioner, Rect, SubsurfaceState, SurfaceId, SurfaceRole, Viewport, Visibility,
};
use crate::scene::port::{Clipboard, Clock, Windows};
use crate::scene::service::{surface_at, BufferChange, Commit};
use crate::{Compositor, FrameOutcome};

use super::present::{AdapterPresenter, Observations, StoredBuffer};

/// Initial floating size a toplevel is configured to before it commits real content.
const INITIAL_TOPLEVEL_SIZE: (i32, i32) = (800, 600);

/// The host monotonic clock the scene paces on.
pub struct MonotonicClock {
    start: Instant,
}

impl MonotonicClock {
    pub fn new() -> MonotonicClock {
        MonotonicClock {
            start: Instant::now(),
        }
    }
}

impl Default for MonotonicClock {
    fn default() -> MonotonicClock {
        MonotonicClock::new()
    }
}

impl Clock for MonotonicClock {
    fn now_nanos(&self) -> u64 {
        self.start.elapsed().as_nanos() as u64
    }
}

/// Per-client protocol state Smithay threads through its handlers.
#[derive(Default)]
pub struct ClientState {
    pub compositor: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

/// The Smithay dispatch aggregate: protocol cores + the neutral compositor engine.
pub struct HlState {
    /// A clone of the server `DisplayHandle`, kept so focus changes can resolve a `WlSurface`'s owning
    /// `Client` and retarget the clipboard (data-device) focus via [`set_data_device_focus`].
    display: DisplayHandle,
    pub compositor: CompositorState,
    pub shm: ShmState,
    /// Owns the `zwp_linux_dmabuf_v1` global — the ACCELERATED present path real toolkits (GTK/Qt EGL)
    /// and Chrome's ozone/GPU probe instead of `wl_shm`. Advertised as a v4/v5 feedback global carrying a
    /// single main tranche of LINEAR ARGB8888/XRGB8888 (see [`dmabuf_formats`]); a v3 binder receives the
    /// same pairs as `modifier` events. LINEAR is the only modifier a SOFTWARE presenter can truthfully
    /// import: the buffer's plane fd is plain byte-linear CPU memory, so [`read_dmabuf_rgba`] `pread`s and
    /// unpacks it exactly like an shm buffer (no GPU detile). A tiled/GPU-modifier or multi-plane buffer is
    /// rejected at import ([`DmabufHandler::dmabuf_imported`]) so the client falls back to `wl_shm` rather
    /// than committing a buffer the compositor could never turn into pixels. Held for the state's lifetime
    /// so the global keeps advertising. See the `dmabuf_present` demo.
    pub dmabuf: DmabufState,
    pub xdg_shell: XdgShellState,
    /// Owns the `zxdg_decoration_manager_v1` global. A client (GTK/Chrome/Qt) binds it, calls
    /// `get_toplevel_decoration` on its toplevel, and negotiates server-side vs client-side decorations;
    /// without an answering `configure(mode)` those toolkits stall waiting to learn whether to draw their
    /// own CSD. Held for the state's lifetime so the global keeps advertising.
    pub xdg_decoration: XdgDecorationState,
    /// Owns the `wl_data_device_manager` global (clipboard / drag-and-drop). GDK4's Wayland backend —
    /// and Chrome/Qt — hard-require this interface at display-open: without it `gdk_display_open` aborts
    /// with "The Wayland compositor does not provide one or more of the required interfaces" before any
    /// GL/EGL is touched. A client binds the manager, calls `get_data_device(seat)` to obtain a
    /// `wl_data_device`, and drives selection (copy/paste) + DnD through it; clipboard focus follows the
    /// keyboard focus via [`set_data_device_focus`]. Held for the state's lifetime so the global keeps
    /// advertising.
    pub data_device: DataDeviceState,
    /// Owns the `zwp_primary_selection_device_manager_v1` global (the middle-click PRIMARY selection —
    /// the X11-style select-to-copy / middle-click-to-paste clipboard, distinct from the CTRL+C
    /// `wl_data_device` selection). GTK/Qt terminals + editors bind it; a `zwp_primary_selection_source`
    /// set while a surface holds keyboard focus becomes readable by the next focused client over a real fd,
    /// exactly like the data-device clipboard. Primary-selection focus follows keyboard focus via
    /// [`set_primary_focus`] (see [`Self::set_keyboard_focus`]). Held for the state's lifetime so the global
    /// keeps advertising.
    pub primary_selection: PrimarySelectionState,
    /// Owns the `xdg_activation_v1` global (cross-client focus stealing / startup notification). A launcher
    /// client requests an activation token (`xdg_activation_token_v1`, optionally carrying the seat+serial of
    /// the input event that triggered a launch), hands the token string to the client it launched, and that
    /// client calls `xdg_activation_v1.activate(token, surface)` to ask the compositor to bring `surface` to
    /// the front. The headless policy honours it by giving the target surface keyboard focus (see
    /// [`XdgActivationHandler::request_activation`]) — the client observes the activation as a
    /// `wl_keyboard.enter`. Held for the state's lifetime so the global keeps advertising.
    _xdg_activation: XdgActivationState,
    /// Owns the `zwp_idle_inhibit_manager_v1` global (inhibit the screensaver / DPMS while a surface is
    /// visible — video players, presentations). A client calls `create_inhibitor(surface)`; the compositor
    /// TRACKS the inhibitor (there is no reply event) and, on a real host, suppresses idle while the surface
    /// is mapped. Headless there is nothing to suppress, so the handler records the inhibited surface in
    /// [`Observations`] (create → tracked, `zwp_idle_inhibitor_v1.destroy` → untracked). Held for the state's
    /// lifetime so the global keeps advertising.
    _idle_inhibit: IdleInhibitManagerState,
    /// Owns the `wp_content_type_manager_v1` global (the per-surface content-type hint: `photo`/`video`/
    /// `game`, used to pick tearing/scaling/latency policy). A client calls `get_surface_content_type(surface)`
    /// then `set_content_type(hint)`; the hint is double-buffered and applied at commit. The adapter reads it
    /// from the committed [`ContentTypeSurfaceCachedState`] each commit and records it in [`Observations`]
    /// (there is no reply event). Held for the state's lifetime so the global keeps advertising.
    _content_type: ContentTypeState,
    /// Owns the `zwp_relative_pointer_manager_v1` global. A client binds it and calls
    /// `get_relative_pointer(wl_pointer)` to receive UNACCELERATED relative motion deltas
    /// (`zwp_relative_pointer_v1.relative_motion`) independent of the absolute `wl_pointer.motion` — what
    /// FPS games / 3D viewports / pointer-lock web content consume. The adapter delivers the delta on every
    /// injected motion via [`PointerHandle::relative_motion`]. Held for the state's lifetime.
    _relative_pointer: RelativePointerManagerState,
    /// Owns the `zwp_pointer_constraints_v1` global (pointer lock / confinement). A client binds it and calls
    /// `lock_pointer` / `confine_pointer` on a surface + `wl_pointer`; the compositor activates the
    /// constraint while that surface holds pointer focus (see [`PointerConstraintsHandler::new_constraint`]).
    /// A LOCKED pointer stops receiving absolute `wl_pointer.motion` (its position is frozen) and drives the
    /// client purely through relative motion — the standard pointer-lock experience. Held for the state's
    /// lifetime.
    _pointer_constraints: PointerConstraintsState,
    /// Owns the `wp_presentation` global (presentation timing feedback). A client binds it and calls
    /// `feedback(surface, callback)` to learn WHEN a committed frame actually hit the screen; the adapter
    /// answers `wp_presentation_feedback.presented` (monotonic timestamp + refresh + sequence) when that
    /// frame's tree presents, or `.discarded` when the frame is torn down unshown. Advertised with
    /// `CLOCK_MONOTONIC` as its clock id (matching [`MonotonicClock`]). Held for the state's lifetime.
    _presentation: PresentationState,
    /// Owns the `wp_viewporter` global. A client binds it, calls `get_viewport(surface)`, and sets a
    /// source crop (`set_source`) and/or destination size (`set_destination`) — HiDPI/media clients
    /// (video, browsers) use it to crop letterboxing and scale a buffer to a logical size without
    /// re-rendering. Smithay caches the state (`ViewportCachedState`) per surface; the adapter reads it at
    /// commit and mirrors it into the neutral scene, which resolves the on-screen logical size. Held for
    /// the state's lifetime so the global keeps advertising.
    _viewporter: ViewporterState,
    /// Owns the `wp_fractional_scale_manager_v1` global. A client binds it and calls
    /// `get_fractional_scale(surface)` to learn the compositor's preferred fractional scale
    /// (`preferred_scale`, sent as scale×120) so it can render crisply on HiDPI without integer-only
    /// `wl_surface.set_buffer_scale`. Held for the state's lifetime so the global keeps advertising.
    _fractional_scale: FractionalScaleManagerState,
    /// Owns the `zwp_text_input_manager_v3` global (on-screen keyboard / IME text entry). A client
    /// (GTK/Qt/Chrome) binds it, calls `get_text_input(seat)`, `enable`s text input on its focused surface,
    /// and then receives `preedit_string` (composing text) + `commit_string` (+ `delete_surrounding_text`)
    /// from the compositor's input method, applied on each `done`. Smithay routes text-input entirely
    /// through an input method (`zwp_input_method_v2`): a text-input request is only honoured, and `enter`
    /// only sent, while an input method instance exists — which is why the manager below is also advertised.
    /// The host IME seam ([`InputCommand::ImeCommitString`] etc.) delivers the events on the focused
    /// text-input; Smithay stamps the correct `done` serial. Held for the state's lifetime.
    _text_input: TextInputManagerState,
    /// Owns the `zwp_input_method_manager_v2` global (the INPUT-METHOD side of text entry — what an IME
    /// backend like ibus/fcitx binds). Advertised because Smithay gates all `zwp_text_input_v3` delivery on
    /// an input method instance existing on the seat (`TextInputHandle` only sends `enter` and only routes
    /// commit/preedit while `input_method.has_instance()`). A headless IME "backend" client binds this and
    /// calls `get_input_method(seat)` so `has_instance()` is true; the compositor then sends the real
    /// text-input events (driven by the host seam) with Smithay's own serial tracking. Held for the state's
    /// lifetime. See the `text_input_ime` demo.
    _input_method: InputMethodManagerState,
    /// Owns the `zwp_pointer_gestures_v1` global (multi-finger touchpad gestures: pinch + swipe + hold).
    /// A client binds it and calls `get_swipe_gesture`/`get_pinch_gesture(wl_pointer)` to receive
    /// `zwp_pointer_gesture_swipe_v1` / `_pinch_v1` begin/update/end events grouped with the pointer focus —
    /// what a browser (pinch-to-zoom) or a document viewer (two-finger swipe) consumes from a trackpad. The
    /// adapter delivers them through the seat's [`PointerHandle`] gesture methods (see
    /// [`HlState::inject_gesture_swipe_begin`] etc.); the focused surface (set by the last pointer motion) is
    /// the target. Held for the state's lifetime so the global keeps advertising.
    _pointer_gestures: PointerGesturesState,
    /// Owns the `zwp_tablet_manager_v2` global (graphics tablet / stylus). A client binds it, calls
    /// `get_tablet_seat(seat)`, and receives `tablet_added` + `tool_added` for the advertised
    /// [`TabletHandle`] / [`TabletToolHandle`] below, then `zwp_tablet_tool_v2` proximity/tip/motion/pressure
    /// events as the host seam drives the tool (see [`HlState::inject_tablet_tool_proximity_in`] etc.). Held
    /// for the state's lifetime so the global keeps advertising.
    _tablet_manager: TabletManagerState,
    /// The single advertised graphics tablet — the device a `zwp_tablet_tool_v2` reports proximity against.
    /// Added to the seat's tablet-seat at construction so a client that binds `get_tablet_seat` after the
    /// fact still receives `tablet_added` for it (smithay re-advertises existing tablets on bind). Held so
    /// the tool's proximity_in can name it.
    tablet: TabletHandle,
    /// The single advertised tablet TOOL (a pen with pressure capability). The host
    /// stylus seam ([`InputCommand::TabletToolProximityIn`] etc.) drives it; smithay serializes the
    /// `zwp_tablet_tool_v2` wire events (proximity_in/out, down/up, motion, pressure, frame) to the focused
    /// client. Held for the state's lifetime.
    tablet_tool: TabletToolHandle,
    /// Owns the `ext_session_lock_manager_v1` global (screen lock). A client (a lock screen / screensaver)
    /// binds it and calls `lock`; the compositor confirms the lock ([`SessionLockHandler::lock`]), HIDES every
    /// normal toplevel (sets it [`Visibility::Occluded`](crate::scene::model::Visibility) so its present is
    /// withheld), and the client presents its own lock surface (given a toplevel role in
    /// [`SessionLockHandler::new_surface`]). `unlock` restores every normal surface to visible. The lock state
    /// is mirrored into [`Observations`] so a test asserts the lock/unlock transition. Held for the state's
    /// lifetime so the global keeps advertising.
    _session_lock: SessionLockManagerState,
    /// Whether the session is currently locked (an `ext_session_lock_v1` is live and confirmed). While true,
    /// every normal toplevel is occluded (its present withheld) and only the lock surface(s) present. Mirrored
    /// into [`Observations::session_locked`](super::present::Observations) for the test.
    session_locked: bool,
    /// The scene surfaces that are ext-session-lock LOCK SURFACES (given a toplevel role in
    /// [`SessionLockHandler::new_surface`]). Tracked so [`Self::set_session_locked`] never occludes a lock
    /// surface (only the NORMAL toplevels are hidden), and pruned on teardown.
    lock_surfaces: Vec<SurfaceId>,
    /// Backs the `delegate_xdg_shell` `SeatHandler` bound (popup grabs reference a seat) AND owns the
    /// `wl_seat` capabilities the toolkits enumerate for input.
    pub seat_state: SeatState<HlState>,
    /// The `wl_seat` this compositor advertises. Created via [`SeatState::new_wl_seat`] and given pointer
    /// + keyboard capabilities (with a default xkb keymap) so a client that binds `wl_seat` and creates
    ///   `wl_pointer`/`wl_keyboard` succeeds. No live input source is wired headless — the capabilities and
    ///   objects exist, but no motion/key events are injected.
    pub seat: Seat<HlState>,
    /// Owns the `zxdg_output_manager_v1` global (xdg-output) so a client that enumerates it for logical
    /// output geometry gets an answer consistent with `wl_output`. Kept alive for the life of the state;
    /// the `wl_output` dispatch reads the [`WlOutputHandle`] out of its own per-global data, not this.
    _output_manager: OutputManagerState,
    /// Every smithay `wl_output` this compositor advertises, one per scene [`crate::scene::model::Output`]
    /// (mode size in px + refresh, integer scale, layout position), keyed by the neutral [`OutputId`] so
    /// surface→output membership can pick the right one. The default single-output layout holds exactly one;
    /// `$HL_OUTPUTS` stands up several. Kept alive so their globals keep advertising; a bind delivers
    /// geometry/mode/scale/name/done for each to the client.
    outputs: Vec<(OutputId, WlOutputHandle)>,
    /// The `wl_output` global ids (held so they stay advertised for the state's lifetime).
    _output_globals: Vec<GlobalId>,
    /// The neutral policy: scene graph + selected presenter + monotonic clock. All compositing/pacing
    /// decisions live here; `HlState` only translates the wire into calls on it.
    pub engine: Compositor<AdapterPresenter, MonotonicClock>,
    /// `wl_surface` protocol object → neutral scene surface id. The scene mints collision-free ids; this
    /// is the neutral analogue of `HlState::surface_ids`.
    surface_ids: HashMap<ObjectId, SurfaceId>,
    /// Neutral scene surface id → the live `wl_surface` protocol object. The inverse of `surface_ids`,
    /// needed so input injection can hand smithay's `PointerHandle`/`KeyboardHandle` the concrete
    /// `WlSurface` focus (its `PointerTarget`/`KeyboardTarget` impls serialize the wire events). Kept in
    /// lockstep with `surface_ids` (registered/torn down together).
    surfaces_by_id: HashMap<SurfaceId, WlSurface>,
    _surface_identity_manager: Option<GlobalId>,
    surface_tokens: HashMap<SurfaceId, u64>,
    token_surfaces: HashMap<u64, SurfaceId>,
    identity_surfaces: HashMap<ObjectId, SurfaceId>,
    pending_associations: HashMap<SurfaceId, u64>,
    last_associations: HashMap<SurfaceId, u64>,
    #[cfg(feature = "macos-surface")]
    native: Option<native::NativeState>,
    #[cfg(feature = "macos-surface")]
    native_buffers: HashMap<WeakDmabuf, u64>,
    /// `wl_surface.frame` callbacks the client is owed but that have NOT yet been fired, keyed by the
    /// neutral surface they were requested on. A callback is held (not fired at commit) until the frame it
    /// belongs to actually reaches the presenter — so a throttled frame does not prematurely tell the
    /// client "your content is on screen, draw the next one". Fired by [`Self::fire_tree_callbacks`] when
    /// the surface's window root presents (or is cleanly skipped).
    pending_callbacks: HashMap<SurfaceId, Vec<WlCallback>>,
    /// The active `xdg_popup.grab` chain, ordered outer → inner (a menu, then any submenu opened under
    /// it). A press outside this chain — or the grab otherwise breaking — dismisses it: each popup is sent
    /// `xdg_popup.popup_done` innermost-first (see [`Self::dismiss_popup_grabs`]). Tooltips take no grab, so
    /// they are absent here and are not dismissed on an outside click. Kept as the concrete Smithay
    /// [`PopupSurface`] (it owns `send_popup_done`); pruned when a popup is destroyed.
    popup_grabs: Vec<PopupSurface>,
    /// Window roots with a repaint owed at the recorded host-monotonic deadline (ns). Populated when a
    /// commit is throttled (or a present is retryable): the serve loop drains these in
    /// [`Self::drive_due_repaints`] to re-drive `present_root` at the next refresh boundary, so the
    /// retained frame ships and its callbacks release even if the client has since gone idle. A later real
    /// commit that presents the same root supersedes the entry (it is cleared on a completing present).
    pending_repaints: HashMap<SurfaceId, u64>,
    /// Toplevel roots that currently hold a `wl_surface.enter(wl_output)`, mapped to WHICH output they
    /// entered. A toplevel is "on" its selected output (see [`crate::scene::model::Scene::selected_output`])
    /// while it has a committed (mapped) buffer, off it once unmapped. Tracked so enter/leave are sent
    /// exactly once per transition — and, under a multi-output layout, so a routing change emits a `leave`
    /// for the old output and an `enter` for the new one (see [`Self::update_output_membership`]).
    entered_outputs: HashMap<SurfaceId, OutputId>,
    /// `wp_presentation_feedback` callbacks a client is owed but that have NOT yet been answered, keyed by
    /// the neutral surface the `feedback` request named. Drained from the surface's committed
    /// [`PresentationFeedbackCachedState`] at commit and held (like `pending_callbacks`) until the frame the
    /// feedback belongs to actually reaches the presenter — then answered `presented` (see
    /// [`Self::fire_tree_callbacks`]) or, if the frame is torn down unshown, `discarded`
    /// ([`Self::drop_tree_callbacks`] / [`Self::teardown_surface`]).
    pending_presentation: HashMap<SurfaceId, Vec<PresentationFeedbackCallback>>,
    /// Protocol objects captured with an asynchronous host-display submission.
    pending_host_frames: HashMap<SurfaceId, frame::HostFrame>,
    /// The last RAW injected pointer position in root space — the reference `inject_pointer_motion` computes
    /// the relative-motion delta against. Distinct from the neutral seat's `pointer_location` (the delivered
    /// ABSOLUTE position, which freezes under a `zwp_locked_pointer_v1` lock): the raw position keeps
    /// tracking every injected move so relative motion reports the real device delta even while locked.
    last_injected_pointer: (f64, f64),
    last_pointer_click_count: u8,
    /// Toplevels whose host presentation occupies a native full-screen Space while the guest may remain
    /// XDG maximized. GTK intentionally removes its client-side header in XDG fullscreen; keeping this
    /// separate lets the macOS full-screen control retain the application's header and controls.
    host_fullscreen: HashSet<SurfaceId>,
    /// Owns the `wp_cursor_shape_manager_v1` global (named cursor shapes). Chrome/Ozone and modern GTK/Qt
    /// set the pointer cursor by SHAPE NAME (`pointer`/`text`/`grab`/…) through this instead of attaching a
    /// pixel buffer. Smithay decodes `set_shape` and routes it through [`SeatHandler::cursor_image`] as
    /// `CursorImageStatus::Named`; the handler records the requested shape name into [`Observations`]. Held
    /// for the state's lifetime so the global keeps advertising.
    _cursor_shape: CursorShapeManagerState,
    /// Owns the `wp_single_pixel_buffer_manager_v1` global (a 1×1 solid-color `wl_buffer` with no shm pool).
    /// Chrome/Ozone and video players use it for solid-color quads (backgrounds / letterbox bars) without a
    /// shared-memory allocation. The commit read path ([`read_single_pixel_rgba`]) turns the buffer's RGBA
    /// color into a real 1×1 pixel the presenter composites. Held for the state's lifetime.
    _single_pixel_buffer: SinglePixelBufferState,
    /// Owns the `zwp_keyboard_shortcuts_inhibit_manager_v1` global (key-grab). Terminals, remote-desktop /
    /// VNC clients, and games ask the compositor to stop intercepting its own keyboard shortcuts for a
    /// surface so ALL keys reach the app. The handler activates each inhibitor (sending the client the
    /// `active` event) and records the inhibited surface into [`Observations`]; the neutral seat exposes
    /// `keyboard_shortcuts_inhibited()` so a shortcut handler could consult it. Held for the state's lifetime.
    keyboard_shortcuts_inhibit: KeyboardShortcutsInhibitState,
    /// The `wp_tearing_control_manager_v1` global id (staging, hand-dispatched — Smithay ships no handler).
    /// Held so the global keeps advertising for the state's lifetime.
    _tearing_manager: GlobalId,
    /// Shared handle onto the presenter's [`Observations`] — the non-pixel adapter state (idle-inhibit /
    /// content-type) a test reads back. Cloned from the presenter at construction (before it moves into the
    /// engine), so the protocol handlers here write exactly where the test reads. See [`Observations`].
    observations: Arc<Mutex<Observations>>,
    clipboard_tx: Sender<String>,
    clipboard_rx: Receiver<String>,
}

mod buffer;
mod command;
mod commit;
mod configuration;
mod extensions;
mod frame;
mod identity;
mod input;
mod interaction;
mod lifecycle;
#[cfg(feature = "macos-surface")]
mod native;
mod output;
mod protocol;
mod surface;
mod tearing;
mod xdg;

#[cfg(test)]
mod conformance;
#[cfg(test)]
mod tests;

pub use command::InputCommand;
