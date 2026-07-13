//! `dd-compositor` — a Smithay-native Wayland compositor for the dd host renderer.
//!
//! This crate is the flag-gated replacement for the ~4900-line hand-written protocol machine in
//! `dd-display/src/server.rs`. Instead of decoding every `wl_*` request by hand, it stands up
//! Smithay's maintained `wayland_frontend` state cores ([`CompositorState`], [`ShmState`],
//! [`XdgShellState`], [`SeatState`], [`OutputManagerState`], [`ViewporterState`],
//! [`PresentationState`]) and supplies only the handful of `Handler` callbacks a compositor must
//! implement. The `delegate_*!` macros generate the request dispatch.
//!
//! ## The platform seam is REUSED, not rewritten
//! The Cocoa/Metal window backend, the `Presenter` trait, and the XKB keymap all live in
//! `dd-display` and are consumed here unchanged. `commit()` pulls the committed `wl_shm` buffer,
//! repacks it into a [`dd_display::present::SurfaceBuffer`], and hands it to a boxed
//! [`dd_display::present::Presenter`] — exactly the seam `server.rs` uses. On macOS that Presenter is
//! `CocoaPresenter`/`MetalPresenter` (one NSWindow per surface, IOSurface/Metal present + HiDPI); on
//! the Linux dev host it is `PngPresenter`, so the whole path stays headless-testable. The Smithay
//! CORE here contains NO Cocoa/Metal — the platform swap lives entirely behind `Presenter`, matching
//! the "thin guest, fat native host; mac-first, all-platforms-eventually" steering.
//!
//! ## Module layout
//! The shared aggregate [`DdState`] lives here in `lib.rs`; the per-protocol `Handler` impls are split
//! into [`handlers`] submodules so successive waves of work (popup/subsurface, data-device, …) can own
//! a file without colliding:
//!   - [`handlers::compositor`] — `wl_compositor`/`wl_shm` commit → present path + buffer repack.
//!   - [`handlers::xdg`] — `xdg_shell` window management (configure/ack handshake, maximize/fullscreen,
//!     move/resize, min/max size, host-window-resize reflow).
//!   - [`handlers::seat`] — `wl_seat` input injection + `wp_cursor_shape` mapping.
//!   - [`handlers::output`] — `wl_output` / `xdg_output` + multi-output registration.
//!   - [`handlers::scale`] — `wp_fractional_scale_v1` preferred-scale policy (non-integer HiDPI).
//! Because Rust privacy grants descendant modules access to a parent module's private items, those
//! submodules read/write `DdState`'s private fields directly — no accessor churn.
//!
//! ## Native library requirement (libxkbcommon)
//! `smithay` links the system `libxkbcommon` unconditionally (it compiles the seat's XKB keymap at
//! runtime). It is NOT a Linux-only dependency — it builds on macOS (Homebrew / nixpkgs). For dev:
//!   build: `RUSTFLAGS="-L native=<libxkbcommon>/lib"`   run: `DYLD_LIBRARY_PATH="<libxkbcommon>/lib"`.
//! For a shipped `dd.app` this is the host-provides-everything model: bundle `libxkbcommon.dylib` in
//! `dd.app/Contents/Frameworks` and link with `-rpath @executable_path/../Frameworks` (or statically
//! link it). No guest/user install is ever required.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use dd_display::present::Presenter;

use smithay::{
    input::{
        keyboard::KeyboardHandle,
        pointer::PointerHandle,
        Seat, SeatState,
    },
    output::{Mode as OutMode, Output, PhysicalProperties, Scale, Subpixel},
    reexports::{
        wayland_protocols::xdg::shell::server::xdg_toplevel::WmCapabilities,
        wayland_server::{
            backend::{protocol::ProtocolError, ClientData, ClientId, DisconnectReason, GlobalId, ObjectId},
            protocol::{wl_buffer::WlBuffer, wl_callback::WlCallback, wl_surface::WlSurface},
            DisplayHandle, Resource,
        },
    },
    utils::{Serial, Size, SERIAL_COUNTER},
    wayland::{
        compositor::{CompositorClientState, CompositorState},
        content_type::ContentTypeState,
        cursor_shape::CursorShapeManagerState,
        dmabuf::DmabufState,
        fractional_scale::FractionalScaleManagerState,
        idle_inhibit::IdleInhibitManagerState,
        keyboard_shortcuts_inhibit::KeyboardShortcutsInhibitState,
        output::OutputManagerState,
        pointer_constraints::PointerConstraintsState,
        pointer_gestures::PointerGesturesState,
        presentation::PresentationState,
        relative_pointer::RelativePointerManagerState,
        selection::{data_device::DataDeviceState, primary_selection::PrimarySelectionState},
        shell::xdg::{decoration::XdgDecorationState, PopupSurface, XdgShellState},
        shm::ShmState,
        single_pixel_buffer::SinglePixelBufferState,
        tablet_manager::TabletManagerState,
        viewporter::ViewporterState,
        xdg_activation::XdgActivationState,
        xdg_foreign::XdgForeignState,
    },
};

/// GPU IR executor lifecycle (Phase 6.1–6.2): starts the dd-gpu executor for the Smithay path so
/// accelerated guests reach a host GPU backend, since the `DD_DISPLAY_SMITHAY=1` exec replaces
/// `dd-display` before it would have started the executor itself.
pub mod gpu;
pub mod handlers;

/// `wp_presentation` clock domain reported to the client. The `feedback.presented` timestamp is read
/// back by the GUEST via its own `clock_gettime()`, so this must be the value Linux libc uses for
/// `CLOCK_MONOTONIC` (== 1), NOT the host macOS libc value (== 6). Mirrors `server.rs`'s
/// `CLOCK_MONOTONIC_LINUX`; weston reports its `compositor->presentation_clock` the same way.
const CLOCK_MONOTONIC_LINUX: u32 = 1;

/// The single mode we advertise on `wl_output` (mHz). Also drives the `presented.refresh` interval.
pub(crate) const OUTPUT_REFRESH_MHZ: i64 = 60_000;

/// The floating (un-maximized) size a freshly mapped toplevel is configured to before it commits its
/// first buffer. Chrome/GTK draw into whatever size the initial configure carries.
const INITIAL_TOPLEVEL_SIZE: (i32, i32) = (1000, 700);

/// Per-client data Smithay hands back on every request. `CompositorClientState` is mandatory.
///
/// `disconnect_sink` is the seam that makes the Smithay client-disconnect callback observable to the
/// compositor: when the client's connection drops, [`ClientData::disconnected`] records the [`ClientId`]
/// here so [`DdState::drain_client_disconnects`] can run [`DdState::drop_client_gpu_state`] and reclaim
/// any client-owned executor allocations / in-flight GPU fences that were not tied to an individual
/// surface. Clients created with [`ClientState::default`] get a private (detached) sink — every surface
/// is still reclaimed individually through `CompositorHandler::destroyed` — so existing tests are
/// unaffected; the runtime wires [`DdState::new_client_state`] to share the compositor's sink.
#[derive(Default)]
pub struct ClientState {
    pub compositor: CompositorClientState,
    disconnect_sink: Arc<Mutex<Vec<ClientId>>>,
}

impl ClientState {
    /// Build a `ClientState` whose disconnect events flow into the compositor's shared `sink`.
    pub fn new(disconnect_sink: Arc<Mutex<Vec<ClientId>>>) -> ClientState {
        ClientState { compositor: CompositorClientState::default(), disconnect_sink }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RenderLimits {
    pub surfaces_per_client: usize,
    pub surfaces_global: usize,
    pub retained_callbacks_per_client: usize,
    pub retained_callbacks_global: usize,
    pub cpu_cache_bytes_per_client: usize,
    pub cpu_cache_bytes_global: usize,
    pub shm_pool_bytes_per_client: usize,
    pub shm_pool_bytes_global: usize,
    /// dmabuf plane file descriptors held open on behalf of a client's accelerated buffers.
    pub fds_per_client: usize,
    pub fds_global: usize,
    /// Accepted zero-copy dmabuf imports (each references a host IOSurface allocation).
    pub dmabuf_imports_per_client: usize,
    pub dmabuf_imports_global: usize,
    /// Native presenter windows (one host NSWindow / IOSurface-backed target per mapped surface).
    pub presenter_objects_per_client: usize,
    pub presenter_objects_global: usize,
    /// In-flight host-executor allocations (a GPU IOSurface use awaiting render/present completion).
    pub executor_allocations_per_client: usize,
    pub executor_allocations_global: usize,
}

impl Default for RenderLimits {
    fn default() -> Self {
        Self {
            surfaces_per_client: 1024,
            surfaces_global: 8192,
            retained_callbacks_per_client: 4096,
            retained_callbacks_global: 32768,
            cpu_cache_bytes_per_client: 256 * 1024 * 1024,
            cpu_cache_bytes_global: 1024 * 1024 * 1024,
            shm_pool_bytes_per_client: 256 * 1024 * 1024,
            shm_pool_bytes_global: 1024 * 1024 * 1024,
            fds_per_client: 4096,
            fds_global: 32768,
            dmabuf_imports_per_client: 1024,
            dmabuf_imports_global: 8192,
            presenter_objects_per_client: 1024,
            presenter_objects_global: 8192,
            executor_allocations_per_client: 1024,
            executor_allocations_global: 8192,
        }
    }
}

#[derive(Default, Clone, Copy)]
struct RenderUsage {
    surfaces: usize,
    retained_callbacks: usize,
    cpu_cache_bytes: usize,
    /// dmabuf plane file descriptors currently charged to this client.
    fds: usize,
    /// Accepted zero-copy dmabuf imports currently charged to this client.
    dmabuf_imports: usize,
    /// Native presenter objects (host windows) currently charged to this client.
    presenter_objects: usize,
    /// In-flight host-executor allocations currently charged to this client.
    executor_allocations: usize,
}

impl RenderUsage {
    /// Whether every charged dimension is zero — the condition for dropping the per-client record.
    fn is_empty(&self) -> bool {
        self.surfaces == 0
            && self.retained_callbacks == 0
            && self.cpu_cache_bytes == 0
            && self.fds == 0
            && self.dmabuf_imports == 0
            && self.presenter_objects == 0
            && self.executor_allocations == 0
    }
}

/// Public, totals-only snapshot of the per-client render-resource accounting. Ownership identities stay
/// private; this reports the whole compositor's charged resources across every dimension the per-client
/// [`RenderResourceQuota`] tracks, so a behavioral gate can prove that fds, dmabuf imports, presenter
/// objects, and executor allocations are charged and refunded alongside surfaces/callbacks/cache bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderBudgetTotals {
    pub surfaces: usize,
    pub retained_callbacks: usize,
    pub cpu_cache_bytes: usize,
    pub fds: usize,
    pub dmabuf_imports: usize,
    pub presenter_objects: usize,
    pub executor_allocations: usize,
}

/// The count-based render-resource dimensions charged with the shared atomic reserve/refund helpers
/// ([`DdState::charge_budget`] / [`DdState::refund_budget`]). Surfaces, retained callbacks, CPU cache
/// bytes, and shm-pool bytes keep their bespoke charge paths; these are the dimensions row 2's residual
/// added (fds, dmabuf imports, presenter objects, executor allocations).
#[derive(Clone, Copy, Debug)]
enum BudgetDim {
    Fds,
    DmabufImports,
    PresenterObjects,
    ExecutorAllocations,
}

impl BudgetDim {
    fn limits(self, l: &RenderLimits) -> (usize, usize) {
        match self {
            BudgetDim::Fds => (l.fds_per_client, l.fds_global),
            BudgetDim::DmabufImports => (l.dmabuf_imports_per_client, l.dmabuf_imports_global),
            BudgetDim::PresenterObjects => {
                (l.presenter_objects_per_client, l.presenter_objects_global)
            }
            BudgetDim::ExecutorAllocations => {
                (l.executor_allocations_per_client, l.executor_allocations_global)
            }
        }
    }
    fn slot(self, u: &mut RenderUsage) -> &mut usize {
        match self {
            BudgetDim::Fds => &mut u.fds,
            BudgetDim::DmabufImports => &mut u.dmabuf_imports,
            BudgetDim::PresenterObjects => &mut u.presenter_objects,
            BudgetDim::ExecutorAllocations => &mut u.executor_allocations,
        }
    }
}
impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    /// The client's connection dropped. Record its id so the compositor can reclaim any client-owned
    /// GPU/executor state on the next drain (per-surface state is also reclaimed through
    /// `CompositorHandler::destroyed`). Never panics on a poisoned lock — a disconnect must not abort.
    fn disconnected(&self, client_id: ClientId, _reason: DisconnectReason) {
        if let Ok(mut sink) = self.disconnect_sink.lock() {
            sink.push(client_id);
        }
    }
}

