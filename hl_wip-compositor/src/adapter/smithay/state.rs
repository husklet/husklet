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
use std::time::Instant;

use hl_log::{hl_count, hl_debug, hl_info, tag};

use smithay::reexports::wayland_server::{
    backend::{ClientData, ClientId, DisconnectReason, GlobalId, ObjectId},
    protocol::{wl_buffer::WlBuffer, wl_callback::WlCallback, wl_shm, wl_surface::WlSurface},
    Client, DisplayHandle, Resource,
};
use smithay::backend::input::{Axis, AxisSource, ButtonState, KeyState};
use smithay::input::{
    keyboard::{FilterResult, Keycode, XkbConfig},
    pointer::{AxisFrame, ButtonEvent, MotionEvent},
    Seat, SeatHandler, SeatState,
};
use smithay::utils::{Logical, Point, SERIAL_COUNTER};
use smithay::output::{
    Mode as OutputMode, Output as WlOutputHandle, PhysicalProperties, Scale, Subpixel,
};
use smithay::utils::Transform;
use smithay::wayland::{
    buffer::BufferHandler,
    compositor::{
        get_children, get_parent, is_sync_subsurface, with_states, BufferAssignment,
        CompositorClientState, CompositorHandler, CompositorState, Damage, SubsurfaceCachedState,
        SurfaceAttributes,
    },
    fractional_scale::{
        with_fractional_scale, FractionalScaleHandler, FractionalScaleManagerState,
    },
    output::{OutputHandler, OutputManagerState},
    selection::{
        data_device::{
            set_data_device_focus, ClientDndGrabHandler, DataDeviceHandler, DataDeviceState,
            ServerDndGrabHandler,
        },
        SelectionHandler,
    },
    shell::xdg::{
        decoration::{XdgDecorationHandler, XdgDecorationState},
        PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
    },
    shm::{with_buffer_contents, ShmHandler, ShmState},
    viewporter::{ViewportCachedState, ViewporterState},
};

/// The `xdg_positioner` anchor/gravity/constraint-adjustment wire enums — mapped onto the neutral
/// [`crate::scene::model`] positioner value types so the scene's placement math (not Smithay's) resolves
/// the popup geometry.
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_positioner::{
    Anchor as WireAnchor, ConstraintAdjustment as WireConstraint, Gravity as WireGravity,
};
use smithay::reexports::wayland_server::protocol::wl_seat::WlSeat;
use smithay::utils::{Rectangle, Serial};

/// The zxdg-decoration mode the wire speaks (`ServerSide` / `ClientSide`).
use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode;
/// The `xdg_toplevel` state enum (`Activated` / `Maximized` / `Fullscreen` / …) sent in a configure.
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State as XdgToplevelState;

use crate::scene::model::{
    Anchor, BufferState, ConstraintAdjustment, Format, Gravity, Output, OutputId, PopupState,
    Positioner, Rect, SubsurfaceState, SurfaceId, SurfaceRole, Viewport,
};
use crate::scene::port::Clock;
use crate::scene::service::{constrain_popup, surface_at, BufferChange, Commit};
use crate::{Compositor, FrameOutcome};

use super::present::{PngPresenter, StoredBuffer};

/// Initial floating size a toplevel is configured to before it commits real content.
const INITIAL_TOPLEVEL_SIZE: (i32, i32) = (800, 600);

/// The host monotonic clock the scene paces on.
pub struct MonotonicClock {
    start: Instant,
}

