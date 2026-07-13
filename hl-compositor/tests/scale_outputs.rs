//! Offline client proof for the compatibility globals added on top of the core compositor:
//! `wp_fractional_scale_v1`, `zxdg_output_manager_v1` (xdg-output geometry/name), `wp_single_pixel_buffer_v1`,
//! and multi-output readiness. A minimal in-process Wayland client (built on `hl_display::wire`) connects
//! over a `socketpair` handed to Smithay's `Display`, drives each handshake, and asserts the wire:
//!   1. `wp_fractional_scale_manager_v1` is advertised; a surface's `wp_fractional_scale_v1` receives
//!      `preferred_scale` == round(scale × 120) (1.5× ⇒ 180) from the compositor's fractional backing.
//!   2. `zxdg_output_manager_v1` is advertised; an `zxdg_output_v1` reports logical size (mode ÷ scale),
//!      logical position, and a name — what GTK/Qt need for multi-monitor + scaling.
//!   3. `wp_single_pixel_buffer_v1` is advertised and a 1×1 solid-color buffer is created without error.
//!   4. A second output (via `DdState::add_output`) is advertised as its own `wl_output` + xdg-output —
//!      the output plumbing is not hard-wired to exactly one.
//!
//! This lives in its OWN test binary (separate process) so it does not share `wayland-server`'s
//! process-global `Display` state with `client_roundtrip.rs`. Runs headlessly on Linux + macOS.

use hl_compositor::{ClientState, DdState};
use hl_display::present::{PresentError, PresentOutcome, Presenter, SurfaceBuffer};
use hl_display::wire::{Conn, Message};
use smithay::reexports::wayland_server::Display;
use std::os::unix::io::{FromRawFd, RawFd};
use std::sync::Arc;

const WL_DISPLAY: u32 = 1;

/// A tiny Wayland client: registry decode + per-object event capture for the handful of events this
/// test asserts (fractional_scale.preferred_scale, xdg_output.{logical_position,logical_size,name}).
struct Client {
    conn: Conn,
    next_id: u32,
    globals: std::collections::HashMap<String, (u32, u32)>,
    /// Every `wl_output` global `name` advertised by the registry (multiple, for multi-output).
    wl_output_names: Vec<u32>,
    events: Vec<(u32, u16)>,
    /// The client's `wp_fractional_scale_v1` id → the last `preferred_scale` (120ths) it received.
    frac_id: u32,
    frac_preferred: Option<u32>,
    /// The client's `zxdg_output_v1` id → decoded geometry/name.
    xdg_output_id: u32,
    xo_logical_position: Option<(i32, i32)>,
    xo_logical_size: Option<(i32, i32)>,
    xo_name: Option<String>,
}

impl Client {
    fn new(fd: RawFd) -> Client {
        Client {
            conn: Conn::new(fd),
            next_id: 2,
            globals: Default::default(),
            wl_output_names: Vec::new(),
            events: Vec::new(),
            frac_id: 0,
            frac_preferred: None,
            xdg_output_id: 0,
            xo_logical_position: None,
            xo_logical_size: None,
            xo_name: None,
        }
    }
    fn alloc(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
    fn flush(&mut self) {
        self.conn.flush().unwrap();
    }
    fn drain(&mut self) {
        loop {
            match self.conn.fill().unwrap() {
                0 | -1 => break,
                _ => {}
            }
        }
        while let Some(m) = self.conn.next_message() {
            self.events.push((m.object, m.opcode));
            if m.opcode == 0 && m.object == 2 {
                // wl_registry.global(name, iface, version)
                let mut r = m.reader();
                let name = r.u32();
                let iface = r.string();
                let ver = r.u32();
                if iface == "wl_output" && !self.wl_output_names.contains(&name) {
                    self.wl_output_names.push(name);
                }
                self.globals.insert(iface, (name, ver));
            } else if self.frac_id != 0 && m.object == self.frac_id && m.opcode == 0 {
                // wp_fractional_scale_v1.preferred_scale(scale): fixed-point, 120ths.
                self.frac_preferred = Some(m.reader().u32());
            } else if self.xdg_output_id != 0 && m.object == self.xdg_output_id {
                match m.opcode {
                    // zxdg_output_v1.logical_position(x, y)
                    0 => {
                        let mut r = m.reader();
                        self.xo_logical_position = Some((r.i32(), r.i32()));
                    }
                    // zxdg_output_v1.logical_size(w, h)
                    1 => {
                        let mut r = m.reader();
                        self.xo_logical_size = Some((r.i32(), r.i32()));
                    }
                    // zxdg_output_v1.name(name) — opcode 3 (after logical_position=0, logical_size=1, done=2)
                    3 => self.xo_name = Some(m.reader().string()),
                    _ => {}
                }
            }
        }
    }
    fn saw(&self, object: u32, opcode: u16) -> bool {
        self.events.contains(&(object, opcode))
    }
}

/// A headless presenter that advertises an integer HiDPI backing scale of 2 (Retina), so the primary
/// output's `wl_output.scale` is 2 and its xdg-output logical size is mode ÷ 2. All other Presenter hooks
/// keep their default no-ops.
struct Scale2Presenter;
impl Presenter for Scale2Presenter {
    fn present(&mut self, _surf: &SurfaceBuffer) -> Result<PresentOutcome, PresentError> {
        Ok(PresentOutcome::Delivered { serial: 0, timing: None })
    }
    fn output_scale(&self) -> i32 {
        2
    }
}

fn socketpair_nonblocking() -> (RawFd, RawFd) {
    let mut sv = [0i32; 2];
    assert_eq!(
        unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, sv.as_mut_ptr()) },
        0
    );
    for fd in sv {
        unsafe {
            let fl = libc::fcntl(fd, libc::F_GETFL);
            libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
        }
    }
    (sv[0], sv[1])
}

