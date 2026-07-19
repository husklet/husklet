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
use smithay::utils::{Logical, Point, SERIAL_COUNTER};
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
    viewporter::{ViewportCachedState, ViewporterState},
    xdg_activation::{
        XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
    },
};

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
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State as XdgToplevelState;

use crate::scene::model::{
    Anchor, BufferState, BufferTransform, ConstraintAdjustment, Format, Gravity, Output, OutputId,
    PopupState, Positioner, Rect, SubsurfaceState, SurfaceId, SurfaceRole, Viewport, Visibility,
};
use crate::scene::port::{Clock, Presenter};
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
    /// The single advertised tablet TOOL (a pen with pressure + distance + tilt capabilities). The host
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
    /// `wl_pointer`/`wl_keyboard` succeeds. No live input source is wired headless — the capabilities and
    /// objects exist, but no motion/key events are injected.
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
    /// Monotonic presentation SEQUENCE counter — the `seq` a `wp_presentation_feedback.presented` carries
    /// (a per-output vblank / frame counter). Incremented once per PRESENT CYCLE that answers feedback (not
    /// once per feedback): every feedback released together by one present shares the frame's `seq` (and its
    /// `now` timestamp), so a client sees a strictly increasing, contiguous, gap-free sequence — one number
    /// per frame that actually reached the screen. See [`Self::answer_tree_feedback`].
    present_seq: u64,
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

impl HlState {
    /// Stand up the protocol globals and the neutral engine, seeded with one output.
    pub fn new(dh: &DisplayHandle, presenter: impl Into<AdapterPresenter>) -> HlState {
        let presenter = presenter.into();
        // Grab the presenter's shared observation handle BEFORE it moves into the engine, so the
        // idle-inhibit / content-type handlers below write exactly where a test reads (mirrors `captures`).
        let observations = presenter.observations();
        let (clipboard_tx, clipboard_rx) = mpsc::channel();
        let compositor = CompositorState::new::<HlState>(dh);
        // Smithay always advertises Argb8888 + Xrgb8888; additionally advertise the byte-swapped
        // Abgr8888 / Xbgr8888 (R and B channels swapped), which `read_shm_rgba` unpacks. Toolkits that
        // prefer BGR-order buffers (some GL/EGL paths) can then present without a client-side repack.
        let shm =
            ShmState::new::<HlState>(dh, vec![wl_shm::Format::Abgr8888, wl_shm::Format::Xbgr8888]);
        // Advertise `zwp_linux_dmabuf_v1` (accelerated present path). A v4/v5 feedback global carrying a
        // single LINEAR ARGB8888/XRGB8888 tranche, so GTK/Qt EGL + Chrome's ozone/GPU get a TRUTHFUL
        // format table to probe — and a client that hands us a LINEAR dmabuf has it CPU-imported by
        // `pread` (a real fd import, no GPU). See [`new_dmabuf_state`].
        let dmabuf = DmabufAdapter::new(dh).state();
        let xdg_shell = XdgShellState::new::<HlState>(dh);
        // Advertise `zxdg_decoration_manager_v1` so CSD-vs-SSD negotiation resolves instead of hanging.
        let xdg_decoration = XdgDecorationState::new::<HlState>(dh);
        // Advertise `wl_data_device_manager` (clipboard / drag-and-drop). GDK4 (and Chrome/Qt) require
        // this global at display-open; without it `gdk_display_open` aborts before any GL is created.
        let data_device = DataDeviceState::new::<HlState>(dh);
        // Advertise `zwp_primary_selection_device_manager_v1` (the middle-click PRIMARY selection) so a
        // GTK/Qt terminal or editor can set + read the select-to-copy clipboard, distinct from the
        // CTRL+C `wl_data_device` one. Primary-selection focus follows keyboard focus like the data device.
        let primary_selection = PrimarySelectionState::new::<HlState>(dh);
        // Advertise `xdg_activation_v1` (cross-client activation / focus request), `zwp_idle_inhibit_manager_v1`
        // (screensaver inhibition), and `wp_content_type_manager_v1` (per-surface content-type hint) — the
        // window-management/surface-semantics globals real toolkits (GTK/Qt/players) probe. Activation is
        // honoured as a keyboard-focus change; idle-inhibit + content-type are tracked into `Observations`.
        let xdg_activation = XdgActivationState::new::<HlState>(dh);
        let idle_inhibit = IdleInhibitManagerState::new::<HlState>(dh);
        let content_type = ContentTypeState::new::<HlState>(dh);
        // Advertise `wp_cursor_shape_manager_v1` (named cursors), `wp_single_pixel_buffer_manager_v1` (1×1
        // solid-color buffers), and `zwp_keyboard_shortcuts_inhibit_manager_v1` (key-grab) — the
        // surface/seat protocols Chrome/Ozone + modern GTK/Qt bind. Cursor shapes route through the seat's
        // `cursor_image`, single-pixel buffers composite via `read_single_pixel_rgba`, and each shortcut
        // inhibitor is activated + tracked in `Observations`.
        let cursor_shape = CursorShapeManagerState::new::<HlState>(dh);
        let single_pixel_buffer = SinglePixelBufferState::new::<HlState>(dh);
        let keyboard_shortcuts_inhibit = KeyboardShortcutsInhibitState::new::<HlState>(dh);
        // Advertise `wp_tearing_control_manager_v1` (staging) — Chrome/Ozone requests it to hint immediate
        // (`async`, tearing-allowed) vs `vsync` present. Smithay ships no handler, so its manager + per-
        // surface object are dispatched by the hand-written `GlobalDispatch`/`Dispatch` impls below; the
        // per-surface hint is double-buffered and read at commit into `Observations`.
        let tearing_manager = dh.create_global::<HlState, WpTearingControlManagerV1, ()>(1, ());
        // Advertise `zwp_relative_pointer_manager_v1` (unaccelerated relative motion deltas) and
        // `zwp_pointer_constraints_v1` (pointer lock / confinement) — the input protocols FPS games, 3D
        // viewports, and pointer-lock web content require. The seat's `wl_pointer` backs both.
        let relative_pointer = RelativePointerManagerState::new::<HlState>(dh);
        let pointer_constraints = PointerConstraintsState::new::<HlState>(dh);
        // Advertise `wp_presentation` (presentation-timing feedback). `CLOCK_MONOTONIC` (id 1) is the clock
        // the reported timestamps are in — the same monotonic timeline [`MonotonicClock`] paces on.
        const CLOCK_MONOTONIC: u32 = 1;
        let presentation = PresentationState::new::<HlState>(dh, CLOCK_MONOTONIC);
        // Advertise `wp_viewporter` (surface crop/scale) and `wp_fractional_scale_manager_v1` (HiDPI
        // preferred-scale hint) so media/browser clients can crop+scale a buffer and learn the fractional
        // render scale — the surface-semantics globals a modern toolkit probes at startup.
        let viewporter = ViewporterState::new::<HlState>(dh);
        let fractional_scale = FractionalScaleManagerState::new::<HlState>(dh);
        // Advertise `zwp_text_input_manager_v3` (client text-input / IME) + `zwp_input_method_manager_v2`
        // (the input-method backend side). Both are required for real text entry: Smithay only delivers
        // text-input events while an input method instance exists on the seat, so GTK/Chrome text-input +
        // an IME backend are both first-class. The `|_| true` filter lets any client bind the input-method
        // manager (headless there is no trust boundary to enforce).
        let text_input = TextInputManagerState::new::<HlState>(dh);
        let input_method = InputMethodManagerState::new::<HlState, _>(dh, |_client| true);
        // Advertise `zwp_pointer_gestures_v1` (trackpad pinch/swipe/hold), `zwp_tablet_manager_v2`
        // (graphics tablet / stylus), and `ext_session_lock_manager_v1` (screen lock) — the remaining input
        // + session protocols a modern desktop toolkit / lock screen probes. The `|_| true` filter lets any
        // client bind the session-lock manager (headless there is no trust boundary to enforce).
        let pointer_gestures = PointerGesturesState::new::<HlState>(dh);
        let tablet_manager = TabletManagerState::new::<HlState>(dh);
        let session_lock = SessionLockManagerState::new::<HlState, _>(dh, |_client| true);
        let mut seat_state = SeatState::new();

        let mut engine = Compositor::new(presenter, MonotonicClock::new());
        // The scene's output layout. Default: one 1920×1080\@60 output at `(0, 0)` (with the advertised
        // `wl_output.transform` from `$HL_OUTPUT_TRANSFORM` applied). `$HL_OUTPUTS` overrides it with a
        // multi-output layout (`WxH@X,Y[*scale]` specs separated by `;`) so a demo can stand two monitors up
        // side by side with distinct position / mode / scale; existing single-output demos leave it unset.
        for output in env_outputs() {
            engine.scene.add_output(output);
        }

        // Advertise a `wl_output` (+ xdg-output) for EACH scene output, so toolkits that enumerate outputs
        // for HiDPI geometry / mode / scale / window sizing get a consistent answer per monitor.
        let output_manager = OutputManagerState::new_with_xdg_output::<HlState>(dh);
        let mut outputs: Vec<(OutputId, WlOutputHandle)> = Vec::new();
        let mut output_globals: Vec<GlobalId> = Vec::new();
        for scene_output in engine.scene.outputs().to_vec() {
            let (wl_output, output_global) = WaylandOutput::new(dh, &scene_output).build();
            outputs.push((scene_output.id, wl_output));
            output_globals.push(output_global);
        }

        // Advertise a `wl_seat` with pointer + keyboard + TOUCH capabilities so toolkits that bind it for
        // input succeed in creating `wl_pointer`/`wl_keyboard`/`wl_touch`. No live input is injected headless.
        let mut seat = seat_state.new_wl_seat(dh, "seat-0");
        seat.add_pointer();
        // `wl_touch` — a multi-touch capability so a touchscreen client (Chrome/GTK on a tablet) receives
        // down/motion/up/frame/cancel. Driven by the host touch seam ([`InputCommand::TouchDown`] etc.).
        seat.add_touch();
        // A default xkb keymap (evdev rules) — enough for the keyboard object + keymap fd to be handed to
        // the client. If libxkbcommon cannot build even the default keymap the seat still advertises the
        // pointer; the keyboard capability is simply omitted rather than panicking the whole compositor.
        if let Err(e) = seat.add_keyboard(XkbConfig::default(), 200, 25) {
            eprintln!("hl-compositor: wl_seat keyboard keymap unavailable, pointer only: {e}");
        }

        // Advertise a single graphics tablet + pen tool on the seat's tablet-seat, so a client that binds
        // `zwp_tablet_manager_v2.get_tablet_seat` receives `tablet_added` + `tool_added` and can then be
        // driven with real stylus proximity/tip/motion/pressure. The tool declares PRESSURE + DISTANCE +
        // TILT capabilities (what the host stylus seam feeds). `add_tablet` needs no `HlState` value; the
        // `add_tool` below (which would notify already-bound clients) is deferred until after the struct is
        // built, since it takes `&mut HlState`.
        let tablet_seat = seat.tablet_seat();
        let tablet_desc = TabletDescriptor {
            name: "hl-virtual-tablet".to_string(),
            usb_id: None,
            syspath: None,
        };
        let tablet = tablet_seat.add_tablet::<HlState>(dh, &tablet_desc);

        hl_info!(
            tag::WAYLAND,
            "globals bound: compositor shm dmabuf xdg seat output data_device primary_selection relative_pointer pointer_constraints pointer_gestures tablet session_lock presentation xdg_activation idle_inhibit content_type cursor_shape single_pixel_buffer keyboard_shortcuts_inhibit tearing_control text_input input_method"
        );
        // The pen tool declares pressure + distance + tilt axes on top of the mandatory x/y + tip.
        let tool_desc = TabletToolDescriptor {
            tool_type: TabletToolType::Pen,
            hardware_serial: 0xB007_0001,
            hardware_id_wacom: 0,
            capabilities: TabletToolCapabilities::PRESSURE
                | TabletToolCapabilities::DISTANCE
                | TabletToolCapabilities::TILT,
        };
        let mut state = HlState {
            display: dh.clone(),
            compositor,
            shm,
            dmabuf,
            xdg_shell,
            xdg_decoration,
            data_device,
            primary_selection,
            _text_input: text_input,
            _input_method: input_method,
            _pointer_gestures: pointer_gestures,
            _tablet_manager: tablet_manager,
            tablet,
            tablet_tool: TabletToolHandle::default(),
            _session_lock: session_lock,
            session_locked: false,
            lock_surfaces: Vec::new(),
            _xdg_activation: xdg_activation,
            _idle_inhibit: idle_inhibit,
            _content_type: content_type,
            _cursor_shape: cursor_shape,
            _single_pixel_buffer: single_pixel_buffer,
            keyboard_shortcuts_inhibit,
            _tearing_manager: tearing_manager,
            _relative_pointer: relative_pointer,
            _pointer_constraints: pointer_constraints,
            _presentation: presentation,
            _viewporter: viewporter,
            _fractional_scale: fractional_scale,
            seat_state,
            seat,
            _output_manager: output_manager,
            outputs,
            _output_globals: output_globals,
            engine,
            surface_ids: HashMap::new(),
            surfaces_by_id: HashMap::new(),
            popup_grabs: Vec::new(),
            pending_callbacks: HashMap::new(),
            pending_repaints: HashMap::new(),
            entered_outputs: HashMap::new(),
            pending_presentation: HashMap::new(),
            last_injected_pointer: (0.0, 0.0),
            last_pointer_click_count: 1,
            host_fullscreen: HashSet::new(),
            present_seq: 0,
            observations,
            clipboard_tx,
            clipboard_rx,
        };
        // Register the pen tool on the tablet-seat now that `state` exists (`add_tool` takes `&mut HlState`
        // to notify already-bound clients; there are none yet at construction, but the signature requires
        // it). The returned handle is what the host stylus seam drives.
        let tablet_seat = state.seat.tablet_seat();
        state.tablet_tool = tablet_seat.add_tool::<HlState>(&mut state, dh, &tool_desc);
        state
    }

    /// Fresh per-client protocol state for `insert_client`.
    pub fn new_client_state(&self) -> ClientState {
        ClientState::default()
    }

    /// Synchronize UTF-8 clipboard text across the Wayland selection and the active host presenter.
    /// Client sources write on a pipe asynchronously, so neither the Wayland dispatch nor AppKit loop
    /// blocks waiting for the guest to answer a selection request.
    pub fn sync_clipboard(&mut self) {
        while let Ok(text) = self.clipboard_rx.try_recv() {
            self.engine.presenter_mut().set_clipboard_text(&text);
        }
        let Some(text) = self.engine.presenter_mut().take_clipboard_text() else {
            return;
        };
        let display = self.display.clone();
        let seat = self.seat.clone();
        set_data_device_selection::<HlState>(
            &display,
            &seat,
            vec![
                "text/plain;charset=utf-8".into(),
                "text/plain".into(),
                "UTF8_STRING".into(),
            ],
            text,
        );
    }

    /// The neutral scene id for a `wl_surface`, if registered.
    fn sid(&self, surface: &WlSurface) -> Option<SurfaceId> {
        self.surface_ids.get(&surface.id()).copied()
    }

    fn reconcile_window(&mut self, surface: SurfaceId) {
        if let Some(toplevel) = self
            .xdg_shell
            .toplevel_surfaces()
            .iter()
            .find(|toplevel| self.sid(toplevel.wl_surface()) == Some(surface))
        {
            let state = toplevel.current_state();
            if let Some(scene_surface) = self.engine.scene.get_mut(surface) {
                scene_surface.maximized = state.states.contains(XdgToplevelState::Maximized);
                scene_surface.fullscreen = state.states.contains(XdgToplevelState::Fullscreen)
                    || self.host_fullscreen.contains(&surface);
            }
        }
        if let Some(window) = self.engine.scene.window_state(surface) {
            self.engine.presenter_mut().reconcile_window(&window);
        }
    }

    /// Register a fresh `wl_surface`, minting a neutral scene surface for it.
    fn register_surface(&mut self, surface: &WlSurface) {
        let sid = self.engine.scene.create_surface();
        self.surface_ids.insert(surface.id(), sid);
        self.surfaces_by_id.insert(sid, surface.clone());
    }

    /// Drop a `wl_surface` and its scene surface.
    fn teardown_surface(&mut self, surface: &WlSurface) {
        // A destroyed surface can never anchor an active grab (a menu's `wl_surface` going away breaks its
        // grab), so drop it from the chain before the scene reference is gone.
        self.popup_grabs.retain(|p| p.wl_surface() != surface);
        if let Some(sid) = self.surface_ids.remove(&surface.id()) {
            self.lock_surfaces.retain(|&s| s != sid);
            self.host_fullscreen.remove(&sid);
            // The window root this surface belonged to, resolved WHILE its tree links still exist. If the
            // surface was a child (popup/subsurface), its removal changes what the root composites — a
            // dismissed popup or a torn-down subsurface must visibly LEAVE the screen. Nothing else marks
            // the root dirty (the client owning the toplevel may never commit again after closing its own
            // popup), so a removed child would otherwise linger on the last presented frame forever.
            let owning_root = self.engine.scene.window_root(sid).filter(|&r| r != sid);
            self.surfaces_by_id.remove(&sid);
            self.engine.presenter_mut().destroy_window(sid);
            self.engine.scene.remove_surface(sid);
            // Reclaim any frame callbacks/repaint owed to a surface that just went away: a client that
            // destroys a surface mid-frame (or disconnects) must not leave a dangling repaint that keeps
            // re-driving a removed root, nor stale callback objects for a dead protocol resource.
            self.pending_callbacks.remove(&sid);
            self.pending_repaints.remove(&sid);
            self.entered_outputs.remove(&sid);
            // A destroyed surface's owed presentation feedback can never be answered `presented`; discard it
            // (per spec: content the client did not see) so the client's `wp_presentation_feedback` resolves.
            if let Some(feedbacks) = self.pending_presentation.remove(&sid) {
                for feedback in feedbacks {
                    feedback.discarded();
                }
            }
            // Re-present the owning root without the removed child: mark it dirty (so the compose is not
            // skipped as clean) and arm a repaint at the next refresh boundary. The serve loop's
            // `drive_due_repaints` ships it even if the client is now idle, so the child disappears.
            if let Some(root) = owning_root {
                if self.engine.scene.contains(root) {
                    self.engine.scene.mark_dirty(root);
                    self.arm_repaint(root);
                }
            }
        }
    }

    /// Refresh `surface`'s own subsurface role (if it is one) from Smithay's applied state, then recurse
    /// into its children — a parent commit applies its synchronized children's buffered `set_position` /
    /// sync state, so the whole subtree may have moved.
    fn sync_subsurface_tree(&mut self, surface: &WlSurface) {
        self.refresh_subsurface_role(surface);
        for child in get_children(surface) {
            if &child == surface {
                continue;
            }
            self.sync_subsurface_tree(&child);
        }
    }

    /// If `surface` is a `wl_subsurface`, mirror its Smithay-applied state into the scene: link it to its
    /// parent at the committed `set_position` offset with its current sync/desync mode, and push its
    /// parent's `get_children` z-order (which reflects `place_above`/`place_below`) into the scene. A
    /// no-op for a surface with no subsurface parent (toplevels, popups, roleless).
    fn refresh_subsurface_role(&mut self, surface: &WlSurface) {
        let Some(parent_wl) = get_parent(surface) else {
            return; // not a subsurface
        };
        let (Some(sid), Some(parent)) = (self.sid(surface), self.sid(&parent_wl)) else {
            return;
        };
        let (x, y) = with_states(surface, |states| {
            let loc = states
                .cached_state
                .get::<SubsurfaceCachedState>()
                .current()
                .location;
            (loc.x, loc.y)
        });
        let sync = is_sync_subsurface(surface);
        self.engine.scene.set_role(
            sid,
            SurfaceRole::Subsurface(SubsurfaceState { parent, x, y, sync }),
        );
        // Mirror the wire z-order: Smithay keeps `place_above`/`place_below` in `get_children` (bottom →
        // top, excluding the parent's self-entry). Map those to scene ids and reorder the scene's children.
        let order: Vec<SurfaceId> = get_children(&parent_wl)
            .iter()
            .filter(|c| *c != &parent_wl)
            .filter_map(|c| self.sid(c))
            .collect();
        self.engine.scene.set_subsurface_order(parent, &order);
    }