impl MonotonicClock {
    pub fn new() -> MonotonicClock {
        MonotonicClock { start: Instant::now() }
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
    /// The single smithay `wl_output` this compositor advertises, driven from the scene's primary
    /// [`crate::scene::model::Output`] (mode size in px + refresh, integer scale). Kept alive so its
    /// global keeps advertising; a bind delivers geometry/mode/scale/name/done to the client. (Held as
    /// the handle that would drive future scene→output changes; not mutated after construction yet.)
    _wl_output: WlOutputHandle,
    /// The `wl_output` global id (held so it stays advertised for the state's lifetime).
    _output_global: GlobalId,
    /// The neutral policy: scene graph + `PngPresenter` + monotonic clock. All compositing/pacing
    /// decisions live here; `HlState` only translates the wire into calls on it.
    pub engine: Compositor<PngPresenter, MonotonicClock>,
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
    /// Toplevel roots for which a `wl_surface.enter(wl_output)` has been sent and not yet balanced by a
    /// `leave`. The compositor advertises a single `wl_output`; a toplevel is "on" it while it has a
    /// committed (mapped) buffer, off it once unmapped. Tracked so enter/leave are sent exactly once per
    /// transition. (Position-based multi-output routing is not modeled headless — there is one output.)
    entered_outputs: HashSet<SurfaceId>,
}

impl HlState {
    /// Stand up the protocol globals and the neutral engine, seeded with one output.
    pub fn new(dh: &DisplayHandle, presenter: PngPresenter) -> HlState {
        let compositor = CompositorState::new::<HlState>(dh);
        // Smithay always advertises Argb8888 + Xrgb8888; pass no extra formats.
        let shm = ShmState::new::<HlState>(dh, Vec::new());
        let xdg_shell = XdgShellState::new::<HlState>(dh);
        // Advertise `zxdg_decoration_manager_v1` so CSD-vs-SSD negotiation resolves instead of hanging.
        let xdg_decoration = XdgDecorationState::new::<HlState>(dh);
        // Advertise `wl_data_device_manager` (clipboard / drag-and-drop). GDK4 (and Chrome/Qt) require
        // this global at display-open; without it `gdk_display_open` aborts before any GL is created.
        let data_device = DataDeviceState::new::<HlState>(dh);
        // Advertise `wp_viewporter` (surface crop/scale) and `wp_fractional_scale_manager_v1` (HiDPI
        // preferred-scale hint) so media/browser clients can crop+scale a buffer and learn the fractional
        // render scale — the surface-semantics globals a modern toolkit probes at startup.
        let viewporter = ViewporterState::new::<HlState>(dh);
        let fractional_scale = FractionalScaleManagerState::new::<HlState>(dh);
        let mut seat_state = SeatState::new();

        let mut engine = Compositor::new(presenter, MonotonicClock::new());
        engine.scene.add_output(Output::new(OutputId(1), "HL-0", 1920, 1080, 60_000));

        // Advertise a `wl_output` (+ xdg-output) driven from the scene's primary output, so toolkits that
        // enumerate outputs for HiDPI geometry / mode / scale / window sizing get a consistent answer.
        let output_manager = OutputManagerState::new_with_xdg_output::<HlState>(dh);
        let (wl_output, output_global) = build_wl_output(dh, engine.scene.primary_output());

        // Advertise a `wl_seat` with pointer + keyboard capabilities so toolkits that bind it for input
        // succeed in creating `wl_pointer`/`wl_keyboard`. No live input is injected headless.
        let mut seat = seat_state.new_wl_seat(dh, "seat-0");
        seat.add_pointer();
        // A default xkb keymap (evdev rules) — enough for the keyboard object + keymap fd to be handed to
        // the client. If libxkbcommon cannot build even the default keymap the seat still advertises the
        // pointer; the keyboard capability is simply omitted rather than panicking the whole compositor.
        if let Err(e) = seat.add_keyboard(XkbConfig::default(), 200, 25) {
            eprintln!("hl_wip-compositor: wl_seat keyboard keymap unavailable, pointer only: {e}");
        }

        hl_info!(tag::WAYLAND, "globals bound: compositor shm xdg seat output data_device");
        HlState {
            display: dh.clone(),
            compositor,
            shm,
            xdg_shell,
            xdg_decoration,
            data_device,
            _viewporter: viewporter,
            _fractional_scale: fractional_scale,
            seat_state,
            seat,
            _output_manager: output_manager,
            _wl_output: wl_output,
            _output_global: output_global,
            engine,
            surface_ids: HashMap::new(),
            surfaces_by_id: HashMap::new(),
            popup_grabs: Vec::new(),
            pending_callbacks: HashMap::new(),
            pending_repaints: HashMap::new(),
            entered_outputs: HashSet::new(),
        }
    }

    /// Fresh per-client protocol state for `insert_client`.
    pub fn new_client_state(&self) -> ClientState {
        ClientState::default()
    }