#[test]
fn fractional_scale_xdg_output_single_pixel_and_multi_output() {
    // Drive the compositor's fractional backing scale to a true non-integer (1.5×) so the wire value is
    // unambiguously 180 (round(1.5 × 120)) rather than an integer fallback. Set before DdState::new.
    std::env::set_var("DD_DISPLAY_FRACTIONAL_SCALE", "1.5");

    let mut display: Display<DdState> = Display::new().unwrap();
    let mut dh = display.handle();
    let mut state = DdState::new(dh.clone(), Box::new(Scale2Presenter));

    // Register a SECOND output before the client connects, at a logical position to the right of the
    // primary. Proves the output plumbing handles >1 output cleanly.
    state.add_output("dd-1", "dd-display-2", (1920, 1080), 1, (1280, 0));

    let (client_fd, server_fd) = socketpair_nonblocking();
    dh.insert_client(
        unsafe { std::os::unix::net::UnixStream::from_raw_fd(server_fd) },
        Arc::new(ClientState::default()),
    )
    .unwrap();

    let mut c = Client::new(client_fd);

    macro_rules! pump {
        () => {{
            c.flush();
            display.dispatch_clients(&mut state).unwrap();
            display.flush_clients().unwrap();
            c.drain();
        }};
    }

    // get_registry → all globals advertised.
    let reg = c.alloc();
    c.conn.send(&Message::new(WL_DISPLAY, 1).u32(reg));
    pump!();

    for iface in [
        "wl_compositor",
        "wl_subcompositor",
        "wp_fractional_scale_manager_v1",
        "zxdg_output_manager_v1",
        "wp_single_pixel_buffer_manager_v1",
        "wl_output",
    ] {
        assert!(
            c.globals.contains_key(iface),
            "global {iface} not advertised; got {:?}",
            c.globals.keys().collect::<Vec<_>>()
        );
    }

    let bind = |c: &mut Client, iface: &str, ver: u32| -> u32 {
        let id = c.alloc();
        let name = c.globals[iface].0;
        c.conn
            .send(&Message::new(2, 0).u32(name).string(iface).u32(ver).u32(id));
        id
    };

    let comp = bind(&mut c, "wl_compositor", 4);

    // (1) wp_fractional_scale: create a surface, get its fractional-scale object → preferred_scale(180).
    let surface = c.alloc();
    c.conn.send(&Message::new(comp, 0).u32(surface)); // create_surface
    let frac_mgr = bind(&mut c, "wp_fractional_scale_manager_v1", 1);
    let frac = c.alloc();
    c.conn
        .send(&Message::new(frac_mgr, 1).u32(frac).u32(surface)); // get_fractional_scale(id, surface)
    c.frac_id = frac;
    pump!();
    assert!(
        c.saw(frac, 0),
        "expected wp_fractional_scale_v1.preferred_scale (opcode 0); saw {:?}",
        c.events
    );
    assert_eq!(
        c.frac_preferred,
        Some(180),
        "preferred_scale should be round(1.5 × 120) = 180 (120ths); got {:?}",
        c.frac_preferred
    );

    // (2) zxdg_output_manager_v1: get_xdg_output for the PRIMARY wl_output → logical geometry + name.
    // Bind the FIRST wl_output advertised (primary dd-0 at scale 2, mode 2560×1440 ⇒ logical 1280×720). The
    // `globals` map keeps only the last per-iface name, so use the ordered `wl_output_names` list instead.
    let primary_output_name = c.wl_output_names[0];
    let wl_out = c.alloc();
    c.conn.send(
        &Message::new(2, 0)
            .u32(primary_output_name)
            .string("wl_output")
            .u32(4)
            .u32(wl_out),
    );
    let xdg_out_mgr = bind(&mut c, "zxdg_output_manager_v1", 3);
    let xdg_out = c.alloc();
    c.conn
        .send(&Message::new(xdg_out_mgr, 1).u32(xdg_out).u32(wl_out)); // get_xdg_output(id, output)
    c.xdg_output_id = xdg_out;
    pump!();
    assert_eq!(
        c.xo_logical_size,
        Some((1280, 720)),
        "primary xdg-output logical size should be mode(2560×1440) ÷ scale(2) = 1280×720; got {:?}",
        c.xo_logical_size
    );
    assert_eq!(
        c.xo_logical_position,
        Some((0, 0)),
        "primary xdg-output logical position should be the origin; got {:?}",
        c.xo_logical_position
    );
    assert_eq!(
        c.xo_name.as_deref(),
        Some("dd-0"),
        "primary xdg-output name should be dd-0; got {:?}",
        c.xo_name
    );

    // (3) wp_single_pixel_buffer_v1: create a 1×1 opaque-red buffer. Success == no protocol error and the
    // buffer id is live (a follow-up destroy is accepted).
    let spb_mgr = bind(&mut c, "wp_single_pixel_buffer_manager_v1", 1);
    let px = c.alloc();
    // create_u32_rgba_buffer(id, r, g, b, a) — opcode 1 (destroy is 0). Opaque red.
    c.conn.send(
        &Message::new(spb_mgr, 1)
            .u32(px)
            .u32(u32::MAX)
            .u32(0)
            .u32(0)
            .u32(u32::MAX),
    );
    pump!();
    // No wl_display.error (object 1, opcode 0) should have been delivered.
    assert!(
        !c.saw(WL_DISPLAY, 0),
        "single-pixel buffer creation must not raise a protocol error; events {:?}",
        c.events
    );
    c.conn.send(&Message::new(px, 0)); // wl_buffer.destroy — accepted for a live buffer
    pump!();
    assert!(
        !c.saw(WL_DISPLAY, 0),
        "destroying the single-pixel buffer must not raise a protocol error; events {:?}",
        c.events
    );

    // (4) Multi-output: TWO distinct wl_output globals must be advertised (primary dd-0 + dd-1). Bind each,
    // query its xdg-output, and prove the dd-1 instance carries the logical geometry we placed it at
    // (position (1280, 0), size mode(1920×1080) ÷ scale(1) = 1920×1080). Order-independent: we match by name.
    assert_eq!(
        c.wl_output_names.len(),
        2,
        "expected exactly two wl_output globals (primary + dd-1); got {:?}",
        c.wl_output_names
    );
    let mut found_dd1 = None;
    let names: Vec<u32> = c.wl_output_names.clone();
    for name in names {
        let wl = c.alloc();
        c.conn.send(
            &Message::new(2, 0)
                .u32(name)
                .string("wl_output")
                .u32(4)
                .u32(wl),
        );
        let xo = c.alloc();
        c.conn.send(&Message::new(xdg_out_mgr, 1).u32(xo).u32(wl)); // get_xdg_output(id, output)
        c.xdg_output_id = xo;
        c.xo_name = None;
        c.xo_logical_position = None;
        c.xo_logical_size = None;
        pump!();
        if c.xo_name.as_deref() == Some("dd-1") {
            found_dd1 = Some((c.xo_logical_position, c.xo_logical_size));
        }
    }
    let (pos, size) = found_dd1.expect("the dd-1 xdg-output should have been advertised");
    assert_eq!(
        pos,
        Some((1280, 0)),
        "dd-1 logical position should be (1280, 0); got {pos:?}"
    );
    assert_eq!(
        size,
        Some((1920, 1080)),
        "dd-1 logical size should be 1920×1080; got {size:?}"
    );

    // State-level readiness: the compositor genuinely tracks the extra output (not hard-wired to one).
    assert_eq!(
        state.extra_outputs.len(),
        1,
        "one extra output should be registered"
    );
    assert_eq!(
        state.extra_outputs[0].name(),
        "dd-1",
        "the extra output should be named dd-1"
    );

    // Move this independent root from dd-0 to dd-1. Membership changes are ordered enter(new) then
    // leave(old), and every later presenter/feedback lookup reads this same selected output.
    c.events.clear();
    let subcomp = bind(&mut c, "wl_subcompositor", 1);
    let child = c.alloc();
    c.conn.send(&Message::new(comp, 0).u32(child));
    let subsurface = c.alloc();
    c.conn.send(&Message::new(subcomp, 1).u32(subsurface).u32(child).u32(surface));
    c.conn.send(&Message::new(child, 6));
    pump!();
    c.events.clear();
    assert!(state.route_surface_to_output(1, "dd-1"));
    display.flush_clients().unwrap();
    c.drain();
    assert_eq!(state.surface_output_name(1).as_deref(), Some("dd-1"));
    assert!(c.saw(surface, 0), "route must emit wl_surface.enter for dd-1");
    assert!(c.saw(surface, 1), "route must emit wl_surface.leave for dd-0");
    assert_eq!(state.surface_output_name(2).as_deref(), Some("dd-1"));
    assert!(c.saw(child, 0), "child must enter its root's replacement output");
    assert!(c.saw(child, 1), "child must leave its root's old output");

    let independent = c.alloc();
    c.conn.send(&Message::new(comp, 0).u32(independent));
    pump!();
    assert_eq!(
        state.surface_output_name(3).as_deref(),
        Some("dd-0"),
        "routing one root must not move an independent root"
    );

    // Hot-unplug dd-1 migrates the complete first tree back to deterministic dd-0 before withdrawal.
    c.events.clear();
    assert!(state.remove_output("dd-1"));
    display.flush_clients().unwrap();
    c.drain();
    assert_eq!(state.surface_output_name(1).as_deref(), Some("dd-0"));
    assert_eq!(state.surface_output_name(2).as_deref(), Some("dd-0"));
    assert_eq!(state.surface_output_name(3).as_deref(), Some("dd-0"));
    assert!(c.saw(surface, 0) && c.saw(surface, 1));
    assert!(c.saw(child, 0) && c.saw(child, 1));

    // Removing the final output has an explicit headless policy: no fallback membership and no
    // fabricated presentation until a new output is added.
    assert!(state.remove_output("dd-0"));
    assert!(state.is_headless());
    assert_eq!(state.surface_output_name(1), None);
    assert_eq!(state.surface_output_name(3), None);
    // Flush + drain the leave events the final withdrawal emitted BEFORE clearing, so they are not
    // mistaken for recovery events below.
    display.flush_clients().unwrap();
    c.drain();

    // Recover from headless with a new output. The compositor records each surface's membership on dd-2
    // and enters them into dd-2's surface set immediately; but `wl_surface.enter` can only be delivered
    // against a wl_output the CLIENT has bound, and dd-2's global was just advertised. A correct client
    // binds the newly-advertised output — and Smithay's wl_output bind handler then replays `enter` for
    // every surface already on dd-2 (exactly one per surface). Track the fresh advertisement in isolation
    // (global names may be reused after dd-0/dd-1 were withdrawn).
    c.events.clear();
    c.wl_output_names.clear();
    state.add_output("dd-2", "replacement", (1600, 900), 2, (0, 0));
    display.flush_clients().unwrap();
    c.drain();
    assert!(!state.is_headless());
    assert_eq!(c.wl_output_names.len(), 1, "exactly one new wl_output (dd-2) is advertised on recovery");
    let dd2_name = c.wl_output_names[0];
    let dd2 = c.alloc();
    c.conn.send(&Message::new(2, 0).u32(dd2_name).string("wl_output").u32(4).u32(dd2));
    c.conn.flush().unwrap();
    display.dispatch_clients(&mut state).unwrap();
    display.flush_clients().unwrap();
    c.drain();
    for (sid, object) in [(1, surface), (2, child), (3, independent)] {
        assert_eq!(state.surface_output_name(sid).as_deref(), Some("dd-2"));
        assert_eq!(
            c.events.iter().filter(|&&(event_object, opcode)| event_object == object && opcode == 0).count(),
            1,
            "headless recovery must emit exactly one enter for sid {sid}"
        );
        assert_eq!(
            c.events.iter().filter(|&&(event_object, opcode)| event_object == object && opcode == 1).count(),
            0,
            "headless recovery has no old output to leave for sid {sid}"
        );
    }
}