    /// Dismiss the whole active popup-grab chain: send `xdg_popup.popup_done` innermost-first (a submenu
    /// closes before the menu that spawned it), then clear the grab stack. Returns how many popups were
    /// dismissed. The client tears the popups down in response (its `popup_destroyed` / `wl_surface`
    /// destroy then reclaims scene state). Driven by a press outside the chain (see
    /// [`Self::inject_pointer_button`]) or callable directly by a host.
    pub fn dismiss_popup_grabs(&mut self) -> usize {
        let chain = std::mem::take(&mut self.popup_grabs);
        let n = chain.len();
        for popup in chain.into_iter().rev() {
            popup.send_popup_done();
        }
        n
    }

    /// Whether root-local logical point `(x, y)` falls OUTSIDE every popup in the active grab chain — the
    /// press-dismisses-the-menu test. A point inside any grabbing popup's on-screen rectangle (its
    /// resolved offset within the toplevel + its logical size) keeps the chain; a point outside all of them
    /// dismisses it. With no grab active this is vacuously `false` (nothing to dismiss).
    fn press_outside_grab_chain(&self, x: f64, y: f64) -> bool {
        if self.popup_grabs.is_empty() {
            return false;
        }
        let (px, py) = (x.floor() as i32, y.floor() as i32);
        for popup in &self.popup_grabs {
            let Some(sid) = self.sid(popup.wl_surface()) else {
                continue;
            };
            let Some((_, ox, oy, _)) = self.engine.scene.popup_offset_to_toplevel(sid) else {
                continue;
            };
            if let Some((w, h)) = self.engine.scene.get(sid).and_then(|s| s.logical_size()) {
                hl_debug!(
                    tag::WAYLAND,
                    "popup grab hit surface={} point={},{} rect={},{} {}x{}",
                    sid.0,
                    px,
                    py,
                    ox,
                    oy,
                    w,
                    h
                );
                if Rect::new(ox, oy, w, h).contains_point(px, py) {
                    return false; // inside a grabbing popup — do not dismiss
                }
            }
        }
        true
    }

    /// Record `surface`'s committed `wp_content_type_v1` hint into the shared [`Observations`], keyed by the
    /// `wl_surface` protocol id. Read from the committed [`ContentTypeSurfaceCachedState`] (default `none`
    /// when the client attached no content-type object), stored as the wire value so a test can assert the
    /// exact hint. A no-op beyond the write; the headless compositor applies no content-type-driven policy.
    fn record_content_type(&mut self, surface: &WlSurface) {
        let ct = with_states(surface, |states| {
            *states
                .cached_state
                .get::<ContentTypeSurfaceCachedState>()
                .current()
                .content_type()
        });
        let wire = match ct {
            ContentType::None => 0,
            ContentType::Photo => 1,
            ContentType::Video => 2,
            ContentType::Game => 3,
            _ => 0,
        };
        self.observations
            .lock()
            .unwrap()
            .content_type
            .insert(surface.id().protocol_id(), wire);
    }

    /// Record `surface`'s committed `wp_tearing_control_v1` presentation hint into the shared
    /// [`Observations`], keyed by the `wl_surface` protocol id. Read from the committed
    /// [`TearingControlCachedState`] (default `vsync` = wire 0 when no `wp_tearing_control_v1` is attached),
    /// stored as the wire value (`0` vsync / `1` async) so a test can assert the exact hint. Like
    /// `wp_content_type`, there is no reply event and the headless presenter applies no tearing policy — this
    /// write is the observable proof the present path read the hint the client committed.
    fn record_tearing_hint(&mut self, surface: &WlSurface) {
        let hint = with_states(surface, |states| {
            states
                .cached_state
                .get::<TearingControlCachedState>()
                .current()
                .hint
        });
        self.observations
            .lock()
            .unwrap()
            .tearing_hint
            .insert(surface.id().protocol_id(), hint);
    }

    /// The commit → present path (the neutral analogue of `on_commit`): read the committed double-buffered
    /// state Smithay has already applied, deposit the surface's pixels for the presenter, translate the
    /// commit into a [`Commit`], drive the neutral engine (which composes + presents + paces), then fire
    /// the client's `wl_surface.frame` callbacks so it keeps drawing.
    fn on_commit(&mut self, surface: &WlSurface) {
        let Some(sid) = self.sid(surface) else {
            return;
        };

        // Mirror Smithay's just-applied subsurface state (set_position offset, sync/desync, and the
        // place_above/place_below z-order) into the scene BEFORE the engine composes/paces this commit: a
        // parent commit atomically applies its synchronized children's buffered state, so refresh the whole
        // committed subtree, not just this surface. Without this the scene would composite a subsurface at a
        // stale offset (or present a sync child that should ship with its parent).
        self.sync_subsurface_tree(surface);

        // Record the surface's just-committed `wp_content_type_v1` hint (double-buffered like the buffer /
        // damage, applied at commit) into the shared observations. There is no reply event, so this is the
        // only way a test can assert the compositor read the exact hint the client set.
        self.record_content_type(surface);

        // Record the surface's just-committed `wp_tearing_control_v1` presentation hint (also double-buffered
        // and applied at commit) into the shared observations — the present path's honest read of whether the
        // client asked for `async` (tearing-allowed) vs `vsync` present.
        self.record_tearing_hint(surface);

        // Snapshot the committed state Smithay applied, taking ownership of the buffer assignment and
        // draining this commit's damage + frame callbacks (the compositor is expected to consume both).
        let (
            assignment,
            damage,
            scale,
            transform,
            frame_callbacks,
            viewport,
            feedbacks,
            input_region,
            opaque_region,
            buffer_damage,
            window_geometry,
            min_size,
            max_size,
        ) = with_states(surface, |states| {
            // Drain this commit's `wp_presentation_feedback` callbacks (double-buffered like the frame
            // callbacks): held until the frame they belong to actually presents, then answered
            // `presented`/`discarded` per the pacing outcome below.
            let feedbacks = std::mem::take(
                &mut states
                    .cached_state
                    .get::<PresentationFeedbackCachedState>()
                    .current()
                    .callbacks,
            );
            let mut attrs = states.cached_state.get::<SurfaceAttributes>();
            let cur = attrs.current();
            let assignment = cur.buffer.take();
            let committed_damage = std::mem::take(&mut cur.damage);
            let buffer_damage = committed_damage
                .iter()
                .map(|damage| match damage {
                    Damage::Buffer(r) => Some(Rect::new(r.loc.x, r.loc.y, r.size.w, r.size.h)),
                    Damage::Surface(_) => None,
                })
                .collect::<Option<Vec<_>>>();
            let damage: Vec<Rect> = committed_damage
                .iter()
                .map(|d| match d {
                    Damage::Surface(r) => Rect::new(r.loc.x, r.loc.y, r.size.w, r.size.h),
                    Damage::Buffer(r) => Rect::new(r.loc.x, r.loc.y, r.size.w, r.size.h),
                })
                .collect();
            let scale = cur.buffer_scale.max(1);
            // `wl_surface.set_buffer_transform` (double-buffered) — the rotation/flip the presenter applies
            // to the buffer so it displays upright. Always re-read so a reverted transform reverts too.
            let transform = BufferTransform::from(cur.buffer_transform);
            let callbacks = std::mem::take(&mut cur.frame_callbacks);
            // `wl_surface.set_input_region` / `set_opaque_region` (both double-buffered, applied at commit).
            // The neutral scene models each as a single logical `Rect` and USES them: the input region gates
            // pointer hit-testing (`surface_at` → `accepts_input_at`), and the opaque region drives the
            // occlusion present-skip (`is_tree_dirty` → `opaque_covers`). Re-read every commit (like the
            // buffer transform / viewport) so a client that CLEARS its region reverts to the default.
            let input_region = Region::new(&cur.input_region).input();
            let opaque_region = Region::new(&cur.opaque_region).opaque();
            drop(attrs);
            // The just-applied `wp_viewport` state (src crop in logical coords, dst logical size), mirrored
            // into the neutral scene so it resolves the on-screen logical size and the presenter samples the
            // cropped+scaled region. Always re-read (double-buffered) so a cleared viewport reverts too.
            let mut vp = states.cached_state.get::<ViewportCachedState>();
            let cur_vp = vp.current();
            let viewport = Viewport {
                src: cur_vp.src.map(|r| (r.loc.x, r.loc.y, r.size.w, r.size.h)),
                dst: cur_vp.dst.map(|s| (s.w, s.h)),
            };
            let mut xdg = states.cached_state.get::<XdgSurfaceCachedState>();
            let current_xdg = xdg.current();
            let window_geometry = current_xdg.geometry.map(|geometry| {
                Rect::new(
                    geometry.loc.x,
                    geometry.loc.y,
                    geometry.size.w,
                    geometry.size.h,
                )
            });
            let min_size = (
                (current_xdg.min_size.w > 0).then_some(current_xdg.min_size.w),
                (current_xdg.min_size.h > 0).then_some(current_xdg.min_size.h),
            );
            let max_size = (
                (current_xdg.max_size.w > 0).then_some(current_xdg.max_size.w),
                (current_xdg.max_size.h > 0).then_some(current_xdg.max_size.h),
            );
            (
                assignment,
                damage,
                scale,
                transform,
                callbacks,
                viewport,
                feedbacks,
                input_region,
                opaque_region,
                buffer_damage,
                window_geometry,
                min_size,
                max_size,
            )
        });

        // Build the neutral commit from the buffer assignment, depositing pixels for the presenter.
        let commit = match assignment {
            Some(BufferAssignment::NewBuffer(buffer)) => {
                // Try the shm read first (the common path); if the buffer is a `zwp_linux_dmabuf_v1`
                // buffer instead, CPU-import its LINEAR pixels by `pread`ing the plane fd. Either yields
                // tight top-left RGBA the presenter composites identically — the dmabuf pixels are
                // GENUINELY read from the client's fd (there is no GPU here), so the composited frame
                // matches the buffer EXACTLY, just like shm.
                let reader = BufferReader::new(&buffer);
                match reader
                    .shm_rgba()
                    .or_else(|| reader.dmabuf_rgba())
                    .or_else(|| reader.single_pixel_rgba())
                {
                    Some((mut stored, format)) => {
                        stored.damage = buffer_damage.filter(|damage| !damage.is_empty());
                        let state = BufferState {
                            tex_w: stored.width,
                            tex_h: stored.height,
                            format,
                            buffer_scale: scale,
                            gpu: false,
                        };
                        self.engine.presenter_mut().deposit(sid, stored);
                        // Synchronous CPU copy is done — release the buffer so the client may reuse it.
                        buffer.release();
                        let mut c = Commit::attach(state);
                        c.damage = damage;
                        c
                    }
                    // Neither an shm nor an importable dmabuf buffer (or malformed) — no-content commit.
                    None => Commit::default(),
                }
            }
            Some(BufferAssignment::Removed) => {
                self.engine.presenter_mut().forget(sid);
                Commit {
                    buffer: BufferChange::Removed,
                    ..Commit::default()
                }
            }
            None => Commit {
                buffer: BufferChange::Keep,
                damage,
                ..Commit::default()
            },
        };
        // Apply the just-read `wp_viewport` state and `wl_surface.set_buffer_transform` on every commit
        // (both double-buffered): the scene resolves the logical size from them and the presenter samples
        // the cropped+scaled or rotated/flipped region.
        let commit = Commit {
            viewport: Some(viewport),
            buffer_transform: Some(transform),
            // Apply the just-read regions on every commit (`Some(value)` = "this commit sets it"); smithay
            // reports the current applied state, so a cleared region reverts to the whole-surface default.
            input_region: Some(input_region),
            opaque_region: Some(opaque_region),
            window_geometry: Some(window_geometry),
            ..commit
        };

        // Hold this commit's `wl_surface.frame` callbacks until the frame they belong to actually reaches
        // the presenter. Firing them here — before the present decision — would tell the client "your
        // content is on screen, draw the next frame" even when the frame was throttled and NEVER shown,
        // which drops the just-committed content (the client overwrites it) or, if the client then idles,
        // strands stale content on screen forever. The neutral engine models callbacks as a per-surface
        // count; the adapter owns the concrete `wl_callback` objects and releases them per the pacing
        // outcome below.
        self.pending_callbacks
            .entry(sid)
            .or_default()
            .extend(frame_callbacks);
        // Hold this commit's presentation-feedback callbacks on the same terms: answered `presented` when
        // the frame reaches the screen, `discarded` if it is torn down unshown.
        if !feedbacks.is_empty() {
            self.pending_presentation
                .entry(sid)
                .or_default()
                .extend(feedbacks);
        }

        // Drive the neutral policy: apply + (unless cursor / sync-subsurface) compose, present, pace.
        hl_count!(tag::WAYLAND, "commits");
        let changed = self.engine.apply_commit(sid, commit);
        if let Some(surface) = self.engine.scene.get_mut(sid) {
            surface.min_size = min_size;
            surface.max_size = max_size;
        }
        self.reconcile_window(sid);
        let outcome = self.engine.complete_commit(sid, changed);
        let (cw, ch) = self
            .engine
            .scene
            .get(sid)
            .and_then(|s| s.logical_size())
            .unwrap_or((0, 0));
        hl_debug!(
            tag::WAYLAND,
            "commit surf={} {}x{} changed={}",
            sid.0,
            cw,
            ch,
            outcome.changed
        );

        // Release or retain the held callbacks — and schedule a repaint if the frame was withheld.
        match outcome.frame {
            Some(frame) => {
                let root = self.engine.scene.window_root(sid).unwrap_or(sid);
                self.settle_frame(root, &frame);
            }
            // No window present was driven this commit (a cursor image or a synchronized subsurface, which
            // ships atomically with its parent's next present). There is no per-frame boundary to gate on
            // here, so release immediately — matching the pre-existing behavior for these roles and
            // avoiding a stall if the parent never commits again.
            None => self.fire_callbacks_for(sid),
        }

        // Reflect this surface's tree onto the advertised `wl_output`: a toplevel that just mapped enters
        // the output (so a client learns which output — and thus scale — it is displayed on); one that
        // unmapped leaves it. Sent exactly once per map/unmap transition.
        self.update_output_membership(sid);
    }

    /// Emit `wl_surface.enter` / `wl_surface.leave` as the toplevel root owning `sid` maps (gains a
    /// committed buffer), unmaps (loses it), or is routed to a different output. The target output is the
    /// root's SELECTED output (its position-based route, else the primary — see
    /// [`crate::scene::model::Scene::selected_output`]). Subsurfaces/popups follow their root, so only the
    /// toplevel root is tracked. Sent exactly once per transition: a mapped surface whose selected output
    /// changed gets a `leave` for the old `wl_output` and an `enter` for the new one; an unmapped surface
    /// gets a `leave` for whichever it was on. A no-op when the client has not (yet) bound the target
    /// `wl_output` beyond the bookkeeping — smithay re-sends `enter` for tracked surfaces on a later bind.
    fn update_output_membership(&mut self, sid: SurfaceId) {
        let Some(root) = self.engine.scene.window_root(sid) else {
            return;
        };
        if !matches!(
            self.engine.scene.get(root).map(|s| &s.role),
            Some(SurfaceRole::Toplevel)
        ) {
            return;
        }
        let Some(wl_surface) = self.surfaces_by_id.get(&root).cloned() else {
            return;
        };
        let mapped = self.engine.scene.get(root).and_then(|s| s.buffer).is_some();
        let current = self.entered_outputs.get(&root).copied();
        let target = self.engine.scene.selected_output(root).map(|o| o.id);
        if mapped {
            let Some(target) = target else { return };
            if current != Some(target) {
                // Leave the output we were on (if any) before entering the new one, so a client observes a
                // clean handoff (leave A, then enter B) rather than being on two outputs at once.
                if let Some(cur) = current {
                    if let Some(handle) = self.wl_output_handle(cur) {
                        handle.leave(&wl_surface);
                    }
                }
                if let Some(handle) = self.wl_output_handle(target) {
                    handle.enter(&wl_surface);
                }
                self.entered_outputs.insert(root, target);
            }
        } else if let Some(cur) = current {
            if let Some(handle) = self.wl_output_handle(cur) {
                handle.leave(&wl_surface);
            }
            self.entered_outputs.remove(&root);
        }
    }

    /// The smithay `wl_output` handle for a neutral [`OutputId`], if advertised.
    fn wl_output_handle(&self, id: OutputId) -> Option<&WlOutputHandle> {
        self.outputs
            .iter()
            .find(|(oid, _)| *oid == id)
            .map(|(_, h)| h)
    }

    /// The primary output's `wl_output` handle (the first advertised) — the fallback the presentation
    /// feedback names when a surface has no resolvable selected output.
    fn primary_wl_output(&self) -> Option<&WlOutputHandle> {
        self.outputs.first().map(|(_, h)| h)
    }

    /// The integer scale of the output surface `sid`'s window root is displayed on (its selected output,
    /// else the primary). Sources the fractional-scale hint so a surface on a HiDPI output learns a larger
    /// preferred scale than one on a scale-1 output.
    fn output_scale_for(&self, sid: SurfaceId) -> i32 {
        let root = self.engine.scene.window_root(sid).unwrap_or(sid);
        self.engine
            .scene
            .selected_output(root)
            .map(|o| o.scale.max(1))
            .unwrap_or(1)
    }

    /// (Re)send `wp_fractional_scale_v1.preferred_scale` for `sid` from its current output's scale. A no-op
    /// if the client created no `wp_fractional_scale_v1` on the surface, or if the value is unchanged
    /// (smithay's `set_preferred_scale` dedups) — so it is safe to call on every route change.
    fn send_preferred_fractional_scale(&self, sid: SurfaceId) {
        let scale = self.output_scale_for(sid) as f64;
        if let Some(surface) = self.surfaces_by_id.get(&sid) {
            with_states(surface, |states| {
                with_fractional_scale(states, |fractional| {
                    fractional.set_preferred_scale(scale);
                });
            });
        }
    }

    /// Route the toplevel at index `n` (ascending surface-id order) to the output whose logical rectangle
    /// contains global logical point `(x, y)`, then emit the resulting `wl_surface.leave`/`enter` and
    /// refresh its preferred fractional scale. The host/window-manager seam a multi-output demo drives to
    /// "place" a window on a monitor: real position-based routing (the compositor decides which output a
    /// window is on from where it sits), reduced to the smallest correct form — a point tested against each
    /// output's `logical_rect`. A point outside every output, or an out-of-range index, is ignored.
    fn move_toplevel_to_point(&mut self, n: usize, x: i32, y: i32) {
        let Some(root) = self.toplevel_at(n) else {
            return;
        };
        let Some(output_id) = self.output_at_point(x, y) else {
            return;
        };
        self.engine.scene.route_surface_to_output(root, output_id);
        self.update_output_membership(root);
        self.send_preferred_fractional_scale(root);
    }

    /// The neutral [`OutputId`] whose logical rectangle contains global logical point `(x, y)`, if any.
    fn output_at_point(&self, x: i32, y: i32) -> Option<OutputId> {
        self.engine
            .scene
            .outputs()
            .iter()
            .find(|o| o.contains_point(x, y))
            .map(|o| o.id)
    }

