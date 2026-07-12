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
//!   - [`handlers::output`] — `wl_output`.
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

use std::collections::HashMap;
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
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::{wl_buffer::WlBuffer, wl_surface::WlSurface},
            DisplayHandle, Resource,
        },
    },
    utils::{Size, SERIAL_COUNTER},
    wayland::{
        compositor::{CompositorClientState, CompositorState},
        cursor_shape::CursorShapeManagerState,
        output::OutputManagerState,
        presentation::PresentationState,
        shell::xdg::{PopupSurface, XdgShellState},
        shm::ShmState,
        viewporter::ViewporterState,
    },
};

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
    pub seat_state: SeatState<Self>,
    pub output_manager: OutputManagerState,
    pub viewporter: ViewporterState,
    pub presentation: PresentationState,
    pub cursor_shape: CursorShapeManagerState,
    pub seat: Seat<Self>,
    pub keyboard: KeyboardHandle<Self>,
    pub pointer: PointerHandle<Self>,
    pub output: Output,

    /// The reused platform present half (`CocoaPresenter`/`MetalPresenter` on macOS, `PngPresenter`
    /// headless). Keyed internally by surface id — the same `u32` sid model as `server.rs`.
    pub presenter: Box<dyn Presenter>,

    /// The surface that currently has keyboard focus (the most recently mapped toplevel).
    pub focus: Option<WlSurface>,
    /// Per-surface window titles, so the Presenter can label each NSWindow.
    pub titles: HashMap<u32, String>,
    /// Last pointer location in logical/point space (Cocoa delivers point-space coords).
    pub ptr_loc: (f64, f64),

    /// Last committed `wl_shm` buffer per surface (`sid` → buffer). A subsurface or popup that redraws
    /// only occasionally does not re-attach a buffer every parent frame, yet its pixels must persist in
    /// the composited window; Smithay clears `current().buffer` on a bufferless commit, so we hold the
    /// last one here and re-read it whenever the root window re-composites (mirrors `server.rs`'s
    /// per-surface `current_buffer`). Also lets a popup/child commit re-present its ROOT toplevel.
    pub(crate) buffers: HashMap<u32, WlBuffer>,
    /// The active `xdg_popup` grab chain (outer→inner). A popup created with `xdg_popup.grab` is dismissed
    /// (with `popup_done`) together with its whole submenu chain when the user clicks outside it; the
    /// input/present loop drives that via [`DdState::dismiss_popup_grabs`]. Tooltips (mapped without a
    /// grab) are NOT listed here, so they are not torn down on an outside click.
    pub(crate) popup_grabs: Vec<PopupSurface>,
    /// Last on-screen window size we sent an `xdg_toplevel.configure` for, so a host-driven window
    /// resize is debounced to one configure per distinct size (mirrors `server.rs`'s `last_cfg`).
    pub(crate) last_cfg: Option<(i32, i32)>,
    /// `wp_presentation` MSC / vblank counter, bumped once per presented frame (mirrors `server.rs`'s
    /// `present_seq`). Chrome/viz feeds the sequence into its BeginFrame vsync estimator.
    pub(crate) present_seq: u64,
    pub(crate) start: Instant,
}

impl DdState {
    /// Stand up every global `server.rs` advertises by hand. `output_scale` comes from the Presenter so
    /// a Retina backing store advertises `wl_output.scale = 2` (HiDPI), matching `dd-display`'s
    /// `present_cocoa` HiDPI advert.
    pub fn new(dh: DisplayHandle, presenter: Box<dyn Presenter>) -> DdState {
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
        let viewporter = ViewporterState::new::<Self>(&dh); // wp_viewporter
        // wp_presentation: advertise the GUEST's CLOCK_MONOTONIC id (Linux == 1), NOT the host macOS
        // libc value, so the client interprets our `presented` timestamps in its own clock domain.
        let presentation = PresentationState::new::<Self>(&dh, CLOCK_MONOTONIC_LINUX);
        // wp_cursor_shape_manager_v1: themed cursor requests → CursorImageStatus::Named → NSCursor.
        let cursor_shape = CursorShapeManagerState::new::<Self>(&dh);

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
        output.create_global::<Self>(&dh);
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
            seat_state,
            output_manager,
            viewporter,
            presentation,
            cursor_shape,
            seat,
            keyboard,
            pointer,
            output,
            presenter,
            focus: None,
            titles: HashMap::new(),
            ptr_loc: (0.0, 0.0),
            buffers: HashMap::new(),
            popup_grabs: Vec::new(),
            last_cfg: None,
            present_seq: 0,
            start: Instant::now(),
        }
    }

    /// Milliseconds since construction, the timestamp domain for `wl_callback.done` / input events.
    pub(crate) fn now_ms(&self) -> u32 {
        self.start.elapsed().as_millis() as u32
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

    /// Make `surface` the keyboard focus (called when a toplevel maps).
    pub(crate) fn focus_surface(&mut self, surface: WlSurface) {
        self.focus = Some(surface.clone());
        let kbd = self.keyboard.clone();
        let serial = SERIAL_COUNTER.next_serial();
        kbd.set_focus(self, Some(surface), serial);
    }
}

/// The `wl_surface` id (`u32` protocol id) — the sid the Presenter keys windows by.
pub(crate) fn surface_id(surface: &WlSurface) -> u32 {
    surface.id().protocol_id()
}