/// Aggregate compositor protocol state — the Smithay equivalent of `server.rs`'s `Server<P>` struct.
/// Smithay owns every per-protocol table; we hold the state handles plus the reused platform seam
/// (the boxed [`Presenter`]) and the small amount of compositor policy Smithay leaves to us
/// (window focus, per-surface titles, the pointer location).
pub struct DdState {
    pub dh: DisplayHandle,
    pub compositor: CompositorState,
    pub shm: ShmState,
    pub xdg_shell: XdgShellState,
    /// `zxdg_decoration_manager_v1`: server-side-vs-client-side decoration negotiation (see handlers/xdg.rs).
    pub xdg_decoration: XdgDecorationState,
    /// `xdg_activation_v1`: focus/raise-on-request via activation tokens (see handlers/xdg.rs).
    pub xdg_activation: XdgActivationState,
    pub seat_state: SeatState<Self>,
    pub output_manager: OutputManagerState,
    pub viewporter: ViewporterState,
    pub presentation: PresentationState,
    pub cursor_shape: CursorShapeManagerState,
    pub data_device: DataDeviceState,
    /// `zwp_linux_dmabuf_v1` delegate. Holds the dmabuf global that lets GPU clients (GLES/Vulkan)
    /// present via a dd IOSurface-backed buffer — see [`handlers::dmabuf`].
    pub dmabuf_state: DmabufState,
    /// `zwp_primary_selection_v1` — the X11-style primary/middle-click-paste selection (terminals, GTK/Qt
    /// apps). Guest↔guest transfer is driven entirely by Smithay through the shared [`SelectionHandler`];
    /// the compositor only follows keyboard focus with it (see [`DdState::focus_surface`]).
    pub primary_selection: PrimarySelectionState,
    /// `zwp_relative_pointer_v1` — unaccelerated relative motion deltas for games/3D (FPS mouselook). The
    /// global is advertised here; deltas are emitted through the pointer via [`DdState::relative_motion`].
    pub relative_pointer: RelativePointerManagerState,
    /// `zwp_pointer_constraints_v1` — pointer LOCK / CONFINE (FPS mouselook, drawing apps). The constraint
    /// is created per surface+pointer and activated by [`DdState::new_constraint`]; the injection path
    /// freezes the absolute pointer while a lock is active and clamps motion to a confine region.
    pub pointer_constraints: PointerConstraintsState,
    /// `wp_fractional_scale_manager_v1` — lets GTK/Qt/Chrome learn the non-integer buffer scale to render
    /// at (1.5×, 1.25×) for correct crisp output on scaled Retina modes; paired with `wp_viewporter`.
    /// Advertised here; the per-surface `preferred_scale` is pushed from [`handlers::scale`].
    pub fractional_scale: FractionalScaleManagerState,
    /// `wp_single_pixel_buffer_v1` — the 1×1 solid-color buffer optimization (backgrounds/solid surfaces).
    /// Smithay decodes the RGBA and stores it on the `wl_buffer`; the commit path reads it back.
    pub single_pixel_buffer: SinglePixelBufferState,

    // ---- Modern GUI protocol groups composed from the vendored Smithay tree (codex-rendering §5.2/§9.4) ----
    // Each state holds a global advertised behind DD_DISPLAY_SMITHAY (the whole binary is gated on it).
    // Policy + delegates live in the same-named `handlers::*` submodules. tearing-control is absent from
    // vendored smithay-0.7.0 and is therefore NOT composed.
    /// `zwp_pointer_gestures_v1` — touchpad swipe/pinch/hold (see [`handlers::pointer_gestures`]).
    pub pointer_gestures: PointerGesturesState,
    /// `zwp_tablet_manager_v2` — graphics-tablet/stylus (see [`handlers::tablet`]).
    pub tablet_manager: TabletManagerState,
    /// `zwp_idle_inhibit_manager_v1` — keep-session-awake intent (see [`handlers::idle_inhibit`]).
    pub idle_inhibit: IdleInhibitManagerState,
    /// `wp_content_type_manager_v1` — per-surface photo/video/game hint (see [`handlers::content_type`]).
    pub content_type: ContentTypeState,
    /// `zxdg_exporter_v2`/`zxdg_importer_v2` — cross-client toplevel parenting (see [`handlers::xdg_foreign`]).
    pub xdg_foreign: XdgForeignState,
    /// `zwp_keyboard_shortcuts_inhibit_manager_v1` — forward-all-keys (see [`handlers::keyboard_shortcuts_inhibit`]).
    pub keyboard_shortcuts_inhibit: KeyboardShortcutsInhibitState,
    /// Surfaces that currently hold a `zwp_idle_inhibitor_v1` (host records intent; keyed by surface id).
    pub(crate) idle_inhibitors: HashSet<u32>,
    /// Per-surface committed `wp_content_type` (sid → wire enum value: photo=1/video=2/game=3).
    pub(crate) content_types: HashMap<u32, u32>,
    /// `zwp_linux_explicit_synchronization_v1` — per-surface acquire/release fence contract for GPU
    /// clients (wait acquire before sampling, signal release after GPU completion). See
    /// [`handlers::explicit_sync`].
    pub(crate) explicit_sync: handlers::explicit_sync::ExplicitSyncState,
    /// `wp_color_manager_v1` — per-surface color descriptions + output color profile + gamma-correct
    /// linear-light conversion to the target output. See [`handlers::color`].
    pub(crate) color: handlers::color::ColorManagementState,

    pub seat: Seat<Self>,
    pub keyboard: KeyboardHandle<Self>,
    pub pointer: PointerHandle<Self>,
    pub output: Output,
    /// Additional outputs beyond the primary `output` (multi-monitor guests). Each has its own
    /// `wl_output` + `zxdg_output_v1` advertised by the shared [`OutputManagerState`]; registered via
    /// [`DdState::add_output`]. Empty in the single-output default — the state is not hard-wired to one.
    pub extra_outputs: Vec<Output>,
    pub(crate) output_globals: HashMap<String, GlobalId>,
    pub(crate) headless: bool,
    /// Selected output for each live surface/root. New surfaces start on the primary output.
    pub(crate) surface_outputs: HashMap<u32, Output>,

    /// `zwp_text_input_v3` — text-input for editors/address-bars/forms + the host IME (marked-text)
    /// bridge. The compositor IS the input method here (dd has no separate IME client), so it owns the
    /// text-input instances directly; see [`handlers::text_input`]. Text-input focus follows the keyboard
    /// focus via [`DdState::set_text_input_focus`].
    pub(crate) text_input: handlers::text_input::TextInputState,

    /// The reused platform present half (`CocoaPresenter`/`MetalPresenter` on macOS, `PngPresenter`
    /// headless). Keyed internally by surface id — the same `u32` sid model as `server.rs`.
    pub presenter: Box<dyn Presenter>,

    /// The surface that currently has keyboard focus (the most recently mapped toplevel).
    pub focus: Option<WlSurface>,
    /// Per-surface window titles, so the Presenter can label each NSWindow.
    pub titles: HashMap<u32, String>,
    /// Last pointer location in logical/point space (Cocoa delivers point-space coords).
    pub ptr_loc: (f64, f64),
    /// Recent input-event serials the compositor issued (pointer button / key presses), newest last,
    /// bounded. An `xdg_toplevel.move`/`resize` grab must echo the serial of the input event that began
    /// it (the implicit pointer-button grab), so [`DdState::is_recent_input_serial`] validates the
    /// request against this window — rejecting a client that tries to start a drag without a real user
    /// gesture. The seat input path records each press via [`DdState::note_input_serial`].
    pub(crate) recent_serials: VecDeque<Serial>,

    /// Last committed `wl_shm` buffer per surface (`sid` → buffer). A subsurface or popup that redraws
    /// only occasionally does not re-attach a buffer every parent frame, yet its pixels must persist in
    /// the composited window; Smithay clears `current().buffer` on a bufferless commit, so we hold the
    /// last one here and re-read it whenever the root window re-composites (mirrors `server.rs`'s
    /// per-surface `current_buffer`). Also lets a popup/child commit re-present its ROOT toplevel.
    pub(crate) buffers: HashMap<u32, WlBuffer>,
    /// Per-surface repacked tight-BGRA texture cache (`sid` → last uploaded pixels), the CPU half of
    /// damage tracking. Instead of re-repacking a surface's WHOLE `wl_shm` buffer on every commit, a
    /// commit with `wl_surface.damage`/`damage_buffer` copies only the changed rows into this persistent
    /// buffer; a re-composite of an unchanged child reuses it without re-reading `wl_shm` at all. The
    /// cache always holds the complete, correct frame, so the composited output is identical to the
    /// full-upload path (see [`handlers::compositor::RepackCache`]).
    pub(crate) repacks: HashMap<u32, handlers::compositor::RepackCache>,
    /// Surfaces whose pixels changed (new buffer or damage) since their window tree was last presented.
    /// A commit whose entire presented tree is clean skips the expensive present/upload but still fires
    /// the committing surface's `wl_surface.frame` callbacks, so a client that committed only to obtain a
    /// frame callback never stalls. Cleared for the whole tree when it is presented.
    pub(crate) dirty: HashSet<u32>,
    /// Toplevel visibility keyed by render-root sid. Absence means visible.
    pub(crate) visibility: HashMap<u32, dd_display::present::SurfaceVisibility>,
    /// Toplevels the compositor has made fullscreen (by sid), tracked as compositor intent independent of
    /// the client's ack timing. Drives the output-hotplug reconfigure: a fullscreen toplevel whose output
    /// is unplugged is re-configured at the fallback output's size. Cleared on unset_fullscreen/teardown.
    pub(crate) fullscreen_surfaces: HashSet<u32>,
    /// The active `xdg_popup` grab chain (outer→inner). A popup created with `xdg_popup.grab` is dismissed
    /// (with `popup_done`) together with its whole submenu chain when the user clicks outside it; the
    /// input/present loop drives that via [`DdState::dismiss_popup_grabs`]. Tooltips (mapped without a
    /// grab) are NOT listed here, so they are not torn down on an outside click.
    pub(crate) popup_grabs: Vec<PopupSurface>,
    /// Last on-screen window size we sent an `xdg_toplevel.configure` for, so a host-driven window
    /// resize is debounced to one configure per distinct size (mirrors `server.rs`'s `last_cfg`).
    pub(crate) last_cfg: Option<(i32, i32)>,
    pub(crate) start: Instant,

    // ---- input + clipboard follow-ups (handlers/seat.rs) ----
    /// Last device-independent modifier bitmask applied from the host (bit0 Shift, bit1 Ctrl, bit2 Alt,
    /// bit3 Super/Cmd, bit4 CapsLock). `update_modifiers` diffs against this so a macOS `FlagsChanged`
    /// turns into the matching modifier-key press/release through the XKB state.
    pub(crate) mod_mask: u32,
    /// The host clipboard generation (`Presenter::clipboard_host_generation`) we last mirrored into a
    /// guest-facing selection, so the runtime re-offers the host clipboard only when it actually changed
    /// (and never ping-pongs our own guest→host push back to the guest).
    pub(crate) host_clip_gen: u64,
    /// Mime types a guest just offered on its selection (a copy) that still need exporting to the host
    /// clipboard. Drained by the runtime loop, which reads the guest source and calls
    /// `Presenter::clipboard_set_host`. `None` when there is nothing pending.
    pub(crate) pending_host_copy: Option<Vec<String>>,

    /// The surface a client set as its custom pointer image via `wl_pointer.set_cursor` (the surface with
    /// the `cursor_image` role), if any. Its committed buffer is turned into a host cursor (see
    /// `handlers::seat`) instead of being presented as a window; tracked so a later cursor re-commit updates
    /// the host cursor and so clearing it (a named/hidden cursor, or a different surface) is detectable.
    pub(crate) cursor_surface: Option<WlSurface>,
    /// Whether the system pointer is currently hidden BECAUSE a `zwp_pointer_constraints` lock is active
    /// (FPS mouselook). Tracked so the compositor un-hides exactly the cursor it hid when the lock releases
    /// or focus leaves — without clobbering a cursor a client hid deliberately (`set_cursor(null)`).
    pub(crate) cursor_hidden_by_lock: bool,

    /// `wl_surface.frame` callbacks retained per surface across a FAILED present (the frame did not reach
    /// the screen, so the client must NOT be told it can draw again). Fired on the next accepted present
    /// of that surface; bounded (see `MAX_RETAINED_CALLBACKS`) so a permanently-dead presenter cannot grow
    /// the queue without limit. See `handlers::compositor::{pace_surface, retain_frame_callbacks}`.
    pub(crate) retained_callbacks: HashMap<u32, VecDeque<WlCallback>>,
    pub(crate) retained_feedback: HashMap<u32, VecDeque<smithay::wayland::presentation::PresentationFeedbackCallback>>,

    /// Collision-free host identity for every live `wl_surface`. Wayland protocol ids are local to a
    /// client and may be reused after destroy; `ObjectId` includes both ownership and object generation.
    /// The monotonically allocated host id is what the existing Presenter ABI consumes.
    surface_ids: HashMap<ObjectId, u32>,
    next_surface_id: u32,
    presenter_windows: HashSet<u32>,
    /// Host sids of surfaces adopted as X11 (XWayland) windows — see [`DdState::adopt_x11_window`]. An
    /// X11 window's `wl_surface` presents through the same commit→present path as a native toplevel; this
    /// set only records which windows came from the X11 bridge (for policy/diagnostics).
    x11_windows: HashSet<u32>,
    surface_owners: HashMap<u32, ClientId>,
    /// Clients that have ever held keyboard focus — a proxy for "this client bound `wl_seat` and can
    /// consume input" used by the split-client input router (`handlers::input_routing`). Chrome's browser
    /// connection owns the seat + focus while a SEPARATE gpu/shim connection owns the visible IOSurface
    /// window; an event on the gpu window is forwarded to the browser client. See
    /// [`DdState::surface_can_receive_input`].
    seat_input_clients: HashSet<ClientId>,
    /// Server-wide temporary logical crop mirrored from the input (browser) connection onto the visible
    /// (gpu/shim) connection so its IOSurface is cropped to the browser window's region at present time.
    /// Set by [`DdState::set_external_logical_crop`]; applied in `snapshot_surface`.
    external_logical_crop: Option<handlers::input_routing::ExternalLogicalCrop>,
    /// Client charged a presenter-object (native window) budget unit for surface `sid`, so the charge can
    /// be refunded on `drop_window` even after the surface's protocol object / owner record is gone.
    presenter_object_charges: HashMap<u32, ClientId>,
    surface_resources: HashMap<u32, WlSurface>,
    render_usage: HashMap<ClientId, RenderUsage>,
    global_render_usage: RenderUsage,
    render_limits: RenderLimits,
    shm_budget: Arc<Mutex<ShmBudgetLedger>>,
    surface_buffer_uses: HashMap<u32, BufferUse>,
    /// Zero-copy (IOSurface/dmabuf) buffer uses that were PRESENTED and are awaiting host-GPU/present
    /// completion before their `wl_buffer` may be released. Each carries the presenter completion serial
    /// (`completion_serial`) it was submitted under; [`DdState::retire_completed_buffer_uses`] releases
    /// every use whose serial the presenter reports complete — possibly out of submission order.
    inflight_zero_copy: Vec<BufferUse>,
    next_buffer_use_generation: u64,
    /// In-flight GPU fence state per surface: the host-executor allocation + presenter completion serial
    /// a zero-copy surface currently depends on. Reclaimed by [`DdState::fence_drop`] on teardown so a
    /// destroyed surface / disconnected client never leaks a cross-queue fence or executor allocation.
    surface_fences: HashMap<u32, GpuFence>,
    /// Shared sink the per-client [`ClientState::disconnected`] callback pushes disconnected client ids
    /// into; drained by [`DdState::drain_client_disconnects`].
    client_disconnects: Arc<Mutex<Vec<ClientId>>>,