    /// Act on the pacing outcome of a just-driven present for window root `root`: fire, retain, or drop
    /// the frame callbacks held for its tree, and arm/clear a repaint so a withheld frame still ships.
    ///
    ///  - `throttled` — the frame was coalesced by the vsync throttle (a commit landed within one refresh
    ///    interval of the last present). Retain the callbacks and arm a repaint at the next refresh
    ///    boundary; nothing else re-drives `present_root`, so without this the retained frame would never
    ///    present if the client goes idle.
    ///  - a completing present (`Presented` / `Skipped`, `complete_callbacks`) — the frame reached the
    ///    screen (or the tree was already clean): fire the held callbacks and clear any pending repaint.
    ///  - a retryable failure — keep the callbacks and retry at the next boundary (the engine keeps the
    ///    tree dirty).
    ///  - a terminal failure — the frame can never ship: drop the held callbacks and the repaint.
    ///
    /// `wp_presentation` feedback is answered on the SAME outcome, but keyed on the pacing's
    /// `present_feedback` / `terminal_cleanup` (NOT `complete_callbacks`): only a `Presented` frame — real
    /// pixels reaching the screen — answers `presented`; a `Skipped` (clean-tree) frame completes callbacks
    /// but leaves the feedback held (its content was not newly shown), so a surface that never presents real
    /// content is answered `discarded` when it is torn down instead of falsely `presented`.
    fn settle_frame(&mut self, root: SurfaceId, frame: &FrameOutcome) {
        if frame.throttled {
            hl_debug!(
                tag::PRESENT,
                "settle root={} throttled=1 (repaint armed)",
                root.0
            );
            self.arm_repaint(root);
            return;
        }
        let policy = frame.pacing.policy();
        hl_debug!(
            tag::PRESENT,
            "settle root={} present_feedback={} complete_cb={} terminal={}",
            root.0,
            policy.present_feedback,
            policy.complete_callbacks,
            policy.terminal_cleanup
        );
        if policy.complete_callbacks {
            self.pending_repaints.remove(&root);
            self.fire_tree_callbacks(root);
        } else if policy.terminal_cleanup {
            self.pending_repaints.remove(&root);
            self.drop_tree_callbacks(root);
        } else {
            // Retryable: retain the callbacks and try again on a later tick.
            self.arm_repaint(root);
        }
        // Presentation feedback: `presented` only for a real pixel present; `discarded` on terminal cleanup.
        // A `Skipped` frame leaves it held (answered when the tree next actually presents, or discarded at
        // teardown).
        if policy.present_feedback {
            self.answer_tree_feedback(root, true);
        } else if policy.terminal_cleanup {
            self.answer_tree_feedback(root, false);
        }
    }

    /// Record that `root` owes a repaint at its next refresh boundary (or immediately, if it has never
    /// presented yet — a first present that failed retryably). Earlier deadlines win.
    fn arm_repaint(&mut self, root: SurfaceId) {
        if self.engine.scene.window_state(root).is_none() {
            self.pending_repaints.remove(&root);
            return;
        }
        let due = self
            .engine
            .next_present_due_ns(root)
            .unwrap_or_else(|| self.engine.clock().now_nanos());
        self.pending_repaints
            .entry(root)
            .and_modify(|d| *d = (*d).min(due))
            .or_insert(due);
    }

    /// The earliest host-monotonic deadline (ns) at which a repaint is owed, if any — the serve loop
    /// clamps its next wait to this so a throttled frame ships promptly rather than a fixed tick late.
    pub fn next_repaint_deadline(&self) -> Option<u64> {
        self.pending_repaints.values().copied().min()
    }

    /// Re-drive `present_root` for every window root whose repaint deadline has arrived, releasing the
    /// callbacks of any frame that now ships. Called by the serve loop each iteration. A root whose
    /// present is STILL not due (a clock that has not advanced a full interval) is left armed for a later
    /// tick rather than busy-looped.
    pub fn drive_due_repaints(&mut self) {
        let now = self.engine.clock().now_nanos();
        let due: Vec<SurfaceId> = self
            .pending_repaints
            .iter()
            .filter(|(_, &deadline)| now >= deadline)
            .map(|(&root, _)| root)
            .collect();
        for root in due {
            // Only surfaces that still exist and still root a window can present.
            if self.engine.scene.window_state(root).is_none() {
                self.pending_repaints.remove(&root);
                self.pending_callbacks.remove(&root);
                continue;
            }
            let frame = self.engine.present_root(root);
            if frame.throttled {
                // Not actually due yet (deadline race with a non-monotonic clock read): leave armed.
                self.arm_repaint(root);
            } else {
                self.settle_frame(root, &frame);
            }
        }
    }

    /// Fire (and remove) the frame callbacks held for every surface whose window root is `root` — the
    /// whole presented tree (root + subsurfaces + popups all resolve to it via `window_root`).
    fn fire_tree_callbacks(&mut self, root: SurfaceId) {
        let time_ms = (self.engine.clock().now_nanos() / 1_000_000) as u32;
        let targets: Vec<SurfaceId> = self
            .pending_callbacks
            .keys()
            .copied()
            .filter(|&sid| self.engine.scene.window_root(sid) == Some(root) || sid == root)
            .collect();
        for sid in targets {
            if let Some(callbacks) = self.pending_callbacks.remove(&sid) {
                for callback in callbacks {
                    callback.done(time_ms);
                }
            }
        }
    }

    /// Answer the `wp_presentation_feedback` callbacks held for `root`'s tree: `presented(now, refresh,
    /// seq)` when `presented`, or `discarded` when the frame was torn down unshown. The timestamp is the
    /// host-monotonic clock (`CLOCK_MONOTONIC`, the id `wp_presentation` advertised), the refresh is the
    /// primary output's frame interval, and `seq` is the monotonic present counter — one increment per
    /// PRESENT CYCLE (all feedbacks released by this one present share the frame's `seq` + `now`), so a
    /// client sees a strictly increasing, contiguous sequence: one number per frame that reached the screen.
    fn answer_tree_feedback(&mut self, root: SurfaceId, presented: bool) {
        if self.pending_presentation.is_empty() {
            return;
        }
        let targets: Vec<SurfaceId> = self
            .pending_presentation
            .keys()
            .copied()
            .filter(|&sid| self.engine.scene.window_root(sid) == Some(root) || sid == root)
            .collect();
        if targets.is_empty() {
            return;
        }
        let now = std::time::Duration::from_nanos(self.engine.clock().now_nanos());
        // Frame interval from the primary output's refresh (mHz → ns): 60_000 mHz ⇒ ~16.67 ms.
        let refresh_mhz = self
            .engine
            .scene
            .primary_output()
            .map(|o| o.refresh_mhz.max(1))
            .unwrap_or(60_000);
        let refresh = Refresh::fixed(std::time::Duration::from_nanos(
            1_000_000_000_000u64 / refresh_mhz as u64,
        ));
        // ONE presentation sequence number for THIS present cycle. Every feedback answered in this call
        // resolved against the SAME frame reaching the screen at the SAME timestamp `now` (a burst of
        // commits coalesced by the vsync throttle accumulates several feedbacks that all release on this one
        // present), so they must all carry the SAME `seq`: a `wp_presentation` sequence is a per-output
        // vblank counter — one frame is one number. Stamping each feedback with a distinct `seq` would
        // report several vblanks at one identical instant, which no real display can produce and which
        // corrupts a client's (Chrome's) vsync-phase estimate. Allocated lazily so a cycle that only
        // discards never advances the counter, which would otherwise leave a gap in the presented run.
        let mut frame_seq: Option<u64> = None;
        for sid in targets {
            // Name the output this surface's frame presented on: its currently-entered output, else its
            // selected output, else the primary. Cloned (smithay's `Output` is an `Arc` handle) so the
            // `present_seq` / `pending_presentation` mutations below don't conflict with the borrow.
            let root = self.engine.scene.window_root(sid).unwrap_or(sid);
            let output_handle = self
                .entered_outputs
                .get(&root)
                .copied()
                .or_else(|| self.engine.scene.selected_output(root).map(|o| o.id))
                .and_then(|id| self.wl_output_handle(id))
                .or_else(|| self.primary_wl_output())
                .cloned();
            let Some(feedbacks) = self.pending_presentation.remove(&sid) else {
                continue;
            };
            for feedback in feedbacks {
                if presented {
                    if let Some(output_handle) = &output_handle {
                        // Allocate this cycle's sequence number on first real present, then reuse it for
                        // every remaining feedback in the cycle (same frame ⇒ same seq + same `now`).
                        let seq = match frame_seq {
                            Some(s) => s,
                            None => {
                                self.present_seq += 1;
                                frame_seq = Some(self.present_seq);
                                self.present_seq
                            }
                        };
                        feedback.presented(
                            output_handle,
                            now,
                            refresh,
                            seq,
                            wp_presentation_feedback::Kind::Vsync,
                        );
                    } else {
                        feedback.discarded();
                    }
                } else {
                    feedback.discarded();
                }
            }
        }
    }

    /// Fire (and remove) the frame callbacks held for a single surface.
    fn fire_callbacks_for(&mut self, sid: SurfaceId) {
        let time_ms = (self.engine.clock().now_nanos() / 1_000_000) as u32;
        if let Some(callbacks) = self.pending_callbacks.remove(&sid) {
            for callback in callbacks {
                callback.done(time_ms);
            }
        }
    }

    /// Drop (without firing) the frame callbacks held for `root`'s tree — a terminally-failed frame.
    fn drop_tree_callbacks(&mut self, root: SurfaceId) {
        let targets: Vec<SurfaceId> = self
            .pending_callbacks
            .keys()
            .copied()
            .filter(|&sid| self.engine.scene.window_root(sid) == Some(root) || sid == root)
            .collect();
        for sid in targets {
            self.pending_callbacks.remove(&sid);
        }
    }
}

// ------------------------------------- input injection ---------------------------------------
//
// The headless compositor has NO hardware input source, so these methods are the seam a host (or a
// test) drives to deliver pointer + keyboard input to the focused client. Each routes through smithay's
// `PointerHandle`/`KeyboardHandle`, whose `PointerTarget`/`KeyboardTarget` impls for `WlSurface` do the
// actual wire serialization (enter/leave/motion/button/axis/frame and enter/leave/key/modifiers). The
// coordinate model matches smithay's `PointerHandle::motion` contract: we pass the pointer location in
// GLOBAL compositor space plus the focused surface's ORIGIN in global space, and smithay derives the
// surface-local coordinate the client receives as `location - origin`.
//
// The neutral scene tracks no global on-screen window position (every toplevel roots its own tree at
// `(0, 0)`), so "global" here is that shared root space: injected `(x, y)` are root-local logical
// points, and a surface's global origin is its accumulated offset within its window root.

/// One host/test-driven input action, delivered to [`HlState`] over the serve loop's input channel (see
/// [`super::serve::run_auto_with_input`]) or applied directly via [`HlState::apply_input`]. Keyboard
/// focus is expressed by intent (`FocusTopmostKeyboard`) rather than a surface id, because a remote
/// driver across the serve-thread boundary has no handle to the neutral [`SurfaceId`].
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
    /// the legacy `axis_discrete` on v5-7), all grouped in ONE `wl_pointer.frame`.
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

impl HlState {
    /// Apply one host/test-driven [`InputCommand`], routing it through the seat's pointer/keyboard.
    pub fn apply_input(&mut self, cmd: InputCommand) {
        // Latency trace: stamp the host-monotonic time this input was DISPATCHED into the compositor (the
        // start of the input→present cycle). Terse key=val, gated with the rest of `tag::WAYLAND` — pairs
        // with the `present_done … t_us=` line the engine logs when the resulting frame ships, so a trace
        // can subtract the two for the real input→present latency.
        hl_debug!(
            tag::WAYLAND,
            "input_dispatch t_us={}",
            self.engine.clock().now_nanos() / 1_000
        );
        match cmd {
            InputCommand::PointerMotion { x, y } => self.inject_pointer_motion(x, y),
            InputCommand::PointerMotionOn { window, x, y } => {
                self.inject_pointer_motion_on(Some(window), x, y)
            }
            InputCommand::PointerButton { button, pressed } => {
                self.inject_pointer_button(button, pressed)
            }
            InputCommand::PointerButtonOn {
                window,
                button,
                pressed,
                click_count,
            } => {
                self.last_pointer_click_count = click_count.max(1);
                self.set_keyboard_focus(self.engine.scene.window_root(window));
                self.inject_pointer_button(button, pressed);
            }
            InputCommand::ResizeSurface {
                surface,
                width,
                height,
                maximized,
                fullscreen,
                resizing,
            } => self
                .configure_native_resize(surface, width, height, maximized, fullscreen, resizing),
            InputCommand::ResizeSurfaceEnd { surface } => self.finish_native_resize(surface),
            InputCommand::PointerAxis {
                horizontal,
                vertical,
            } => self.inject_pointer_axis(horizontal, vertical),
            InputCommand::PointerAxisDiscrete {
                horizontal,
                vertical,
                h120,
                v120,
            } => self.inject_pointer_axis_discrete(horizontal, vertical, h120, v120),
            InputCommand::MoveToplevelToPoint { index, x, y } => {
                self.move_toplevel_to_point(index, x, y)
            }
            InputCommand::Key { keycode, pressed } => self.inject_key(keycode, pressed),
            InputCommand::FocusTopmostKeyboard => {
                let target = self.topmost_toplevel();
                self.set_keyboard_focus(target);
            }
            InputCommand::FocusSurface(surface) => {
                let target = self.engine.scene.window_root(surface);
                self.set_keyboard_focus(target);
            }
            InputCommand::FocusToplevelIndex(n) => {
                let target = self.toplevel_at(n);
                self.set_keyboard_focus(target);
            }
            InputCommand::ClearKeyboardFocus => self.set_keyboard_focus(None),
            InputCommand::ImeCommitString(text) => self.inject_ime_commit_string(text),
            InputCommand::ImePreeditString {
                text,
                cursor_begin,
                cursor_end,
            } => self.inject_ime_preedit_string(text, cursor_begin, cursor_end),
            InputCommand::ImeDeleteSurrounding {
                before_length,
                after_length,
            } => self.inject_ime_delete_surrounding(before_length, after_length),
            InputCommand::CloseTopmostToplevel => self.close_topmost_toplevel(),
            InputCommand::CloseSurface(surface) => self.close_toplevel(surface),
            InputCommand::TouchDown { id, x, y } => self.inject_touch_down(id, x, y),
            InputCommand::TouchMotion { id, x, y } => self.inject_touch_motion(id, x, y),
            InputCommand::TouchUp { id } => self.inject_touch_up(id),
            InputCommand::TouchFrame => self.inject_touch_frame(),
            InputCommand::TouchCancel => self.inject_touch_cancel(),
            InputCommand::GestureSwipeBegin { fingers } => self.inject_gesture_swipe_begin(fingers),
            InputCommand::GestureSwipeUpdate { dx, dy } => self.inject_gesture_swipe_update(dx, dy),
            InputCommand::GestureSwipeEnd { cancelled } => self.inject_gesture_swipe_end(cancelled),
            InputCommand::GesturePinchBegin { fingers } => self.inject_gesture_pinch_begin(fingers),
            InputCommand::GesturePinchUpdate {
                dx,
                dy,
                scale,
                rotation,
            } => self.inject_gesture_pinch_update(dx, dy, scale, rotation),
            InputCommand::GesturePinchEnd { cancelled } => self.inject_gesture_pinch_end(cancelled),
            InputCommand::TabletToolProximityIn { x, y } => self.inject_tablet_proximity_in(x, y),
            InputCommand::TabletToolMotion { x, y, pressure } => {
                self.inject_tablet_motion(x, y, pressure)
            }
            InputCommand::TabletToolTipDown => self.inject_tablet_tip_down(),
            InputCommand::TabletToolTipUp => self.inject_tablet_tip_up(),
            InputCommand::TabletToolProximityOut => self.inject_tablet_proximity_out(),
            InputCommand::SessionLock => self.lock_session(),
            InputCommand::SessionUnlock => self.unlock_session(),
        }
    }

    fn configure_native_resize(
        &mut self,
        surface: SurfaceId,
        width: u32,
        height: u32,
        maximized: bool,
        fullscreen: bool,
        resizing: bool,
    ) {
        let toplevel = self
            .xdg_shell
            .toplevel_surfaces()
            .iter()
            .find(|toplevel| self.sid(toplevel.wl_surface()) == Some(surface))
            .cloned();
        let Some(toplevel) = toplevel else { return };
        let host_fullscreen = self.host_fullscreen.contains(&surface);
        if !fullscreen {
            self.host_fullscreen.remove(&surface);
        }
        hl_debug!(
            tag::WAYLAND,
            "native resize configure surface={} size={}x{}",
            surface.0,
            width,
            height
        );
        let size = (
            width.clamp(1, i32::MAX as u32) as i32,
            height.clamp(1, i32::MAX as u32) as i32,
        );
        toplevel.with_pending_state(|state| {
            state.size = Some(size.into());
            let guest_maximized = maximized || (fullscreen && host_fullscreen);
            let guest_fullscreen = fullscreen && !host_fullscreen;
            if guest_maximized {
                state.states.set(XdgToplevelState::Maximized);
            } else {
                state.states.unset(XdgToplevelState::Maximized);
            }
            if guest_fullscreen {
                state.states.set(XdgToplevelState::Fullscreen);
            } else {
                state.states.unset(XdgToplevelState::Fullscreen);
            }
            state.states.set(XdgToplevelState::Activated);
            if resizing {
                state.states.set(XdgToplevelState::Resizing);
            } else {
                state.states.unset(XdgToplevelState::Resizing);
            }
        });
        toplevel.send_configure();
    }

    fn finish_native_resize(&mut self, surface: SurfaceId) {
        let toplevel = self
            .xdg_shell
            .toplevel_surfaces()
            .iter()
            .find(|toplevel| self.sid(toplevel.wl_surface()) == Some(surface))
            .cloned();
        let Some(toplevel) = toplevel else { return };
        toplevel.with_pending_state(|state| {
            state.states.unset(XdgToplevelState::Resizing);
        });
        toplevel.send_configure();
        hl_debug!(tag::WAYLAND, "native resize end surface={}", surface.0);
    }

    /// The most recently mapped toplevel (highest surface id) — the "topmost" window an input-focus
    /// intent targets. `None` if no toplevel is mapped. A stand-in for real z-order/stacking, which the
    /// neutral scene does not model.
    pub fn topmost_toplevel(&self) -> Option<SurfaceId> {
        self.engine.scene.toplevels().max()
    }

    /// The toplevel at index `n` in ascending surface-id order (0 = earliest-mapped). `None` if `n` is
    /// out of range. Backs [`InputCommand::FocusToplevelIndex`] — a stable, inspectable way for a
    /// host/test to target a specific window in a multi-window stack (`toplevels()` is unordered, so it
    /// is sorted here).
    pub fn toplevel_at(&self, n: usize) -> Option<SurfaceId> {
        let mut tls: Vec<SurfaceId> = self.engine.scene.toplevels().collect();
        tls.sort();
        tls.get(n).copied()
    }

    /// The current-frame timestamp (ms) events are stamped with — the same host-monotonic clock the
    /// frame callbacks read, so input and frame time share one timeline.
    fn input_time_ms(&self) -> u32 {
        (self.engine.clock().now_nanos() / 1_000_000) as u32
    }

    /// Candidate window roots a pointer hit-test walks, best-first: the keyboard-focused window (so an
    /// overlapping stack resolves to the active window), then every other toplevel. Because the neutral
    /// scene carries no global window offsets, all roots sit at `(0, 0)`; focus is the only tie-break.
    fn candidate_roots(&self) -> Vec<SurfaceId> {
        let mut roots = Vec::new();
        if let Some(focus) = self.engine.scene.seat().keyboard_focus {
            if let Some(root) = self.engine.scene.window_root(focus) {
                roots.push(root);
            }
        }
        for tl in self.engine.scene.toplevels() {
            if !roots.contains(&tl) {
                roots.push(tl);
            }
        }
        roots
    }

