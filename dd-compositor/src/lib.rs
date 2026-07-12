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
//! ## Native library requirement (libxkbcommon)
//! `smithay` links the system `libxkbcommon` unconditionally (it compiles the seat's XKB keymap at
//! runtime). It is NOT a Linux-only dependency — it builds on macOS (Homebrew / nixpkgs). For dev:
//!   build: `RUSTFLAGS="-L native=<libxkbcommon>/lib"`   run: `DYLD_LIBRARY_PATH="<libxkbcommon>/lib"`.
//! For a shipped `dd.app` this is the host-provides-everything model: bundle `libxkbcommon.dylib` in
//! `dd.app/Contents/Frameworks` and link with `-rpath @executable_path/../Frameworks` (or statically
//! link it). No guest/user install is ever required.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use dd_display::present::{Presenter, SurfaceBuffer};

use smithay::{
    delegate_compositor, delegate_cursor_shape, delegate_output, delegate_presentation,
    delegate_seat, delegate_shm, delegate_viewporter, delegate_xdg_shell,
    input::{
        keyboard::{FilterResult, KeyboardHandle, Keycode},
        pointer::{
            AxisFrame, ButtonEvent, CursorIcon, CursorImageStatus, MotionEvent, PointerHandle,
        },
        Seat, SeatHandler, SeatState,
    },
    output::{Mode as OutMode, Output, PhysicalProperties, Scale, Subpixel},
    reexports::{
        wayland_protocols::wp::presentation_time::server::wp_presentation_feedback,
        wayland_server::{
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::{wl_buffer::WlBuffer, wl_seat::WlSeat, wl_shm, wl_surface::WlSurface},
            Client, DisplayHandle, Resource,
        },
    },
    utils::{Serial, Size, SERIAL_COUNTER},
    wayland::{
        buffer::BufferHandler,
        compositor::{
            with_states, BufferAssignment, CompositorClientState, CompositorHandler,
            CompositorState, SurfaceAttributes,
        },
        cursor_shape::CursorShapeManagerState,
        output::{OutputHandler, OutputManagerState},
        presentation::{PresentationFeedbackCachedState, PresentationState, Refresh},
        shell::xdg::{
            PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
        },
        shm::{with_buffer_contents, ShmHandler, ShmState},
        tablet_manager::TabletSeatHandler,
        viewporter::{ViewportCachedState, ViewporterState},
    },
};

/// `wp_presentation` clock domain reported to the client. The `feedback.presented` timestamp is read
/// back by the GUEST via its own `clock_gettime()`, so this must be the value Linux libc uses for
/// `CLOCK_MONOTONIC` (== 1), NOT the host macOS libc value (== 6). Mirrors `server.rs`'s
/// `CLOCK_MONOTONIC_LINUX`; weston reports its `compositor->presentation_clock` the same way.
const CLOCK_MONOTONIC_LINUX: u32 = 1;

/// The single mode we advertise on `wl_output` (mHz). Also drives the `presented.refresh` interval.
const OUTPUT_REFRESH_MHZ: i64 = 60_000;

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
    /// `wp_presentation` MSC / vblank counter, bumped once per presented frame (mirrors `server.rs`'s
    /// `present_seq`). Chrome/viz feeds the sequence into its BeginFrame vsync estimator.
    present_seq: u64,
    start: Instant,
}

