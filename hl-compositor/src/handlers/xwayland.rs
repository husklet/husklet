//! XWayland bridge — X11-only guest apps as first-class windows in the Smithay compositor.
//!
//! ## What this composes
//! Behind `--features xwayland` (see `Cargo.toml` for why it is not a declared cargo feature on the
//! offline dev host) this module composes Smithay's XWayland support into `DdState`:
//!   - [`smithay::xwayland::XWayland`] — spawns and supervises the `Xwayland` server as a subprocess of
//!     the compositor; `Xwayland` connects back to us as an ordinary Wayland client.
//!   - [`smithay::wayland::xwayland_shell::XWaylandShellState`] — the `xwayland_shell_v1` global that pairs
//!     each X11 window with its backing `wl_surface`.
//!   - [`smithay::xwayland::X11Wm`] — the X11 window manager; its [`XwmHandler`] callbacks (implemented
//!     here on `DdState`) map/unmap/configure/destroy X11 windows and route resize/move/state requests.
//!
//! ## X11 windows use the SAME present + input path as Wayland toplevels
//! An X11 window's pixels arrive as a normal `wl_surface` buffer commit (Xwayland is a Wayland client), so
//! the ordinary `commit → present_render_root` path shows it — an X11 window's `wl_surface` is roleless,
//! so it is its own `window_root` and presents as its own native window (Metal/IOSurface or shm) exactly
//! like an `xdg_toplevel`. On map we call [`DdState::adopt_x11_window`], which labels the surface with the
//! X11 title and gives it keyboard focus; pointer/keyboard input then reaches it through the same seat
//! hit-testing (`input_surface_at`) as any other window. No X11-specific present or input path exists.
//!
//! ## Runtime gating
//! [`DdState::start_xwayland`] is called only under `DD_XWAYLAND` (itself only reachable under
//! `DD_DISPLAY_SMITHAY`). On macOS the `Xwayland` binary is a Linux ELF, so it must be launched through
//! the dd JIT engine (a host-runtime concern); on the Linux dev host a native `/usr/bin/Xwayland` is used.
//!
//! ## Validation status
//! The window-management integration and the feature-independent core ([`DdState::adopt_x11_window`] /
//! [`DdState::withdraw_x11_window`], proven by the in-process `xwayland_window_model_tests`) are complete.
//! This feature-gated glue cannot be COMPILED or run on the offline dev host — Smithay's `xwayland`
//! feature pulls in `x11rb`/`encoding_rs`/etc. which are not in the cargo cache and cannot be fetched
//! (crates.io egress is TLS-intercepted). A live X11-app journey (xeyes/an X11 GTK app rendering, taking
//! input, and clipboard) is therefore PENDING a build with the deps available plus an `Xwayland` binary
//! reachable from the compositor. The clipboard/selection bridge below is structured against Smithay's
//! X11 selection API but is likewise unverified.

use std::collections::HashMap;
use std::os::fd::OwnedFd;
use std::process::Stdio;

use smithay::reexports::calloop::LoopHandle;
use smithay::reexports::wayland_server::{protocol::wl_surface::WlSurface, DisplayHandle};
use smithay::utils::{Logical, Rectangle};
use smithay::wayland::selection::SelectionTarget;
use smithay::wayland::xwayland_shell::{XWaylandShellHandler, XWaylandShellState};
use smithay::xwayland::xwm::{Reorder, ResizeEdge, X11Window, XwmId};
use smithay::xwayland::{X11Surface, X11Wm, XWayland, XWaylandEvent, XwmHandler};

use crate::DdState;

/// All XWayland bridge state, held (as `Option`) on [`DdState::xwayland`] once started.
pub struct XwaylandState {
    /// The `xwayland_shell_v1` global that pairs X11 windows with their `wl_surface`s.
    pub shell: XWaylandShellState,
    /// The Xwayland server handle. Dropping it shuts Xwayland down, so it is kept alive here.
    pub xwayland: XWayland,
    /// The Wayland client the Xwayland server connects to us as — needed to start the X11 window manager.
    pub client: smithay::reexports::wayland_server::Client,
    /// The running X11 window manager, set once [`XWaylandEvent::Ready`] fires.
    pub xwm: Option<X11Wm>,
    /// Every X11 window we have been told about, keyed by its X11 window id (for stacking / lookup).
    pub windows: HashMap<X11Window, X11Surface>,
}