    /// Hit-test the tree(s) at root-local logical `(x, y)`: the input-sensitive surface under the point
    /// and its window root + root-space origin `(ox, oy)`. Returns `(root, hit_surface, ox, oy)`.
    fn hit_test(&self, x: f64, y: f64) -> Option<(SurfaceId, SurfaceId, i32, i32)> {
        let (ix, iy) = (x.floor() as i32, y.floor() as i32);
        for root in self.candidate_roots() {
            if let Some((sid, ox, oy)) = surface_at(&self.engine.scene, root, ix, iy) {
                return Some((root, sid, ox, oy));
            }
        }
        None
    }

    /// Move the pointer to root-local logical `(x, y)`. Hit-tests the surface under the point, updates
    /// the neutral seat, and drives smithay's `PointerHandle::motion` + `frame` with that focus so the
    /// client receives `wl_pointer.leave`/`enter` (on the surface under the cursor changing) and
    /// `wl_pointer.motion` at the correct surface-local coordinate (`(x, y)` minus the surface origin).
    pub fn inject_pointer_motion(&mut self, x: f64, y: f64) {
        self.inject_pointer_motion_on(None, x, y);
    }

    fn inject_pointer_motion_on(&mut self, window: Option<SurfaceId>, x: f64, y: f64) {
        hl_debug!(tag::WAYLAND, "input motion x={:.0} y={:.0}", x, y);
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let hit = window
            .and_then(|window| self.engine.scene.window_root(window))
            .and_then(|root| {
                let (ix, iy) = (x.floor() as i32, y.floor() as i32);
                surface_at(&self.engine.scene, root, ix, iy)
                    .map(|(surface, ox, oy)| (root, surface, ox, oy))
            })
            .or_else(|| window.is_none().then(|| self.hit_test(x, y)).flatten());

        // Build smithay's focus: the concrete `WlSurface` + its origin in global (root) space.
        let focus = hit.and_then(|(_, sid, ox, oy)| {
            self.surfaces_by_id
                .get(&sid)
                .cloned()
                .map(|wl| (wl, Point::<f64, Logical>::from((ox as f64, oy as f64))))
        });
        // Whether the focused surface holds an ACTIVE `zwp_locked_pointer_v1` — if so the pointer is frozen
        // in place: no absolute `wl_pointer.motion` is delivered and the neutral seat location does not move.
        // The client drives purely off relative motion (below), the standard pointer-lock experience.
        let locked = focus
            .as_ref()
            .is_some_and(|(wl, _)| self.pointer_locked_on(wl));

        let serial = SERIAL_COUNTER.next_serial();
        let time = self.input_time_ms();

        // Relative motion (delta since the last RAW injected position) — delivered to any bound
        // `zwp_relative_pointer_v1` on the focused surface. A no-op if the client bound no relative pointer.
        // Computed against the raw position (not the delivered absolute one) so a locked pointer still
        // reports the real per-move device delta while its absolute position stays frozen.
        let (old_x, old_y) = self.last_injected_pointer;
        self.last_injected_pointer = (x, y);
        let (dx, dy) = (x - old_x, y - old_y);
        if dx != 0.0 || dy != 0.0 {
            pointer.relative_motion(
                self,
                focus.clone(),
                &RelativeMotionEvent {
                    delta: (dx, dy).into(),
                    delta_unaccel: (dx, dy).into(),
                    utime: time as u64 * 1_000, // ms → µs (the relative-pointer protocol's unit)
                },
            );
        }

        if locked {
            // Pointer position is locked: skip absolute motion and leave the neutral seat frozen. Still
            // emit a `wl_pointer.frame` so the relative motion above is a complete, framed update.
            pointer.frame(self);
            return;
        }

        // Keep the neutral seat consistent with what we deliver over the wire (for inspection/tests). Log
        // the enter/leave transition (focus changed) so the pointer-focus handoff is traceable in a trace.
        let new_focus = hit.map(|(_, sid, _, _)| sid);
        let prev_focus = self.engine.scene.seat().pointer_focus;
        if new_focus != prev_focus {
            hl_debug!(
                tag::WAYLAND,
                "pointer focus from={:?} to={:?} at x={:.0} y={:.0}",
                prev_focus.map(|s| s.0),
                new_focus.map(|s| s.0),
                x,
                y
            );
        }
        self.engine.scene.seat_mut().pointer_location = (x, y);
        self.engine.scene.seat_mut().pointer_focus = new_focus;
        pointer.motion(
            self,
            focus,
            &MotionEvent {
                location: (x, y).into(),
                serial,
                time,
            },
        );
        pointer.frame(self);
    }

    /// Whether `surface` currently holds an ACTIVE `zwp_locked_pointer_v1` constraint on this seat's
    /// pointer — the check [`Self::inject_pointer_motion`] uses to freeze the absolute pointer position.
    fn pointer_locked_on(&self, surface: &WlSurface) -> bool {
        let Some(pointer) = self.seat.get_pointer() else {
            return false;
        };
        with_pointer_constraint(
            surface,
            &pointer,
            |constraint| matches!(constraint, Some(c) if c.is_active() && matches!(&*c, PointerConstraint::Locked(_))),
        )
    }

    /// Press or release a pointer button. Uses the pointer's CURRENT focus (from the last motion), which
    /// smithay tracks internally — so a button lands on whatever surface the cursor is over.
    pub fn inject_pointer_button(&mut self, button: u32, pressed: bool) {
        hl_debug!(tag::WAYLAND, "input button={} pressed={}", button, pressed);
        // An explicit popup grab (menu / context-menu) dismisses on a press that lands outside the whole
        // popup chain — the click-outside-closes-the-menu semantics. Uses the pointer's last known location
        // (set by the preceding `inject_pointer_motion`). The button itself is still delivered below.
        if pressed {
            let (x, y) = self.engine.scene.seat().pointer_location;
            if self.press_outside_grab_chain(x, y) {
                self.dismiss_popup_grabs();
            }
        }
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let serial = SERIAL_COUNTER.next_serial();
        let time = self.input_time_ms();
        let state = if pressed {
            ButtonState::Pressed
        } else {
            ButtonState::Released
        };
        pointer.button(
            self,
            &ButtonEvent {
                serial,
                time,
                button,
                state,
            },
        );
        pointer.frame(self);
    }

    /// Scroll the pointer by logical `horizontal`/`vertical` amounts (wheel source). A zero component is
    /// omitted so the client only sees the axes that actually moved.
    pub fn inject_pointer_axis(&mut self, horizontal: f64, vertical: f64) {
        hl_debug!(
            tag::WAYLAND,
            "input axis h={:.1} v={:.1}",
            horizontal,
            vertical
        );
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let time = self.input_time_ms();
        let mut frame = AxisFrame::new(time).source(AxisSource::Wheel);
        if horizontal != 0.0 {
            frame = frame.value(Axis::Horizontal, horizontal);
        }
        if vertical != 0.0 {
            frame = frame.value(Axis::Vertical, vertical);
        }
        pointer.axis(self, frame);
        pointer.frame(self);
    }

    /// Scroll by DISCRETE wheel steps: emit the smooth `horizontal`/`vertical` value AND the discrete
    /// `h120`/`v120` step counts (120 units = one detent) in one framed `wl_pointer` axis event, so a
    /// client that reads discrete notches (`wl_pointer.axis_value120` on v8+, `axis_discrete` on v5-7)
    /// sees the exact step count + sign alongside the smooth value — what a real mouse wheel delivers.
    pub fn inject_pointer_axis_discrete(
        &mut self,
        horizontal: f64,
        vertical: f64,
        h120: i32,
        v120: i32,
    ) {
        hl_debug!(
            tag::WAYLAND,
            "input axis h={:.1} v={:.1} h120={} v120={}",
            horizontal,
            vertical,
            h120,
            v120
        );
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let time = self.input_time_ms();
        let mut frame = AxisFrame::new(time).source(AxisSource::Wheel);
        if horizontal != 0.0 {
            frame = frame.value(Axis::Horizontal, horizontal);
        }
        if vertical != 0.0 {
            frame = frame.value(Axis::Vertical, vertical);
        }
        if h120 != 0 {
            frame = frame.v120(Axis::Horizontal, h120);
        }
        if v120 != 0 {
            frame = frame.v120(Axis::Vertical, v120);
        }
        pointer.axis(self, frame);
        pointer.frame(self);
    }

    /// Give keyboard focus to `sid` (or clear it with `None`). Drives smithay's `KeyboardHandle::set_focus`
    /// — which emits `wl_keyboard.leave` to the old focus and `wl_keyboard.enter` (+ the keymap already
    /// sent at bind) to the new — and mirrors the change into the neutral seat.
    pub fn set_keyboard_focus(&mut self, sid: Option<SurfaceId>) {
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        let surface = sid.and_then(|s| self.surfaces_by_id.get(&s).cloned());
        // Follow the keyboard focus with the clipboard (data-device) focus so the newly focused client's
        // `wl_data_device` receives selection offers and its `set_selection` is honored — the standard
        // Wayland "clipboard follows keyboard focus" rule. `None` clears it (no client owns the clipboard).
        let focus_client = surface
            .as_ref()
            .and_then(|s| self.display.get_client(s.id()).ok());
        set_data_device_focus(&self.display, &self.seat, focus_client.clone());
        // The PRIMARY (middle-click) selection follows keyboard focus by the same rule, so the newly focused
        // client's `zwp_primary_selection_device_v1` receives the current primary offer and its
        // `set_selection` is honored.
        set_primary_focus(&self.display, &self.seat, focus_client);
        // Mirror into the neutral seat so scene focus bookkeeping stays truthful.
        let prev = self.engine.scene.seat().keyboard_focus;
        self.engine.scene.seat_mut().keyboard_focus = sid;
        let serial = SERIAL_COUNTER.next_serial();
        hl_debug!(
            tag::WAYLAND,
            "focus kbd from={:?} to={:?} serial={:?}",
            prev.map(|s| s.0),
            sid.map(|s| s.0),
            serial
        );
        keyboard.set_focus(self, surface, serial);
    }

    /// Press or release a key by EVDEV keycode. smithay's keymap is keyed on X11 keycodes (evdev + 8),
    /// and its `KeyboardTarget` impl sends `evdev` back to the client (`raw - 8`); we add the 8 here so
    /// the caller speaks Linux `input-event-codes` and the client receives the same value. Modifiers are
    /// tracked by smithay's xkb state across presses. No compositor keybinding filter — always forward.
    pub fn inject_key(&mut self, keycode: u32, pressed: bool) {
        hl_debug!(tag::WAYLAND, "input key={} pressed={}", keycode, pressed);
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        let serial = SERIAL_COUNTER.next_serial();
        let time = self.input_time_ms();
        let state = if pressed {
            KeyState::Pressed
        } else {
            KeyState::Released
        };
        keyboard.input::<(), _>(
            self,
            Keycode::new(keycode + 8),
            state,
            serial,
            time,
            |_, _, _| FilterResult::Forward,
        );
    }

    /// Deliver an IME `commit_string` (+ `done`) to the focused, enabled `zwp_text_input_v3` — the host
    /// text-entry seam, mirroring [`Self::inject_key`] but for composed text. Smithay routes text-input
    /// through the seat's [`TextInputHandle`](smithay::wayland::text_input); `with_active_text_input` targets
    /// the text-input instance the focused client ENABLED (which requires an input method instance to exist,
    /// hence the advertised `zwp_input_method_manager_v2`), and `done(false)` stamps the correct serial (the
    /// client's own commit count, tracked by Smithay) so the client applies the change. A no-op if no
    /// text-input is focused+active.
    pub fn inject_ime_commit_string(&mut self, text: String) {
        hl_debug!(tag::WAYLAND, "input ime commit {:?}", text);
        let ti = self.seat.text_input();
        ti.with_active_text_input(|obj, _surface| obj.commit_string(Some(text.clone())));
        ti.done(false);
    }

    /// Deliver an IME `preedit_string` (composing text, with byte-offset cursor) + `done` to the focused,
    /// enabled `zwp_text_input_v3`. See [`Self::inject_ime_commit_string`] for the routing; the preedit is
    /// transient text the client shows before a commit replaces it.
    pub fn inject_ime_preedit_string(&mut self, text: String, cursor_begin: i32, cursor_end: i32) {
        hl_debug!(
            tag::WAYLAND,
            "input ime preedit {:?} [{},{}]",
            text,
            cursor_begin,
            cursor_end
        );
        let ti = self.seat.text_input();
        ti.with_active_text_input(|obj, _surface| {
            obj.preedit_string(Some(text.clone()), cursor_begin, cursor_end)
        });
        ti.done(false);
    }

    /// Deliver an IME `delete_surrounding_text` (+ `done`) to the focused, enabled `zwp_text_input_v3` —
    /// delete `before`/`after` bytes around the cursor. See [`Self::inject_ime_commit_string`] for routing.
    pub fn inject_ime_delete_surrounding(&mut self, before: u32, after: u32) {
        hl_debug!(
            tag::WAYLAND,
            "input ime delete_surrounding before={} after={}",
            before,
            after
        );
        let ti = self.seat.text_input();
        ti.with_active_text_input(|obj, _surface| obj.delete_surrounding_text(before, after));
        ti.done(false);
    }

    /// Ask the topmost mapped toplevel to close (`xdg_toplevel.close`). The compositor-initiated close
    /// request a window-manager close affordance sends: the client receives `close` and typically destroys
    /// the toplevel (there is no reply event, so nothing is acked). A no-op if no toplevel is mapped or its
    /// Smithay `ToplevelSurface` is no longer live. Resolves the concrete [`ToplevelSurface`] by matching the
    /// topmost neutral surface's `wl_surface` against the shell's live toplevels (which own `send_close`).
    pub fn close_topmost_toplevel(&mut self) {
        let Some(sid) = self.topmost_toplevel() else {
            return;
        };
        self.close_toplevel(sid);
    }

    fn close_toplevel(&mut self, surface: SurfaceId) {
        let Some(sid) = self.engine.scene.window_root(surface) else {
            return;
        };
        let Some(wl) = self.surfaces_by_id.get(&sid).cloned() else {
            return;
        };
        let toplevel = self
            .xdg_shell
            .toplevel_surfaces()
            .iter()
            .find(|t| t.wl_surface() == &wl)
            .cloned();
        if let Some(toplevel) = toplevel {
            hl_debug!(tag::WAYLAND, "toplevel close surf={}", sid.0);
            toplevel.send_close();
        }
    }

    // ------------------------------------ wl_touch ------------------------------------------
    //
    // The touch seam mirrors the pointer seam: each touch point (a "slot", keyed by the client-facing
    // touch id) hit-tests independently against the surface tree, and smithay's `TouchTarget` impl for
    // `WlSurface` serializes down/motion/up/frame/cancel. Multiple live ids coexist (real multi-touch);
    // the caller closes each atomic batch with `TouchFrame`.

    /// Deliver `wl_touch.down` for point `id` at root-local logical `(x, y)`: hit-test the surface under
    /// the point and hand smithay's `TouchHandle::down` the focus + surface-local coordinate.
    pub fn inject_touch_down(&mut self, id: i32, x: f64, y: f64) {
        hl_debug!(
            tag::WAYLAND,
            "input touch down id={} x={:.0} y={:.0}",
            id,
            x,
            y
        );
        let Some(touch) = self.seat.get_touch() else {
            return;
        };
        let focus = self.touch_focus(x, y);
        let serial = SERIAL_COUNTER.next_serial();
        let time = self.input_time_ms();
        touch.down(
            self,
            focus,
            &TouchDownEvent {
                slot: TouchSlot::from(Some(id as u32)),
                location: (x, y).into(),
                serial,
                time,
            },
        );
    }

    /// Deliver `wl_touch.motion` for point `id` at root-local logical `(x, y)`.
    pub fn inject_touch_motion(&mut self, id: i32, x: f64, y: f64) {
        hl_debug!(
            tag::WAYLAND,
            "input touch motion id={} x={:.0} y={:.0}",
            id,
            x,
            y
        );
        let Some(touch) = self.seat.get_touch() else {
            return;
        };
        let focus = self.touch_focus(x, y);
        let time = self.input_time_ms();
        touch.motion(
            self,
            focus,
            &TouchMotionEvent {
                slot: TouchSlot::from(Some(id as u32)),
                location: (x, y).into(),
                time,
            },
        );
    }

    /// Deliver `wl_touch.up` for point `id` (the finger lifted).
    pub fn inject_touch_up(&mut self, id: i32) {
        hl_debug!(tag::WAYLAND, "input touch up id={}", id);
        let Some(touch) = self.seat.get_touch() else {
            return;
        };
        let serial = SERIAL_COUNTER.next_serial();
        let time = self.input_time_ms();
        touch.up(
            self,
            &TouchUpEvent {
                slot: TouchSlot::from(Some(id as u32)),
                serial,
                time,
            },
        );
    }

    /// Deliver `wl_touch.frame` — close the current atomic touch batch.
    pub fn inject_touch_frame(&mut self) {
        hl_debug!(tag::WAYLAND, "input touch frame");
        let Some(touch) = self.seat.get_touch() else {
            return;
        };
        touch.frame(self);
    }

    /// Deliver `wl_touch.cancel` — the compositor took the whole touch sequence over.
    pub fn inject_touch_cancel(&mut self) {
        hl_debug!(tag::WAYLAND, "input touch cancel");
        let Some(touch) = self.seat.get_touch() else {
            return;
        };
        touch.cancel(self);
    }

    /// The touch focus `(WlSurface, origin)` under root-local logical `(x, y)`, matching the pointer
    /// hit-test — the surface the touch point lands on and its origin in root space.
    fn touch_focus(&self, x: f64, y: f64) -> Option<(WlSurface, Point<f64, Logical>)> {
        self.hit_test(x, y).and_then(|(_, sid, ox, oy)| {
            self.surfaces_by_id
                .get(&sid)
                .cloned()
                .map(|wl| (wl, Point::<f64, Logical>::from((ox as f64, oy as f64))))
        })
    }

    // ------------------------------ zwp_pointer_gestures_v1 ---------------------------------
    //
    // Trackpad pinch/swipe. These target the pointer's CURRENT focus (set by the last `PointerMotion`),
    // so a demo positions the pointer over the surface first, then drives the gesture. smithay routes each
    // to the focused surface's bound `zwp_pointer_gesture_{swipe,pinch}_v1`.

    /// `zwp_pointer_gesture_swipe_v1.begin` with `fingers` fingers.
    pub fn inject_gesture_swipe_begin(&mut self, fingers: u32) {
        hl_debug!(
            tag::WAYLAND,
            "input gesture swipe begin fingers={}",
            fingers
        );
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let serial = SERIAL_COUNTER.next_serial();
        let time = self.input_time_ms();
        pointer.gesture_swipe_begin(
            self,
            &GestureSwipeBeginEvent {
                serial,
                time,
                fingers,
            },
        );
    }

    /// `zwp_pointer_gesture_swipe_v1.update` by logical center delta `(dx, dy)`.
    pub fn inject_gesture_swipe_update(&mut self, dx: f64, dy: f64) {
        hl_debug!(
            tag::WAYLAND,
            "input gesture swipe update dx={:.1} dy={:.1}",
            dx,
            dy
        );
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let time = self.input_time_ms();
        pointer.gesture_swipe_update(
            self,
            &GestureSwipeUpdateEvent {
                time,
                delta: (dx, dy).into(),
            },
        );
    }

