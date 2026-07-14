//! [`HlState`]: the Smithay dispatch aggregate (OVERVIEW-v2 §7) — an ADAPTER object.
//!
//! It holds Smithay's `wayland_frontend` state cores (`CompositorState` / `ShmState` / `XdgShellState`)
//! and the neutral [`crate::Compositor`] engine (scene + presenter + clock). The `Handler` callbacks
//! decode the wire (Smithay did the hard part) and translate each `wl_*`/`xdg_*` event into a call on
//! the neutral `scene::service` layer via `engine` — NO compositing/pacing policy lives here, only the
//! translation. Ported from `hl-compositor`'s `HlState` (`register_surface` / `on_commit` /
//! `ingest_buffer`), with the GPU/budget/Cocoa machinery dropped and Smithay reads mapped onto the
//! neutral [`crate::scene::model`].

use std::collections::HashMap;
use std::time::Instant;

use smithay::reexports::wayland_server::{
    backend::{ClientData, ClientId, DisconnectReason, ObjectId},
    protocol::{wl_buffer::WlBuffer, wl_callback::WlCallback, wl_shm, wl_surface::WlSurface},
    Client, DisplayHandle, Resource,
};
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::wayland::{
    buffer::BufferHandler,
    compositor::{
        with_states, BufferAssignment, CompositorClientState, CompositorHandler, CompositorState,
        Damage, SurfaceAttributes,
    },
    shell::xdg::{
        PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
    },
    shm::{with_buffer_contents, ShmHandler, ShmState},
};

use crate::scene::model::{BufferState, Format, Output, OutputId, Rect, SurfaceId, SurfaceRole};
use crate::scene::port::Clock;
use crate::scene::service::{BufferChange, Commit};
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
    pub compositor: CompositorState,
    pub shm: ShmState,
    pub xdg_shell: XdgShellState,
    /// Held only to satisfy `delegate_xdg_shell`'s `SeatHandler` bound (popup grabs reference a seat).
    /// The headless e2e proof drives no input, so no `wl_seat` global is created.
    pub seat_state: SeatState<HlState>,
    /// The neutral policy: scene graph + `PngPresenter` + monotonic clock. All compositing/pacing
    /// decisions live here; `HlState` only translates the wire into calls on it.
    pub engine: Compositor<PngPresenter, MonotonicClock>,
    /// `wl_surface` protocol object → neutral scene surface id. The scene mints collision-free ids; this
    /// is the neutral analogue of `HlState::surface_ids`.
    surface_ids: HashMap<ObjectId, SurfaceId>,
    /// `wl_surface.frame` callbacks the client is owed but that have NOT yet been fired, keyed by the
    /// neutral surface they were requested on. A callback is held (not fired at commit) until the frame it
    /// belongs to actually reaches the presenter — so a throttled frame does not prematurely tell the
    /// client "your content is on screen, draw the next one". Fired by [`Self::fire_tree_callbacks`] when
    /// the surface's window root presents (or is cleanly skipped).
    pending_callbacks: HashMap<SurfaceId, Vec<WlCallback>>,
    /// Window roots with a repaint owed at the recorded host-monotonic deadline (ns). Populated when a
    /// commit is throttled (or a present is retryable): the serve loop drains these in
    /// [`Self::drive_due_repaints`] to re-drive `present_root` at the next refresh boundary, so the
    /// retained frame ships and its callbacks release even if the client has since gone idle. A later real
    /// commit that presents the same root supersedes the entry (it is cleared on a completing present).
    pending_repaints: HashMap<SurfaceId, u64>,
}