impl DdState {
    /// Spawn and wire the Xwayland server + X11 window manager into `loop_handle`. Called once at startup
    /// under `DD_XWAYLAND`. Creates the `xwayland_shell_v1` global, spawns `Xwayland`, and inserts its
    /// event source; the `Ready` event starts the [`X11Wm`]. Idempotent-ish: a second call replaces state.
    pub fn start_xwayland(&mut self, loop_handle: LoopHandle<'static, DdState>) -> std::io::Result<()> {
        let shell = XWaylandShellState::new::<DdState>(&self.dh);
        // Spawn Xwayland. `open_abstract_socket = true` matches the Linux abstract-socket convention;
        // stdout/stderr are silenced (Xwayland is chatty). The returned `Client` is how Xwayland talks to
        // us and is required to start the window manager.
        let (xwayland, client) = XWayland::spawn(
            &self.dh,
            None,
            std::iter::empty::<(String, String)>(),
            true,
            Stdio::null(),
            Stdio::null(),
            |_user_data| {},
        )?;
        let display_number = xwayland.display_number();
        eprintln!("dd-compositor: XWayland starting on DISPLAY=:{display_number}");

        let handle_for_wm = loop_handle.clone();
        loop_handle
            .insert_source(xwayland, move |event, _, state: &mut DdState| match event {
                XWaylandEvent::Ready { x11_socket, display_number } => {
                    match X11Wm::start_wm(handle_for_wm.clone(), x11_socket, state.xwayland_client()) {
                        Ok(wm) => {
                            eprintln!(
                                "dd-compositor: XWayland X11 window manager attached (DISPLAY=:{display_number})"
                            );
                            if let Some(xstate) = state.xwayland.as_mut() {
                                xstate.xwm = Some(wm);
                            }
                        }
                        Err(e) => eprintln!("dd-compositor: failed to attach X11 window manager: {e}"),
                    }
                }
                XWaylandEvent::Error => {
                    eprintln!("dd-compositor: XWayland server failed to start");
                }
            })
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("{e}")))?;

        self.xwayland = Some(XwaylandState { shell, xwayland, client, xwm: None, windows: HashMap::new() });
        Ok(())
    }

    /// The Xwayland client handle (for `X11Wm::start_wm`). Panics only if called before `start_xwayland`.
    fn xwayland_client(&self) -> smithay::reexports::wayland_server::Client {
        self.xwayland
            .as_ref()
            .expect("xwayland_client called before start_xwayland")
            .client
            .clone()
    }

    /// The X11 window's backing `wl_surface`, if it has been paired yet.
    fn x11_wl_surface(window: &X11Surface) -> Option<WlSurface> {
        window.wl_surface()
    }
}

impl XWaylandShellHandler for DdState {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        &mut self
            .xwayland
            .as_mut()
            .expect("xwayland_shell dispatched without an XWayland state")
            .shell
    }
}

impl XwmHandler for DdState {
    fn xwm_state(&mut self, _xwm: XwmId) -> &mut X11Wm {
        self.xwayland
            .as_mut()
            .and_then(|x| x.xwm.as_mut())
            .expect("xwm_state dispatched without a running X11Wm")
    }

    /// A new managed X11 window exists (not yet mapped). Remember it; nothing is presented until it maps
    /// and commits a buffer.
    fn new_window(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(x) = self.xwayland.as_mut() {
            x.windows.insert(window.window_id(), window);
        }
    }

    /// A new override-redirect X11 window (menus/tooltips/DND) — the client positions these itself and the
    /// WM must not manage their geometry. Remember it; it presents on its own commit.
    fn new_override_redirect_window(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(x) = self.xwayland.as_mut() {
            x.windows.insert(window.window_id(), window);
        }
    }