    /// XWayland bridge state (Xwayland server handle, `wl_surface`↔X11 shell global, and the running X11
    /// window manager), present only when built with `--features xwayland` and started at runtime under
    /// DD_XWAYLAND. `None` until [`handlers::xwayland::DdState::start_xwayland`] runs. See
    /// `handlers/xwayland.rs`.
    #[cfg(feature = "xwayland")]
    pub(crate) xwayland: Option<handlers::xwayland::XwaylandState>,
}

/// The in-flight GPU synchronization a zero-copy surface owns while its host-executor render/present is
/// outstanding: the executor allocation charged for it and the presenter completion serial that must
/// signal before the fence is safe to drop. This is the compositor-side half of the acquire/release
/// fence contract — a real Linux-syncobj/`MTLSharedEvent` bridge is a separate ledger row, but the
/// lifetime tracked here is what teardown must reclaim so a fence/allocation never outlives its surface.
#[derive(Clone, Copy, Debug)]
pub(crate) struct GpuFence {
    /// Buffer-use generation this fence guards (links the fence to the exact zero-copy [`BufferUse`]).
    generation: u64,
    /// Presenter completion serial this surface's outstanding GPU work was submitted under, if presented.
    completion_serial: Option<u64>,
}

#[derive(Default)]
struct ShmBudgetLedger {
    per_client: HashMap<ClientId, usize>,
    global: usize,
}

struct DdShmPoolQuota {
    ledger: Arc<Mutex<ShmBudgetLedger>>,
    owner: ClientId,
    size: Mutex<usize>,
    per_client_limit: usize,
    global_limit: usize,
}

impl smithay::wayland::shm::ShmPoolQuota for DdShmPoolQuota {
    fn resize(&self, new_size: usize) -> bool {
        let mut size = self.size.lock().unwrap();
        let mut ledger = self.ledger.lock().unwrap();
        let current_client = ledger.per_client.get(&self.owner).copied().unwrap_or(0);
        let Some(next_client) = current_client.checked_sub(*size).and_then(|n| n.checked_add(new_size)) else {
            return false;
        };
        let Some(next_global) = ledger.global.checked_sub(*size).and_then(|n| n.checked_add(new_size)) else {
            return false;
        };
        if next_client > self.per_client_limit || next_global > self.global_limit {
            return false;
        }
        ledger.per_client.insert(self.owner.clone(), next_client);
        ledger.global = next_global;
        *size = new_size;
        true
    }
}