impl HlState {
    /// Stand up the protocol globals and the neutral engine, seeded with one output.
    pub fn new(dh: &DisplayHandle, presenter: PngPresenter) -> HlState {
        let compositor = CompositorState::new::<HlState>(dh);
        // Smithay always advertises Argb8888 + Xrgb8888; pass no extra formats.
        let shm = ShmState::new::<HlState>(dh, Vec::new());
        let xdg_shell = XdgShellState::new::<HlState>(dh);
        let seat_state = SeatState::new();

        let mut engine = Compositor::new(presenter, MonotonicClock::new());
        engine.scene.add_output(Output::new(OutputId(1), "HL-0", 1920, 1080, 60_000));

        HlState {
            compositor,
            shm,
            xdg_shell,
            seat_state,
            engine,
            surface_ids: HashMap::new(),
            pending_callbacks: HashMap::new(),
            pending_repaints: HashMap::new(),
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
    }

    /// Drop a `wl_surface` and its scene surface.
    fn teardown_surface(&mut self, surface: &WlSurface) {
        if let Some(sid) = self.surface_ids.remove(&surface.id()) {
            self.engine.presenter_mut().forget(sid);
            self.engine.scene.remove_surface(sid);
            // Reclaim any frame callbacks/repaint owed to a surface that just went away: a client that
            // destroys a surface mid-frame (or disconnects) must not leave a dangling repaint that keeps
            // re-driving a removed root, nor stale callback objects for a dead protocol resource.
            self.pending_callbacks.remove(&sid);
            self.pending_repaints.remove(&sid);
        }
    }

    /// The commit → present path (the neutral analogue of `on_commit`): read the committed double-buffered
    /// state Smithay has already applied, deposit the surface's pixels for the presenter, translate the
    /// commit into a [`Commit`], drive the neutral engine (which composes + presents + paces), then fire
    /// the client's `wl_surface.frame` callbacks so it keeps drawing.
    fn on_commit(&mut self, surface: &WlSurface) {
        let Some(sid) = self.sid(surface) else {
            return;
        };

        // Snapshot the committed state Smithay applied, taking ownership of the buffer assignment and
        // draining this commit's damage + frame callbacks (the compositor is expected to consume both).
        let (assignment, damage, scale, frame_callbacks) = with_states(surface, |states| {
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
            (assignment, damage, scale, callbacks)
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

        // Hold this commit's `wl_surface.frame` callbacks until the frame they belong to actually reaches
        // the presenter. Firing them here — before the present decision — would tell the client "your
        // content is on screen, draw the next frame" even when the frame was throttled and NEVER shown,
        // which drops the just-committed content (the client overwrites it) or, if the client then idles,
        // strands stale content on screen forever. The neutral engine models callbacks as a per-surface
        // count; the adapter owns the concrete `wl_callback` objects and releases them per the pacing
        // outcome below.
        self.pending_callbacks.entry(sid).or_default().extend(frame_callbacks);

        // Drive the neutral policy: apply + (unless cursor / sync-subsurface) compose, present, pace.
        let outcome = self.engine.commit(sid, commit);

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

impl XdgShellHandler for HlState {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell
    }

    /// A toplevel mapped: assign the scene `Toplevel` role, send the initial configure (a floating size +
    /// `Activated` + output bounds) so the client draws its first frame.
    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State as XdgState;
        if let Some(sid) = self.sid(surface.wl_surface()) {
            self.engine.scene.set_role(sid, SurfaceRole::Toplevel);
        }
        let bounds = self.engine.scene.output_logical_size();
        surface.with_pending_state(|s| {
            s.size = Some(INITIAL_TOPLEVEL_SIZE.into());
            s.bounds = Some(bounds.into());
            s.states.set(XdgState::Activated);
        });
        surface.send_configure();
    }

    /// An `xdg_popup` mapped: send its initial configure so the client can draw. (Placement policy lives
    /// in `scene::service::popup`; the headless e2e proof only exercises toplevels.)
    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        surface.send_configure().ok();
    }

    fn grab(&mut self, _surface: PopupSurface, _seat: smithay::reexports::wayland_server::protocol::wl_seat::WlSeat, _serial: smithay::utils::Serial) {}

    fn reposition_request(&mut self, surface: PopupSurface, _positioner: PositionerState, token: u32) {
        surface.send_repositioned(token);
    }
}

smithay::delegate_compositor!(HlState);
smithay::delegate_shm!(HlState);
smithay::delegate_xdg_shell!(HlState);

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
