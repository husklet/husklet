use super::*;

const TABLET_TOOL_CAPABILITIES: TabletToolCapabilities = TabletToolCapabilities::PRESSURE;
fn surface_identity_version(native_frames: bool) -> Option<u32> {
    native_frames.then_some(hl_surface_protocol::VERSION)
}
use super::{
    buffer::DmabufAdapter, configuration::env_outputs, output::WaylandOutput,
    tearing::TearingControlCachedState, xdg::HONOURED_WM_CAPABILITIES,
};
impl HlState {
    /// Stand up the protocol globals and the neutral engine, seeded with one output.
    pub fn new(dh: &DisplayHandle, presenter: impl Into<AdapterPresenter>) -> HlState {
        Self::build(dh, presenter.into(), false)
    }

    #[cfg(feature = "macos-surface")]
    pub(crate) fn with_native_frames(
        dh: &DisplayHandle,
        presenter: AdapterPresenter,
        frames: crate::adapter::smithay::native::NativeFrames,
    ) -> HlState {
        let mut state = Self::build(dh, presenter, true);
        state.set_native_frames(frames);
        state
    }

    fn build(dh: &DisplayHandle, presenter: AdapterPresenter, native_frames: bool) -> HlState {
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
        let dmabuf = DmabufAdapter::new(dh, native_frames).state();
        // Only the capabilities performed here; Smithay's default also claims window_menu.
        let xdg_shell =
            XdgShellState::new_with_capabilities::<HlState>(dh, HONOURED_WM_CAPABILITIES);
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
        let surface_identity_manager = surface_identity_version(native_frames)
            .map(|version| dh.create_global::<HlState, HlSurfaceManagerV1, ()>(version, ()));
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
        // driven with real stylus proximity/tip/motion/pressure. The tool declares only PRESSURE because
        // that is the only optional axis translated by every host seam. Advertising an axis is a protocol
        // contract: do not add distance or tilt until their host units are normalized and emitted.
        // `add_tablet` needs no `HlState` value; the
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
        // The pen tool declares pressure on top of the mandatory x/y + tip.
        let tool_desc = TabletToolDescriptor {
            tool_type: TabletToolType::Pen,
            hardware_serial: 0xB007_0001,
            hardware_id_wacom: 0,
            capabilities: TABLET_TOOL_CAPABILITIES,
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
            _surface_identity_manager: surface_identity_manager,
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
            surface_tokens: HashMap::new(),
            token_surfaces: HashMap::new(),
            identity_surfaces: HashMap::new(),
            pending_associations: HashMap::new(),
            last_associations: HashMap::new(),
            #[cfg(feature = "macos-surface")]
            native: None,
            #[cfg(feature = "macos-surface")]
            native_buffers: HashMap::new(),
            popup_grabs: Vec::new(),
            pending_callbacks: HashMap::new(),
            pending_repaints: HashMap::new(),
            entered_outputs: HashMap::new(),
            pending_presentation: HashMap::new(),
            pending_host_frames: HashMap::new(),
            last_injected_pointer: (0.0, 0.0),
            last_pointer_click_count: 1,
            host_fullscreen: HashSet::new(),
            resize_grab: None,
            observations,
            cursor_surface: None,
            cursor_pixels: None,
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
    pub(super) fn sid(&self, surface: &WlSurface) -> Option<SurfaceId> {
        self.surface_ids.get(&surface.id()).copied()
    }

    pub(super) fn reconcile_window(&mut self, surface: SurfaceId) {
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
    pub(super) fn register_surface(&mut self, surface: &WlSurface) {
        let sid = self.engine.scene.create_surface();
        self.surface_ids.insert(surface.id(), sid);
        self.surfaces_by_id.insert(sid, surface.clone());
    }

    /// Drop a `wl_surface` and its scene surface.
    pub(super) fn teardown_surface(&mut self, surface: &WlSurface) {
        // A destroyed surface can never anchor an active grab (a menu's `wl_surface` going away breaks its
        // grab), so drop it from the chain before the scene reference is gone.
        self.popup_grabs.retain(|p| p.wl_surface() != surface);
        if let Some(sid) = self.surface_ids.remove(&surface.id()) {
            let cancelled_root = self.engine.scene.window_root(sid).unwrap_or(sid);
            self.cancel_host_root(cancelled_root);
            // External dmabuf commits are keyed by their buffer token, not necessarily this surface's
            // protocol identity token. Cancel by surface before retiring that identity so destruction
            // settles every deferred callback/feedback and releases its buffer lease.
            #[cfg(feature = "macos-surface")]
            self.cancel_native_commit(sid);
            self.retire_surface_identity(sid);
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
                crate::adapter::smithay::state::frame::discard_owed(feedbacks, sid, "surface_destroyed");
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
    pub(super) fn sync_subsurface_tree(&mut self, surface: &WlSurface) {
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
    pub(super) fn refresh_subsurface_role(&mut self, surface: &WlSurface) {
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
            .filter_map(|child| {
                if child == &parent_wl {
                    Some(parent)
                } else {
                    self.sid(child)
                }
            })
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
    pub(super) fn press_outside_grab_chain(&self, x: f64, y: f64) -> bool {
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
    pub(super) fn record_content_type(&mut self, surface: &WlSurface) {
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
    pub(super) fn record_tearing_hint(&mut self, surface: &WlSurface) {
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
}

#[cfg(test)]
include!("lifetime.rs");