impl Drop for DdShmPoolQuota {
    fn drop(&mut self) {
        let size = *self.size.lock().unwrap();
        let mut ledger = self.ledger.lock().unwrap();
        if let Some(client) = ledger.per_client.get_mut(&self.owner) {
            *client = client.checked_sub(size).expect("shm client budget underflow");
            if *client == 0 {
                ledger.per_client.remove(&self.owner);
            }
        }
        ledger.global = ledger.global.checked_sub(size).expect("shm global budget underflow");
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BufferUseKind {
    ShmCopy,
    ZeroCopy,
}

pub(crate) struct BufferUse {
    pub buffer: WlBuffer,
    pub generation: u64,
    pub kind: BufferUseKind,
    released: bool,
    /// Surface this use belongs to (so a use retired from the in-flight queue can drop its fence).
    sid: u32,
    /// Owning client, retained so zero-copy resource charges (import/fds/executor) can be refunded even
    /// after the surface's protocol object is gone.
    owner: Option<ClientId>,
    /// dmabuf plane fds charged for this zero-copy use (refunded exactly on release).
    fds_charged: usize,
    /// Whether a dmabuf-import charge is held for this use (refunded exactly on release).
    charged_import: bool,
    /// Whether a host-executor-allocation charge is held for this use (refunded exactly on release).
    charged_executor: bool,
    /// Presenter completion serial this zero-copy use was submitted under; `None` until it is presented.
    /// A zero-copy `wl_buffer` is released only once the presenter reports this serial complete
    /// (`release_after_present`), never merely because `present()` returned (`retain_buffer` until then).
    completion_serial: Option<u64>,
}

impl DdState {
    /// Stand up every global `server.rs` advertises by hand. `output_scale` comes from the Presenter so
    /// a Retina backing store advertises `wl_output.scale = 2` (HiDPI), matching `dd-display`'s
    /// `present_cocoa` HiDPI advert.
    pub fn new(dh: DisplayHandle, presenter: Box<dyn Presenter>) -> DdState {
        Self::new_with_render_limits(dh, presenter, RenderLimits::default())
    }

    pub fn new_with_render_limits(
        dh: DisplayHandle,
        presenter: Box<dyn Presenter>,
        render_limits: RenderLimits,
    ) -> DdState {
        let compositor = CompositorState::new::<Self>(&dh); // wl_compositor v5 + wl_subcompositor
        // wl_shm: Argb8888/Xrgb8888 are always advertised by Smithay.
        let shm = ShmState::new::<Self>(&dh, Vec::new());
        // xdg_wm_base + xdg_surface/toplevel/popup. Advertise the WM capabilities we honour so clients
        // enable their maximize/minimize/fullscreen affordances (a client hides the button for a
        // capability the compositor does not report — xdg_toplevel.wm_capabilities, v5+).
        let xdg_shell = XdgShellState::new_with_capabilities::<Self>(
            &dh,
            [
                WmCapabilities::Maximize,
                WmCapabilities::Fullscreen,
                WmCapabilities::Minimize,
                WmCapabilities::WindowMenu,
            ],
        );
        // zxdg_decoration_manager_v1: lets a client (Qt/GTK) negotiate server-side vs client-side window
        // decorations. Our policy honours the client's request within what the host window can render and
        // defaults to the native macOS titlebar seam (DD_DISPLAY_WINDOW_DECORATIONS) — see handlers/xdg.rs.
        let xdg_decoration = XdgDecorationState::new::<Self>(&dh);
        // xdg_activation_v1: an app can present an activation token to ask the compositor to focus/raise a
        // toplevel (e.g. a launcher activating the window it spawned, or a background tab raising itself).
        let xdg_activation = XdgActivationState::new::<Self>(&dh);
        let viewporter = ViewporterState::new::<Self>(&dh); // wp_viewporter
        // wp_presentation: advertise the GUEST's CLOCK_MONOTONIC id (Linux == 1), NOT the host macOS
        // libc value, so the client interprets our `presented` timestamps in its own clock domain.
        let presentation = PresentationState::new::<Self>(&dh, CLOCK_MONOTONIC_LINUX);
        // wp_cursor_shape_manager_v1: themed cursor requests → CursorImageStatus::Named → NSCursor.
        let cursor_shape = CursorShapeManagerState::new::<Self>(&dh);
        // wl_data_device_manager v3: clipboard (selection) + drag-and-drop. Smithay drives the whole
        // guest↔guest transfer; the compositor bridges the selection to the host clipboard via the
        // `Presenter` clipboard hooks (see handlers/seat.rs).
        let data_device = DataDeviceState::new::<Self>(&dh);
        // zwp_linux_dmabuf_v1: advertise the GPU present path so GLES/Vulkan/GPU-composited clients
        // can attach IOSurface-backed buffers (glmark2, es2tri, GPU browsers). See handlers/dmabuf.rs.
        let dmabuf_state = handlers::dmabuf::new_dmabuf_state(&dh);
        // zwp_primary_selection_device_manager_v1: X11-style primary (middle-click) selection. Smithay drives
        // the guest↔guest transfer through the same SelectionHandler as the clipboard; the compositor follows
        // keyboard focus with it (see focus_surface). Terminals/toolkits (GTK/Qt) rely on it.
        let primary_selection = PrimarySelectionState::new::<Self>(&dh);
        // zwp_relative_pointer_manager_v1: unaccelerated relative motion for games/3D. Deltas are delivered
        // through the existing pointer (see relative_motion); this only advertises the manager global.
        let relative_pointer = RelativePointerManagerState::new::<Self>(&dh);
        // zwp_pointer_constraints_v1: pointer lock/confine for FPS mouselook and drawing apps.
        let pointer_constraints = PointerConstraintsState::new::<Self>(&dh);
        // wp_fractional_scale_manager_v1: advertise the fractional-scale global so toolkits can request a
        // non-integer buffer scale (1.5×/1.25×) for scaled Retina modes. The per-surface preferred_scale is
        // sent from handlers/scale.rs; combined with wp_viewporter for a correct composited result.
        let fractional_scale = FractionalScaleManagerState::new::<Self>(&dh);
        // wp_single_pixel_buffer_v1: the 1×1 solid-color buffer fast path (backgrounds/solid surfaces).
        let single_pixel_buffer = SinglePixelBufferState::new::<Self>(&dh);

        // ---- Modern GUI protocol groups composed from the vendored Smithay tree (codex-rendering §5.2/§9.4).
        // Advertise + supply host policy for protocols the vendored smithay-0.7.0 implements but dd did not
        // compose. Policy lives in the same-named handlers::* submodules; see each module's docs.
        // zwp_pointer_gestures_v1: touchpad swipe/pinch/hold; no host gesture device, so no events by default.
        let pointer_gestures = PointerGesturesState::new::<Self>(&dh);
        // zwp_tablet_manager_v2: graphics tablet/stylus; dd has no tablet hardware, so the seat exposes none.
        let tablet_manager = Self::new_tablet_manager(&dh);
        // zwp_idle_inhibit_manager_v1: keep-session-awake; dd records the intent (handlers::idle_inhibit).
        let idle_inhibit = IdleInhibitManagerState::new::<Self>(&dh);
        // wp_content_type_manager_v1: per-surface photo/video/game hint; stored on commit (handlers::content_type).
        let content_type = ContentTypeState::new::<Self>(&dh);
        let explicit_sync = handlers::explicit_sync::ExplicitSyncState::new(&dh);
        let color = handlers::color::ColorManagementState::new(&dh);
        // zxdg_exporter_v2 + zxdg_importer_v2: cross-client toplevel parenting; Smithay issues real handles.
        let xdg_foreign = XdgForeignState::new::<Self>(&dh);
        // zwp_keyboard_shortcuts_inhibit_manager_v1: forward all keys; dd honours it (handlers::keyboard_shortcuts_inhibit).
        let keyboard_shortcuts_inhibit = KeyboardShortcutsInhibitState::new::<Self>(&dh);
        // zwp_text_input_manager_v3: text-input for editors/forms + the host IME (marked-text) bridge.
        // Advertised here; the compositor owns the text-input instances directly (see handlers/text_input.rs).
        let text_input = handlers::text_input::TextInputState::new(&dh);

        let mut seat_state = SeatState::<Self>::new();
        let mut seat = seat_state.new_wl_seat(&dh, "dd-seat-0"); // wl_seat v5
        // add_keyboard compiles an XKB keymap through libxkbcommon (US default layout).
        let keyboard = seat
            .add_keyboard(Default::default(), 200, 25)
            .expect("compile XKB keymap via libxkbcommon");
        let pointer = seat.add_pointer();

        // wl_output v4 (name/description) + xdg_output, advertised at the presenter's HiDPI scale.
        let output_manager = OutputManagerState::new_with_xdg_output::<Self>(&dh);
        let scale = presenter.output_scale().max(1);
        let output = Output::new(
            "dd-0".into(),
            PhysicalProperties {
                size: (600, 340).into(),
                subpixel: Subpixel::Unknown,
                make: "dd".into(),
                model: "dd-display".into(),
            },
        );
        let output_global = output.create_global::<Self>(&dh);
        let mode = OutMode {
            size: Size::from((2560, 1440)),
            refresh: 60_000,
        };
        output.change_current_state(Some(mode), None, Some(Scale::Integer(scale)), None);
        output.set_preferred(mode);

        DdState {
            dh,
            compositor,
            shm,
            xdg_shell,
            xdg_decoration,
            xdg_activation,
            seat_state,
            output_manager,
            viewporter,
            presentation,
            cursor_shape,
            data_device,
            dmabuf_state,
            primary_selection,
            relative_pointer,
            pointer_constraints,
            fractional_scale,
            single_pixel_buffer,
            // ---- Modern GUI protocol groups (codex-rendering §5.2/§9.4) ----
            pointer_gestures,
            tablet_manager,
            idle_inhibit,
            content_type,
            xdg_foreign,
            keyboard_shortcuts_inhibit,
            idle_inhibitors: HashSet::new(),
            content_types: HashMap::new(),
            explicit_sync,
            color,
            seat,
            keyboard,
            pointer,
            output,
            extra_outputs: Vec::new(),
            output_globals: HashMap::from([("dd-0".to_string(), output_global)]),
            headless: false,
            surface_outputs: HashMap::new(),
            text_input,
            presenter,
            focus: None,
            titles: HashMap::new(),
            ptr_loc: (0.0, 0.0),
            recent_serials: VecDeque::new(),
            buffers: HashMap::new(),
            repacks: HashMap::new(),
            dirty: HashSet::new(),
            visibility: HashMap::new(),
            fullscreen_surfaces: HashSet::new(),
            popup_grabs: Vec::new(),
            last_cfg: None,
            start: Instant::now(),
            mod_mask: 0,
            host_clip_gen: 0,
            pending_host_copy: None,
            cursor_surface: None,
            cursor_hidden_by_lock: false,
            retained_callbacks: HashMap::new(),
            retained_feedback: HashMap::new(),
            surface_ids: HashMap::new(),
            next_surface_id: 1,
            presenter_windows: HashSet::new(),
            surface_owners: HashMap::new(),
            presenter_object_charges: HashMap::new(),
            x11_windows: HashSet::new(),
            seat_input_clients: HashSet::new(),
            external_logical_crop: None,
            surface_resources: HashMap::new(),
            render_usage: HashMap::new(),
            global_render_usage: RenderUsage::default(),
            render_limits,
            shm_budget: Arc::new(Mutex::new(ShmBudgetLedger::default())),
            surface_buffer_uses: HashMap::new(),
            inflight_zero_copy: Vec::new(),
            next_buffer_use_generation: 1,
            surface_fences: HashMap::new(),
            client_disconnects: Arc::new(Mutex::new(Vec::new())),
            #[cfg(feature = "xwayland")]
            xwayland: None,
        }
    }

    pub(crate) fn register_surface(&mut self, surface: &WlSurface) {
        let object = surface.id();
        if self.surface_ids.contains_key(&object) {
            return;
        }
        let client = surface.client().expect("wl_surface has no owning client");
        let owner = client.id();
        if !self.reserve_surface(&owner) {
            client.kill(
                &self.dh,
                ProtocolError {
                    code: 2,
                    object_id: 1,
                    object_interface: "wl_display".into(),
                    message: "compositor per-client/global surface budget exhausted".into(),
                },
            );
            return;
        }
        let sid = self.next_surface_id;
        self.next_surface_id = self
            .next_surface_id
            .checked_add(1)
            .expect("surface id space exhausted");
        self.surface_ids.insert(object, sid);
        self.surface_owners.insert(sid, owner);
        self.surface_resources.insert(sid, surface.clone());
        self.surface_outputs.insert(sid, self.output.clone());
        self.output.enter(surface);
    }

    /// Return the compositor-global, generation-safe id assigned in `new_surface`.
    pub(crate) fn surface_id(&self, surface: &WlSurface) -> u32 {
        *self
            .surface_ids
            .get(&surface.id())
            .expect("live surface was not registered")
    }

    /// Fallible [`Self::surface_id`] for destruction callbacks. When a client destroys a surface with a
    /// role, wayland-server tears down the role object AND the `wl_surface` in one cleanup pass; the
    /// `wl_surface`'s own `destroyed` (which unregisters the id) may run BEFORE the role's destroy handler.
    /// A role-destroy handler must therefore tolerate an already-unregistered surface rather than panic.
    pub(crate) fn surface_id_opt(&self, surface: &WlSurface) -> Option<u32> {
        self.surface_ids.get(&surface.id()).copied()
    }

    /// Reclaim every dd-owned resource associated with a surface. This is deliberately idempotent:
    /// role teardown and `wl_surface.destroy` can arrive through different protocol paths.
    pub(crate) fn teardown_surface(&mut self, surface: &WlSurface) {
        let Some(sid) = self.surface_ids.remove(&surface.id()) else {
            return;
        };
        // Reclaim the surface's in-flight GPU fence + zero-copy executor allocation (refunding the
        // import/fd/executor charges) BEFORE `release_surface_resources` drops the owner record those
        // refunds are keyed on. This closes the row-1 residual: surface destruction reclaims client-owned
        // executor resources and in-flight GPU fences, not just the CPU/cache/callback/window state.
        self.fence_drop(sid);
        self.release_surface_resources(sid);
        self.surface_resources.remove(&sid);
        if let Some(output) = self.surface_outputs.remove(&sid) {
            output.leave(surface);
        }
        self.buffers.remove(&sid);
        self.repacks.remove(&sid);
        self.dirty.remove(&sid);
        self.visibility.remove(&sid);
        self.fullscreen_surfaces.remove(&sid);
        self.x11_windows.remove(&sid);
        self.titles.remove(&sid);
        self.retained_callbacks.remove(&sid);
        if let Some(feedback) = self.retained_feedback.remove(&sid) {
            for callback in feedback { callback.discarded(); }
        }
        self.idle_inhibitors.remove(&sid);
        self.content_types.remove(&sid);
        self.popup_grabs.retain(|p| p.wl_surface() != surface);
        if self.focus.as_ref() == Some(surface) {
            self.focus = None;
            self.last_cfg = None;
            self.set_text_input_focus(None);
        }
        if self.cursor_surface.as_ref() == Some(surface) {
            self.cursor_surface = None;
        }
        self.drop_surface_window(sid);
    }

    pub(crate) fn drop_surface_window(&mut self, sid: u32) {
        // Refund the presenter-object (native window) budget unit charged when the window was created.
        if let Some(owner) = self.presenter_object_charges.remove(&sid) {
            self.refund_budget(&owner, BudgetDim::PresenterObjects, 1);
        }
        if self.presenter_windows.remove(&sid) {
            self.presenter.drop_window(sid);
        }
    }

    /// Charge one presenter-object (native window) budget unit for surface `sid` the first time it is
    /// presented into a host window. Idempotent per surface (a re-present does not re-charge). Best-effort:
    /// on quota exhaustion the client is disconnected, exactly like the surface/callback/cache paths.
    pub(crate) fn charge_presenter_window(&mut self, sid: u32) {
        if self.presenter_object_charges.contains_key(&sid) {
            return;
        }
        let Some(owner) = self.surface_owners.get(&sid).cloned() else { return; };
        if self.charge_budget(&owner, BudgetDim::PresenterObjects, 1) {
            self.presenter_object_charges.insert(sid, owner);
        } else {
            self.reject_budget_exhaustion(sid, "presenter window");
        }
    }

    /// Atomically reserve `n` units of a count-based render-resource dimension against BOTH the per-client
    /// and global limits, or reserve nothing and return `false`. The single reserve path for fds, dmabuf
    /// imports, presenter objects, and executor allocations (`try_reserve` semantics).
    fn charge_budget(&mut self, owner: &ClientId, dim: BudgetDim, n: usize) -> bool {
        if n == 0 {
            return true;
        }
        let (per_client_limit, global_limit) = dim.limits(&self.render_limits);
        let cur_client = {
            let usage = self.render_usage.entry(owner.clone()).or_default();
            *dim.slot(usage)
        };
        let client_next = match cur_client.checked_add(n) {
            Some(v) if v <= per_client_limit => v,
            _ => return false,
        };
        let cur_global = *dim.slot(&mut self.global_render_usage);
        let global_next = match cur_global.checked_add(n) {
            Some(v) if v <= global_limit => v,
            _ => return false,
        };
        *dim.slot(self.render_usage.get_mut(owner).unwrap()) = client_next;
        *dim.slot(&mut self.global_render_usage) = global_next;
        true
    }

    /// Refund `n` units of a count-based render-resource dimension (the `release` half of the atomic
    /// reserve). Drops the per-client record once every dimension returns to zero.
    fn refund_budget(&mut self, owner: &ClientId, dim: BudgetDim, n: usize) {
        if n == 0 {
            return;
        }
        if let Some(usage) = self.render_usage.get_mut(owner) {
            let slot = dim.slot(usage);
            *slot = slot.checked_sub(n).expect("per-client budget refund underflow");
            if usage.is_empty() {
                self.render_usage.remove(owner);
            }
        }
        let slot = dim.slot(&mut self.global_render_usage);
        *slot = slot.checked_sub(n).expect("global budget refund underflow");
    }

    /// Wire the per-client [`ClientState`] to this compositor's shared disconnect sink so a dropped
    /// connection reaches [`Self::drain_client_disconnects`]. The runtime uses this in place of
    /// `ClientState::default()` when accepting a client.
    pub fn new_client_state(&self) -> ClientState {
        ClientState::new(self.client_disconnects.clone())
    }

    /// Reclaim GPU/executor state for every client whose connection dropped since the last drain. Called
    /// from the runtime dispatch loop. Per-surface state is also reclaimed through `destroyed`; this
    /// guarantees any client-owned executor allocation / in-flight fence is released even if a surface
    /// destroy did not arrive first.
    pub fn drain_client_disconnects(&mut self) {
        let ids: Vec<ClientId> = {
            let mut sink = self.client_disconnects.lock().unwrap();
            std::mem::take(&mut *sink)
        };
        for id in ids {
            self.drop_client_gpu_state(&id);
        }
    }

    /// Reclaim every surface's in-flight GPU fence and outstanding zero-copy executor allocation for a
    /// disconnected client. Idempotent — a surface already torn down via `destroyed` contributes nothing.
    pub(crate) fn drop_client_gpu_state(&mut self, client: &ClientId) {
        let sids: Vec<u32> = self
            .surface_owners
            .iter()
            .filter(|(_, owner)| *owner == client)
            .map(|(sid, _)| *sid)
            .collect();
        for sid in sids {
            self.fence_drop(sid);
        }
        self.seat_input_clients.remove(client);
        // Any in-flight zero-copy use whose owner is this client but whose surface record is already gone.
        let mut i = 0;
        while i < self.inflight_zero_copy.len() {
            if self.inflight_zero_copy[i].owner.as_ref() == Some(client) {
                let use_ = self.inflight_zero_copy.remove(i);
                self.release_buffer_use(use_);
            } else {
                i += 1;
            }
        }
    }

    pub(crate) fn begin_buffer_use(&mut self, sid: u32, buffer: WlBuffer, kind: BufferUseKind) {
        self.retire_buffer_use(sid);
        let generation = self.next_buffer_use_generation;
        self.next_buffer_use_generation = self
            .next_buffer_use_generation
            .checked_add(1)
            .expect("buffer-use generation exhausted");
        let owner = self.surface_owners.get(&sid).cloned();
        let (fds_charged, charged_import, charged_executor) = if kind == BufferUseKind::ZeroCopy {
            // A zero-copy commit takes a real host-executor allocation (the IOSurface the guest asked the
            // host GPU to render/present into) and holds the dmabuf's plane fds open. Charge all three to
            // the owning client so accelerated buffers count against the per-client render budget, and
            // register the in-flight GPU fence this surface now depends on.
            let planes = smithay::wayland::dmabuf::get_dmabuf(&buffer)
                .map(|d| d.num_planes())
                .unwrap_or(0);
            let (mut fds, mut import, mut exec) = (0usize, false, false);
            if let Some(owner) = owner.as_ref() {
                if self.charge_budget(owner, BudgetDim::DmabufImports, 1) {
                    import = true;
                }
                if self.charge_budget(owner, BudgetDim::Fds, planes) {
                    fds = planes;
                }
                if self.charge_budget(owner, BudgetDim::ExecutorAllocations, 1) {
                    exec = true;
                }
            }
            self.surface_fences.insert(sid, GpuFence { generation, completion_serial: None });
            (fds, import, exec)
        } else {
            (0, false, false)
        };
        self.surface_buffer_uses.insert(
            sid,
            BufferUse {
                buffer,
                generation,
                kind,
                released: false,
                sid,
                owner,
                fds_charged,
                charged_import,
                charged_executor,
                completion_serial: None,
            },
        );
    }

    /// Release a `wl_buffer` after its pixels have been safely copied (shm) — the exact completion point
    /// for a CPU buffer. Zero-copy buffers are NOT released here (their GPU use is still outstanding); see
    /// [`Self::submit_zero_copy_use`] / [`Self::retire_completed_buffer_uses`].
    pub(crate) fn complete_buffer_use(&mut self, sid: u32) {
        let Some(use_) = self.surface_buffer_uses.get_mut(&sid) else {
            return;
        };
        debug_assert!(use_.generation > 0);
        if use_.kind == BufferUseKind::ShmCopy && !use_.released {
            use_.buffer.release();
            use_.released = true;
        }
    }

    /// A zero-copy surface's tree just reached the screen under presenter completion serial `serial`. Move
    /// its active use out of the live slot and into the in-flight queue, tagged with the serial that must
    /// signal (`release_after_present`) before the buffer may be released. The buffer is RETAINED
    /// (`retain_buffer`) until then — a present() return is not GPU completion.
    pub(crate) fn submit_zero_copy_use(&mut self, sid: u32, serial: u64) {
        let Some(use_) = self.surface_buffer_uses.get(&sid) else { return; };
        if use_.kind != BufferUseKind::ZeroCopy || use_.released {
            return;
        }
        let mut use_ = self.surface_buffer_uses.remove(&sid).unwrap();
        use_.completion_serial = Some(serial);
        if let Some(fence) = self.surface_fences.get_mut(&sid) {
            if fence.generation == use_.generation {
                fence.completion_serial = Some(serial);
            }
        }
        self.inflight_zero_copy.push(use_);
    }

    /// Release every in-flight zero-copy buffer whose presenter completion serial appears in `completed`.
    /// Retirement is by serial membership, NOT submission order, so out-of-order GPU/present completion
    /// releases exactly the right buffers (a later-submitted frame whose GPU work finishes first is
    /// retired first, while an earlier still-pending frame keeps its buffer).
    pub fn retire_completed_buffer_uses(&mut self, completed: &[u64]) {
        let mut i = 0;
        while i < self.inflight_zero_copy.len() {
            let done = matches!(
                self.inflight_zero_copy[i].completion_serial,
                Some(s) if completed.contains(&s)
            );
            if done {
                let use_ = self.inflight_zero_copy.remove(i);
                self.release_buffer_use(use_);
            } else {
                i += 1;
            }
        }
    }

    /// Release a buffer use's `wl_buffer` (if not already) and refund every resource charge it held. The
    /// single retirement point for both the live and in-flight paths so a charge is refunded exactly once.
    fn release_buffer_use(&mut self, mut use_: BufferUse) {
        debug_assert!(use_.generation > 0);
        if !use_.released {
            use_.buffer.release();
            use_.released = true;
        }
        if let Some(owner) = use_.owner.clone() {
            if use_.charged_import {
                self.refund_budget(&owner, BudgetDim::DmabufImports, 1);
            }
            if use_.fds_charged > 0 {
                self.refund_budget(&owner, BudgetDim::Fds, use_.fds_charged);
            }
            if use_.charged_executor {
                self.refund_budget(&owner, BudgetDim::ExecutorAllocations, 1);
            }
        }
        if let Some(fence) = self.surface_fences.get(&use_.sid) {
            if fence.generation == use_.generation {
                self.surface_fences.remove(&use_.sid);
            }
        }
    }

    pub(crate) fn retire_buffer_use(&mut self, sid: u32) {
        let Some(use_) = self.surface_buffer_uses.remove(&sid) else {
            return;
        };
        // A shm use whose pixels were already copied is released; a zero-copy use being replaced/detached
        // before it was ever presented never reached the host GPU under a completion serial, so releasing
        // it now is safe (a new buffer supersedes it). Charges are refunded exactly once here.
        self.release_buffer_use(use_);
    }

    /// Reclaim a surface's in-flight GPU fence + any outstanding zero-copy executor allocation: retire its
    /// live use and drain every in-flight use it still owns. Called on surface teardown and client
    /// disconnect so a destroyed surface never leaves a cross-queue fence or executor allocation live.
    pub(crate) fn fence_drop(&mut self, sid: u32) {
        self.surface_fences.remove(&sid);
        self.retire_buffer_use(sid);
        let mut i = 0;
        while i < self.inflight_zero_copy.len() {
            if self.inflight_zero_copy[i].sid == sid {
                let use_ = self.inflight_zero_copy.remove(i);
                self.release_buffer_use(use_);
            } else {
                i += 1;
            }
        }
    }

    pub(crate) fn forget_destroyed_buffer(&mut self, buffer: &WlBuffer) {
        let live: Vec<u32> = self
            .surface_buffer_uses
            .iter()
            .filter(|(_, use_)| use_.buffer == *buffer)
            .map(|(sid, _)| *sid)
            .collect();
        for sid in live {
            if let Some(use_) = self.surface_buffer_uses.remove(&sid) {
                self.release_buffer_use(use_);
            }
        }
        let mut i = 0;
        while i < self.inflight_zero_copy.len() {
            if self.inflight_zero_copy[i].buffer == *buffer {
                let use_ = self.inflight_zero_copy.remove(i);
                self.release_buffer_use(use_);
            } else {
                i += 1;
            }
        }
        self.buffers.retain(|_, live| live != buffer);
    }

    fn reserve_surface(&mut self, owner: &ClientId) -> bool {
        let usage = self.render_usage.entry(owner.clone()).or_default();
        if usage.surfaces >= self.render_limits.surfaces_per_client
            || self.global_render_usage.surfaces >= self.render_limits.surfaces_global
        {
            return false;
        }
        usage.surfaces += 1;
        self.global_render_usage.surfaces += 1;
        true
    }

    fn release_surface_resources(&mut self, sid: u32) {
        let Some(owner) = self.surface_owners.remove(&sid) else {
            return;
        };
        let cache_bytes = self.repacks.get(&sid).map_or(0, |cache| cache.bgra.len());
        let callbacks = self.retained_callbacks.get(&sid).map_or(0, VecDeque::len);
        let empty = if let Some(usage) = self.render_usage.get_mut(&owner) {
            usage.surfaces = usage.surfaces.checked_sub(1).expect("surface charge underflow");
            usage.cpu_cache_bytes = usage
                .cpu_cache_bytes
                .checked_sub(cache_bytes)
                .expect("CPU cache charge underflow");
            usage.retained_callbacks = usage
                .retained_callbacks
                .checked_sub(callbacks)
                .expect("callback charge underflow");
            usage.is_empty()
        } else {
            false
        };
        if empty {
            self.render_usage.remove(&owner);
        }
        self.global_render_usage.surfaces = self
            .global_render_usage
            .surfaces
            .checked_sub(1)
            .expect("global surface charge underflow");
        self.global_render_usage.cpu_cache_bytes =
            self.global_render_usage.cpu_cache_bytes.checked_sub(cache_bytes).expect("global CPU cache charge underflow");
        self.global_render_usage.retained_callbacks = self
            .global_render_usage
            .retained_callbacks
            .checked_sub(callbacks)
            .expect("global callback charge underflow");
    }

    pub(crate) fn replace_cache_charge(&mut self, sid: u32, old: usize, new: usize) -> bool {
        let Some(owner) = self.surface_owners.get(&sid).cloned() else {
            return false;
        };
        let usage = self
            .render_usage
            .get_mut(&owner)
            .expect("surface owner has no budget");
        let client_next = usage
            .cpu_cache_bytes
            .checked_sub(old)
            .and_then(|n| n.checked_add(new));
        let global_next = self
            .global_render_usage
            .cpu_cache_bytes
            .checked_sub(old)
            .and_then(|n| n.checked_add(new));
        let (Some(client_next), Some(global_next)) = (client_next, global_next) else {
            return false;
        };
        if client_next > self.render_limits.cpu_cache_bytes_per_client
            || global_next > self.render_limits.cpu_cache_bytes_global
        {
            return false;
        }
        usage.cpu_cache_bytes = client_next;
        self.global_render_usage.cpu_cache_bytes = global_next;
        true
    }

    pub(crate) fn remove_repack_cache(&mut self, sid: u32) {
        let old = self.repacks.get(&sid).map_or(0, |cache| cache.bgra.len());
        if old != 0 {
            let _ = self.replace_cache_charge(sid, old, 0);
        }
        self.repacks.remove(&sid);
    }

    pub(crate) fn reject_budget_exhaustion(&self, sid: u32, domain: &str) {
        let Some(surface) = self.surface_resources.get(&sid) else {
            return;
        };
        let Some(client) = surface.client() else {
            return;
        };
        client.kill(
            &self.dh,
            ProtocolError {
                code: 2,
                object_id: 1,
                object_interface: "wl_display".into(),
                message: format!("compositor {domain} budget exhausted"),
            },
        );
    }

    pub(crate) fn reserve_callback(&mut self, sid: u32) -> bool {
        let Some(owner) = self.surface_owners.get(&sid).cloned() else {
            return false;
        };
        let usage = self
            .render_usage
            .get_mut(&owner)
            .expect("surface owner has no budget");
        if usage.retained_callbacks >= self.render_limits.retained_callbacks_per_client
            || self.global_render_usage.retained_callbacks
                >= self.render_limits.retained_callbacks_global
        {
            return false;
        }
        usage.retained_callbacks += 1;
        self.global_render_usage.retained_callbacks += 1;
        true
    }

    pub(crate) fn release_callbacks(&mut self, sid: u32, count: usize) {
        let Some(owner) = self.surface_owners.get(&sid) else {
            return;
        };
        if let Some(usage) = self.render_usage.get_mut(owner) {
            usage.retained_callbacks = usage
                .retained_callbacks
                .checked_sub(count)
                .expect("callback charge underflow");
        }
        self.global_render_usage.retained_callbacks = self
            .global_render_usage
            .retained_callbacks
            .checked_sub(count)
            .expect("global callback charge underflow");
    }

    /// Test/diagnostic snapshot of compositor-owned resources. This intentionally reports totals only;
    /// ownership identities remain private.
    #[doc(hidden)]
    pub fn render_usage_totals(&self) -> (usize, usize, usize) {
        (
            self.global_render_usage.surfaces,
            self.global_render_usage.retained_callbacks,
            self.global_render_usage.cpu_cache_bytes,
        )
    }

    /// Totals-only snapshot across EVERY charged render-resource dimension — surfaces, retained
    /// callbacks, CPU cache bytes, plus the row-2 residual dimensions (fds, dmabuf imports, presenter
    /// objects, executor allocations). Ownership identities stay private.
    #[doc(hidden)]
    pub fn render_budget_totals(&self) -> RenderBudgetTotals {
        RenderBudgetTotals {
            surfaces: self.global_render_usage.surfaces,
            retained_callbacks: self.global_render_usage.retained_callbacks,
            cpu_cache_bytes: self.global_render_usage.cpu_cache_bytes,
            fds: self.global_render_usage.fds,
            dmabuf_imports: self.global_render_usage.dmabuf_imports,
            presenter_objects: self.global_render_usage.presenter_objects,
            executor_allocations: self.global_render_usage.executor_allocations,
        }
    }

    /// Poll the presenter for completed present serials, then retire every in-flight zero-copy buffer use
    /// whose GPU/present work has completed. The runtime calls this each dispatch tick so a zero-copy
    /// `wl_buffer` is released promptly once — and only once — its last GPU use finishes.
    pub fn retire_completed_presents(&mut self) {
        let completed = self.presenter.completed_present_serials();
        self.retire_completed_buffer_uses(&completed);
    }

    pub(crate) fn reserve_shm_pool(
        &self,
        owner: &ClientId,
        size: usize,
    ) -> Option<Box<dyn smithay::wayland::shm::ShmPoolQuota>> {
        let mut ledger = self.shm_budget.lock().unwrap();
        let client = ledger.per_client.get(owner).copied().unwrap_or(0);
        let next_client = client.checked_add(size)?;
        let next_global = ledger.global.checked_add(size)?;
        if next_client > self.render_limits.shm_pool_bytes_per_client
            || next_global > self.render_limits.shm_pool_bytes_global
        {
            return None;
        }
        ledger.per_client.insert(owner.clone(), next_client);
        ledger.global = next_global;
        drop(ledger);
        Some(Box::new(DdShmPoolQuota {
            ledger: self.shm_budget.clone(),
            owner: owner.clone(),
            size: Mutex::new(size),
            per_client_limit: self.render_limits.shm_pool_bytes_per_client,
            global_limit: self.render_limits.shm_pool_bytes_global,
        }))
    }

    /// Milliseconds since construction, the timestamp domain for `wl_callback.done` / input events.
    pub(crate) fn now_ms(&self) -> u32 {
        self.start.elapsed().as_millis() as u32
    }

    /// Record an input-event serial the seat just issued (a pointer button or key press), so a later
    /// `xdg_toplevel.move`/`resize` (or any serial-gated grab) can be validated as backed by a real user
    /// gesture. Bounded so the window can't grow without limit under sustained input.
    pub(crate) fn note_input_serial(&mut self, serial: Serial) {
        const MAX: usize = 64;
        if self.recent_serials.len() >= MAX {
            self.recent_serials.pop_front();
        }
        self.recent_serials.push_back(serial);
    }

    /// Whether `serial` matches a recently issued input event — the guard on interactive move/resize grabs.
    /// A client MUST echo the serial of the pointer-button press that began the drag; anything else (a
    /// spoofed or stale serial) is rejected, so a window can't yank itself around without user input.
    pub(crate) fn is_recent_input_serial(&self, serial: Serial) -> bool {
        self.recent_serials.contains(&serial)
    }

    /// Microseconds since construction, the timestamp domain `zwp_relative_pointer_v1.relative_motion`
    /// carries (`utime`, split hi/lo by Smithay). Same monotonic clock as [`Self::now_ms`].
    pub(crate) fn now_us(&self) -> u64 {
        self.start.elapsed().as_micros() as u64
    }

    /// The output's logical size (device mode divided by the integer scale) — the bounds a maximized or
    /// fullscreen toplevel is configured to, and the `configure_bounds` hint a floating toplevel gets.
    pub(crate) fn output_logical_size(&self) -> (i32, i32) {
        let scale = self.output.current_scale().integer_scale().max(1);
        match self.output.current_mode() {
            Some(m) => ((m.size.w / scale).max(1), (m.size.h / scale).max(1)),
            None => INITIAL_TOPLEVEL_SIZE,
        }
    }

    /// Make `surface` the keyboard focus (called when a toplevel maps). Keyboard focus drives the data
    /// device too: the clipboard/selection follows keyboard focus (weston/wlroots do the same), so the
    /// focused client is the one that receives `wl_data_device.selection` offers and may set the selection.
    pub(crate) fn focus_surface(&mut self, surface: WlSurface) {
        use smithay::wayland::selection::data_device::set_data_device_focus;
        use smithay::wayland::selection::primary_selection::set_primary_focus;
        self.focus = Some(surface.clone());
        // Record the focused surface's client as input-capable (the split-client router uses this to tell
        // Chrome's browser/input connection from its gpu/shim connection).
        if let Some(client) = surface.client() {
            self.seat_input_clients.insert(client.id());
        }
        // Text-input focus follows keyboard focus (zwp_text_input_v3): the newly focused surface's
        // text-input instances get `enter`, the previously focused ones `leave`.
        self.set_text_input_focus(Some(surface.clone()));
        let client = surface.client();
        let kbd = self.keyboard.clone();
        let serial = SERIAL_COUNTER.next_serial();
        kbd.set_focus(self, Some(surface), serial);
        let dh = self.dh.clone();
        let seat = self.seat.clone();
        set_data_device_focus(&dh, &seat, client.clone());
        // The primary (middle-click) selection follows keyboard focus exactly as the clipboard does, so the
        // focused client is the one offered the current primary selection and allowed to set it.
        set_primary_focus(&dh, &seat, client);
    }
}

#[cfg(test)]
mod zero_copy_release_tests {
    //! ROW 3 proof (`compositor_releases_buffers_only_after_the_last_cpu_or_gpu_use`) plus the zero-copy
    //! half of ROW 1/2: a zero-copy buffer's `wl_buffer` is released ONLY once the presenter reports its
    //! GPU/present completion serial, completion is honoured OUT OF ORDER, the executor/import charges
    //! refund exactly on release, and a surface teardown / client disconnect reclaims a still-in-flight
    //! GPU fence. The `zwp_linux_dmabuf` SCM_RIGHTS import wire is unusable on this Linux dev host (the
    //! pre-existing `dmabuf_present` gate is red on the same path), so this drives the identical public
    //! zero-copy [`BufferUse`] lifecycle with a committed `wl_shm` buffer re-tagged as `ZeroCopy` — the
    //! same charge/submit/retire code the real dmabuf commit path runs, minus the untestable fd import.

    use super::*;
    use dd_display::present::{PresentError, PresentOutcome, Presenter, SurfaceBuffer};
    use dd_display::wire::{Conn, Message};
    use smithay::reexports::wayland_server::Display;
    use std::os::unix::io::{FromRawFd, RawFd};

    struct NullPresenter;
    impl Presenter for NullPresenter {
        fn present(&mut self, _surf: &SurfaceBuffer) -> Result<PresentOutcome, PresentError> {
            Ok(PresentOutcome::Delivered { serial: 1, timing: None })
        }
    }

    fn socketpair() -> (RawFd, RawFd) {
        let mut sv = [0i32; 2];
        assert_eq!(unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, sv.as_mut_ptr()) }, 0);
        for fd in sv {
            unsafe {
                let fl = libc::fcntl(fd, libc::F_GETFL);
                libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
            }
        }
        (sv[0], sv[1])
    }