    /// The X11 client asked to MAP a managed window. Acknowledge the map to Xwayland, size it, and adopt it
    /// into the window model (title + focus + present through the ordinary path).
    fn map_window_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Err(e) = window.set_mapped(true) {
            eprintln!("dd-compositor: X11 set_mapped failed: {e}");
            return;
        }
        // Configure the X11 window to its requested geometry so the client draws at the right size. A
        // zero/degenerate geometry falls back to a sane default.
        let mut geo = window.geometry();
        if geo.size.w <= 0 || geo.size.h <= 0 {
            geo = Rectangle::from_loc_and_size((0, 0), (800, 600));
        }
        let _ = window.configure(Some(geo));
        let _ = window.set_activated(true);
        if let Some(surface) = Self::x11_wl_surface(&window) {
            self.adopt_x11_window(&surface, window_title(&window));
        }
    }

    /// An override-redirect window became mapped: present it as its own window WITHOUT stealing keyboard
    /// focus (it is a transient menu/tooltip that manages itself).
    fn mapped_override_redirect_window(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(surface) = Self::x11_wl_surface(&window) {
            self.present_unfocused_x11_window(&surface, window_title(&window));
        }
    }

    /// The X11 window unmapped (still exists): withdraw it from the window model.
    fn unmapped_window(&mut self, _xwm: XwmId, window: X11Surface) {
        let _ = window.set_mapped(false);
        if let Some(surface) = Self::x11_wl_surface(&window) {
            self.withdraw_x11_window(&surface);
        }
    }

    /// The X11 window was destroyed: drop our records; the `wl_surface` destroy still runs teardown.
    fn destroyed_window(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(surface) = Self::x11_wl_surface(&window) {
            self.withdraw_x11_window(&surface);
        }
        if let Some(x) = self.xwayland.as_mut() {
            x.windows.remove(&window.window_id());
        }
    }

    /// The X11 client asked to reposition/resize/restack a window. Honour the requested geometry (a real
    /// window manager would constrain to the output; the dd model shows one native window per surface, so
    /// the size is what matters for the client's buffer).
    fn configure_request(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        x: Option<i32>,
        y: Option<i32>,
        w: Option<u32>,
        h: Option<u32>,
        _reorder: Option<Reorder>,
    ) {
        let mut geo = window.geometry();
        if let Some(x) = x {
            geo.loc.x = x;
        }
        if let Some(y) = y {
            geo.loc.y = y;
        }
        if let Some(w) = w {
            geo.size.w = w as i32;
        }
        if let Some(h) = h {
            geo.size.h = h as i32;
        }
        let _ = window.configure(Some(geo));
    }

    /// Xwayland notified us a window's geometry changed (e.g. an override-redirect window moved itself).
    /// Nothing to reconcile in the one-window-per-surface model; the surface size drives presentation.
    fn configure_notify(
        &mut self,
        _xwm: XwmId,
        _window: X11Surface,
        _geometry: Rectangle<i32, Logical>,
        _above: Option<X11Window>,
    ) {
    }

    /// The X11 client began an interactive resize (a title-bar/edge drag). Route it to the presenter the
    /// same way an `xdg_toplevel.resize` does, mapping the X11 resize edge to the `xdg` edge bitmask.
    fn resize_request(&mut self, _xwm: XwmId, window: X11Surface, _button: u32, resize_edge: ResizeEdge) {
        if let Some(surface) = Self::x11_wl_surface(&window) {
            if let Some(sid) = self.surface_id_opt(&surface) {
                self.presenter.begin_interactive_resize(sid, resize_edge_to_xdg(resize_edge));
            }
        }
    }

    /// The X11 client began an interactive move (title-bar drag). Route it to the presenter like
    /// `xdg_toplevel.move`.
    fn move_request(&mut self, _xwm: XwmId, window: X11Surface, _button: u32) {
        if let Some(surface) = Self::x11_wl_surface(&window) {
            if let Some(sid) = self.surface_id_opt(&surface) {
                self.presenter.begin_interactive_move(sid);
            }
        }
    }

    fn maximize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        let _ = window.set_maximized(true);
    }
    fn unmaximize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        let _ = window.set_maximized(false);
    }
    fn fullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
        let _ = window.set_fullscreen(true);
    }
    fn unfullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
        let _ = window.set_fullscreen(false);
    }

    /// The X11 client asked to minimize: hide the native window and pause its pacing, mirroring
    /// `xdg_toplevel.set_minimized`.
    fn minimize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(surface) = Self::x11_wl_surface(&window) {
            self.set_surface_visibility(&surface, hl_display::present::SurfaceVisibility::Minimized);
        }
    }
    fn unminimize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(surface) = Self::x11_wl_surface(&window) {
            self.set_surface_visibility(&surface, hl_display::present::SurfaceVisibility::Visible);
        }
    }

    // ---- X11 ⇄ Wayland clipboard/selection bridge ------------------------------------------------------
    // Structured against Smithay's X11 selection API; UNVERIFIED (no compile/live on the offline host).

    /// Whether an X11 client may read the current Wayland selection. Permissive: the guest's X11 apps and
    /// Wayland apps share one logical session, so cross-reads are allowed.
    fn allow_selection_access(&mut self, _xwm: XwmId, _selection: SelectionTarget) -> bool {
        true
    }

    /// An X11 client SET a selection (copy): advertise its mime types to Wayland clients so they can paste.
    /// The reverse read is served on demand via [`XwmHandler::send_selection`] when a Wayland client
    /// requests the data. (Full wiring of the Wayland-side offer is the remaining live-validation item.)
    fn new_selection(&mut self, _xwm: XwmId, selection: SelectionTarget, mime_types: Vec<String>) {
        eprintln!(
            "dd-compositor: X11 offered a {selection:?} selection with {} mime type(s); \
             Wayland-side offer wiring pending live validation",
            mime_types.len()
        );
    }

    /// A Wayland client requested the X11-owned selection data for `mime_type`: Xwayland hands us the fd to
    /// write it into. Piping the X11 selection bytes into `fd` is handled by the X11Wm; here it is the
    /// remaining live-validation item.
    fn send_selection(
        &mut self,
        _xwm: XwmId,
        selection: SelectionTarget,
        mime_type: String,
        _fd: OwnedFd,
    ) {
        eprintln!(
            "dd-compositor: send_selection {selection:?} {mime_type}: X11→Wayland pipe pending live validation"
        );
    }

    fn cleared_selection(&mut self, _xwm: XwmId, _selection: SelectionTarget) {}
}