    /// The neutral scene id for a `wl_surface`, if registered.
    fn sid(&self, surface: &WlSurface) -> Option<SurfaceId> {
        self.surface_ids.get(&surface.id()).copied()
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
            // The window root this surface belonged to, resolved WHILE its tree links still exist. If the
            // surface was a child (popup/subsurface), its removal changes what the root composites — a
            // dismissed popup or a torn-down subsurface must visibly LEAVE the screen. Nothing else marks
            // the root dirty (the client owning the toplevel may never commit again after closing its own
            // popup), so a removed child would otherwise linger on the last presented frame forever.
            let owning_root = self.engine.scene.window_root(sid).filter(|&r| r != sid);
            self.surfaces_by_id.remove(&sid);
            self.engine.presenter_mut().forget(sid);
            self.engine.scene.remove_surface(sid);
            // Reclaim any frame callbacks/repaint owed to a surface that just went away: a client that
            // destroys a surface mid-frame (or disconnects) must not leave a dangling repaint that keeps
            // re-driving a removed root, nor stale callback objects for a dead protocol resource.
            self.pending_callbacks.remove(&sid);
            self.pending_repaints.remove(&sid);
            self.entered_outputs.remove(&sid);
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
            let loc = states.cached_state.get::<SubsurfaceCachedState>().current().location;
            (loc.x, loc.y)
        });
        let sync = is_sync_subsurface(surface);
        self.engine
            .scene
            .set_role(sid, SurfaceRole::Subsurface(SubsurfaceState { parent, x, y, sync }));
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
            let Some(sid) = self.sid(popup.wl_surface()) else { continue };
            let Some((_, ox, oy, _)) = self.engine.scene.popup_offset_to_toplevel(sid) else {
                continue;
            };
            if let Some((w, h)) = self.engine.scene.get(sid).and_then(|s| s.logical_size()) {
                if Rect::new(ox, oy, w, h).contains_point(px, py) {
                    return false; // inside a grabbing popup — do not dismiss
                }
            }
        }
        true
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

        // Snapshot the committed state Smithay applied, taking ownership of the buffer assignment and
        // draining this commit's damage + frame callbacks (the compositor is expected to consume both).
        let (assignment, damage, scale, frame_callbacks, viewport) = with_states(surface, |states| {
            let mut attrs = states.cached_state.get::<SurfaceAttributes>();
            let cur = attrs.current();
            let assignment = cur.buffer.take();
            let damage: Vec<Rect> = std::mem::take(&mut cur.damage)
                .iter()
                .map(|d| match d {
                    Damage::Surface(r) => Rect::new(r.loc.x, r.loc.y, r.size.w, r.size.h),
                    Damage::Buffer(r) => Rect::new(r.loc.x, r.loc.y, r.size.w, r.size.h),
                })
                .collect();
            let scale = cur.buffer_scale.max(1);
            let callbacks = std::mem::take(&mut cur.frame_callbacks);
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
            (assignment, damage, scale, callbacks, viewport)
        });

        // Build the neutral commit from the buffer assignment, depositing pixels for the presenter.
        let commit = match assignment {
            Some(BufferAssignment::NewBuffer(buffer)) => {
                match read_shm_rgba(&buffer) {
                    Some((stored, format)) => {
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
                    // Not an shm buffer (or malformed) — treat as a no-content commit.
                    None => Commit::default(),
                }
            }
            Some(BufferAssignment::Removed) => {
                self.engine.presenter_mut().forget(sid);
                Commit { buffer: BufferChange::Removed, ..Commit::default() }
            }
            None => Commit { buffer: BufferChange::Keep, damage, ..Commit::default() },
        };
        // Apply the just-read `wp_viewport` state on every commit (double-buffered): the scene resolves the
        // logical size from it and the presenter samples the cropped+scaled region.
        let commit = Commit { viewport: Some(viewport), ..commit };

        // Hold this commit's `wl_surface.frame` callbacks until the frame they belong to actually reaches
        // the presenter. Firing them here — before the present decision — would tell the client "your
        // content is on screen, draw the next frame" even when the frame was throttled and NEVER shown,
        // which drops the just-committed content (the client overwrites it) or, if the client then idles,
        // strands stale content on screen forever. The neutral engine models callbacks as a per-surface
        // count; the adapter owns the concrete `wl_callback` objects and releases them per the pacing
        // outcome below.
        self.pending_callbacks.entry(sid).or_default().extend(frame_callbacks);

        // Drive the neutral policy: apply + (unless cursor / sync-subsurface) compose, present, pace.
        hl_count!(tag::WAYLAND, "commits");
        let outcome = self.engine.commit(sid, commit);
        let (cw, ch) = self.engine.scene.get(sid).and_then(|s| s.logical_size()).unwrap_or((0, 0));
        hl_debug!(tag::WAYLAND, "commit surf={} {}x{} changed={}", sid.0, cw, ch, outcome.changed);

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

    /// Emit `wl_surface.enter` / `wl_surface.leave` for the primary `wl_output` as the toplevel root owning
    /// `sid` maps (gains a committed buffer) or unmaps (loses it). Subsurfaces/popups follow their root, so
    /// only the toplevel root is tracked. A no-op when the client has not (yet) bound `wl_output` beyond the
    /// membership bookkeeping — smithay re-sends `enter` for tracked surfaces when the client binds the
    /// output later.
    fn update_output_membership(&mut self, sid: SurfaceId) {
        let Some(root) = self.engine.scene.window_root(sid) else {
            return;
        };
        if !matches!(self.engine.scene.get(root).map(|s| &s.role), Some(SurfaceRole::Toplevel)) {
            return;
        }
        let Some(wl_surface) = self.surfaces_by_id.get(&root).cloned() else {
            return;
        };
        let mapped = self.engine.scene.get(root).and_then(|s| s.buffer).is_some();
        let entered = self.entered_outputs.contains(&root);
        if mapped && !entered {
            self._wl_output.enter(&wl_surface);
            self.entered_outputs.insert(root);
        } else if !mapped && entered {
            self._wl_output.leave(&wl_surface);
            self.entered_outputs.remove(&root);
        }
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
    fn settle_frame(&mut self, root: SurfaceId, frame: &FrameOutcome) {
        if frame.throttled {
            self.arm_repaint(root);
            return;
        }
        let policy = frame.pacing.policy();
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
    }

    /// Record that `root` owes a repaint at its next refresh boundary (or immediately, if it has never
    /// presented yet — a first present that failed retryably). Earlier deadlines win.
    fn arm_repaint(&mut self, root: SurfaceId) {
        let due = self.engine.next_present_due_ns(root).unwrap_or_else(|| self.engine.clock().now_nanos());
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
            if !self.engine.scene.contains(root) {
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
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum InputCommand {
    /// Move the pointer to root-local logical `(x, y)`; re-hit-tests focus and emits enter/leave/motion.
    PointerMotion { x: f64, y: f64 },
    /// Press/release a pointer button (Linux `input-event-codes`, e.g. `0x110` = BTN_LEFT).
    PointerButton { button: u32, pressed: bool },
    /// Scroll: `horizontal`/`vertical` are logical scroll amounts (wheel source).
    PointerAxis { horizontal: f64, vertical: f64 },
    /// Press/release a key by EVDEV keycode (Linux `input-event-codes`, e.g. `30` = KEY_A) — the same
    /// value the client receives on `wl_keyboard.key`.
    Key { keycode: u32, pressed: bool },
    /// Give keyboard focus to the topmost toplevel (emits `wl_keyboard.leave`/`enter` + keymap).
    FocusTopmostKeyboard,
    /// Give keyboard focus to the toplevel at index `n` in ascending surface-id order (0 = the
    /// earliest-mapped toplevel). Lets a host/test target a SPECIFIC window in a multi-window stack —
    /// `FocusTopmostKeyboard` can only reach the highest id. Out-of-range `n` clears focus (no such
    /// window). The neutral scene models no global stacking, so ascending id (== map order for
    /// sequentially-mapped windows) is the stable, inspectable ordering a driver can reason about.
    FocusToplevelIndex(usize),
    /// Clear keyboard focus (emits `wl_keyboard.leave` to the previously focused surface).
    ClearKeyboardFocus,
}

impl HlState {
    /// Apply one host/test-driven [`InputCommand`], routing it through the seat's pointer/keyboard.
    pub fn apply_input(&mut self, cmd: InputCommand) {
        match cmd {
            InputCommand::PointerMotion { x, y } => self.inject_pointer_motion(x, y),
            InputCommand::PointerButton { button, pressed } => self.inject_pointer_button(button, pressed),
            InputCommand::PointerAxis { horizontal, vertical } => self.inject_pointer_axis(horizontal, vertical),
            InputCommand::Key { keycode, pressed } => self.inject_key(keycode, pressed),
            InputCommand::FocusTopmostKeyboard => {
                let target = self.topmost_toplevel();
                self.set_keyboard_focus(target);
            }
            InputCommand::FocusToplevelIndex(n) => {
                let target = self.toplevel_at(n);
                self.set_keyboard_focus(target);
            }
            InputCommand::ClearKeyboardFocus => self.set_keyboard_focus(None),
        }
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
        hl_debug!(tag::WAYLAND, "input motion x={:.0} y={:.0}", x, y);
        let Some(pointer) = self.seat.get_pointer() else { return };
        let hit = self.hit_test(x, y);

        // Keep the neutral seat consistent with what we deliver over the wire (for inspection/tests).
        self.engine.scene.seat_mut().pointer_location = (x, y);
        self.engine.scene.seat_mut().pointer_focus = hit.map(|(_, sid, _, _)| sid);

        // Build smithay's focus: the concrete `WlSurface` + its origin in global (root) space.
        let focus = hit.and_then(|(_, sid, ox, oy)| {
            self.surfaces_by_id
                .get(&sid)
                .cloned()
                .map(|wl| (wl, Point::<f64, Logical>::from((ox as f64, oy as f64))))
        });
        let serial = SERIAL_COUNTER.next_serial();
        let time = self.input_time_ms();
        pointer.motion(self, focus, &MotionEvent { location: (x, y).into(), serial, time });
        pointer.frame(self);
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
        let Some(pointer) = self.seat.get_pointer() else { return };
        let serial = SERIAL_COUNTER.next_serial();
        let time = self.input_time_ms();
        let state = if pressed { ButtonState::Pressed } else { ButtonState::Released };
        pointer.button(self, &ButtonEvent { serial, time, button, state });
        pointer.frame(self);
    }

    /// Scroll the pointer by logical `horizontal`/`vertical` amounts (wheel source). A zero component is
    /// omitted so the client only sees the axes that actually moved.
    pub fn inject_pointer_axis(&mut self, horizontal: f64, vertical: f64) {
        hl_debug!(tag::WAYLAND, "input axis h={:.1} v={:.1}", horizontal, vertical);
        let Some(pointer) = self.seat.get_pointer() else { return };
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

    /// Give keyboard focus to `sid` (or clear it with `None`). Drives smithay's `KeyboardHandle::set_focus`
    /// — which emits `wl_keyboard.leave` to the old focus and `wl_keyboard.enter` (+ the keymap already
    /// sent at bind) to the new — and mirrors the change into the neutral seat.
    pub fn set_keyboard_focus(&mut self, sid: Option<SurfaceId>) {
        let Some(keyboard) = self.seat.get_keyboard() else { return };
        let surface = sid.and_then(|s| self.surfaces_by_id.get(&s).cloned());
        // Follow the keyboard focus with the clipboard (data-device) focus so the newly focused client's
        // `wl_data_device` receives selection offers and its `set_selection` is honored — the standard
        // Wayland "clipboard follows keyboard focus" rule. `None` clears it (no client owns the clipboard).
        let focus_client = surface.as_ref().and_then(|s| self.display.get_client(s.id()).ok());
        set_data_device_focus(&self.display, &self.seat, focus_client);
        // Mirror into the neutral seat so scene focus bookkeeping stays truthful.
        self.engine.scene.seat_mut().keyboard_focus = sid;
        let serial = SERIAL_COUNTER.next_serial();
        keyboard.set_focus(self, surface, serial);
    }

    /// Press or release a key by EVDEV keycode. smithay's keymap is keyed on X11 keycodes (evdev + 8),
    /// and its `KeyboardTarget` impl sends `evdev` back to the client (`raw - 8`); we add the 8 here so
    /// the caller speaks Linux `input-event-codes` and the client receives the same value. Modifiers are
    /// tracked by smithay's xkb state across presses. No compositor keybinding filter — always forward.
    pub fn inject_key(&mut self, keycode: u32, pressed: bool) {
        hl_debug!(tag::WAYLAND, "input key={} pressed={}", keycode, pressed);
        let Some(keyboard) = self.seat.get_keyboard() else { return };
        let serial = SERIAL_COUNTER.next_serial();
        let time = self.input_time_ms();
        let state = if pressed { KeyState::Pressed } else { KeyState::Released };
        keyboard.input::<(), _>(
            self,
            Keycode::new(keycode + 8),
            state,
            serial,
            time,
            |_, _, _| FilterResult::Forward,
        );
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
                SurfaceRole::Subsurface(SubsurfaceState { parent: parent_sid, x: 0, y: 0, sync: true }),
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

impl SeatHandler for HlState {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<HlState> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, _seat: &Seat<HlState>, _focused: Option<&WlSurface>) {}
    fn cursor_image(&mut self, _seat: &Seat<HlState>, _image: smithay::input::pointer::CursorImageStatus) {}
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
    type SelectionUserData = ();
}

/// A client-initiated drag-and-drop grab (a client dragging one of its surfaces). No compositor-side DnD
/// policy headless — accept the defaults; the client manages its own data transfer.
impl ClientDndGrabHandler for HlState {}

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

    /// A toplevel set its title. Smithay has already stored it in the surface's role attributes; the
    /// headless compositor has no task bar to reflect it into, so this is an explicit accept.
    fn title_changed(&mut self, _surface: ToplevelSurface) {}

    /// A toplevel set its app id. Stored by smithay; accepted here (no launcher/grouping policy headless).
    fn app_id_changed(&mut self, _surface: ToplevelSurface) {}

    /// The client asked to be maximized. A headless compositor grants it against the primary output's
    /// logical size and reconfigures with the `Maximized` state (kept `Activated`) so the client redraws
    /// to fill the output and drops its resize affordances. `set_min_size`/`set_max_size` land in
    /// smithay's committed `SurfaceCachedState` automatically; they are not re-sent (they are client→server
    /// hints, not part of the configure).
    fn maximize_request(&mut self, surface: ToplevelSurface) {
        let (w, h) = self.engine.scene.output_logical_size();
        surface.with_pending_state(|s| {
            s.size = Some((w, h).into());
            s.states.set(XdgToplevelState::Maximized);
            s.states.set(XdgToplevelState::Activated);
        });
        surface.send_configure();
    }

    /// The client asked to leave maximized: drop the state and return to the floating size.
    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        surface.with_pending_state(|s| {
            s.states.unset(XdgToplevelState::Maximized);
            s.size = Some(INITIAL_TOPLEVEL_SIZE.into());
        });
        surface.send_configure();
    }

    /// The client asked for fullscreen. Grant it at the output's logical size with the `Fullscreen` state
    /// (the headless compositor has one output; the requested `output` hint is not needed).
    fn fullscreen_request(&mut self, surface: ToplevelSurface, _output: Option<smithay::reexports::wayland_server::protocol::wl_output::WlOutput>) {
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
        surface.with_pending_state(|s| {
            s.states.unset(XdgToplevelState::Fullscreen);
            s.size = Some(INITIAL_TOPLEVEL_SIZE.into());
        });
        surface.send_configure();
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
        let neutral = map_positioner(&positioner);
        let geometry = constrain_popup(&self.engine.scene, &neutral);
        // Link the scene popup to its parent (toplevel or parent popup). Without a mapped parent we still
        // configure the client, but it cannot composite until its parent exists.
        if let (Some(sid), Some(parent)) = (
            self.sid(surface.wl_surface()),
            surface.get_parent_surface().and_then(|p| self.sid(&p)),
        ) {
            self.engine.scene.set_role(
                sid,
                SurfaceRole::Popup(PopupState { parent, positioner: neutral, geometry, grabbed: false }),
            );
        }
        // Tell the client where it was placed (Smithay emits `xdg_popup.configure` from this pending
        // geometry, paired with `xdg_surface.configure`). MUST precede the client's first buffer attach.
        surface.with_pending_state(|state| {
            state.positioner = positioner;
            state.geometry = rect_to_smithay(geometry);
        });
        surface.send_configure().ok();
    }

    /// `xdg_popup.grab(seat, serial)`: the client takes an explicit popup grab (menus / context menus do;
    /// tooltips do not). Record the popup in the grab chain and flag it in the scene so a press outside the
    /// chain dismisses it (see [`HlState::inject_pointer_button`] → [`HlState::dismiss_popup_grabs`]). The
    /// chain is ordered outer → inner, so a submenu opened under an existing grab extends it.
    fn grab(&mut self, surface: PopupSurface, _seat: WlSeat, _serial: Serial) {
        if let Some(sid) = self.sid(surface.wl_surface()) {
            if let Some(SurfaceRole::Popup(p)) = self.engine.scene.get_mut(sid).map(|s| &mut s.role) {
                p.grabbed = true;
            }
        }
        if !self.popup_grabs.iter().any(|p| p.wl_surface() == surface.wl_surface()) {
            self.popup_grabs.push(surface);
        }
    }

    /// `xdg_popup.reposition(positioner, token)` (xdg-shell v3): a mapped popup is re-anchored (e.g. a menu
    /// re-placing as the pointer walks a menu bar). Recompute the geometry from the NEW positioner, update
    /// the scene popup in place, and answer `xdg_popup.repositioned(token)` (which also emits the fresh
    /// configure/ack). The scene composites at the new offset once the client acks and re-commits.
    fn reposition_request(&mut self, surface: PopupSurface, positioner: PositionerState, token: u32) {
        let neutral = map_positioner(&positioner);
        let geometry = constrain_popup(&self.engine.scene, &neutral);
        if let Some(sid) = self.sid(surface.wl_surface()) {
            if let Some(SurfaceRole::Popup(p)) = self.engine.scene.get_mut(sid).map(|s| &mut s.role) {
                p.positioner = neutral;
                p.geometry = geometry;
            }
        }
        surface.with_pending_state(|state| {
            state.positioner = positioner;
            state.geometry = rect_to_smithay(geometry);
        });
        surface.send_repositioned(token);
    }

    /// A popup's role was destroyed (the client tore the menu/tooltip down, or honoured a grab dismissal).
    /// Drop it from the grab chain; the scene surface + popup-registry entry are reclaimed when its
    /// `wl_surface` is destroyed (`teardown_surface`), and the owning toplevel re-presents on its next
    /// commit so the menu visibly disappears.
    fn popup_destroyed(&mut self, surface: PopupSurface) {
        self.popup_grabs.retain(|p| p.wl_surface() != surface.wl_surface());
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
/// [`HlState::update_output_membership`]); position-based multi-output routing is not modeled (there is
/// one output).
impl OutputHandler for HlState {}

/// A client created a `wp_fractional_scale_v1` for a surface. Tell it the compositor's preferred
/// fractional render scale so it can rasterize crisply on HiDPI without integer-only
/// `wl_surface.set_buffer_scale`. We source the scale from the primary output's scale (consistent with the
/// legacy integer `wl_output.scale`); smithay serializes it as `round(scale × 120)`.
impl FractionalScaleHandler for HlState {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        let scale = self.engine.scene.primary_output().map(|o| o.scale.max(1)).unwrap_or(1) as f64;
        with_states(&surface, |states| {
            with_fractional_scale(states, |fractional| {
                fractional.set_preferred_scale(scale);
            });
        });
    }
}

smithay::delegate_compositor!(HlState);
smithay::delegate_shm!(HlState);
smithay::delegate_xdg_shell!(HlState);
smithay::delegate_xdg_decoration!(HlState);
smithay::delegate_output!(HlState);
smithay::delegate_seat!(HlState);
smithay::delegate_data_device!(HlState);
smithay::delegate_viewporter!(HlState);
smithay::delegate_fractional_scale!(HlState);

/// Build the compositor's single `wl_output` from the scene's primary [`Output`], creating its global and
/// pushing the current mode / scale / preferred mode so a binding client receives geometry + mode + scale
/// + name + done consistent with what compose/present uses. Falls back to a 1080p\@60 output if the scene
/// has none registered (it always does, but keep this total).
fn build_wl_output(dh: &DisplayHandle, scene: Option<&Output>) -> (WlOutputHandle, GlobalId) {
    // Values sourced from the scene so `wl_output` reports exactly what the scene composites onto.
    let (name, mode_w, mode_h, refresh_mhz, scale) = match scene {
        Some(o) => (o.name.clone(), o.mode_w, o.mode_h, o.refresh_mhz, o.scale.max(1)),
        None => ("HL-0".to_string(), 1920, 1080, 60_000, 1),
    };

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
    let global = output.create_global::<HlState>(dh);

    // `refresh` on a smithay `Mode` is millihertz (same unit as the scene's `refresh_mhz`).
    let mode = OutputMode { size: (mode_w, mode_h).into(), refresh: refresh_mhz as i32 };
    output.change_current_state(
        Some(mode),
        Some(Transform::Normal),
        Some(Scale::Integer(scale)),
        Some((0, 0).into()),
    );
    output.set_preferred(mode);

    (output, global)
}

/// Map a Smithay `xdg_positioner` [`PositionerState`] onto the neutral [`Positioner`] value type the
/// scene's `place_popup` resolves. A straight field/enum translation — the placement math itself
/// (anchor/gravity/offset + flip/slide/resize) lives in `scene::service::popup`, not here, so the neutral
/// core owns the policy and the adapter only decodes the wire.
fn map_positioner(p: &PositionerState) -> Positioner {
    Positioner {
        anchor_rect: Rect::new(
            p.anchor_rect.loc.x,
            p.anchor_rect.loc.y,
            p.anchor_rect.size.w,
            p.anchor_rect.size.h,
        ),
        size: (p.rect_size.w, p.rect_size.h),
        anchor: map_anchor(p.anchor_edges),
        gravity: map_gravity(p.gravity),
        constraint_adjustment: map_constraint(p.constraint_adjustment),
        offset: (p.offset.x, p.offset.y),
    }
}

/// Translate the `xdg_positioner.set_anchor` edge onto the neutral [`Anchor`].
fn map_anchor(a: WireAnchor) -> Anchor {
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

/// Translate the `xdg_positioner.set_gravity` direction onto the neutral [`Gravity`].
fn map_gravity(g: WireGravity) -> Gravity {
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

/// Translate the `xdg_positioner.set_constraint_adjustment` bitmask onto the neutral per-axis
/// flip/slide/resize flags the scene applies in that order.
fn map_constraint(c: WireConstraint) -> ConstraintAdjustment {
    ConstraintAdjustment {
        flip_x: c.contains(WireConstraint::FlipX),
        flip_y: c.contains(WireConstraint::FlipY),
        slide_x: c.contains(WireConstraint::SlideX),
        slide_y: c.contains(WireConstraint::SlideY),
        resize_x: c.contains(WireConstraint::ResizeX),
        resize_y: c.contains(WireConstraint::ResizeY),
    }
}

/// Lift a neutral [`Rect`] into the Smithay `Rectangle<i32, Logical>` the popup configure carries.
fn rect_to_smithay(r: Rect) -> Rectangle<i32, smithay::utils::Logical> {
    Rectangle::new((r.x, r.y).into(), (r.w, r.h).into())
}

/// Read a `wl_shm` buffer's pixels into tight top-left RGBA8888 plus its neutral [`Format`].
///
/// `wl_shm` Argb/Xrgb8888 are 32-bit little-endian, so bytes in memory are `[B, G, R, A]`; this unpacks
/// them to `[R, G, B, A]`. A bounds check refuses a malformed geometry that would read past the mapping.
fn read_shm_rgba(buffer: &WlBuffer) -> Option<(StoredBuffer, Format)> {
    let result = with_buffer_contents(buffer, |ptr, len, data| {
        let (w, h, stride, offset) = (data.width, data.height, data.stride, data.offset);
        if w <= 0 || h <= 0 || stride < w * 4 || offset < 0 {
            return None;
        }
        // Highest byte read = offset + (h-1)*stride + w*4; must fit the mapping.
        let last_row = offset as usize + (h as usize - 1) * stride as usize;
        if last_row.checked_add((w * 4) as usize).map(|m| m > len).unwrap_or(true) {
            return None;
        }
        let (format, has_alpha) = match data.format {
            wl_shm::Format::Xrgb8888 => (Format::Xrgb8888, false),
            _ => (Format::Argb8888, true),
        };
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            let row = offset as isize + y as isize * stride as isize;
            for x in 0..w {
                let src = unsafe { ptr.offset(row + (x * 4) as isize) };
                let b = unsafe { *src };
                let g = unsafe { *src.offset(1) };
                let r = unsafe { *src.offset(2) };
                let a = if has_alpha { unsafe { *src.offset(3) } } else { 255 };
                let di = ((y * w + x) * 4) as usize;
                rgba[di] = r;
                rgba[di + 1] = g;
                rgba[di + 2] = b;
                rgba[di + 3] = a;
            }
        }
        Some((StoredBuffer { width: w, height: h, rgba }, format))
    });
    result.ok().flatten()
}
