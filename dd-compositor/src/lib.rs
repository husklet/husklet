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
#[derive(Default)]
pub struct ClientState {
    pub compositor: CompositorClientState,
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
        }
    }
}

#[derive(Default)]
struct RenderUsage {
    surfaces: usize,
    retained_callbacks: usize,
    cpu_cache_bytes: usize,
}
impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
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

    /// Collision-free host identity for every live `wl_surface`. Wayland protocol ids are local to a
    /// client and may be reused after destroy; `ObjectId` includes both ownership and object generation.
    /// The monotonically allocated host id is what the existing Presenter ABI consumes.
    surface_ids: HashMap<ObjectId, u32>,
    next_surface_id: u32,
    presenter_windows: HashSet<u32>,
    surface_owners: HashMap<u32, ClientId>,
    surface_resources: HashMap<u32, WlSurface>,
    render_usage: HashMap<ClientId, RenderUsage>,
    global_render_usage: RenderUsage,
    render_limits: RenderLimits,
    shm_budget: Arc<Mutex<ShmBudgetLedger>>,
    surface_buffer_uses: HashMap<u32, BufferUse>,
    retired_zero_copy_uses: Vec<BufferUse>,
    next_buffer_use_generation: u64,
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
            popup_grabs: Vec::new(),
            last_cfg: None,
            start: Instant::now(),
            mod_mask: 0,
            host_clip_gen: 0,
            pending_host_copy: None,
            cursor_surface: None,
            cursor_hidden_by_lock: false,
            retained_callbacks: HashMap::new(),
            surface_ids: HashMap::new(),
            next_surface_id: 1,
            presenter_windows: HashSet::new(),
            surface_owners: HashMap::new(),
            surface_resources: HashMap::new(),
            render_usage: HashMap::new(),
            global_render_usage: RenderUsage::default(),
            render_limits,
            shm_budget: Arc::new(Mutex::new(ShmBudgetLedger::default())),
            surface_buffer_uses: HashMap::new(),
            retired_zero_copy_uses: Vec::new(),
            next_buffer_use_generation: 1,
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

    /// Reclaim every dd-owned resource associated with a surface. This is deliberately idempotent:
    /// role teardown and `wl_surface.destroy` can arrive through different protocol paths.
    pub(crate) fn teardown_surface(&mut self, surface: &WlSurface) {
        let Some(sid) = self.surface_ids.remove(&surface.id()) else {
            return;
        };
        self.release_surface_resources(sid);
        self.retire_buffer_use(sid);
        self.surface_resources.remove(&sid);
        if let Some(output) = self.surface_outputs.remove(&sid) {
            output.leave(surface);
        }
        self.buffers.remove(&sid);
        self.repacks.remove(&sid);
        self.dirty.remove(&sid);
        self.visibility.remove(&sid);
        self.titles.remove(&sid);
        self.retained_callbacks.remove(&sid);
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
        if self.presenter_windows.remove(&sid) {
            self.presenter.drop_window(sid);
        }
    }

    pub(crate) fn begin_buffer_use(&mut self, sid: u32, buffer: WlBuffer, kind: BufferUseKind) {
        self.retire_buffer_use(sid);
        let generation = self.next_buffer_use_generation;
        self.next_buffer_use_generation = self
            .next_buffer_use_generation
            .checked_add(1)
            .expect("buffer-use generation exhausted");
        self.surface_buffer_uses.insert(
            sid,
            BufferUse { buffer, generation, kind, released: false },
        );
    }

    pub(crate) fn complete_buffer_use(&mut self, sid: u32) {
        let Some(use_) = self.surface_buffer_uses.get_mut(&sid) else {
            return;
        };
        debug_assert!(use_.generation > 0);
        if !use_.released {
            use_.buffer.release();
            use_.released = true;
        }
    }

    pub(crate) fn retire_buffer_use(&mut self, sid: u32) {
        let Some(use_) = self.surface_buffer_uses.remove(&sid) else {
            return;
        };
        debug_assert!(use_.generation > 0);
        if !use_.released {
            if use_.kind == BufferUseKind::ShmCopy {
                use_.buffer.release();
            } else {
                // The Presenter ABI has no completion fence for a failed/scheduled zero-copy use. Keep
                // the proxy alive rather than issuing a premature release; a future completion-token
                // extension will retire this queue in serial order.
                self.retired_zero_copy_uses.push(use_);
                return;
            }
        }
    }

    pub(crate) fn forget_destroyed_buffer(&mut self, buffer: &WlBuffer) {
        self.surface_buffer_uses.retain(|_, use_| use_.buffer != *buffer);
        self.retired_zero_copy_uses.retain(|use_| use_.buffer != *buffer);
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
            usage.surfaces == 0 && usage.cpu_cache_bytes == 0 && usage.retained_callbacks == 0
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