    /// `zwp_pointer_gesture_swipe_v1.end` (`cancelled` = aborted).
    pub fn inject_gesture_swipe_end(&mut self, cancelled: bool) {
        hl_debug!(
            tag::WAYLAND,
            "input gesture swipe end cancelled={}",
            cancelled
        );
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let serial = SERIAL_COUNTER.next_serial();
        let time = self.input_time_ms();
        pointer.gesture_swipe_end(
            self,
            &GestureSwipeEndEvent {
                serial,
                time,
                cancelled,
            },
        );
    }

    /// `zwp_pointer_gesture_pinch_v1.begin` with `fingers` fingers.
    pub fn inject_gesture_pinch_begin(&mut self, fingers: u32) {
        hl_debug!(
            tag::WAYLAND,
            "input gesture pinch begin fingers={}",
            fingers
        );
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let serial = SERIAL_COUNTER.next_serial();
        let time = self.input_time_ms();
        pointer.gesture_pinch_begin(
            self,
            &GesturePinchBeginEvent {
                serial,
                time,
                fingers,
            },
        );
    }

    /// `zwp_pointer_gesture_pinch_v1.update`: center delta `(dx, dy)`, absolute `scale`, `rotation` degrees.
    pub fn inject_gesture_pinch_update(&mut self, dx: f64, dy: f64, scale: f64, rotation: f64) {
        hl_debug!(
            tag::WAYLAND,
            "input gesture pinch update scale={:.2} rot={:.1}",
            scale,
            rotation
        );
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let time = self.input_time_ms();
        pointer.gesture_pinch_update(
            self,
            &GesturePinchUpdateEvent {
                time,
                delta: (dx, dy).into(),
                scale,
                rotation,
            },
        );
    }

    /// `zwp_pointer_gesture_pinch_v1.end` (`cancelled` = aborted).
    pub fn inject_gesture_pinch_end(&mut self, cancelled: bool) {
        hl_debug!(
            tag::WAYLAND,
            "input gesture pinch end cancelled={}",
            cancelled
        );
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let serial = SERIAL_COUNTER.next_serial();
        let time = self.input_time_ms();
        pointer.gesture_pinch_end(
            self,
            &GesturePinchEndEvent {
                serial,
                time,
                cancelled,
            },
        );
    }

    // -------------------------------- zwp_tablet_tool_v2 ------------------------------------
    //
    // The pen. `proximity_in` hovers the tool over the surface under a point; `motion` (carrying a queued
    // `pressure`) tracks it; `tip_down`/`tip_up` are contact; `proximity_out` ends the hover. smithay
    // auto-frames each action and serializes the `zwp_tablet_tool_v2` wire events to the focused client.

    /// `zwp_tablet_tool_v2.proximity_in` for the surface under root-local logical `(x, y)`.
    pub fn inject_tablet_proximity_in(&mut self, x: f64, y: f64) {
        hl_debug!(
            tag::WAYLAND,
            "input tablet proximity_in x={:.0} y={:.0}",
            x,
            y
        );
        let Some((wl, origin)) = self.touch_focus(x, y) else {
            return;
        };
        let serial = SERIAL_COUNTER.next_serial();
        let time = self.input_time_ms();
        self.tablet_tool
            .proximity_in((x, y).into(), (wl, origin), &self.tablet, serial, time);
    }

    /// `zwp_tablet_tool_v2.motion` (+ queued `pressure`) at root-local logical `(x, y)`.
    pub fn inject_tablet_motion(&mut self, x: f64, y: f64, pressure: f64) {
        hl_debug!(
            tag::WAYLAND,
            "input tablet motion x={:.0} y={:.0} p={:.2}",
            x,
            y,
            pressure
        );
        let focus = self.touch_focus(x, y);
        let serial = SERIAL_COUNTER.next_serial();
        let time = self.input_time_ms();
        // Pressure is queued and shipped alongside the motion below.
        self.tablet_tool.pressure(pressure);
        self.tablet_tool
            .motion((x, y).into(), focus, &self.tablet, serial, time);
    }

    /// `zwp_tablet_tool_v2.down` — the tip made contact.
    pub fn inject_tablet_tip_down(&mut self) {
        hl_debug!(tag::WAYLAND, "input tablet tip_down");
        let serial = SERIAL_COUNTER.next_serial();
        let time = self.input_time_ms();
        self.tablet_tool.tip_down(serial, time);
    }

    /// `zwp_tablet_tool_v2.up` — the tip lifted.
    pub fn inject_tablet_tip_up(&mut self) {
        hl_debug!(tag::WAYLAND, "input tablet tip_up");
        let time = self.input_time_ms();
        self.tablet_tool.tip_up(time);
    }

    /// `zwp_tablet_tool_v2.proximity_out` — the pen left proximity.
    pub fn inject_tablet_proximity_out(&mut self) {
        hl_debug!(tag::WAYLAND, "input tablet proximity_out");
        let time = self.input_time_ms();
        self.tablet_tool.proximity_out(time);
    }

    // ---------------------------- ext_session_lock (screen lock) ---------------------------

    /// Host-initiated session lock (the symmetric seam to the client-driven `ext_session_lock`).
    pub fn lock_session(&mut self) {
        self.set_session_locked(true);
    }

    /// Host-initiated session unlock.
    pub fn unlock_session(&mut self) {
        self.set_session_locked(false);
    }

    /// Apply a lock/unlock transition: occlude (hide) or restore every NORMAL toplevel, mirror the state
    /// into [`Observations`], and — on unlock — mark the restored surfaces dirty + arm a repaint so they
    /// visibly return at the next refresh boundary even if their clients are idle. Lock surfaces (tracked in
    /// `lock_surfaces`) are never occluded: they are what stays on screen while locked.
    fn set_session_locked(&mut self, locked: bool) {
        hl_info!(tag::WAYLAND, "session lock={}", locked);
        self.session_locked = locked;
        let vis = if locked {
            Visibility::Occluded
        } else {
            Visibility::Visible
        };
        let toplevels: Vec<SurfaceId> = self.engine.scene.toplevels().collect();
        for tl in toplevels {
            if self.lock_surfaces.contains(&tl) {
                continue;
            }
            self.engine.scene.set_visibility(tl, vis);
            self.reconcile_window(tl);
            if !locked {
                // Restore: force a fresh present so the unhidden surface reappears.
                self.engine.scene.mark_dirty(tl);
                self.arm_repaint(tl);
            }
        }
        self.observations.lock().unwrap().session_locked = locked;
    }
}

// ------------------------------------- protocol handlers -------------------------------------

impl CompositorHandler for HlState {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor
    }

    fn new_surface(&mut self, surface: &WlSurface) {
        self.register_surface(surface);
    }

    /// A `wl_subsurface` was created (`wl_subcompositor.get_subsurface(surface, parent)`). Establish the
    /// scene parent linkage immediately: subsurfaces map SYNC by default (they present atomically with the
    /// parent until `set_desync`), at offset `(0, 0)` until the first `set_position` commit. The concrete
    /// offset / sync mode / z-order are refreshed from Smithay on each commit (`sync_subsurface_tree`).
    fn new_subsurface(&mut self, surface: &WlSurface, parent: &WlSurface) {
        if let (Some(sid), Some(parent_sid)) = (self.sid(surface), self.sid(parent)) {
            self.engine.scene.set_role(
                sid,
                SurfaceRole::Subsurface(SubsurfaceState {
                    parent: parent_sid,
                    x: 0,
                    y: 0,
                    sync: true,
                }),
            );
        }
    }

    fn commit(&mut self, surface: &WlSurface) {
        self.on_commit(surface);
    }

    fn destroyed(&mut self, surface: &WlSurface) {
        self.teardown_surface(surface);
    }
}

impl BufferHandler for HlState {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {}
}

impl ShmHandler for HlState {
    fn shm_state(&self) -> &ShmState {
        &self.shm
    }
}

impl DmabufHandler for HlState {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf
    }

    /// A client finished a `zwp_linux_buffer_params_v1` create/create_immed. Accept the buffer iff it is
    /// something this SOFTWARE presenter can truthfully turn into pixels — a single-plane LINEAR
    /// ARGB/XRGB (or byte-swapped ABGR/XBGR) buffer, whose plane fd is plain CPU memory we `pread` at
    /// commit ([`read_dmabuf_rgba`]). REJECT anything else (a tiled/GPU modifier we cannot detile without
    /// a GPU, or a multi-plane/YUV layout we do not unpack) via `notifier.failed()`, so the client falls
    /// back to `wl_shm` rather than committing a buffer we could only ever present as blank. Honest by
    /// construction: we never accept an import we cannot actually read.
    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) {
        let fmt = dmabuf.format();
        let importable = fmt.modifier == Modifier::Linear
            && dmabuf.num_planes() == 1
            && matches!(
                fmt.code,
                Fourcc::Argb8888 | Fourcc::Xrgb8888 | Fourcc::Abgr8888 | Fourcc::Xbgr8888
            );
        if importable {
            let _ = notifier.successful::<HlState>();
        } else {
            notifier.failed();
        }
    }
}

impl SeatHandler for HlState {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<HlState> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, _seat: &Seat<HlState>, _focused: Option<&WlSurface>) {}

    /// The focused client set the pointer cursor. A `wp_cursor_shape_device_v1.set_shape` arrives here as
    /// [`CursorImageStatus::Named`] (Smithay decoded the shape enum to a `CursorIcon`); a legacy
    /// `wl_pointer.set_cursor` with a surface arrives as `Surface`, and hiding it as `Hidden`. Headless there
    /// is no on-screen cursor to repaint, but recording the requested NAMED shape into [`Observations`] is
    /// the observable proof the compositor honoured `wp_cursor_shape` (it carries no reply event), and lets a
    /// test assert Chrome's cursor name reached the seat. `Surface`/`Hidden` clear the recorded name.
    fn cursor_image(
        &mut self,
        _seat: &Seat<HlState>,
        image: smithay::input::pointer::CursorImageStatus,
    ) {
        use smithay::input::pointer::CursorImageStatus;
        let named = match image {
            CursorImageStatus::Named(icon) => Some(icon.name().to_string()),
            CursorImageStatus::Hidden | CursorImageStatus::Surface(_) => None,
        };
        if let Some(name) = &named {
            hl_debug!(
                tag::WAYLAND,
                "wp_cursor_shape set_shape -> named cursor '{name}'"
            );
        }
        self.observations.lock().unwrap().cursor_shape = named;
    }
}

/// Selection (clipboard / primary) plumbing for `wl_data_device`. Headless we carry no per-selection
/// user data (`()`), and the default `new_selection` / `send_selection` (no-op / not-answered) are
/// sufficient: the compositor never provides a selection of its OWN (that is what `send_selection`
/// would answer). CLIENT-to-client transfer is fully live — when the offered source is another client's
/// `wl_data_source`, Smithay forwards the receiving client's `wl_data_offer.receive` fd straight to the
/// source client as `wl_data_source.send`, and the source writes the bytes over that pipe. Combined with
/// clipboard focus tracking (via [`set_data_device_focus`], driven from keyboard focus in
/// [`Self::set_keyboard_focus`]), this makes a real cross-client copy/paste round-trip work headless —
/// see the `clipboard_selection_roundtrip` demo, which asserts the exact bytes A "copies" are what B
/// "pastes".
impl SelectionHandler for HlState {
    type SelectionUserData = String;

    fn new_selection(
        &mut self,
        ty: SelectionTarget,
        source: Option<SelectionSource>,
        _seat: Seat<Self>,
    ) {
        if ty != SelectionTarget::Clipboard {
            return;
        }
        let Some(source) = source else { return };
        let mime_types = source.mime_types();
        hl_debug!(
            tag::WAYLAND,
            "clipboard client source mimes={:?}",
            mime_types
        );
        let mime = ["text/plain;charset=utf-8", "text/plain", "UTF8_STRING"]
            .into_iter()
            .find(|candidate| mime_types.iter().any(|mime| mime == candidate));
        let Some(mime) = mime else { return };
        let Ok((mut reader, writer)) = UnixStream::pair() else {
            return;
        };
        source.send(mime.to_owned(), writer.into());
        let tx = self.clipboard_tx.clone();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if reader.read_to_end(&mut bytes).is_ok() {
                hl_debug!(tag::WAYLAND, "clipboard client bytes={}", bytes.len());
                let text = String::from_utf8_lossy(&bytes).into_owned();
                let _ = tx.send(text);
            }
        });
    }

    fn send_selection(
        &mut self,
        ty: SelectionTarget,
        _mime_type: String,
        fd: std::os::fd::OwnedFd,
        _seat: Seat<Self>,
        text: &Self::SelectionUserData,
    ) {
        if ty != SelectionTarget::Clipboard {
            return;
        }
        let text = text.clone();
        std::thread::spawn(move || {
            let mut file = std::fs::File::from(fd);
            let _ = file.write_all(text.as_bytes());
        });
    }
}

/// A client-initiated drag-and-drop grab (a client dragging one of its surfaces). The neutral headless
/// compositor applies no DnD policy of its own — the client manages the data transfer — but the two grab
/// lifecycle callbacks are recorded into the shared [`Observations`] so a test can observe the SOURCE side
/// of the drag (which emits no client-visible wire event beyond the grab itself): `started` fires when a
/// `start_drag` is honoured (Smithay has replaced the implicit pointer grab with its DnD grab, so the drag
/// pointer path is now live and a test may inject the motion that carries the offer to a target), and
/// `dropped` fires when the user releases the last button, carrying whether the drop was NEGOTIATED
/// (`validated` — the target accepted a mime + a non-empty action, so `wl_data_device.drop` was delivered).
/// See the `drag_and_drop` demo.
impl ClientDndGrabHandler for HlState {
    fn started(
        &mut self,
        _source: Option<smithay::reexports::wayland_server::protocol::wl_data_source::WlDataSource>,
        _icon: Option<WlSurface>,
        _seat: Seat<HlState>,
    ) {
        hl_debug!(tag::WAYLAND, "dnd started");
        let mut o = self.observations.lock().unwrap();
        o.dnd_active = true;
        o.dnd_dropped = false;
        o.dnd_drop_validated = false;
    }

    fn dropped(&mut self, _target: Option<WlSurface>, validated: bool, _seat: Seat<HlState>) {
        hl_debug!(tag::WAYLAND, "dnd dropped validated={}", validated);
        let mut o = self.observations.lock().unwrap();
        o.dnd_active = false;
        o.dnd_dropped = true;
        o.dnd_drop_validated = validated;
    }
}

/// A server-initiated drag-and-drop grab (the compositor starting a DnD). Never initiated headless, so
/// the default (empty) handler is correct.
impl ServerDndGrabHandler for HlState {}

/// Server-side handling of `wl_data_device_manager` / `wl_data_device`. `data_device_state` hands
/// Smithay the held [`DataDeviceState`]; every other callback (DnD negotiation, selection transfer)
/// keeps its default, which is enough for a client to bind the manager, obtain a `wl_data_device` from
/// the seat, and set/observe a selection at the object level.
impl DataDeviceHandler for HlState {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device
    }
}