    struct Cli {
        conn: Conn,
        next: u32,
        globals: HashMap<String, u32>,
        releases: HashMap<u32, usize>,
        events: Vec<(u32, u16, Vec<u8>)>,
    }
    impl Cli {
        fn new(fd: RawFd) -> Cli {
            Cli { conn: Conn::new(fd), next: 2, globals: HashMap::new(), releases: HashMap::new(), events: Vec::new() }
        }
        fn alloc(&mut self) -> u32 {
            let id = self.next;
            self.next += 1;
            id
        }
        fn drain(&mut self) {
            loop {
                match self.conn.fill() {
                    Ok(0) | Ok(-1) | Err(_) => break,
                    _ => {}
                }
            }
            while let Some(m) = self.conn.next_message() {
                self.events.push((m.object, m.opcode, m.body.to_vec()));
                if m.object == 2 && m.opcode == 0 {
                    let mut r = m.reader();
                    let name = r.u32();
                    let iface = r.string();
                    let _ = r.u32();
                    self.globals.entry(iface).or_insert(name);
                } else if m.opcode == 0 {
                    // wl_buffer.release is opcode 0 on the buffer object.
                    *self.releases.entry(m.object).or_default() += 1;
                }
            }
        }
        fn bind(&mut self, iface: &str, ver: u32) -> u32 {
            let id = self.alloc();
            let name = self.globals[iface];
            self.conn.send(&Message::new(2, 0).u32(name).string(iface).u32(ver).u32(id));
            id
        }
        fn releases(&mut self, buffer: u32) -> usize {
            self.drain();
            self.releases.get(&buffer).copied().unwrap_or(0)
        }
        /// Latest `xdg_surface.configure(serial)` (opcode 0 on `xdg`).
        fn xdg_configure_serial(&self, xdg: u32) -> Option<u32> {
            self.events.iter().rev().find(|(o, op, _)| *o == xdg && *op == 0)
                .map(|(_, _, b)| u32::from_ne_bytes([b[0], b[1], b[2], b[3]]))
        }
    }

