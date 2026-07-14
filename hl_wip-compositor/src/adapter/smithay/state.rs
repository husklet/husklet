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
    protocol::{wl_buffer::WlBuffer, wl_shm, wl_surface::WlSurface},
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
use crate::Compositor;

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

        HlState { compositor, shm, xdg_shell, seat_state, engine, surface_ids: HashMap::new() }
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

        // Drive the neutral policy: apply + (unless cursor / sync-subsurface) compose, present, pace.
        self.engine.commit(sid, commit);

        // Fire the client's frame callbacks so it draws its next frame (the neutral engine models these
        // as a count; the adapter owns the concrete `wl_callback` objects).
        let time_ms = (self.engine.clock().now_nanos() / 1_000_000) as u32;
        for callback in frame_callbacks {
            callback.done(time_ms);
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