impl XdgShellHandler for HlState {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell
    }

    /// A toplevel mapped: assign the scene `Toplevel` role, send the initial configure (a floating size +
    /// `Activated` + output bounds) so the client draws its first frame. A headless single-window
    /// compositor grants keyboard focus to whatever maps, so the mapped toplevel is `Activated` — GTK/Qt
    /// gate their "focused" styling (and Chrome its window controls) on that state.
    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        if let Some(sid) = self.sid(surface.wl_surface()) {
            self.engine.scene.set_role(sid, SurfaceRole::Toplevel);
            self.reconcile_window(sid);
        }
        let bounds = self.engine.scene.output_logical_size();
        surface.with_pending_state(|s| {
            s.size = Some(INITIAL_TOPLEVEL_SIZE.into());
            // `configure_bounds` (xdg-shell v4+): the largest a client should size itself to. Toolkits
            // clamp their preferred size to this; without it a client can pick a size larger than the
            // output. Sourced from the scene's primary output logical size, same as maximize.
            s.bounds = Some(bounds.into());
            s.states.set(XdgToplevelState::Activated);
        });
        surface.send_configure();
    }

    fn title_changed(&mut self, surface: ToplevelSurface) {
        let Some(sid) = self.sid(surface.wl_surface()) else {
            return;
        };
        let title = with_states(surface.wl_surface(), |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .and_then(|data| data.lock().ok()?.title.clone())
                .unwrap_or_default()
        });
        if let Some(scene_surface) = self.engine.scene.get_mut(sid) {
            scene_surface.title.clone_from(&title);
        }
        self.reconcile_window(sid);
    }

    fn parent_changed(&mut self, surface: ToplevelSurface) {
        let Some(sid) = self.sid(surface.wl_surface()) else {
            return;
        };
        let parent = surface.parent().and_then(|parent| self.sid(&parent));
        if let Some(scene_surface) = self.engine.scene.get_mut(sid) {
            scene_surface.transient_parent = parent;
        }
        self.reconcile_window(sid);
    }

    /// A toplevel set its app id. Stored by smithay; accepted here (no launcher/grouping policy headless).
    fn app_id_changed(&mut self, _surface: ToplevelSurface) {}

    /// The client asked to be maximized. A headless compositor grants it against the primary output's
    /// logical size and reconfigures with the `Maximized` state (kept `Activated`) so the client redraws
    /// to fill the output and drops its resize affordances. `set_min_size`/`set_max_size` land in
    /// smithay's committed `SurfaceCachedState` automatically; they are not re-sent (they are client→server
    /// hints, not part of the configure).
    fn maximize_request(&mut self, surface: ToplevelSurface) {
        let (w, h) = self.engine.scene.output_logical_size();
        let titlebar_double_click = self.last_pointer_click_count >= 2;
        self.last_pointer_click_count = 1;
        let Some(sid) = self.sid(surface.wl_surface()) else {
            return;
        };
        if titlebar_double_click {
            self.host_fullscreen.remove(&sid);
        } else {
            self.host_fullscreen.insert(sid);
        }
        surface.with_pending_state(|s| {
            s.size = Some((w, h).into());
            // Both variants use the maximized guest layout so GTK retains its client-side header.
            // `host_fullscreen` independently chooses a native Space for the single-click control.
            s.states.set(XdgToplevelState::Maximized);
            s.states.unset(XdgToplevelState::Fullscreen);
            s.states.set(XdgToplevelState::Activated);
        });
        surface.send_configure();
    }

    /// The client asked to leave maximized: drop the state and return to the floating size.
    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        if let Some(sid) = self.sid(surface.wl_surface()) {
            self.host_fullscreen.remove(&sid);
        }
        surface.with_pending_state(|s| {
            s.states.unset(XdgToplevelState::Maximized);
            // A zero-size configure lets the client restore its own last floating size. Forcing the
            // compositor's initial 800x600 here discards the user's/app's pre-full-screen geometry.
            s.size = None;
        });
        surface.send_configure();
    }

    /// The client asked for fullscreen. Grant it at the output's logical size with the `Fullscreen` state
    /// (the headless compositor has one output; the requested `output` hint is not needed).
    fn fullscreen_request(
        &mut self,
        surface: ToplevelSurface,
        _output: Option<smithay::reexports::wayland_server::protocol::wl_output::WlOutput>,
    ) {
        let (w, h) = self.engine.scene.output_logical_size();
        surface.with_pending_state(|s| {
            s.size = Some((w, h).into());
            s.states.set(XdgToplevelState::Fullscreen);
            s.states.set(XdgToplevelState::Activated);
        });
        surface.send_configure();
    }

    /// The client asked to leave fullscreen: drop the state and return to the floating size.
    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        if let Some(sid) = self.sid(surface.wl_surface()) {
            self.host_fullscreen.remove(&sid);
        }
        surface.with_pending_state(|s| {
            s.states.unset(XdgToplevelState::Fullscreen);
            s.size = Some(INITIAL_TOPLEVEL_SIZE.into());
        });
        surface.send_configure();
    }

    /// `xdg_toplevel.move` — the client asked to start an interactive, pointer-driven window MOVE (dragging
    /// its titlebar). HONEST INTENTIONAL NO-OP: the neutral headless scene models no global window position
    /// (every toplevel roots its own tree at `(0, 0)` — there is no desktop plane to slide a window across)
    /// and there is no live user drag to track. The request carries no reply event, so nothing is acked or
    /// faked; a real on-screen compositor would begin a move grab here.
    fn move_request(&mut self, surface: ToplevelSurface, _seat: WlSeat, _serial: Serial) {
        if self.last_pointer_click_count >= 2 {
            self.last_pointer_click_count = 1;
            let Some(sid) = self.sid(surface.wl_surface()) else {
                return;
            };
            self.host_fullscreen.remove(&sid);
            let (width, height) = self.engine.scene.output_logical_size();
            surface.with_pending_state(|state| {
                state.size = Some((width, height).into());
                state.states.set(XdgToplevelState::Maximized);
                state.states.unset(XdgToplevelState::Fullscreen);
                state.states.set(XdgToplevelState::Activated);
            });
            surface.send_configure();
            return;
        }
        if let Some(sid) = self.sid(surface.wl_surface()) {
            self.engine
                .presenter_mut()
                .begin_interaction(sid, crate::scene::model::WindowInteraction::Move);
        }
    }

    /// `xdg_toplevel.resize` — the client asked to start an interactive, pointer-driven RESIZE (dragging a
    /// window edge). HONEST INTENTIONAL NO-OP for the same reason as [`Self::move_request`]: an interactive
    /// resize is driven by a live user drag the headless compositor has no input source for, and it carries
    /// no reply event. (Programmatic sizing IS honored — `maximize_request` / `fullscreen_request` send real
    /// `xdg_toplevel.configure`s with a new size.)
    fn resize_request(
        &mut self,
        _surface: ToplevelSurface,
        _seat: WlSeat,
        _serial: Serial,
        _edges: smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::ResizeEdge,
    ) {
    }

    /// Forward client-side minimize controls to the host presenter. Headless presenters intentionally
    /// ignore this port; native presenters map it to their platform's taskbar/dock operation.
    fn minimize_request(&mut self, surface: ToplevelSurface) {
        let Some(root) = self.sid(surface.wl_surface()) else {
            return;
        };
        let mut windows = vec![root];
        windows.extend(
            self.engine
                .scene
                .collect_popups_for_root(root)
                .into_iter()
                .map(|(popup, _, _)| popup),
        );
        for window in windows {
            self.engine
                .scene
                .set_visibility(window, Visibility::Minimized);
            self.reconcile_window(window);
        }
        if self.engine.scene.seat().keyboard_focus == Some(root) {
            self.set_keyboard_focus(None);
        }
    }

    /// `xdg_toplevel.show_window_menu` — the client asked the compositor to show ITS server-side window
    /// menu (maximize/minimize/close/…). HONEST INTENTIONAL NO-OP: this compositor draws no server-side
    /// decorations or menus (it composites the client buffer verbatim — see [`XdgDecorationHandler`]), so
    /// there is no menu to show. The request carries no reply event, so nothing is acked or faked.
    fn show_window_menu(
        &mut self,
        _surface: ToplevelSurface,
        _seat: WlSeat,
        _serial: Serial,
        _location: Point<i32, Logical>,
    ) {
    }

    /// An `xdg_popup` mapped (`xdg_surface.get_popup(parent, positioner)`): a menu / dropdown / combo-box
    /// list / tooltip / context menu. Resolve the client's `xdg_positioner` to a concrete on-screen
    /// geometry via the scene's placement math (anchor rect → anchor point → gravity → offset, then the
    /// flip/slide/resize constraint adjustment against the output area), register the popup in the scene's
    /// popup registry linked to its parent (another popup for a submenu chain, or the owning toplevel), and
    /// complete the initial handshake with `xdg_popup.configure(x,y,w,h)` + the paired
    /// `xdg_surface.configure(serial)`. The popup's committed buffer then routes into the scene through the
    /// ordinary commit path: `window_root` climbs it to its toplevel, whose present composites every popup
    /// in its tree at the resolved offset (`Scene::collect_popups_for_root`).
    fn new_popup(&mut self, surface: PopupSurface, positioner: PositionerState) {
        let neutral = Positioner::from(&positioner);
        let parent = surface.get_parent_surface().and_then(|p| self.sid(&p));
        let geometry = parent
            .map(|parent| {
                self.engine
                    .scene
                    .constrain_popup_for_parent(parent, &neutral)
            })
            .unwrap_or_default();
        // Link the scene popup to its parent (toplevel or parent popup). Without a mapped parent we still
        // configure the client, but it cannot composite until its parent exists.
        if let (Some(sid), Some(parent)) = (self.sid(surface.wl_surface()), parent) {
            self.engine.scene.set_role(
                sid,
                SurfaceRole::Popup(PopupState {
                    parent,
                    positioner: neutral,
                    geometry,
                    grabbed: false,
                }),
            );
            self.reconcile_window(sid);
        }
        // Tell the client where it was placed (Smithay emits `xdg_popup.configure` from this pending
        // geometry, paired with `xdg_surface.configure`). MUST precede the client's first buffer attach.
        surface.with_pending_state(|state| {
            state.positioner = positioner;
            state.geometry = Rectangle::new(
                (geometry.x, geometry.y).into(),
                (geometry.w, geometry.h).into(),
            );
        });
        surface.send_configure().ok();
    }

    /// `xdg_popup.grab(seat, serial)`: the client takes an explicit popup grab (menus / context menus do;
    /// tooltips do not). Record the popup in the grab chain and flag it in the scene so a press outside the
    /// chain dismisses it (see [`HlState::inject_pointer_button`] → [`HlState::dismiss_popup_grabs`]). The
    /// chain is ordered outer → inner, so a submenu opened under an existing grab extends it.
    fn grab(&mut self, surface: PopupSurface, _seat: WlSeat, _serial: Serial) {
        if let Some(sid) = self.sid(surface.wl_surface()) {
            if let Some(SurfaceRole::Popup(p)) = self.engine.scene.get_mut(sid).map(|s| &mut s.role)
            {
                p.grabbed = true;
            }
        }
        if !self
            .popup_grabs
            .iter()
            .any(|p| p.wl_surface() == surface.wl_surface())
        {
            self.popup_grabs.push(surface);
        }
    }

    /// `xdg_popup.reposition(positioner, token)` (xdg-shell v3): a mapped popup is re-anchored (e.g. a menu
    /// re-placing as the pointer walks a menu bar). Recompute the geometry from the NEW positioner, update
    /// the scene popup in place, and answer `xdg_popup.repositioned(token)` (which also emits the fresh
    /// configure/ack). The scene composites at the new offset once the client acks and re-commits.
    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        let neutral = Positioner::from(&positioner);
        let sid = self.sid(surface.wl_surface());
        let geometry = sid
            .and_then(|sid| self.engine.scene.popup_parent(sid))
            .map(|parent| {
                self.engine
                    .scene
                    .constrain_popup_for_parent(parent, &neutral)
            })
            .unwrap_or_default();
        if let Some(sid) = sid {
            if let Some(SurfaceRole::Popup(p)) = self.engine.scene.get_mut(sid).map(|s| &mut s.role)
            {
                p.positioner = neutral;
                p.geometry = geometry;
            }
            self.reconcile_window(sid);
        }
        surface.with_pending_state(|state| {
            state.positioner = positioner;
            state.geometry = Rectangle::new(
                (geometry.x, geometry.y).into(),
                (geometry.w, geometry.h).into(),
            );
        });
        surface.send_repositioned(token);
    }

    /// A popup's role was destroyed (the client tore the menu/tooltip down, or honoured a grab dismissal).
    /// Drop it from the grab chain; the scene surface + popup-registry entry are reclaimed when its
    /// `wl_surface` is destroyed (`teardown_surface`), and the owning toplevel re-presents on its next
    /// commit so the menu visibly disappears.
    fn popup_destroyed(&mut self, surface: PopupSurface) {
        self.popup_grabs
            .retain(|p| p.wl_surface() != surface.wl_surface());
        if let Some(sid) = self.sid(surface.wl_surface()) {
            let root = self.engine.scene.window_root(sid);
            // Destroying xdg_popup unmaps the transient even when the client keeps the underlying
            // wl_surface object alive briefly. AppKit child windows retain their children, so waiting for
            // wl_surface.destroy leaves a visually stuck menu across navigation changes.
            self.engine.presenter_mut().destroy_window(sid);
            self.engine.scene.clear_role(sid);
            if let Some(root) = root {
                self.engine.scene.mark_dirty(root);
                self.arm_repaint(root);
            }
        }
    }
}

/// Server-side handling of `zxdg_decoration_manager_v1`. The negotiation contract: whenever a client
/// creates a decoration object or expresses a preference, it MUST receive a `configure(mode)` telling it
/// whether the compositor draws the frame (server-side) or the client must draw its own (client-side).
/// A toolkit that never hears back stalls before mapping. This headless compositor composites the client
/// buffer verbatim (it draws no frame of its own), so it honors the client's preferred mode when one is
/// given and defaults to server-side (i.e. "no client CSD needed") otherwise. Smithay's
/// `ToplevelSurface::send_configure` emits the `zxdg_toplevel_decoration_v1.configure` from the pending
/// `decoration_mode` for us — we only set the mode and configure.
impl XdgDecorationHandler for HlState {
    /// The client attached a decoration object (`get_toplevel_decoration`) without yet stating a
    /// preference: answer with the server-side default so it knows not to draw CSD.
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|s| {
            s.decoration_mode = Some(DecorationMode::ServerSide);
        });
        toplevel.send_configure();
    }

    /// The client stated a preferred mode (`set_mode`): honor it and re-configure. GTK/Chrome request
    /// `ClientSide` to draw their own titlebar; granting exactly what they ask avoids a mode fight (the
    /// double-decoration / no-decoration hangs a mismatched reply causes).
    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: DecorationMode) {
        toplevel.with_pending_state(|s| {
            s.decoration_mode = Some(mode);
        });
        toplevel.send_configure();
    }

    /// The client withdrew its preference (`unset_mode`): fall back to the server-side default.
    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|s| {
            s.decoration_mode = Some(DecorationMode::ServerSide);
        });
        toplevel.send_configure();
    }
}

/// A client bound `wl_output`. Smithay has already sent geometry/mode/scale/name/done. Surface→output
/// membership (`wl_surface.enter`/`leave`) is driven separately from commit (see
/// [`HlState::update_output_membership`]): a surface enters its SELECTED output, and a position-based route
/// (`InputCommand::MoveToplevelToPoint` → [`HlState::move_toplevel_to_point`]) moves it between the outputs
/// a multi-output `$HL_OUTPUTS` layout advertises. The default layout is a single output.
impl OutputHandler for HlState {}

/// A client created a `wp_fractional_scale_v1` for a surface. Tell it the compositor's preferred
/// fractional render scale so it can rasterize crisply on HiDPI without integer-only
/// `wl_surface.set_buffer_scale`. We source the scale from the primary output's scale (consistent with the
/// legacy integer `wl_output.scale`); smithay serializes it as `round(scale × 120)`.
impl FractionalScaleHandler for HlState {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        // Source the preferred scale from the surface's OWN output (its selected output, else the primary),
        // so a surface already routed to a HiDPI output learns the larger scale — not just the primary's.
        match self.sid(&surface) {
            Some(sid) => self.send_preferred_fractional_scale(sid),
            None => {
                let scale = self
                    .engine
                    .scene
                    .primary_output()
                    .map(|o| o.scale.max(1))
                    .unwrap_or(1) as f64;
                with_states(&surface, |states| {
                    with_fractional_scale(states, |fractional| {
                        fractional.set_preferred_scale(scale);
                    });
                });
            }
        }
    }
}

/// Server-side handling of `zwp_primary_selection_device_manager_v1` (the middle-click PRIMARY selection).
/// Hands Smithay the held [`PrimarySelectionState`]; the default selection transfer is enough for a client
/// to set a `zwp_primary_selection_source_v1` while focused and for the next focused client to read it over
/// a real fd — exactly like the data-device clipboard, but on the primary selection. Focus follows the
/// keyboard via [`set_primary_focus`] (see [`HlState::set_keyboard_focus`]). See the
/// `primary_selection_roundtrip` demo.
impl PrimarySelectionHandler for HlState {
    fn primary_selection_state(&self) -> &PrimarySelectionState {
        &self.primary_selection
    }
}

/// Server-side policy for `zwp_pointer_constraints_v1` (pointer lock / confinement). The compositor decides
/// WHEN a constraint engages; the headless policy is the standard one: activate it as soon as it is created
/// on a surface that currently holds pointer focus. Activation sends the client
/// `zwp_locked_pointer_v1.locked` / `zwp_confined_pointer_v1.confined`; a LOCKED pointer then stops
/// receiving absolute motion (see [`HlState::inject_pointer_motion`]). `cursor_position_hint` (the client's
/// rendered-cursor position while locked) needs no action headless — there is no hardware cursor to warp.
impl PointerConstraintsHandler for HlState {
    fn new_constraint(&mut self, surface: &WlSurface, pointer: &PointerHandle<Self>) {
        // Engage immediately if the constrained surface already holds pointer focus (the common case: a
        // client locks the pointer while the cursor is over it). Otherwise the constraint stays dormant
        // until the surface next gains focus — smithay re-checks activation there is out of scope headless,
        // so a client that constrains before entry re-issues after `wl_pointer.enter`.
        let focused = pointer.current_focus().is_some_and(|f| &f == surface);
        if focused {
            with_pointer_constraint(surface, pointer, |constraint| {
                if let Some(constraint) = constraint {
                    constraint.activate();
                }
            });
        }
    }

    fn cursor_position_hint(
        &mut self,
        _surface: &WlSurface,
        _pointer: &PointerHandle<Self>,
        _location: Point<f64, Logical>,
    ) {
    }
}

/// Server-side policy for `xdg_activation_v1` (cross-client activation / focus request). A client that
/// obtained an activation token (optionally carrying the seat+serial of the input event that triggered it)
/// calls `activate(token, surface)`; the headless single-window policy honours EVERY activation of a known
/// toplevel by granting it keyboard focus — the standard "bring the target to the front / make it active"
/// behaviour, observable to the client as a `wl_keyboard.enter` on the activated surface (and the clipboard /
/// primary selection follow, via [`HlState::set_keyboard_focus`]). `token_created` keeps its default (every
/// token is accepted); a real compositor might reject stale or seat-less tokens here. The token stays in the
/// pool after use — we do not `remove_token`, so it can be inspected — mirroring Smithay's contract that the
/// compositor owns token lifetime.
impl XdgActivationHandler for HlState {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self._xdg_activation
    }

    fn request_activation(
        &mut self,
        _token: XdgActivationToken,
        _token_data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        // Honour the activation by focusing the target toplevel. Only a known toplevel root is activated (a
        // popup/subsurface is not a focus target); an unknown or non-toplevel surface is ignored.
        let Some(sid) = self.sid(&surface) else {
            return;
        };
        let is_toplevel = matches!(
            self.engine.scene.get(sid).map(|s| &s.role),
            Some(SurfaceRole::Toplevel)
        );
        if is_toplevel {
            self.set_keyboard_focus(Some(sid));
        }
    }
}

/// Server-side handling of `zwp_idle_inhibit_manager_v1`. A client creating a `zwp_idle_inhibitor_v1` on a
/// surface asks the compositor to keep the system awake (no screensaver / DPMS) while that surface is
/// visible. There is no reply event — the compositor simply tracks it — so headless the handler records the
/// inhibited surface in the shared [`Observations`] (and drops it on the inhibitor's `destroy`), which is
/// the exact state a test asserts. A real host would additionally suppress its idle timer while the set is
/// non-empty and the surface is mapped.
impl IdleInhibitHandler for HlState {
    fn inhibit(&mut self, surface: WlSurface) {
        self.observations
            .lock()
            .unwrap()
            .idle_inhibited
            .insert(surface.id().protocol_id());
    }

    fn uninhibit(&mut self, surface: WlSurface) {
        self.observations
            .lock()
            .unwrap()
            .idle_inhibited
            .remove(&surface.id().protocol_id());
    }
}

/// Server-side policy for `zwp_keyboard_shortcuts_inhibit_v1` (key-grab). A client (a terminal, an
/// embedded VNC/RDP viewer, a game) that must receive EVERY key — including combos the compositor would
/// otherwise swallow as its own shortcuts — creates an inhibitor for its surface + this seat. Headless
/// there is no compositor shortcut table to suppress, so the policy is to ALWAYS grant: each new inhibitor
/// is immediately [`activate`](KeyboardShortcutsInhibitor::activate)d — which sends the client the `active`
/// event (the client-visible proof the grab took) and flips
/// [`Seat::keyboard_shortcuts_inhibited`](smithay::input::Seat) so a real shortcut handler could consult it
/// — and the inhibited surface is recorded into [`Observations`]. Destroying the inhibitor (or the client
/// vanishing) removes it again.
impl KeyboardShortcutsInhibitHandler for HlState {
    fn keyboard_shortcuts_inhibit_state(&mut self) -> &mut KeyboardShortcutsInhibitState {
        &mut self.keyboard_shortcuts_inhibit
    }

    fn new_inhibitor(&mut self, inhibitor: KeyboardShortcutsInhibitor) {
        // Grant the grab: activate it (delivers `active` to the client) and track the surface so a test can
        // assert BOTH the wire event and the server-side record.
        inhibitor.activate();
        let sid = inhibitor.wl_surface().id().protocol_id();
        hl_debug!(
            tag::WAYLAND,
            "zwp_keyboard_shortcuts_inhibit: activated for surface {sid}"
        );
        self.observations
            .lock()
            .unwrap()
            .shortcuts_inhibited
            .insert(sid);
    }

    fn inhibitor_destroyed(&mut self, inhibitor: KeyboardShortcutsInhibitor) {
        let sid = inhibitor.wl_surface().id().protocol_id();
        self.observations
            .lock()
            .unwrap()
            .shortcuts_inhibited
            .remove(&sid);
    }
}

/// Compositor-side hooks for `zwp_input_method_v2` popups (the candidate-list window an IME draws near the
/// cursor). Headless there is no on-screen IME popup surface to place or composite — the text-input round
/// trip a test drives (preedit/commit) needs no popup — so `parent_geometry` reports a zero rectangle and
/// the popup lifecycle callbacks are honest no-ops. The text-input DELIVERY path (enter + commit/preedit +
/// done) does not depend on any of these; they exist only so `delegate_input_method_manager!` can bind.
impl InputMethodHandler for HlState {
    fn new_popup(&mut self, _surface: ImePopupSurface) {}
    fn dismiss_popup(&mut self, _surface: ImePopupSurface) {}
    fn popup_repositioned(&mut self, _surface: ImePopupSurface) {}
    fn parent_geometry(&self, _parent: &WlSurface) -> Rectangle<i32, Logical> {
        Rectangle::default()
    }
}

/// Server-side policy for `zwp_tablet_manager_v2`. The single advertised tablet + pen tool live on the
/// seat's tablet-seat (added in [`HlState::new`]); a client that binds `get_tablet_seat` receives them and
/// the host stylus seam drives the tool. `tablet_tool_image` (the client asking the compositor to set a
/// hardware cursor for the tool) keeps its default no-op — headless there is no hardware cursor to warp.
impl TabletSeatHandler for HlState {}