impl DdState {
    /// Stand up every global `server.rs` advertises by hand, wired to PARITY (minus the deferred
    /// `wp_cursor_shape_manager_v1`, which additionally needs `TabletSeatHandler` — see the crate
    /// report). `output_scale` comes from the Presenter so a Retina backing store advertises
    /// `wl_output.scale = 2` (HiDPI), matching `dd-display`'s `present_cocoa` HiDPI advert.
    pub fn new(dh: DisplayHandle, presenter: Box<dyn Presenter>) -> DdState {
        let compositor = CompositorState::new::<Self>(&dh); // wl_compositor v5 + wl_subcompositor
        // wl_shm: Argb8888/Xrgb8888 are always advertised by Smithay.
        let shm = ShmState::new::<Self>(&dh, Vec::new());
        let xdg_shell = XdgShellState::new::<Self>(&dh); // xdg_wm_base + xdg_surface/toplevel/popup
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
            present_seq: 0,
            start: Instant::now(),
        }
    }

    fn now_ms(&self) -> u32 {
        self.start.elapsed().as_millis() as u32
    }

    /// The commit → present path: pull the committed `wl_shm` buffer, repack it tight-BGRA into a
    /// [`SurfaceBuffer`], hand it to the Presenter (which opens/updates the NSWindow on macOS), fire
    /// the surface's frame callbacks so the client keeps drawing, and answer any `wp_presentation`
    /// feedback so Chrome/viz's BeginFrameSource keeps its frame clock ticking. This is the exact seam
    /// `server.rs` drives; the difference is Smithay decoded the wire for us.
    fn present_surface(&mut self, surface: &WlSurface) {
        let sid = surface.id().protocol_id();

        // Snapshot the committed state. The three cached-state guards must not overlap, so scope each.
        let (buffer, buffer_scale, callbacks, dst, src, feedback) = with_states(surface, |states| {
            let (buffer, scale, callbacks) = {
                let mut attrs = states.cached_state.get::<SurfaceAttributes>();
                let cur = attrs.current();
                let buffer = match &cur.buffer {
                    Some(BufferAssignment::NewBuffer(b)) => Some(b.clone()),
                    _ => None,
                };
                let callbacks: Vec<_> = std::mem::take(&mut cur.frame_callbacks);
                (buffer, cur.buffer_scale.max(1), callbacks)
            };
            let (dst, src) = {
                let mut vp = states.cached_state.get::<ViewportCachedState>();
                let cur = vp.current();
                // wp_viewport source: a crop rectangle in post-buffer-scale (logical) surface coords.
                let src = cur.src.map(|r| (r.loc.x, r.loc.y, r.size.w, r.size.h));
                (cur.size(), src)
            };
            // wp_presentation_feedback callbacks committed for THIS content update (see the crate's
            // presentation module docs — drain current() before presenting, answer after).
            let feedback = std::mem::take(
                &mut states
                    .cached_state
                    .get::<PresentationFeedbackCachedState>()
                    .current()
                    .callbacks,
            );
            (buffer, scale, callbacks, dst, src, feedback)
        });

        let Some(buffer) = buffer else {
            // No new buffer this commit (e.g. the initial role commit) — still ack frame callbacks and
            // discard the feedback (there is no content update to time).
            let t = self.now_ms();
            for cb in callbacks {
                cb.done(t);
            }
            for fb in feedback {
                fb.discarded();
            }
            return;
        };

        // Present is non-blocking (the Metal path never blocks on nextDrawable — see present_cocoa); a
        // failed present (false) must NOT advance frame pacing, exactly as `server.rs` gates it.
        let did_present = match self.build_surface_buffer(sid, &buffer, buffer_scale, dst, src) {
            Some(surf) => self.presenter.present(&surf),
            None => false,
        };

        // Frame callbacks: without these the client stops after one frame.
        let t = self.now_ms();
        for cb in callbacks {
            cb.done(t);
        }

        // Answer presentation feedback. Presented ⇒ sync_output + presented(monotonic ts, refresh, MSC,
        // vsync); otherwise discarded. The frame is on-screen by the time `present` returns (the analogue
        // of weston answering on the KMS pageflip-complete event).
        self.send_presentation_feedback(feedback, did_present);
    }

    /// Answer every `wp_presentation_feedback` for a just-processed commit, mirroring `server.rs`'s
    /// `send_presentation_feedback` on the Smithay callback objects.
    fn send_presentation_feedback(
        &mut self,
        feedback: Vec<smithay::wayland::presentation::PresentationFeedbackCallback>,
        did_present: bool,
    ) {
        if feedback.is_empty() {
            return;
        }
        if !did_present {
            for fb in feedback {
                fb.discarded();
            }
            return;
        }
        self.present_seq = self.present_seq.wrapping_add(1);
        let seq = self.present_seq;
        let time = monotonic_now();
        let refresh = Refresh::fixed(output_refresh());
        for fb in feedback {
            fb.presented(
                &self.output,
                time,
                refresh,
                seq,
                wp_presentation_feedback::Kind::Vsync,
            );
        }
    }

    /// Repack a committed `wl_shm` buffer into dd-display's tight-BGRA [`SurfaceBuffer`]. The backing
    /// texture is the full buffer; the logical size is the `wp_viewport` destination if set, else the
    /// buffer pixels divided by `wl_surface.buffer_scale` (so a HiDPI 2x buffer maps to logical units).
    fn build_surface_buffer(
        &self,
        sid: u32,
        buffer: &WlBuffer,
        buffer_scale: i32,
        dst: Option<Size<i32, smithay::utils::Logical>>,
        src: Option<(f64, f64, f64, f64)>,
    ) -> Option<SurfaceBuffer> {
        let title = self.titles.get(&sid).cloned().unwrap_or_else(|| "dd".into());
        let res = with_buffer_contents(buffer, |ptr, _len, data| {
            let w = data.width;
            let h = data.height;
            let stride = data.stride;
            let src_off = data.offset;
            let fmt = match data.format {
                wl_shm::Format::Xrgb8888 => 1u32, // opaque (dd-display convention: format==1 ⇒ XRGB)
                _ => 0u32,                        // ARGB8888 (and anything else): honour alpha
            };
            // Tight BGRA copy of the backing texture, honouring the pool offset + row stride.
            let tight = (w * 4) as usize;
            let mut bgra = vec![0u8; tight * h as usize];
            for row in 0..h as isize {
                let src = unsafe { ptr.offset(src_off as isize + row * stride as isize) };
                let dstart = row as usize * tight;
                unsafe {
                    std::ptr::copy_nonoverlapping(src, bgra[dstart..].as_mut_ptr(), tight);
                }
            }
            (w, h, fmt, bgra)
        })
        .ok()?;
        let (tex_w, tex_h, fmt, bgra) = res;

        // wp_viewport source rectangle (given in post-buffer-scale/logical coords) → a normalized
        // sample rect in the backing texture, so a client that renders into an oversized target and
        // crops via the viewport (Chrome's fractional-scale path) presents only the requested region.
        // `dst` (or, absent it, the buffer pixels / buffer_scale) is the on-screen logical size.
        let (log_w, log_h, uv_rect) = match (dst, src) {
            (Some(sz), src) if sz.w > 0 && sz.h > 0 => (sz.w, sz.h, uv_from_src(src, tex_w, tex_h, buffer_scale)),
            (None, Some((_, _, sw, sh))) if sw > 0.0 && sh > 0.0 => (
                (sw.round() as i32).max(1),
                (sh.round() as i32).max(1),
                uv_from_src(src, tex_w, tex_h, buffer_scale),
            ),
            _ => (
                (tex_w / buffer_scale).max(1),
                (tex_h / buffer_scale).max(1),
                [0.0, 0.0, 1.0, 1.0],
            ),
        };

        Some(SurfaceBuffer {
            sid,
            width: log_w,
            height: log_h,
            texture_width: tex_w,
            texture_height: tex_h,
            stride: tex_w * 4,
            format: fmt,
            bgra,
            title,
            iosurface_id: None,
            gpu_render: false,
            uv_rect,
        })
    }

    // ---- Input injection: synthesize seat events (the `NSEvent`-driven callers live in main.rs). The
    // ---- handles are Arc-backed, so we clone them out to avoid aliasing `&mut self`. ----

    /// Absolute pointer motion in logical/point space (top-left origin). Focuses the pointer on the
    /// currently focused toplevel surface.
    pub fn pointer_motion(&mut self, x: f64, y: f64) {
        self.ptr_loc = (x, y);
        let ptr = self.pointer.clone();
        let focus = self
            .focus
            .clone()
            .map(|s| (s, (0.0, 0.0).into()));
        let serial = SERIAL_COUNTER.next_serial();
        let time = self.now_ms();
        ptr.motion(
            self,
            focus,
            &MotionEvent {
                location: (x, y).into(),
                serial,
                time,
            },
        );
        ptr.frame(self);
    }

    /// Pointer button (evdev code, e.g. `BTN_LEFT = 0x110`).
    pub fn pointer_button(&mut self, button: u32, pressed: bool) {
        let ptr = self.pointer.clone();
        let serial = SERIAL_COUNTER.next_serial();
        let time = self.now_ms();
        use smithay::backend::input::ButtonState;
        ptr.button(
            self,
            &ButtonEvent {
                serial,
                time,
                button,
                state: if pressed {
                    ButtonState::Pressed
                } else {
                    ButtonState::Released
                },
            },
        );
        ptr.frame(self);
    }

    /// Vertical/horizontal scroll. `precise` marks a trackpad (continuous) vs a stepped mouse wheel.
    pub fn pointer_axis(&mut self, vx: f64, vy: f64, precise: bool) {
        use smithay::backend::input::{Axis, AxisSource};
        let ptr = self.pointer.clone();
        let time = self.now_ms();
        let mut frame = AxisFrame::new(time).source(if precise {
            AxisSource::Continuous
        } else {
            AxisSource::Wheel
        });
        if vy != 0.0 {
            frame = frame.value(Axis::Vertical, vy);
            if !precise {
                frame = frame.v120(Axis::Vertical, (vy.signum() as i32) * 120);
            }
        }
        if vx != 0.0 {
            frame = frame.value(Axis::Horizontal, vx);
            if !precise {
                frame = frame.v120(Axis::Horizontal, (vx.signum() as i32) * 120);
            }
        }
        ptr.axis(self, frame);
        ptr.frame(self);
    }

    /// Keyboard key (raw evdev keycode; Smithay adds the +8 XKB offset internally is NOT done — we add
    /// it here). The focused client's own xkbcommon turns the keycode into a keysym.
    pub fn key(&mut self, evdev: u32, pressed: bool) {
        use smithay::backend::input::KeyState;
        let kbd = self.keyboard.clone();
        let serial = SERIAL_COUNTER.next_serial();
        let time = self.now_ms();
        let keycode = Keycode::new(evdev + 8);
        kbd.input::<(), _>(
            self,
            keycode,
            if pressed {
                KeyState::Pressed
            } else {
                KeyState::Released
            },
            serial,
            time,
            |_, _, _| FilterResult::Forward,
        );
    }

    /// Make `surface` the keyboard focus (called when a toplevel maps).
    fn focus_surface(&mut self, surface: WlSurface) {
        self.focus = Some(surface.clone());
        let kbd = self.keyboard.clone();
        let serial = SERIAL_COUNTER.next_serial();
        kbd.set_focus(self, Some(surface), serial);
    }
}