    /// Map a toplevel (the proven present path — a roleless surface commit is rejected on this host) and
    /// commit an shm buffer, so the server registers the surface and retains its `WlBuffer`. Returns the
    /// client buffer id.
    fn map_and_commit(
        c: &mut Cli,
        display: &mut Display<DdState>,
        state: &mut DdState,
        comp: u32,
        shm: u32,
        wm: u32,
    ) -> u32 {
        fn drive(c: &mut Cli, display: &mut Display<DdState>, state: &mut DdState) {
            c.conn.flush().unwrap();
            display.dispatch_clients(state).unwrap();
            display.flush_clients().unwrap();
            c.drain();
        }
        let surf = c.alloc();
        c.conn.send(&Message::new(comp, 0).u32(surf)); // create_surface
        let xdg = c.alloc();
        c.conn.send(&Message::new(wm, 2).u32(xdg).u32(surf)); // get_xdg_surface
        let top = c.alloc();
        c.conn.send(&Message::new(xdg, 1).u32(top)); // get_toplevel
        c.conn.send(&Message::new(surf, 6)); // commit -> configure
        drive(c, display, state);
        let serial = c.xdg_configure_serial(xdg).expect("configure serial");
        c.conn.send(&Message::new(xdg, 4).u32(serial)); // ack_configure
        drive(c, display, state);
        let (w, h) = (8i32, 8i32);
        let stride = w * 4;
        let size = (stride * h) as usize;
        let fd = dd_display::keymap::anon_fd_with(&vec![0x20u8; size]).unwrap();
        let pool = c.alloc();
        c.conn.send(&Message::new(shm, 0).u32(pool).u32(size as u32)); // create_pool
        c.conn.queue_fd(fd);
        let buffer = c.alloc();
        c.conn.send(&Message::new(pool, 0).u32(buffer).i32(0).i32(w).i32(h).i32(stride).u32(1)); // create_buffer
        c.conn.send(&Message::new(surf, 1).u32(buffer).i32(0).i32(0)); // attach
        c.conn.send(&Message::new(surf, 2).i32(0).i32(0).i32(w).i32(h)); // damage
        c.conn.send(&Message::new(surf, 6)); // commit -> present
        drive(c, display, state);
        unsafe { libc::close(fd) };
        buffer
    }

    #[test]
    fn zero_copy_buffers_release_only_on_out_of_order_gpu_completion_and_reclaim_on_teardown() {
        let mut display: Display<DdState> = Display::new().unwrap();
        let mut dh = display.handle();
        let mut state = DdState::new(dh.clone(), Box::new(NullPresenter));
        let (cfd, sfd) = socketpair();
        dh.insert_client(unsafe { std::os::unix::net::UnixStream::from_raw_fd(sfd) }, Arc::new(state.new_client_state())).unwrap();
        let mut c = Cli::new(cfd);
        macro_rules! pump {
            () => {{
                c.conn.flush().unwrap();
                display.dispatch_clients(&mut state).unwrap();
                display.flush_clients().unwrap();
                c.drain();
            }};
        }
        let reg = c.alloc();
        c.conn.send(&Message::new(1, 1).u32(reg));
        pump!();
        let comp = c.bind("wl_compositor", 4);
        let shm = c.bind("wl_shm", 1);
        let wm = c.bind("xdg_wm_base", 1);
        pump!();

        // Two toplevels (sid 1, sid 2), each with a committed shm buffer the server now retains.
        let buf1 = map_and_commit(&mut c, &mut display, &mut state, comp, shm, wm);
        pump!();
        let buf2 = map_and_commit(&mut c, &mut display, &mut state, comp, shm, wm);
        pump!();
        // The shm-copy path released each buffer once already (exact copy completion). Record that base.
        let base1 = c.releases(buf1);
        let base2 = c.releases(buf2);
        assert_eq!((base1, base2), (1, 1), "shm copy completion releases each buffer exactly once");

        let wl1 = state.buffers.get(&1).cloned().expect("surface 1 buffer retained");
        let wl2 = state.buffers.get(&2).cloned().expect("surface 2 buffer retained");

        // Re-tag both as zero-copy uses and submit them under present serials 1 and 2 (the exact
        // begin/submit the real dmabuf commit path runs). Nothing is released yet (no completion reported).
        state.begin_buffer_use(1, wl1, BufferUseKind::ZeroCopy);
        state.submit_zero_copy_use(1, 1);
        state.begin_buffer_use(2, wl2, BufferUseKind::ZeroCopy);
        state.submit_zero_copy_use(2, 2);
        let totals = state.render_budget_totals();
        assert_eq!(totals.dmabuf_imports, 2, "two zero-copy imports charged");
        assert_eq!(totals.executor_allocations, 2, "two in-flight executor allocations charged");
        pump!();
        assert_eq!(c.releases(buf1) - base1, 0, "buffer 1 held until its GPU serial completes");
        assert_eq!(c.releases(buf2) - base2, 0, "buffer 2 held until its GPU serial completes");

        // OUT-OF-ORDER completion: serial 2 (submitted second) completes FIRST.
        state.retire_completed_buffer_uses(&[2]);
        pump!();
        assert_eq!(c.releases(buf2) - base2, 1, "buffer 2 releases as soon as serial 2 completes");
        assert_eq!(c.releases(buf1) - base1, 0, "buffer 1 stays retained while serial 1 is pending");
        let totals = state.render_budget_totals();
        assert_eq!(totals.dmabuf_imports, 1, "buffer 2's import charge refunded on release");
        assert_eq!(totals.executor_allocations, 1, "buffer 2's executor allocation refunded on release");

        // Now serial 1 completes.
        state.retire_completed_buffer_uses(&[1]);
        pump!();
        assert_eq!(c.releases(buf1) - base1, 1, "buffer 1 releases once serial 1 completes");
        let totals = state.render_budget_totals();
        assert_eq!(totals.dmabuf_imports, 0, "all zero-copy import charges refunded");
        assert_eq!(totals.executor_allocations, 0, "all executor allocations refunded");

        // ROW 1 (zero-copy reclaim): a still-in-flight GPU fence is reclaimed on surface teardown.
        let wl1b = state.buffers.get(&1).cloned().unwrap();
        state.begin_buffer_use(1, wl1b, BufferUseKind::ZeroCopy);
        state.submit_zero_copy_use(1, 3);
        assert_eq!(state.render_budget_totals().executor_allocations, 1, "a fresh in-flight zero-copy use is charged");
        assert!(state.surface_fences.contains_key(&1), "surface 1 owns an in-flight GPU fence");
        // Destroy surface 1: fence + executor allocation reclaimed (fence_drop), buffer released.
        let surface1_res = state.surface_resources.get(&1).cloned().unwrap();
        state.teardown_surface(&surface1_res);
        assert!(!state.surface_fences.contains_key(&1), "teardown reclaims the in-flight GPU fence");
        assert_eq!(state.render_budget_totals().executor_allocations, 0, "teardown refunds the in-flight executor allocation");

        // ROW 2 (fds dimension): plane-fd accounting uses the same atomic reserve/refund as the other
        // dimensions (the shm proxy carries no dmabuf planes, so exercise the fd charge path directly).
        let owner = state.surface_owners.get(&2).cloned().expect("surface 2 owner");
        assert!(state.charge_budget(&owner, BudgetDim::Fds, 3), "fds charge succeeds within budget");
        assert_eq!(state.render_budget_totals().fds, 3, "plane fds are charged");
        state.refund_budget(&owner, BudgetDim::Fds, 3);
        assert_eq!(state.render_budget_totals().fds, 0, "plane fds refund exactly");
    }
}

#[cfg(test)]
mod region_occlusion_and_pacing_tests {
    //! ROW proofs for `compositor_honors_input_and_opaque_regions_through_surface_transforms` (opaque-
    //! region occlusion + input regions through a buffer transform) and
    //! `compositor_minimize_and_occlusion_control_native_visibility_and_frame_pacing` (host-occlusion /
    //! minimize frame-pacing state machine). CPU path, PngPresenter/NullPresenter; the mac AppKit observer
    //! (`MetalPresenter::surface_visibility` reading `NSWindowOcclusionState`) is mac-gated and drives the
    //! same `note_host_window_visibility` transitions proven here.

    use super::*;
    use crate::handlers::compositor::region_covers_rect;
    use dd_display::present::{PresentError, PresentOutcome, Presenter, SurfaceBuffer};
    use dd_display::wire::{Conn, Message};
    use smithay::reexports::wayland_server::Display;
    use smithay::utils::{Logical, Rectangle};
    use smithay::wayland::compositor::{RectangleKind, RegionAttributes};
    use std::os::unix::io::{FromRawFd, RawFd};

    struct P {
        frames: u32,
    }
    impl Presenter for P {
        fn present(&mut self, _s: &SurfaceBuffer) -> Result<PresentOutcome, PresentError> {
            self.frames += 1;
            Ok(PresentOutcome::Delivered { serial: self.frames as u64, timing: None })
        }
        fn frame_count(&self) -> u32 {
            self.frames
        }
    }

