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
use std::time::Instant;

use dd_display::present::{Presenter, SurfaceBuffer};

use smithay::{
    delegate_compositor, delegate_output, delegate_presentation, delegate_seat, delegate_shm,
    delegate_viewporter, delegate_xdg_shell,
    input::{
        keyboard::{FilterResult, KeyboardHandle, Keycode},
        pointer::{
            AxisFrame, ButtonEvent, CursorImageStatus, MotionEvent, PointerHandle,
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
        output::{OutputHandler, OutputManagerState},
        presentation::PresentationState,
        shell::xdg::{
            PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
        },
        shm::{with_buffer_contents, ShmHandler, ShmState},
        viewporter::{ViewportCachedState, ViewporterState},
    },
};

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
        // wp_presentation: clk_id = CLOCK_MONOTONIC (1) so the guest's clock domain matches.
        let presentation = PresentationState::new::<Self>(&dh, libc::CLOCK_MONOTONIC as u32);

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
            seat,
            keyboard,
            pointer,
            output,
            presenter,
            focus: None,
            titles: HashMap::new(),
            ptr_loc: (0.0, 0.0),
            start: Instant::now(),
        }
    }

    fn now_ms(&self) -> u32 {
        self.start.elapsed().as_millis() as u32
    }

    /// The commit → present path: pull the committed `wl_shm` buffer, repack it tight-BGRA into a
    /// [`SurfaceBuffer`], hand it to the Presenter (which opens/updates the NSWindow on macOS), then
    /// fire the surface's frame callbacks so the client keeps drawing. This is the exact seam
    /// `server.rs` drives; the difference is Smithay decoded the wire for us.
    fn present_surface(&mut self, surface: &WlSurface) {
        let sid = surface.id().protocol_id();

        // Snapshot the committed state. The two cached-state guards must not overlap, so scope each.
        let (buffer, buffer_scale, callbacks, dst) = with_states(surface, |states| {
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
            let dst = {
                let mut vp = states.cached_state.get::<ViewportCachedState>();
                vp.current().size()
            };
            (buffer, scale, callbacks, dst)
        });

        let Some(buffer) = buffer else {
            // No new buffer this commit (e.g. the initial role commit) — still ack frame callbacks.
            let t = self.now_ms();
            for cb in callbacks {
                cb.done(t);
            }
            return;
        };

        if let Some(surf) = self.build_surface_buffer(sid, &buffer, buffer_scale, dst) {
            let presented = self.presenter.present(&surf);
            let _ = presented; // frame pacing (wp_presentation feedback) is a follow-up increment.
        }

        // Frame callbacks: without these the client stops after one frame.
        let t = self.now_ms();
        for cb in callbacks {
            cb.done(t);
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

        let (log_w, log_h) = match dst {
            Some(sz) if sz.w > 0 && sz.h > 0 => (sz.w, sz.h),
            _ => ((tex_w / buffer_scale).max(1), (tex_h / buffer_scale).max(1)),
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
            uv_rect: [0.0, 0.0, 1.0, 1.0],
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
        // Map the requested cursor to a host NSCursor via the reused Presenter seam. The themed
        // wp_cursor_shape path is deferred (needs TabletSeatHandler), but a surface/named request here
        // still drives the native cursor.
        if let CursorImageStatus::Named(icon) = image {
            self.presenter.set_cursor_shape(icon as u32);
        }
    }
    fn focus_changed(&mut self, _seat: &Seat<Self>, _focused: Option<&WlSurface>) {}
}

delegate_compositor!(DdState); // wl_compositor + wl_subcompositor
delegate_shm!(DdState);
delegate_xdg_shell!(DdState);
delegate_seat!(DdState);
delegate_output!(DdState);
delegate_viewporter!(DdState);
delegate_presentation!(DdState);

// Silence an unused-import warning when the presentation feedback type is only referenced in docs.
#[allow(unused_imports)]
use wp_presentation_feedback as _wp_presentation_feedback;