// ---------------------------------------------------------------------------------------------------
// Handler contracts — the ONLY glue a compositor must supply. Each `delegate_*!` macro below emits the
// full Dispatch/GlobalDispatch impls (the bulk of server.rs's 4900 lines), generated not hand-written.
// ---------------------------------------------------------------------------------------------------

impl CompositorHandler for DdState {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor
    }
    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor
    }
    fn commit(&mut self, surface: &WlSurface) {
        self.present_surface(surface);
    }
}

impl BufferHandler for DdState {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {}
}

impl ShmHandler for DdState {
    fn shm_state(&self) -> &ShmState {
        &self.shm
    }
}

impl OutputHandler for DdState {}

impl XdgShellHandler for DdState {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell
    }
    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        // Advertise an initial size + ACTIVATED so the client draws its first frame, then take focus.
        use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
        surface.with_pending_state(|s| {
            s.size = Some((1000, 700).into());
            s.states.set(xdg_toplevel::State::Activated);
        });
        surface.send_configure();
        self.focus_surface(surface.wl_surface().clone());
    }
    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        let sid = surface.wl_surface().id().protocol_id();
        self.titles.remove(&sid);
        self.presenter.drop_window(sid);
        if self.focus.as_ref() == Some(surface.wl_surface()) {
            self.focus = None;
        }
    }
    fn new_popup(&mut self, _surface: PopupSurface, _positioner: PositionerState) {}
    fn grab(&mut self, _surface: PopupSurface, _seat: WlSeat, _serial: Serial) {}
    fn reposition_request(
        &mut self,
        _surface: PopupSurface,
        _positioner: PositionerState,
        _token: u32,
    ) {
    }
}