/// Server-side handling of `ext_session_lock_manager_v1` (screen lock). A client's `lock` request lands in
/// [`Self::lock`]: the compositor HIDES every normal toplevel (so their content stops presenting — the
/// screen "blanks") and confirms the lock with [`SessionLocker::lock`], which sends the client the `locked`
/// event. The client then creates a lock surface per output ([`Self::new_surface`]); the adapter gives it a
/// toplevel role so its committed buffer composites + presents through the ordinary path, and configures it
/// to the output size. `unlock` restores every normal toplevel to visible and re-presents it. The lock/unlock
/// transition is mirrored into [`Observations::session_locked`](super::present::Observations) for the test.
impl SessionLockHandler for HlState {
    fn lock_state(&mut self) -> &mut SessionLockManagerState {
        &mut self._session_lock
    }

    fn lock(&mut self, confirmation: SessionLocker) {
        // Hide the normal surfaces first, THEN confirm — the client must not observe `locked` before the
        // compositor has actually stopped presenting protected content.
        self.set_session_locked(true);
        confirmation.lock();
    }

    fn unlock(&mut self) {
        self.set_session_locked(false);
    }

    fn new_surface(&mut self, surface: LockSurface, _output: WlOutput) {
        // Give the lock surface a toplevel role so its committed buffer composes + presents as a window
        // root (the neutral scene has no dedicated lock layer; a full-output toplevel is the faithful
        // reduction). Track it so `set_session_locked` never occludes it.
        if let Some(sid) = self.sid(surface.wl_surface()) {
            self.engine.scene.set_role(sid, SurfaceRole::Toplevel);
            self.engine.scene.set_visibility(sid, Visibility::Visible);
            self.reconcile_window(sid);
            if !self.lock_surfaces.contains(&sid) {
                self.lock_surfaces.push(sid);
            }
            // The lock surface takes keyboard focus (a real lock screen receives the unlock passphrase).
            self.set_keyboard_focus(Some(sid));
        }
        // Configure it to the output's logical size, as the protocol requires, so the client draws.
        let (w, h) = self.engine.scene.output_logical_size();
        surface.with_pending_state(|state| {
            state.size = Some((w.max(1) as u32, h.max(1) as u32).into());
        });
        surface.send_configure();
    }
}

smithay::delegate_pointer_gestures!(HlState);
smithay::delegate_tablet_manager!(HlState);
smithay::delegate_session_lock!(HlState);
smithay::delegate_text_input_manager!(HlState);
smithay::delegate_input_method_manager!(HlState);
smithay::delegate_compositor!(HlState);
smithay::delegate_shm!(HlState);
smithay::delegate_dmabuf!(HlState);
smithay::delegate_xdg_shell!(HlState);
smithay::delegate_xdg_decoration!(HlState);
smithay::delegate_output!(HlState);
smithay::delegate_seat!(HlState);
smithay::delegate_data_device!(HlState);
smithay::delegate_primary_selection!(HlState);
smithay::delegate_relative_pointer!(HlState);
smithay::delegate_pointer_constraints!(HlState);
smithay::delegate_presentation!(HlState);
smithay::delegate_viewporter!(HlState);
smithay::delegate_fractional_scale!(HlState);
smithay::delegate_xdg_activation!(HlState);
smithay::delegate_idle_inhibit!(HlState);
smithay::delegate_content_type!(HlState);
smithay::delegate_cursor_shape!(HlState);
smithay::delegate_single_pixel_buffer!(HlState);
smithay::delegate_keyboard_shortcuts_inhibit!(HlState);

// ===================== wp_tearing_control_v1 (staging) — hand-dispatched =====================
//
// Smithay 0.7 ships no handler for `wp_tearing_control_v1`, so its manager + per-surface object are
// dispatched here directly on `HlState` (the wayland-server `GlobalDispatch`/`Dispatch` impls), mirroring
// the shape of Smithay's own `content_type` handler. The per-surface presentation hint is double-buffered
// through the compositor's cached-state machinery (a `Cacheable`) so it applies at `wl_surface.commit` and
// reverts correctly — read back at commit by [`HlState::record_tearing_hint`]. The one-inhibitor-per-surface
// rule from the protocol is enforced with a surface-local attached flag (posting `tearing_control_exists`
// on a second `get_tearing_control`), and destroying the object resets the surface to `vsync` at the next
// commit, exactly as the spec requires.

/// The double-buffered `wp_tearing_control_v1` presentation hint (wire value: `0` vsync — do not tear /
/// `1` async — tearing allowed for lowest latency). Committed via the compositor's cached-state machinery so
/// it is applied at `wl_surface.commit` and read back at commit into [`Observations`] — the neutral
/// analogue of Smithay's `ContentTypeSurfaceCachedState`. Default `vsync` (no hint attached).
#[derive(Debug, Clone, Copy, Default)]
struct TearingControlCachedState {
    hint: u32,
}

impl Cacheable for TearingControlCachedState {
    fn commit(&mut self, _dh: &DisplayHandle) -> Self {
        *self
    }
    fn merge_into(self, into: &mut Self, _dh: &DisplayHandle) {
        *into = self;
    }
}

/// Surface-local flag: does this `wl_surface` already have a live `wp_tearing_control_v1`? Enforces the
/// protocol's one-object-per-surface rule (a second `get_tearing_control` is a `tearing_control_exists`
/// protocol error, not a silent overwrite). Set on create, cleared on the object's `destroy`.
#[derive(Debug, Default)]
struct TearingControlSurfaceData {
    attached: std::sync::atomic::AtomicBool,
}

/// User data of a `wp_tearing_control_v1` object — a weak handle to the `wl_surface` it controls, so
/// `set_presentation_hint` / `destroy` can find the surface whose cached hint to update.
#[derive(Debug)]
struct TearingControlUserData(Mutex<Weak<WlSurface>>);

impl TearingControlUserData {
    fn wl_surface(&self) -> Option<WlSurface> {
        self.0.lock().unwrap().upgrade().ok()
    }
}

impl GlobalDispatch<WpTearingControlManagerV1, ()> for HlState {
    fn bind(
        _state: &mut HlState,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<WpTearingControlManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, HlState>,
    ) {
        hl_debug!(tag::WAYLAND, "wp_tearing_control_manager_v1 bound");
        data_init.init(resource, ());
    }
}

impl Dispatch<WpTearingControlManagerV1, ()> for HlState {
    fn request(
        _state: &mut HlState,
        _client: &Client,
        manager: &WpTearingControlManagerV1,
        request: wp_tearing_control_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, HlState>,
    ) {
        use std::sync::atomic::Ordering;
        match request {
            wp_tearing_control_manager_v1::Request::GetTearingControl { id, surface } => {
                // Enforce one `wp_tearing_control_v1` per surface (protocol error otherwise).
                let already = with_states(&surface, |states| {
                    states
                        .data_map
                        .insert_if_missing_threadsafe(TearingControlSurfaceData::default);
                    let data = states.data_map.get::<TearingControlSurfaceData>().unwrap();
                    if data.attached.load(Ordering::Acquire) {
                        true
                    } else {
                        data.attached.store(true, Ordering::Release);
                        false
                    }
                });
                if already {
                    manager.post_error(
                        wp_tearing_control_manager_v1::Error::TearingControlExists,
                        "wl_surface already has a wp_tearing_control_v1",
                    );
                } else {
                    data_init.init(id, TearingControlUserData(Mutex::new(surface.downgrade())));
                }
            }
            wp_tearing_control_manager_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

impl Dispatch<WpTearingControlV1, TearingControlUserData> for HlState {
    fn request(
        _state: &mut HlState,
        _client: &Client,
        _resource: &WpTearingControlV1,
        request: wp_tearing_control_v1::Request,
        data: &TearingControlUserData,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, HlState>,
    ) {
        use std::sync::atomic::Ordering;
        match request {
            wp_tearing_control_v1::Request::SetPresentationHint { hint } => {
                let Some(surface) = data.wl_surface() else {
                    return;
                };
                // `async` = tearing allowed (wire 1); `vsync` (or any unknown value) = do not tear (wire 0).
                let value = match hint {
                    WEnum::Value(PresentationHint::Async) => 1,
                    _ => 0,
                };
                hl_debug!(
                    tag::WAYLAND,
                    "wp_tearing_control set_presentation_hint -> {}",
                    if value == 1 { "async" } else { "vsync" }
                );
                with_states(&surface, |states| {
                    states
                        .cached_state
                        .get::<TearingControlCachedState>()
                        .pending()
                        .hint = value;
                });
            }
            wp_tearing_control_v1::Request::Destroy => {
                // Destroying the object resets the surface to `vsync` at its next commit and frees the
                // per-surface slot so a fresh `wp_tearing_control_v1` may be created.
                if let Some(surface) = data.wl_surface() {
                    with_states(&surface, |states| {
                        states
                            .cached_state
                            .get::<TearingControlCachedState>()
                            .pending()
                            .hint = 0;
                        if let Some(sd) = states.data_map.get::<TearingControlSurfaceData>() {
                            sd.attached.store(false, Ordering::Release);
                        }
                    });
                }
            }
            _ => {}
        }
    }
}

/// Build one `wl_output` from a scene [`Output`], creating its global and pushing the current mode / scale
/// / transform / LAYOUT POSITION + preferred mode so a binding client receives geometry (position +
/// transform) + mode + scale + name + done consistent with what compose/present uses. Called once per
/// scene output so a multi-output layout advertises a distinct `wl_output` per monitor.
struct WaylandOutput<'a> {
    display: &'a DisplayHandle,
    scene: &'a Output,
}

impl<'a> WaylandOutput<'a> {
    fn new(display: &'a DisplayHandle, scene: &'a Output) -> Self {
        Self { display, scene }
    }

    fn build(self) -> (WlOutputHandle, GlobalId) {
        let scene = self.scene;
        // Values sourced from the scene so `wl_output` reports exactly what the scene composites onto.
        let name = scene.name.clone();
        let (mode_w, mode_h) = (scene.mode_w, scene.mode_h);
        let refresh_mhz = scene.refresh_mhz;
        let scale = scene.scale.max(1);
        let transform = Transform::from(scene.transform);

        // Physical size in mm assuming ~96 dpi (25.4 mm/inch) — a plausible value for toolkits that derive DPI
        // from it; the pixel mode + scale below are the load-bearing fidelity, not the millimetre size.
        let phys_w_mm = (mode_w as f64 / 96.0 * 25.4).round() as i32;
        let phys_h_mm = (mode_h as f64 / 96.0 * 25.4).round() as i32;

        let output = WlOutputHandle::new(
            name,
            PhysicalProperties {
                size: (phys_w_mm, phys_h_mm).into(),
                subpixel: Subpixel::Unknown,
                make: "hl".into(),
                model: "hl-virtual".into(),
            },
        );
        let global = output.create_global::<HlState>(self.display);

        // `refresh` on a smithay `Mode` is millihertz (same unit as the scene's `refresh_mhz`). The location is
        // the output's layout position — smithay reports it as `wl_output.geometry.x/y` and derives xdg-output's
        // `logical_position` from it, so a multi-output layout advertises each monitor at its own coordinates.
        let mode = OutputMode {
            size: (mode_w, mode_h).into(),
            refresh: refresh_mhz as i32,
        };
        output.change_current_state(
            Some(mode),
            Some(transform),
            Some(Scale::Integer(scale)),
            Some((scene.pos_x, scene.pos_y).into()),
        );
        output.set_preferred(mode);

        (output, global)
    }
}

/// Map a Smithay `xdg_positioner` [`PositionerState`] onto the neutral [`Positioner`] value type the
/// scene's `place_popup` resolves. A straight field/enum translation — the placement math itself
/// (anchor/gravity/offset + flip/slide/resize) lives in `scene::service::popup`, not here, so the neutral
/// core owns the policy and the adapter only decodes the wire.
impl From<&PositionerState> for Positioner {
    fn from(p: &PositionerState) -> Self {
        Self {
            anchor_rect: Rect::new(
                p.anchor_rect.loc.x,
                p.anchor_rect.loc.y,
                p.anchor_rect.size.w,
                p.anchor_rect.size.h,
            ),
            size: (p.rect_size.w, p.rect_size.h),
            anchor: p.anchor_edges.into(),
            gravity: p.gravity.into(),
            constraint_adjustment: p.constraint_adjustment.into(),
            offset: (p.offset.x, p.offset.y),
        }
    }
}

/// Translate the `xdg_positioner.set_anchor` edge onto the neutral [`Anchor`].
impl From<WireAnchor> for Anchor {
    fn from(a: WireAnchor) -> Self {
        match a {
            WireAnchor::None => Anchor::None,
            WireAnchor::Top => Anchor::Top,
            WireAnchor::Bottom => Anchor::Bottom,
            WireAnchor::Left => Anchor::Left,
            WireAnchor::Right => Anchor::Right,
            WireAnchor::TopLeft => Anchor::TopLeft,
            WireAnchor::BottomLeft => Anchor::BottomLeft,
            WireAnchor::TopRight => Anchor::TopRight,
            WireAnchor::BottomRight => Anchor::BottomRight,
            _ => Anchor::None,
        }
    }
}

/// Translate the `xdg_positioner.set_gravity` direction onto the neutral [`Gravity`].
impl From<WireGravity> for Gravity {
    fn from(g: WireGravity) -> Self {
        match g {
            WireGravity::None => Gravity::None,
            WireGravity::Top => Gravity::Top,
            WireGravity::Bottom => Gravity::Bottom,
            WireGravity::Left => Gravity::Left,
            WireGravity::Right => Gravity::Right,
            WireGravity::TopLeft => Gravity::TopLeft,
            WireGravity::BottomLeft => Gravity::BottomLeft,
            WireGravity::TopRight => Gravity::TopRight,
            WireGravity::BottomRight => Gravity::BottomRight,
            _ => Gravity::None,
        }
    }
}

/// Translate the `xdg_positioner.set_constraint_adjustment` bitmask onto the neutral per-axis
/// flip/slide/resize flags the scene applies in that order.
impl From<WireConstraint> for ConstraintAdjustment {
    fn from(c: WireConstraint) -> Self {
        Self {
            flip_x: c.contains(WireConstraint::FlipX),
            flip_y: c.contains(WireConstraint::FlipY),
            slide_x: c.contains(WireConstraint::SlideX),
            slide_y: c.contains(WireConstraint::SlideY),
            resize_x: c.contains(WireConstraint::ResizeX),
            resize_y: c.contains(WireConstraint::ResizeY),
        }
    }
}

/// Reduce a committed `wl_surface.set_input_region` into the neutral scene's single-[`Rect`] input region
/// (which gates pointer hit-testing in `surface_at`/`accepts_input_at`). `None` — the client never set a
/// region, or set it to null — means the WHOLE surface accepts input (the scene's `None`). A set region is
/// reduced to the bounding box of its ADDITIVE rectangles: EXACT for the common single-rect region a
/// toolkit sets (e.g. GTK excluding its CSD shadow from input), and a safe superset for a shaped one. An
/// EMPTY region (a client making a surface click-through) has no additive rects and reduces to a zero-area
/// `Rect`, which `accepts_input_at` rejects everywhere — so that surface correctly receives no pointer
/// input. Without this the request would be silently dropped and every surface would accept input over its
/// whole rectangle regardless of what the client requested.
struct Region<'a> {
    attributes: &'a Option<RegionAttributes>,
}

impl<'a> Region<'a> {
    fn new(attributes: &'a Option<RegionAttributes>) -> Self {
        Self { attributes }
    }

    fn input(&self) -> Option<Rect> {
        let attrs = self.attributes.as_ref()?;
        Some(Self::additive_bounds(attrs).unwrap_or(Rect::new(0, 0, 0, 0)))
    }

    /// Reduce a committed `wl_surface.set_opaque_region` into the neutral scene's single-[`Rect`] opaque region
    /// — CONSERVATIVELY, because it drives the occlusion present-skip (`is_tree_dirty` → `opaque_covers`) where
    /// OVER-claiming opacity could wrongly hide a surface below and drop its update. Only a region that is
    /// exactly one additive rectangle (the common case — a client marking its whole opaque window so the
    /// compositor may skip redundant work behind it) is trusted verbatim. Anything a single rect cannot model
    /// without over-claiming — a subtracted hole, or multiple disjoint rects — reduces to `None` (proves
    /// nothing opaque), so a present is never wrongly skipped. `None` in (unset) ⇒ `None` out (the whole
    /// surface may be transparent).
    fn opaque(&self) -> Option<Rect> {
        match self.attributes.as_ref()?.rects.as_slice() {
            [(RectangleKind::Add, r)] if r.size.w > 0 && r.size.h > 0 => {
                Some(Rect::new(r.loc.x, r.loc.y, r.size.w, r.size.h))
            }
            _ => None,
        }
    }