impl DdState {
    /// Present an override-redirect X11 window (menu/tooltip) as its own window without taking focus.
    fn present_unfocused_x11_window(&mut self, surface: &WlSurface, title: String) {
        let sid = self.surface_id(surface);
        self.titles.insert(sid, title);
        self.x11_windows.insert(sid);
        if self.buffers.contains_key(&sid) && !self.headless {
            let root = self.window_root(surface).unwrap_or_else(|| surface.clone());
            self.dirty.insert(sid);
            self.present_render_root(&root);
        }
    }
}

/// The X11 window's title, falling back to its class then a generic label.
fn window_title(window: &X11Surface) -> String {
    let title = window.title();
    if !title.is_empty() {
        return title;
    }
    let class = window.class();
    if !class.is_empty() {
        return class;
    }
    "X11".to_string()
}

/// Map an X11 [`ResizeEdge`] to the `xdg_toplevel.resize_edge` bitmask the presenter expects
/// (top=1, bottom=2, left=4, right=8; corners are the OR of two).
fn resize_edge_to_xdg(edge: ResizeEdge) -> u32 {
    match edge {
        ResizeEdge::Top => 1,
        ResizeEdge::Bottom => 2,
        ResizeEdge::Left => 4,
        ResizeEdge::TopLeft => 1 | 4,
        ResizeEdge::BottomLeft => 2 | 4,
        ResizeEdge::Right => 8,
        ResizeEdge::TopRight => 1 | 8,
        ResizeEdge::BottomRight => 2 | 8,
    }
}

smithay::delegate_xwayland_shell!(DdState);