    fn socketpair() -> (RawFd, RawFd) {
        let mut sv = [0i32; 2];
        assert_eq!(unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, sv.as_mut_ptr()) }, 0);
        for fd in sv {
            unsafe {
                let fl = libc::fcntl(fd, libc::F_GETFL);
                libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
            }
        }
        (sv[0], sv[1])
    }

    struct Cli {
        conn: Conn,
        next: u32,
        globals: HashMap<String, u32>,
        events: Vec<(u32, u16, Vec<u8>)>,
    }
    impl Cli {
        fn new(fd: RawFd) -> Cli {
            Cli { conn: Conn::new(fd), next: 2, globals: HashMap::new(), events: Vec::new() }
        }
        fn alloc(&mut self) -> u32 {
            let id = self.next;
            self.next += 1;
            id
        }
        fn drain(&mut self) {
            loop {
                match self.conn.fill() {
                    Ok(0) | Ok(-1) | Err(_) => break,
                    _ => {}
                }
            }
            while let Some(m) = self.conn.next_message() {
                self.events.push((m.object, m.opcode, m.body.to_vec()));
                if m.object == 2 && m.opcode == 0 {
                    let mut r = m.reader();
                    let name = r.u32();
                    let iface = r.string();
                    let _ = r.u32();
                    self.globals.entry(iface).or_insert(name);
                }
            }
        }
        fn bind(&mut self, iface: &str, ver: u32) -> u32 {
            let id = self.alloc();
            let name = self.globals[iface];
            self.conn.send(&Message::new(2, 0).u32(name).string(iface).u32(ver).u32(id));
            id
        }
        fn saw(&self, object: u32, opcode: u16) -> bool {
            self.events.iter().any(|(o, op, _)| *o == object && *op == opcode)
        }
        fn xdg_configure_serial(&self, xdg: u32) -> Option<u32> {
            self.events.iter().rev().find(|(o, op, _)| *o == xdg && *op == 0)
                .map(|(_, _, b)| u32::from_ne_bytes([b[0], b[1], b[2], b[3]]))
        }
    }

    struct Harness {
        display: Display<DdState>,
        state: DdState,
        c: Cli,
        comp: u32,
        subc: u32,
        shm: u32,
        wm: u32,
    }
    impl Harness {
        fn new() -> Harness {
            let display: Display<DdState> = Display::new().unwrap();
            let mut dh = display.handle();
            let state = DdState::new(dh.clone(), Box::new(P { frames: 0 }));
            let (cfd, sfd) = socketpair();
            dh.insert_client(unsafe { std::os::unix::net::UnixStream::from_raw_fd(sfd) }, Arc::new(ClientState::default())).unwrap();
            let mut h = Harness { display, state, c: Cli::new(cfd), comp: 0, subc: 0, shm: 0, wm: 0 };
            let reg = h.c.alloc();
            h.c.conn.send(&Message::new(1, 1).u32(reg));
            h.pump();
            h.comp = h.c.bind("wl_compositor", 4);
            h.subc = h.c.bind("wl_subcompositor", 1);
            h.shm = h.c.bind("wl_shm", 1);
            h.wm = h.c.bind("xdg_wm_base", 1);
            h.pump();
            h
        }
        fn pump(&mut self) {
            self.c.conn.flush().unwrap();
            self.display.dispatch_clients(&mut self.state).unwrap();
            self.display.flush_clients().unwrap();
            self.c.drain();
        }
        fn shm_buffer(&mut self, w: i32, h: i32) -> u32 {
            let stride = w * 4;
            let size = (stride * h) as usize;
            let fd = dd_display::keymap::anon_fd_with(&vec![0x30u8; size]).unwrap();
            let pool = self.c.alloc();
            self.c.conn.send(&Message::new(self.shm, 0).u32(pool).u32(size as u32));
            self.c.conn.queue_fd(fd);
            let buffer = self.c.alloc();
            self.c.conn.send(&Message::new(pool, 0).u32(buffer).i32(0).i32(w).i32(h).i32(stride).u32(1));
            self.pump();
            unsafe { libc::close(fd) };
            buffer
        }
        /// Map a toplevel and complete the configure/ack handshake. Returns (surface, xdg, toplevel).
        fn map_toplevel(&mut self) -> (u32, u32, u32) {
            let surf = self.c.alloc();
            self.c.conn.send(&Message::new(self.comp, 0).u32(surf));
            let xdg = self.c.alloc();
            self.c.conn.send(&Message::new(self.wm, 2).u32(xdg).u32(surf));
            let top = self.c.alloc();
            self.c.conn.send(&Message::new(xdg, 1).u32(top));
            self.c.conn.send(&Message::new(surf, 6));
            self.pump();
            let serial = self.c.xdg_configure_serial(xdg).expect("configure serial");
            self.c.conn.send(&Message::new(xdg, 4).u32(serial));
            self.pump();
            (surf, xdg, top)
        }
        fn opaque_region(&mut self, surf: u32, x: i32, y: i32, w: i32, h: i32) {
            let region = self.c.alloc();
            self.c.conn.send(&Message::new(self.comp, 1).u32(region)); // create_region
            self.c.conn.send(&Message::new(region, 1).i32(x).i32(y).i32(w).i32(h)); // region.add
            self.c.conn.send(&Message::new(surf, 4).u32(region)); // set_opaque_region
        }
        fn surface(&self, sid: u32) -> WlSurface {
            self.state.surface_resources.get(&sid).cloned().expect("surface registered")
        }
    }

    /// The conservative coverage predicate: an Add rect must fully contain the target, and any touching
    /// Subtract punches a hole. Pure, so it proves the occlusion math independent of the wire.
    #[test]
    fn region_covers_rect_is_conservative() {
        let r = |kind, x, y, w, h| (kind, Rectangle::<i32, Logical>::from_loc_and_size((x, y), (w, h)));
        let full = RegionAttributes { rects: vec![r(RectangleKind::Add, 0, 0, 32, 32)] };
        assert!(region_covers_rect(&full, 0, 0, 32, 32), "add rect exactly covers");
        assert!(region_covers_rect(&full, 4, 4, 8, 8), "add rect covers an interior rect");
        assert!(!region_covers_rect(&full, -1, 0, 32, 32), "a rect poking outside is not covered");
        assert!(!region_covers_rect(&full, 0, 0, 0, 0), "an empty rect is never covered");
        let holed = RegionAttributes {
            rects: vec![r(RectangleKind::Add, 0, 0, 32, 32), r(RectangleKind::Subtract, 10, 10, 4, 4)],
        };
        assert!(!region_covers_rect(&holed, 0, 0, 32, 32), "a subtract touching the target breaks coverage");
        assert!(region_covers_rect(&holed, 0, 0, 8, 8), "a rect clear of the hole is still covered");
        let partial = RegionAttributes { rects: vec![r(RectangleKind::Add, 0, 0, 16, 32)] };
        assert!(!region_covers_rect(&partial, 0, 0, 32, 32), "a partial add does not cover the whole");
    }

    /// ROW: an opaque subsurface occludes the base beneath it — damaging only the occluded base does not
    /// force a present (`tree_dirty` == false); damaging the un-occluded overlay does.
    #[test]
    fn opaque_overlay_occludes_base_damage() {
        let mut h = Harness::new();
        let (base, _bx, _bt) = h.map_toplevel();
        let bbuf = h.shm_buffer(32, 32);
        h.c.conn.send(&Message::new(base, 1).u32(bbuf).i32(0).i32(0)); // attach base

        // A subsurface exactly over the base, declared fully opaque.
        let child = h.c.alloc();
        h.c.conn.send(&Message::new(h.comp, 0).u32(child)); // create_surface
        let sub = h.c.alloc();
        h.c.conn.send(&Message::new(h.subc, 1).u32(sub).u32(child).u32(base)); // get_subsurface
        h.opaque_region(child, 0, 0, 32, 32);
        let cbuf = h.shm_buffer(32, 32);
        h.c.conn.send(&Message::new(child, 1).u32(cbuf).i32(0).i32(0)); // attach child
        h.c.conn.send(&Message::new(child, 6)); // commit child (sync)
        h.c.conn.send(&Message::new(base, 6)); // commit base -> applies child
        h.pump();

        let base_res = h.surface(1);
        assert_eq!(h.state.surface_logical_size(&base_res), Some((32, 32)), "base logical size");
        assert_eq!(h.state.surface_logical_size(&h.surface(2)), Some((32, 32)), "child logical size");

        // Only the base changed, and it is fully hidden under the opaque child: no visible change.
        h.state.dirty.clear();
        h.state.dirty.insert(1);
        assert!(!h.state.tree_dirty(&base_res), "base damage under a fully-opaque overlay is occluded");

        // The overlay itself changed: visible → dirty.
        h.state.dirty.clear();
        h.state.dirty.insert(2);
        assert!(h.state.tree_dirty(&base_res), "overlay damage is never occluded");

        // Both dirty → dirty (the overlay's change shows).
        h.state.dirty.clear();
        h.state.dirty.insert(1);
        h.state.dirty.insert(2);
        assert!(h.state.tree_dirty(&base_res), "a visible overlay change keeps the tree dirty");
    }

    /// ROW: a NON-opaque (or absent-opaque-region) overlay does NOT occlude — the base's damage stays
    /// visible. Proves occlusion is driven by the committed opaque region, not mere overlap.
    #[test]
    fn non_opaque_overlay_does_not_occlude() {
        let mut h = Harness::new();
        let (base, _bx, _bt) = h.map_toplevel();
        let bbuf = h.shm_buffer(32, 32);
        h.c.conn.send(&Message::new(base, 1).u32(bbuf).i32(0).i32(0));
        let child = h.c.alloc();
        h.c.conn.send(&Message::new(h.comp, 0).u32(child));
        let sub = h.c.alloc();
        h.c.conn.send(&Message::new(h.subc, 1).u32(sub).u32(child).u32(base));
        // NO opaque region set on the child.
        let cbuf = h.shm_buffer(32, 32);
        h.c.conn.send(&Message::new(child, 1).u32(cbuf).i32(0).i32(0));
        h.c.conn.send(&Message::new(child, 6));
        h.c.conn.send(&Message::new(base, 6));
        h.pump();

        let base_res = h.surface(1);
        h.state.dirty.clear();
        h.state.dirty.insert(1);
        assert!(h.state.tree_dirty(&base_res), "base damage under a non-opaque overlay is still visible");
    }

    /// ROW: input regions are honored in UPRIGHT LOGICAL surface space (post buffer-transform). A 90°
    /// buffer swaps the surface's logical width/height, and both the input-region hit test and the
    /// bounds test use that logical geometry — not raw buffer pixels. Also proves the infinite (absent)
    /// input-region default accepts the whole surface.
    #[test]
    fn input_regions_apply_through_buffer_transform() {
        let mut h = Harness::new();
        let (surf, _x, _t) = h.map_toplevel();
        // Buffer 40x20; a 90° transform makes the upright logical surface 20 wide x 40 tall.
        let buf = h.shm_buffer(40, 20);
        h.c.conn.send(&Message::new(surf, 7).u32(1)); // set_buffer_transform(90)
        h.c.conn.send(&Message::new(surf, 1).u32(buf).i32(0).i32(0)); // attach
        h.c.conn.send(&Message::new(surf, 6)); // commit
        h.pump();

        let root = h.surface(1);
        assert_eq!(h.state.surface_logical_size(&root), Some((20, 40)), "90° transform swaps logical w/h");

        // Infinite (no) input region: any point inside the LOGICAL bounds is accepted.
        assert!(h.state.input_surface_at(&root, 5.0, 5.0).is_some(), "infinite input region accepts inside");
        assert!(h.state.input_surface_at(&root, 5.0, 35.0).is_some(), "logical height is 40 (transformed)");
        // A point past the LOGICAL width (20) is out of bounds — even though the raw buffer is 40 wide.
        assert!(h.state.input_surface_at(&root, 25.0, 5.0).is_none(), "bounds use transformed logical width");

        // Now restrict the input region to the left logical half and prove the hit test respects it.
        let region = h.c.alloc();
        h.c.conn.send(&Message::new(h.comp, 1).u32(region));
        h.c.conn.send(&Message::new(region, 1).i32(0).i32(0).i32(10).i32(40)); // add left half (logical)
        h.c.conn.send(&Message::new(surf, 5).u32(region)); // set_input_region
        h.c.conn.send(&Message::new(surf, 6));
        h.pump();
        assert!(h.state.input_surface_at(&root, 5.0, 20.0).is_some(), "inside the input region hits");
        assert!(h.state.input_surface_at(&root, 15.0, 20.0).is_none(), "outside the input region misses");
    }

    /// ROW: host occlusion / minimize frame-pacing state machine. A fully occluded (or minimized) window
    /// PAUSES its guest — the committed `wl_surface.frame` callback is withheld and no present happens,
    /// but the last frame + callback are retained. Revealing the window RESUMES: the retained content is
    /// presented once and the retained callback fires.
    #[test]
    fn host_occlusion_pauses_and_reveal_resumes_frame_pacing() {
        let mut h = Harness::new();
        let (surf, _x, _t) = h.map_toplevel();

        // Frame 1: visible commit with a frame callback → presented, callback fires.
        let buf = h.shm_buffer(16, 16);
        let cb1 = h.c.alloc();
        h.c.conn.send(&Message::new(surf, 3).u32(cb1)); // frame(cb1)
        h.c.conn.send(&Message::new(surf, 1).u32(buf).i32(0).i32(0));
        h.c.conn.send(&Message::new(surf, 2).i32(0).i32(0).i32(16).i32(16));
        h.c.conn.send(&Message::new(surf, 6));
        h.pump();
        assert!(h.c.saw(cb1, 0), "a visible frame fires its wl_surface.frame callback");
        let frames_visible = h.state.presenter.frame_count();
        assert!(frames_visible >= 1, "the visible frame was presented");

        // Host reports the native window fully occluded.
        assert!(h.state.note_host_window_visibility(1, /*occluded*/ true, /*minimized*/ false));

        // A commit while occluded: request a callback, damage the surface. It must NOT present and the
        // callback must NOT fire — the guest is paused.
        let buf2 = h.shm_buffer(16, 16);
        let cb2 = h.c.alloc();
        h.c.conn.send(&Message::new(surf, 3).u32(cb2));
        h.c.conn.send(&Message::new(surf, 1).u32(buf2).i32(0).i32(0));
        h.c.conn.send(&Message::new(surf, 2).i32(0).i32(0).i32(16).i32(16));
        h.c.conn.send(&Message::new(surf, 6));
        h.pump();
        assert!(!h.c.saw(cb2, 0), "an occluded window withholds the frame callback (guest paused)");
        assert_eq!(h.state.presenter.frame_count(), frames_visible, "no present happens while occluded");

        // Reveal: the retained frame is presented once and the retained callback fires — guest resumes.
        assert!(h.state.note_host_window_visibility(1, false, false));
        h.pump();
        assert!(h.c.saw(cb2, 0), "revealing the window fires the retained frame callback (guest resumes)");
        assert!(h.state.presenter.frame_count() > frames_visible, "reveal presents the retained content once");
    }

    /// ROW: the client protocol `xdg_toplevel.set_minimized` hides the window and pauses pacing (the
    /// compositor's own visibility state), and a host reveal restores it.
    #[test]
    fn protocol_minimize_hides_then_host_reveal_restores() {
        let mut h = Harness::new();
        let (surf, _x, top) = h.map_toplevel();
        let buf = h.shm_buffer(16, 16);
        h.c.conn.send(&Message::new(surf, 1).u32(buf).i32(0).i32(0));
        h.c.conn.send(&Message::new(surf, 6));
        h.pump();
        let root = h.surface(1);
        assert!(h.state.root_is_visible(&root), "mapped toplevel is visible");

        // xdg_toplevel.set_minimized (opcode 13).
        h.c.conn.send(&Message::new(top, 13));
        h.pump();
        assert!(!h.state.root_is_visible(&root), "set_minimized hides the window");

        // Host reveal restores visibility.
        assert!(h.state.note_host_window_visibility(1, false, false));
        assert!(h.state.root_is_visible(&root), "host reveal restores visibility");
    }
}

#[cfg(test)]
mod xwayland_window_model_tests {
    //! In-process proof of the XWayland bridge's feature-independent core (`adopt_x11_window` /
    //! `withdraw_x11_window`): a roleless `wl_surface` — exactly what Xwayland creates for an X11 window —
    //! adopted into the window model presents through the SAME commit→present path as an `xdg_toplevel`,
    //! carries the X11 title, takes keyboard focus, and is recorded as an X11 window; withdraw drops the
    //! native presenter window and clears focus. Runs offline with no live Xwayland (the `XwmHandler` that
    //! drives these calls is thin glue behind `--features xwayland`, unbuildable on this egress-blocked
    //! host — see `handlers/xwayland.rs`).

    use super::*;
    use dd_display::present::{PresentError, PresentOutcome, Presenter, SurfaceBuffer};
    use dd_display::wire::{Conn, Message};
    use smithay::reexports::wayland_server::Display;
    use std::os::unix::io::{FromRawFd, RawFd};