    /// The bounding box of a region's ADDITIVE (`Add`) rectangles, or `None` if it has none. Subtract rects and
    /// degenerate (zero-area) rects are ignored — a single `Rect` cannot model a hole, and the resulting
    /// superset is the SAFE direction for an input region (over-accepting input is a hint, never a correctness
    /// hazard, unlike over-claiming opacity — see [`map_input_region`] vs [`map_opaque_region`]).
    fn additive_bounds(attrs: &RegionAttributes) -> Option<Rect> {
        // (min_x, min_y, max_right, max_bottom) accumulated over the additive rects.
        let mut bounds: Option<(i32, i32, i32, i32)> = None;
        for (kind, r) in &attrs.rects {
            if !matches!(kind, RectangleKind::Add) || r.size.w <= 0 || r.size.h <= 0 {
                continue;
            }
            let (x0, y0, x1, y1) = (r.loc.x, r.loc.y, r.loc.x + r.size.w, r.loc.y + r.size.h);
            bounds = Some(match bounds {
                Some((mx, my, mr, mb)) => (mx.min(x0), my.min(y0), mr.max(x1), mb.max(y1)),
                None => (x0, y0, x1, y1),
            });
        }
        bounds.map(|(mx, my, mr, mb)| Rect::new(mx, my, mr - mx, mb - my))
    }
}

/// Map Smithay's `wl_output::Transform` (the wire enum `wl_surface.set_buffer_transform` speaks) onto the
/// neutral [`BufferTransform`]. A straight enum translation; the rotation/flip math itself lives in the
/// neutral `BufferTransform` (dimension swap) and the presenter (pixel remap), not here.
impl From<smithay::reexports::wayland_server::protocol::wl_output::Transform> for BufferTransform {
    fn from(t: smithay::reexports::wayland_server::protocol::wl_output::Transform) -> Self {
        use smithay::reexports::wayland_server::protocol::wl_output::Transform as WlT;
        match t {
            WlT::Normal => BufferTransform::Normal,
            WlT::_90 => BufferTransform::_90,
            WlT::_180 => BufferTransform::_180,
            WlT::_270 => BufferTransform::_270,
            WlT::Flipped => BufferTransform::Flipped,
            WlT::Flipped90 => BufferTransform::Flipped90,
            WlT::Flipped180 => BufferTransform::Flipped180,
            WlT::Flipped270 => BufferTransform::Flipped270,
            _ => BufferTransform::Normal,
        }
    }
}

/// The advertised output transform, from `$HL_OUTPUT_TRANSFORM` (default `Normal`). Accepts the
/// `wl_output.transform` names: `normal`, `90`, `180`, `270`, `flipped`, `flipped-90`, `flipped-180`,
/// `flipped-270` (also `flipped90` etc.). An unknown value falls back to `Normal`. This is the seam the
/// `output_transform_geometry` demo uses to stand the compositor up on a rotated output.
fn env_output_transform() -> BufferTransform {
    let raw = match std::env::var("HL_OUTPUT_TRANSFORM") {
        Ok(v) => v,
        Err(_) => return BufferTransform::Normal,
    };
    match raw.trim().to_ascii_lowercase().replace('_', "-").as_str() {
        "" | "normal" | "0" => BufferTransform::Normal,
        "90" => BufferTransform::_90,
        "180" => BufferTransform::_180,
        "270" => BufferTransform::_270,
        "flipped" | "flipped-0" => BufferTransform::Flipped,
        "flipped-90" => BufferTransform::Flipped90,
        "flipped-180" => BufferTransform::Flipped180,
        "flipped-270" => BufferTransform::Flipped270,
        other => {
            eprintln!("hl-compositor: unknown HL_OUTPUT_TRANSFORM {other:?}, using Normal");
            BufferTransform::Normal
        }
    }
}

/// The scene's output layout, from `$HL_OUTPUTS` (default: one output).
///
/// Unset (the default): a single `1920×1080@60` output "HL-0" at `(0, 0)`, carrying the advertised
/// `wl_output.transform` from `$HL_OUTPUT_TRANSFORM` — byte-for-byte the pre-multi-output behaviour, so
/// every existing single-output demo is unaffected.
///
/// Set: a `;`-separated list of output specs, each `WxH@X,Y[*S]` — pixel mode `W×H`, layout position
/// `(X, Y)`, optional integer scale `S` (default 1). Refresh comes from
/// `$HL_OUTPUT_REFRESH_MHZ` and defaults to 60 Hz. Outputs are numbered
/// `HL-0`, `HL-1`, … with ids `1, 2, …`; the FIRST is the primary (new surfaces enter it). Example:
/// `HL_OUTPUTS="1920x1080@0,0;2560x1440@1920,0*2"` — a scale-1 1080p output beside a scale-2 1440p one.
/// A malformed spec is skipped with a warning; if nothing parses, the single default is used.
fn env_outputs() -> Vec<Output> {
    let refresh_mhz = std::env::var("HL_OUTPUT_REFRESH_MHZ")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(60_000);
    let raw = match std::env::var("HL_OUTPUTS") {
        Ok(v) if !v.trim().is_empty() => v,
        _ => {
            return vec![Output::new(OutputId(1), "HL-0", 1920, 1080, refresh_mhz)
                .with_transform(env_output_transform())];
        }
    };

    let mut outputs = Vec::new();
    for (i, spec) in raw
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .enumerate()
    {
        match parse_output_spec(spec, i as u32, refresh_mhz) {
            Some(o) => outputs.push(o),
            None => eprintln!("hl-compositor: ignoring malformed HL_OUTPUTS spec {spec:?}"),
        }
    }
    if outputs.is_empty() {
        eprintln!("hl-compositor: HL_OUTPUTS parsed no outputs, using the single default");
        return vec![Output::new(OutputId(1), "HL-0", 1920, 1080, refresh_mhz)
            .with_transform(env_output_transform())];
    }
    outputs
}

/// Parse one `$HL_OUTPUTS` spec `WxH@X,Y[*S]` into an [`Output`] with id/name index `i` (0 → `HL-0`,
/// id `1`). Returns `None` on any malformed field.
fn parse_output_spec(spec: &str, i: u32, refresh_mhz: i64) -> Option<Output> {
    // Split off an optional `*scale` suffix first.
    let (geom, scale) = match spec.split_once('*') {
        Some((g, s)) => (g, s.trim().parse::<i32>().ok().filter(|&s| s > 0)?),
        None => (spec, 1),
    };
    // `WxH@X,Y` — the `@X,Y` position is optional (defaults to origin).
    let (mode, pos) = match geom.split_once('@') {
        Some((m, p)) => (m, Some(p)),
        None => (geom, None),
    };
    let (w, h) = mode.trim().split_once('x')?;
    let (w, h) = (w.trim().parse::<i32>().ok()?, h.trim().parse::<i32>().ok()?);
    if w <= 0 || h <= 0 {
        return None;
    }
    let (x, y) = match pos {
        Some(p) => {
            let (x, y) = p.trim().split_once(',')?;
            (x.trim().parse::<i32>().ok()?, y.trim().parse::<i32>().ok()?)
        }
        None => (0, 0),
    };
    Some(
        Output::new(OutputId(i + 1), format!("HL-{i}"), w, h, refresh_mhz)
            .with_position(x, y)
            .with_scale(scale),
    )
}

/// Map the neutral [`BufferTransform`] onto Smithay's `utils::Transform` (what a `wl_output` advertises).
/// The inverse of [`map_buffer_transform`], used to drive the output's advertised `wl_output.transform`.
impl From<BufferTransform> for Transform {
    fn from(t: BufferTransform) -> Self {
        match t {
            BufferTransform::Normal => Transform::Normal,
            BufferTransform::_90 => Transform::_90,
            BufferTransform::_180 => Transform::_180,
            BufferTransform::_270 => Transform::_270,
            BufferTransform::Flipped => Transform::Flipped,
            BufferTransform::Flipped90 => Transform::Flipped90,
            BufferTransform::Flipped180 => Transform::Flipped180,
            BufferTransform::Flipped270 => Transform::Flipped270,
        }
    }
}

/// Read a `wl_shm` buffer's pixels into tight top-left RGBA8888 plus its neutral [`Format`].
///
/// The four supported 32-bit little-endian formats differ only in channel order and whether the 4th
/// byte is alpha or ignored (opaque):
///   * `Argb8888` → memory `[B, G, R, A]`, alpha honoured.
///   * `Xrgb8888` → memory `[B, G, R, X]`, opaque (alpha forced to 255).
///   * `Abgr8888` → memory `[R, G, B, A]`, alpha honoured (R/B swapped vs ARGB).
///   * `Xbgr8888` → memory `[R, G, B, X]`, opaque.
/// This unpacks any of them to tight `[R, G, B, A]`. A bounds check refuses a malformed geometry that
/// would read past the mapping. Any other advertised/unknown format is treated as `Argb8888`.
struct BufferReader<'a> {
    buffer: &'a WlBuffer,
}

impl<'a> BufferReader<'a> {
    fn new(buffer: &'a WlBuffer) -> Self {
        Self { buffer }
    }

    fn shm_rgba(&self) -> Option<(StoredBuffer, Format)> {
        let result = with_buffer_contents(self.buffer, |ptr, len, data| {
            let (w, h, stride, offset) = (data.width, data.height, data.stride, data.offset);
            // `w * 4` is computed in `i64` so a hostile width near `i32::MAX` (reachable with a large `wl_shm`
            // pool) cannot overflow the row-stride check itself before the geometry is rejected.
            if w <= 0 || h <= 0 || (stride as i64) < w as i64 * 4 || offset < 0 {
                return None;
            }
            // Highest byte read = offset + (h-1)*stride + w*4; must fit the mapping. All widened to `usize`
            // (each factor is now known positive) so the bound check can never overflow before it fires.
            let last_row = offset as usize + (h as usize - 1) * stride as usize;
            if last_row
                .checked_add(w as usize * 4)
                .map(|m| m > len)
                .unwrap_or(true)
            {
                return None;
            }
            // `format` is the neutral opaque/alpha distinction (drives blend); `swap_rb` selects channel
            // order; `has_alpha` whether the 4th byte is honoured or forced opaque.
            let (format, bgra, has_alpha) = match data.format {
                wl_shm::Format::Xrgb8888 => (Format::Xrgb8888, true, false),
                wl_shm::Format::Abgr8888 => (Format::Argb8888, false, true),
                wl_shm::Format::Xbgr8888 => (Format::Xrgb8888, false, false),
                // Argb8888 and any other advertised/unknown format fall through to ARGB semantics.
                _ => (Format::Argb8888, true, true),
            };
            // `w`/`h` are positive and `w·h·4 <= len` (the bound check above), so this `usize` product is
            // bounded by the mapping size and cannot overflow.
            let mut rgba = vec![0u8; w as usize * h as usize * 4];
            for y in 0..h {
                let row = offset as isize + y as isize * stride as isize;
                let di = y as usize * w as usize * 4;
                // Both supported channel orders already match a backend-native four-byte layout. Copy each
                // tight row wholesale; only XRGB/XBGR need a small alpha-fix pass. This replaces the former
                // scalar BGRA→RGBA conversion which macOS immediately reversed before upload.
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        ptr.offset(row),
                        rgba.as_mut_ptr().add(di),
                        w as usize * 4,
                    );
                }
                if !has_alpha {
                    for pixel in rgba[di..di + w as usize * 4].chunks_exact_mut(4) {
                        pixel[3] = 255;
                    }
                }
            }
            Some((
                StoredBuffer {
                    width: w,
                    height: h,
                    rgba,
                    bgra,
                    damage: None,
                },
                format,
            ))
        });
        result.ok().flatten()
    }

    /// Turn a committed `wp_single_pixel_buffer_v1` `wl_buffer` into a tight 1×1 top-left RGBA8888 pixel — the
    /// solid-color quad Chrome/Ozone + video players attach without a shm pool. The buffer carries only a
    /// 4-channel color (no pixels, no fd); [`get_single_pixel_buffer`] returns it and `rgba8888()` collapses the
    /// 32-bit-per-channel wire values to 8-bit R,G,B,A (already the byte order [`StoredBuffer`] stores). A
    /// 1×1 buffer composites like any other — a client that also attaches a `wp_viewport` dst scales it to fill
    /// its surface. Returns `None` for any non-single-pixel buffer (the caller reaches this only after the shm
    /// and dmabuf reads both returned `None`). The neutral [`Format`] is opaque `Xrgb8888` when the color is
    /// fully opaque, else alpha `Argb8888`, so the presenter blends a translucent single-pixel quad correctly.
    fn single_pixel_rgba(&self) -> Option<(StoredBuffer, Format)> {
        let data = get_single_pixel_buffer(self.buffer).ok()?;
        let rgba = data.rgba8888();
        let format = if data.has_alpha() {
            Format::Argb8888
        } else {
            Format::Xrgb8888
        };
        Some((
            StoredBuffer {
                width: 1,
                height: 1,
                rgba: rgba.to_vec(),
                bgra: false,
                damage: None,
            },
            format,
        ))
    }
}

/// The DRM format+modifier pairs the `zwp_linux_dmabuf_v1` global advertises AND the importer accepts:
/// single-plane **LINEAR** ARGB8888 + XRGB8888. LINEAR is the ONE modifier a software (no-GPU) presenter
/// can honestly import — the buffer is plain byte-linear memory the compositor `pread`s and unpacks like
/// shm. No tiled/vendor modifiers are advertised because there is no GPU here to detile them; a client
/// that needs those reads an empty tranche for them and falls back to `wl_shm`. The byte-swapped
/// ABGR/XBGR fourccs are additionally ACCEPTED at import (mirroring the shm read path) but not advertised,
/// since ARGB/XRGB is the universal pair GTK/Qt/Chrome negotiate.
fn dmabuf_formats() -> [DrmFormat; 2] {
    [
        DrmFormat {
            code: Fourcc::Argb8888,
            modifier: Modifier::Linear,
        },
        DrmFormat {
            code: Fourcc::Xrgb8888,
            modifier: Modifier::Linear,
        },
    ]
}

/// The device advertised by dmabuf feedback is the virtual render node supplied by the composed Husklet
/// display runtime. Its DRM identity is stable even though allocations are host-backed IOSurfaces.
const HUSKLET_RENDER_NODE: DmabufDeviceId = DmabufDeviceId::from_linux_dev_t((226u64 << 8) | 128);

/// Stand up a v4 dmabuf global for the composed host-GPU path. Clients use the feedback device to find
/// `/dev/dri/renderD128`; the engine implements that node's discovery and allocation ioctls, while this
/// compositor imports the resulting linear/IOSurface-backed buffers. Keeping the device identity here
/// aligned with the engine wire contract is required for Chromium's Wayland buffer manager.
struct DmabufAdapter<'a> {
    display: &'a DisplayHandle,
}

impl<'a> DmabufAdapter<'a> {
    fn new(display: &'a DisplayHandle) -> Self {
        Self { display }
    }

    fn state(&self) -> DmabufState {
        let mut state = DmabufState::new();
        match DmabufFeedbackBuilder::new(HUSKLET_RENDER_NODE, dmabuf_formats()).build() {
            Ok(feedback) => {
                let _global: DmabufGlobal =
                    state.create_global_with_default_feedback::<HlState>(self.display, &feedback);
            }
            Err(error) => {
                eprintln!(
                "hl-compositor: dmabuf feedback unavailable ({error}); advertising the v3 linear-import contract"
            );
                let _global: DmabufGlobal =
                    state.create_global::<HlState>(self.display, dmabuf_formats());
            }
        }
        state
    }
}

/// Read a committed **LINEAR** dmabuf `wl_buffer`'s pixels into tight top-left RGBA8888 by `pread`ing its
/// single plane fd — the genuine CPU import path for a byte-linear buffer on a software backend (no GPU
/// detile step). This is a REAL fd import: the bytes come off the client's plane fd, not a fabricated
/// copy. Returns `None` for a non-dmabuf buffer (the caller only reaches this after the shm read already
/// returned `None`), or for any dmabuf the importer would not have accepted (non-LINEAR
/// / multi-plane / unsupported fourcc / malformed geometry / backing too small). The four fourcc channel
/// orders map exactly as in [`read_shm_rgba`].
impl<'a> BufferReader<'a> {
    fn dmabuf_rgba(&self) -> Option<(StoredBuffer, Format)> {
        use std::os::unix::fs::FileExt;
        let dmabuf = get_dmabuf(self.buffer).ok()?;
        let drm = dmabuf.format();
        // Only the single-plane LINEAR buffers we advertised/accepted are CPU-importable here.
        if drm.modifier != Modifier::Linear || dmabuf.num_planes() != 1 {
            return None;
        }
        let (format, swap_rb, has_alpha) = match drm.code {
            Fourcc::Xrgb8888 => (Format::Xrgb8888, false, false),
            Fourcc::Abgr8888 => (Format::Argb8888, true, true),
            Fourcc::Xbgr8888 => (Format::Xrgb8888, true, false),
            Fourcc::Argb8888 => (Format::Argb8888, false, true),
            // An unsupported fourcc should never reach here (import rejects it), but be defensive.
            _ => return None,
        };
        let (w, h) = (dmabuf.width() as i32, dmabuf.height() as i32);
        let stride = dmabuf.strides().next()? as i64;
        let offset = dmabuf.offsets().next()? as i64;
        // Same geometry guard as the shm path (all widened to i64 so a hostile near-i32::MAX width cannot
        // overflow the row-stride check before it fires): reject a stride/offset that under-describes a row.
        if w <= 0 || h <= 0 || stride < w as i64 * 4 || offset < 0 {
            return None;
        }
        // Highest byte we will read = offset + (h-1)*stride + w*4; compute the read span with checked math so
        // an overflowing geometry is rejected (None) rather than panicking.
        let span = offset
            .checked_add((h as i64 - 1).checked_mul(stride)?)?
            .checked_add(w as i64 * 4)?;
        // Duplicate the BORROWED plane fd (the `Dmabuf` keeps ownership) and `read_at` the pixel region. A
        // LINEAR dmabuf's plane fd is a plain CPU-readable memory object (here backed by the client's own
        // file/memfd), so `read_exact_at` on the dup'd fd is the no-mmap equivalent of the shm mapping read;
        // a short/undersized backing makes `read_exact_at` fail, which we map to `None` (backing too small).
        let fd = dmabuf.handles().next()?;
        let file = std::fs::File::from(fd.try_clone_to_owned().ok()?);
        let read_len = (span - offset) as usize;
        let mut raw = vec![0u8; read_len];
        file.read_exact_at(&mut raw, offset as u64).ok()?;
        // `raw[0]` corresponds to file offset `offset`, so pixel (x, y) begins at `raw[y*stride + x*4]`.
        let mut rgba = vec![0u8; w as usize * h as usize * 4];
        for y in 0..h {
            let row = (y as i64 * stride) as usize;
            for x in 0..w {
                let si = row + (x as usize) * 4;
                let c0 = raw[si];
                let g = raw[si + 1];
                let c2 = raw[si + 2];
                let a = if has_alpha { raw[si + 3] } else { 255 };
                // ARGB memory is `[B, G, R, A]` (c0=B, c2=R); *BGR memory is `[R, G, B, A]` (c0=R, c2=B).
                let (r, b) = if swap_rb { (c0, c2) } else { (c2, c0) };
                let di = ((y * w + x) * 4) as usize;
                rgba[di] = r;
                rgba[di + 1] = g;
                rgba[di + 2] = b;
                rgba[di + 3] = a;
            }
        }
        Some((
            StoredBuffer {
                width: w,
                height: h,
                rgba,
                bgra: false,
                damage: None,
            },
            format,
        ))
    }
}

#[cfg(test)]
mod region_tests {
    //! Lock the `wl_surface.set_input_region` / `set_opaque_region` reduction from a Smithay
    //! `RegionAttributes` (union/difference of rects) onto the neutral scene's single-`Rect` model — the
    //! decode the commit path feeds into `commit_surface`, driving pointer hit-testing (input) and the
    //! occlusion present-skip (opaque). Pure logic, no Wayland socket.
    use super::*;
    use smithay::utils::{Logical, Rectangle};

    fn add(x: i32, y: i32, w: i32, h: i32) -> (RectangleKind, Rectangle<i32, Logical>) {
        (
            RectangleKind::Add,
            Rectangle::new((x, y).into(), (w, h).into()),
        )
    }
    fn subtract(x: i32, y: i32, w: i32, h: i32) -> (RectangleKind, Rectangle<i32, Logical>) {
        (
            RectangleKind::Subtract,
            Rectangle::new((x, y).into(), (w, h).into()),
        )
    }

    #[test]
    fn input_region_unset_means_whole_surface() {
        assert_eq!(Region::new(&None).input(), None);
    }

    #[test]
    fn input_region_single_rect_is_exact() {
        // The common case: a client restricts input to a sub-rectangle (e.g. its content minus CSD shadow).
        let region = RegionAttributes {
            rects: vec![add(100, 0, 100, 150)],
        };
        assert_eq!(
            Region::new(&Some(region)).input(),
            Some(Rect::new(100, 0, 100, 150))
        );
    }

    #[test]
    fn input_region_empty_is_click_through() {
        // A region object with NO rects => the surface accepts input NOWHERE (click-through overlay).
        let mapped = Region::new(&Some(RegionAttributes { rects: vec![] }))
            .input()
            .expect("a set region always maps to Some(rect)");
        assert!(
            mapped.is_empty(),
            "empty input region must reject all input"
        );
        assert!(!mapped.contains_point(0, 0));
    }

    #[test]
    fn input_region_multi_rect_is_superset_bounding_box() {
        // Two disjoint add rects reduce to their (safe, over-accepting) bounding box.
        let region = RegionAttributes {
            rects: vec![add(0, 0, 10, 10), add(90, 90, 10, 10)],
        };
        assert_eq!(
            Region::new(&Some(region)).input(),
            Some(Rect::new(0, 0, 100, 100))
        );
    }

    #[test]
    fn opaque_region_unset_is_none() {
        assert_eq!(Region::new(&None).opaque(), None);
    }

    #[test]
    fn opaque_region_single_rect_is_trusted() {
        let region = RegionAttributes {
            rects: vec![add(0, 0, 200, 150)],
        };
        assert_eq!(
            Region::new(&Some(region)).opaque(),
            Some(Rect::new(0, 0, 200, 150))
        );
    }

    #[test]
    fn opaque_region_with_hole_is_dropped_conservatively() {
        // A subtracted hole can't be a single opaque rect without over-claiming => prove nothing opaque.
        let region = RegionAttributes {
            rects: vec![add(0, 0, 200, 150), subtract(10, 10, 20, 20)],
        };
        assert_eq!(Region::new(&Some(region)).opaque(), None);
    }

    #[test]
    fn opaque_region_multi_rect_is_dropped_conservatively() {
        let region = RegionAttributes {
            rects: vec![add(0, 0, 10, 10), add(90, 90, 10, 10)],
        };
        assert_eq!(Region::new(&Some(region)).opaque(), None);
    }
}