impl SeatHandler for DdState {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }
    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        // Map the requested themed cursor to a host NSCursor via the reused Presenter seam. Smithay hands
        // us a `CursorIcon` (from wp_cursor_shape_device_v1.set_shape, or a client's own set_cursor); the
        // Presenter expects the `wp_cursor_shape_device_v1.shape` enum number, so translate — the
        // `CursorIcon` enum has NO stable discriminant, so `icon as u32` would pick the wrong cursor.
        if let CursorImageStatus::Named(icon) = image {
            self.presenter.set_cursor_shape(cursor_icon_to_wp_shape(icon));
        }
    }
    fn focus_changed(&mut self, _seat: &Seat<Self>, _focused: Option<&WlSurface>) {}
}

/// `wp_cursor_shape_manager_v1` routes a tablet tool's themed cursor here. We drive a single pointer
/// seat (no tablet), so the default (ignore) behaviour is correct — but the impl is required so
/// `delegate_cursor_shape!` can dispatch the shared manager global.
impl TabletSeatHandler for DdState {}

delegate_compositor!(DdState); // wl_compositor + wl_subcompositor
delegate_shm!(DdState);
delegate_xdg_shell!(DdState);
delegate_seat!(DdState);
delegate_output!(DdState);
delegate_viewporter!(DdState);
delegate_presentation!(DdState);
delegate_cursor_shape!(DdState); // wp_cursor_shape_manager_v1 + wp_cursor_shape_device_v1