    struct XP {
        frames: u32,
        last: Arc<Mutex<Option<(u32, String)>>>,
        dropped: Arc<Mutex<Vec<u32>>>,
    }
    impl Presenter for XP {
        fn present(&mut self, surf: &SurfaceBuffer) -> Result<PresentOutcome, PresentError> {
            self.frames += 1;
            *self.last.lock().unwrap() = Some((surf.sid, surf.title.clone()));
            Ok(PresentOutcome::Delivered { serial: self.frames as u64, timing: None })
        }
        fn frame_count(&self) -> u32 {
            self.frames
        }
        fn drop_window(&mut self, sid: u32) {
            self.dropped.lock().unwrap().push(sid);
        }
    }

    fn socketpair() -> (RawFd, RawFd) {
        let mut sv = [0i32; 2];
        assert_eq!(unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, sv.as_mut_ptr()) }, 0);
        for fd in sv {
            unsafe {
                let fl = libc::fcntl(fd, libc::F_GETFL);
                libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
            }
        }
        (sv[0], sv[1])
    }

    #[test]
    #[allow(unused_assignments)]
    fn adopted_x11_window_presents_focuses_and_withdraws() {
        let display: Display<DdState> = Display::new().unwrap();
        let mut dh = display.handle();
        let mut display = display;
        let last = Arc::new(Mutex::new(None));
        let dropped = Arc::new(Mutex::new(Vec::new()));
        let mut state = DdState::new(dh.clone(), Box::new(XP { frames: 0, last: last.clone(), dropped: dropped.clone() }));
        let (cfd, sfd) = socketpair();
        dh.insert_client(unsafe { std::os::unix::net::UnixStream::from_raw_fd(sfd) }, Arc::new(ClientState::default())).unwrap();
        let mut conn = Conn::new(cfd);
        let mut globals: HashMap<String, u32> = HashMap::new();
        let mut next = 2u32;
        macro_rules! alloc { () => {{ let i = next; next += 1; i }}; }
        macro_rules! pump {
            () => {{
                conn.flush().unwrap();
                display.dispatch_clients(&mut state).unwrap();
                display.flush_clients().unwrap();
                loop { match conn.fill() { Ok(0) | Ok(-1) | Err(_) => break, _ => {} } }
                while let Some(m) = conn.next_message() {
                    if m.object == 2 && m.opcode == 0 {
                        let mut r = m.reader();
                        let name = r.u32(); let iface = r.string(); let _ = r.u32();
                        globals.entry(iface).or_insert(name);
                    }
                }
            }};
        }
        let reg = alloc!();
        conn.send(&Message::new(1, 1).u32(reg));
        pump!();
        let comp = alloc!();
        conn.send(&Message::new(2, 0).u32(globals["wl_compositor"]).string("wl_compositor").u32(4).u32(comp));
        let shm = alloc!();
        conn.send(&Message::new(2, 0).u32(globals["wl_shm"]).string("wl_shm").u32(1).u32(shm));
        pump!();

        // Xwayland creates a plain (roleless) wl_surface for the X11 window and attaches the window pixmap.
        let surf = alloc!();
        conn.send(&Message::new(comp, 0).u32(surf)); // create_surface -> host sid 1
        let (w, h) = (16i32, 16i32);
        let stride = w * 4;
        let size = (stride * h) as usize;
        let fd = dd_display::keymap::anon_fd_with(&vec![0x50u8; size]).unwrap();
        let pool = alloc!();
        conn.send(&Message::new(shm, 0).u32(pool).u32(size as u32));
        conn.queue_fd(fd);
        let buffer = alloc!();
        conn.send(&Message::new(pool, 0).u32(buffer).i32(0).i32(w).i32(h).i32(stride).u32(1));
        conn.send(&Message::new(surf, 1).u32(buffer).i32(0).i32(0)); // attach
        conn.send(&Message::new(surf, 6)); // commit → ingested
        pump!();
        unsafe { libc::close(fd) };

        let wl = state.surface_resources.get(&1).cloned().expect("X11 wl_surface registered");
        assert!(!state.is_x11_window(1), "not yet adopted");

        // The XwmHandler's map_window_request does exactly this: adopt the X11 window with its title.
        state.adopt_x11_window(&wl, "xterm — user@host".to_string());

        // It presented through the ordinary path, carrying the X11 title, and took keyboard focus.
        let (sid, title) = last.lock().unwrap().clone().expect("adopted X11 window presented");
        assert_eq!(sid, 1, "the X11 window's own host surface presented");
        assert_eq!(title, "xterm — user@host", "the X11 window presents with its X11 title");
        assert!(state.is_x11_window(1), "surface is recorded as an X11 window");
        assert_eq!(state.focus.as_ref(), Some(&wl), "the mapped X11 window took keyboard focus");

        // Withdraw (X11 unmap/destroy): the native presenter window is dropped and focus cleared.
        state.withdraw_x11_window(&wl);
        assert!(dropped.lock().unwrap().contains(&1), "withdraw drops the native presenter window");
        assert!(state.focus.is_none(), "withdraw clears focus the X11 window held");
        assert!(!state.is_x11_window(1), "withdraw clears the X11-window record");
    }
}

#[cfg(test)]
mod input_routing_tests {
    //! In-process proof of the multi-window + Chrome split-client input router
    //! (`handlers::input_routing`). Two clients on one Display: A = the "browser" connection (owns the seat
    //! + an xdg toplevel with window geometry → input-capable), B = the "gpu/shim" connection (commits the
    //! visible surface but never holds focus → NOT input-capable). Proves the routing DECISION and the
    //! geometry-mirror state machine without a live macOS multi-window app (the AppKit `window_ptr_to_sid`
    //! → route → deliver wiring in `main.rs` is macOS-only and validated live).

    use super::*;
    use crate::handlers::input_routing::{ExternalLogicalCrop, PointerRoute};
    use dd_display::present::{PresentError, PresentOutcome, Presenter, SurfaceBuffer};
    use dd_display::wire::{Conn, Message};
    use smithay::reexports::wayland_server::Display;
    use std::os::unix::io::{FromRawFd, RawFd};

    struct P;
    impl Presenter for P {
        fn present(&mut self, _s: &SurfaceBuffer) -> Result<PresentOutcome, PresentError> {
            Ok(PresentOutcome::Delivered { serial: 1, timing: None })
        }
    }

    fn sp() -> (RawFd, RawFd) {
        let mut sv = [0i32; 2];
        assert_eq!(unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, sv.as_mut_ptr()) }, 0);
        for fd in sv {
            unsafe {
                let fl = libc::fcntl(fd, libc::F_GETFL);
                libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
            }
        }
        (sv[0], sv[1])
    }

    struct Cli {
        conn: Conn,
        next: u32,
        globals: HashMap<String, u32>,
    }
    impl Cli {
        fn new(fd: RawFd) -> Cli {
            Cli { conn: Conn::new(fd), next: 2, globals: HashMap::new() }
        }
        fn alloc(&mut self) -> u32 {
            let i = self.next;
            self.next += 1;
            i
        }
        fn drain(&mut self) -> Vec<(u32, u16, Vec<u8>)> {
            loop {
                match self.conn.fill() {
                    Ok(0) | Ok(-1) | Err(_) => break,
                    _ => {}
                }
            }
            let mut out = Vec::new();
            while let Some(m) = self.conn.next_message() {
                if m.object == 2 && m.opcode == 0 {
                    let mut r = m.reader();
                    let name = r.u32();
                    let iface = r.string();
                    let _ = r.u32();
                    self.globals.entry(iface).or_insert(name);
                }
                out.push((m.object, m.opcode, m.body.to_vec()));
            }
            out
        }
        fn bind(&mut self, iface: &str, ver: u32) -> u32 {
            let id = self.alloc();
            let name = self.globals[iface];
            self.conn.send(&Message::new(2, 0).u32(name).string(iface).u32(ver).u32(id));
            id
        }
    }

    #[test]
    fn split_client_routing_and_geometry_mirror() {
        let display: Display<DdState> = Display::new().unwrap();
        let mut dh = display.handle();
        let mut display = display;
        let mut state = DdState::new(dh.clone(), Box::new(P));

        let (acf, asf) = sp();
        dh.insert_client(unsafe { std::os::unix::net::UnixStream::from_raw_fd(asf) }, Arc::new(ClientState::default())).unwrap();
        let (bcf, bsf) = sp();
        dh.insert_client(unsafe { std::os::unix::net::UnixStream::from_raw_fd(bsf) }, Arc::new(ClientState::default())).unwrap();
        let mut a = Cli::new(acf);
        let mut b = Cli::new(bcf);

        macro_rules! pump {
            () => {{
                a.conn.flush().unwrap();
                b.conn.flush().unwrap();
                display.dispatch_clients(&mut state).unwrap();
                display.flush_clients().unwrap();
                a.drain();
                b.drain();
            }};
        }

        // Registry for both.
        let ra = a.alloc();
        a.conn.send(&Message::new(1, 1).u32(ra));
        let rb = b.alloc();
        b.conn.send(&Message::new(1, 1).u32(rb));
        pump!();

        // Client A = browser/input connection: wl_compositor + xdg_wm_base, map a toplevel with window
        // geometry. Mapping takes keyboard focus → A becomes input-capable. (sid 1)
        let acomp = a.bind("wl_compositor", 4);
        let awm = a.bind("xdg_wm_base", 1);
        pump!();
        let asurf = a.alloc();
        a.conn.send(&Message::new(acomp, 0).u32(asurf));
        let axdg = a.alloc();
        a.conn.send(&Message::new(awm, 2).u32(axdg).u32(asurf)); // get_xdg_surface
        let atop = a.alloc();
        a.conn.send(&Message::new(axdg, 1).u32(atop)); // get_toplevel
        a.conn.send(&Message::new(axdg, 3).i32(0).i32(0).i32(800).i32(600)); // set_window_geometry
        a.conn.send(&Message::new(asurf, 6)); // commit → maps → focus
        pump!();

        // Client B = gpu/shim connection: just a wl_compositor surface with a committed buffer, never
        // focused → NOT input-capable. (sid 2)
        let bcomp = b.bind("wl_compositor", 4);
        let bshm = b.bind("wl_shm", 1);
        pump!();
        let bsurf = b.alloc();
        b.conn.send(&Message::new(bcomp, 0).u32(bsurf));
        let (w, h) = (1000i32, 1000i32);
        let stride = w * 4;
        let size = (stride * h) as usize;
        let fd = dd_display::keymap::anon_fd_with(&vec![0u8; size]).unwrap();
        let pool = b.alloc();
        b.conn.send(&Message::new(bshm, 0).u32(pool).u32(size as u32));
        b.conn.queue_fd(fd);
        let bbuf = b.alloc();
        b.conn.send(&Message::new(pool, 0).u32(bbuf).i32(0).i32(w).i32(h).i32(stride).u32(1));
        b.conn.send(&Message::new(bsurf, 1).u32(bbuf).i32(0).i32(0));
        b.conn.send(&Message::new(bsurf, 6));
        pump!();
        unsafe { libc::close(fd) };

        // A (host sid 1) is input-capable; B (host sid 2) is not.
        assert!(state.surface_can_receive_input(1), "browser connection can receive input");
        assert!(!state.surface_can_receive_input(2), "gpu/shim connection cannot receive input");

        // Routing: a click on A's window delivers to A; a click on B's (visible) window FORWARDS to A.
        assert_eq!(state.route_window_input(1), PointerRoute::Target { sid: 1 });
        assert_eq!(
            state.route_window_input(2),
            PointerRoute::Forward { target_sid: 1, via_sid: 2 },
            "a click on the gpu window forwards to the browser toplevel"
        );

        // A's focused logical geometry comes from its xdg window geometry.
        let geo = state.focused_logical_geometry(1).expect("A has geometry");
        assert_eq!((geo.w, geo.h, geo.source), (800, 600, "xdg_window_geometry"));

        // Geometry mirror is gated on the env knob (parity with legacy DD_DISPLAY_MIRROR_INPUT_GEOMETRY).
        assert_eq!(state.mirrored_input_crop(2), None, "mirror off by default");
        std::env::set_var("DD_DISPLAY_MIRROR_INPUT_GEOMETRY", "1");
        assert_eq!(
            state.mirrored_input_crop(2),
            Some(ExternalLogicalCrop { source_sid: 1, x: 0, y: 0, w: 800, h: 600, source: "xdg_window_geometry" }),
            "the browser geometry mirrors onto the gpu surface"
        );
        // The mirror never crops the input surface itself.
        assert_eq!(state.mirrored_input_crop(1), None);
        std::env::remove_var("DD_DISPLAY_MIRROR_INPUT_GEOMETRY");

        // apply_external_crop narrows the visible (gpu) surface's presented region to the mirrored window.
        state.set_external_logical_crop(Some(ExternalLogicalCrop { source_sid: 1, x: 0, y: 0, w: 800, h: 600, source: "xdg_window_geometry" }));
        let mut sb = SurfaceBuffer {
            sid: 2, width: 1000, height: 1000, texture_width: 1000, texture_height: 1000, stride: 4000,
            format: 1, bgra: Vec::new(), title: "gpu".into(), iosurface_id: Some(9), gpu_render: false,
            uv_rect: [0.0, 0.0, 1.0, 1.0], damage: None, popup: None, overlays: Vec::new(),
        };
        state.apply_external_crop(&mut sb, 2);
        assert_eq!((sb.width, sb.height), (800, 600), "gpu surface cropped to the browser window size");
        assert_eq!(sb.uv_rect, [0.0, 0.0, 0.8, 0.6], "backing sample rect narrowed to the crop");
        // The crop is not applied to the input surface (sid 1).
        let mut sb1 = SurfaceBuffer {
            sid: 1, width: 800, height: 600, texture_width: 800, texture_height: 600, stride: 3200,
            format: 1, bgra: Vec::new(), title: "browser".into(), iosurface_id: None, gpu_render: false,
            uv_rect: [0.0, 0.0, 1.0, 1.0], damage: None, popup: None, overlays: Vec::new(),
        };
        state.apply_external_crop(&mut sb1, 1);
        assert_eq!((sb1.width, sb1.height), (800, 600), "input surface is never cropped");
    }
}