/// Map Smithay's `CursorIcon` to the `wp_cursor_shape_device_v1.shape` enum number the Presenter
/// (`apply_cursor_shape` → `NSCursor`) understands. This is the inverse of Smithay's internal
/// `shape_to_cursor_icon`; `CursorIcon` is a plain fieldless enum with no `#[repr]`, so it must be
/// matched explicitly rather than cast. Unmapped icons fall back to `default` (1 = arrow).
fn cursor_icon_to_wp_shape(icon: CursorIcon) -> u32 {
    match icon {
        CursorIcon::Default => 1,
        CursorIcon::ContextMenu => 2,
        CursorIcon::Help => 3,
        CursorIcon::Pointer => 4,
        CursorIcon::Progress => 5,
        CursorIcon::Wait => 6,
        CursorIcon::Cell => 7,
        CursorIcon::Crosshair => 8,
        CursorIcon::Text => 9,
        CursorIcon::VerticalText => 10,
        CursorIcon::Alias => 11,
        CursorIcon::Copy => 12,
        CursorIcon::Move => 13,
        CursorIcon::NoDrop => 14,
        CursorIcon::NotAllowed => 15,
        CursorIcon::Grab => 16,
        CursorIcon::Grabbing => 17,
        CursorIcon::EResize => 18,
        CursorIcon::NResize => 19,
        CursorIcon::NeResize => 20,
        CursorIcon::NwResize => 21,
        CursorIcon::SResize => 22,
        CursorIcon::SeResize => 23,
        CursorIcon::SwResize => 24,
        CursorIcon::WResize => 25,
        CursorIcon::EwResize => 26,
        CursorIcon::NsResize => 27,
        CursorIcon::NeswResize => 28,
        CursorIcon::NwseResize => 29,
        CursorIcon::ColResize => 30,
        CursorIcon::RowResize => 31,
        CursorIcon::AllScroll => 32,
        CursorIcon::ZoomIn => 33,
        CursorIcon::ZoomOut => 34,
        _ => 1,
    }
}

/// Normalize a `wp_viewport` source rectangle `(x, y, w, h)` — given in post-buffer-scale/logical
/// coords — into a `[u0, v0, u1, v1]` sample rect over the backing texture (buffer pixels). Returns the
/// full texture when there is no source crop or the texture has no area.
fn uv_from_src(src: Option<(f64, f64, f64, f64)>, tex_w: i32, tex_h: i32, buffer_scale: i32) -> [f32; 4] {
    match src {
        Some((x, y, w, h)) if tex_w > 0 && tex_h > 0 && w > 0.0 && h > 0.0 => {
            let s = buffer_scale.max(1) as f64;
            let (tw, th) = (tex_w as f64, tex_h as f64);
            let u0 = ((x * s) / tw).clamp(0.0, 1.0) as f32;
            let v0 = ((y * s) / th).clamp(0.0, 1.0) as f32;
            let u1 = (((x + w) * s) / tw).clamp(0.0, 1.0) as f32;
            let v1 = (((y + h) * s) / th).clamp(0.0, 1.0) as f32;
            [u0, v0, u1, v1]
        }
        _ => [0.0, 0.0, 1.0, 1.0],
    }
}

/// Host `CLOCK_MONOTONIC` as a `Duration` (the `wp_presentation.presented` timestamp). dd runs the guest
/// clock in the host's monotonic domain, so this is the value the guest reads back — mirrors
/// `server.rs`'s `monotonic_now`.
fn monotonic_now() -> Duration {
    let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
    unsafe {
        libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts);
    }
    Duration::new(ts.tv_sec as u64, (ts.tv_nsec as u32) % 1_000_000_000)
}

/// The output refresh interval (time until the next vblank) derived from the advertised mode. 60000 mHz
/// ⇒ ~16.667 ms; clients add multiples of it to predict future vblanks.
fn output_refresh() -> Duration {
    if OUTPUT_REFRESH_MHZ <= 0 {
        return Duration::ZERO;
    }
    Duration::from_nanos((1_000_000_000_000u64 / OUTPUT_REFRESH_MHZ as u64) as u64)
}

// Silence an unused-import warning when the presentation feedback type is only referenced in docs.
#[allow(unused_imports)]
use wp_presentation_feedback as _wp_presentation_feedback;
