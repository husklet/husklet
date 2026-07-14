//! Client-roundtrip proof for `wp_color_manager_v1` (ledger row
//! `compositor_negotiates_surface_color_and_converts_to_the_target_output_profile`).
//!
//! A minimal in-process Wayland client binds the color manager, waits for its advertised
//! intents/features/transfer-functions/primaries (`done`), builds a parametric image description
//! (BT.2020 primaries + sRGB transfer), attaches it to its surface, and reads the output's color
//! profile. We then prove the compositor (1) stored the surface's negotiated color description and (2)
//! converts a surface pixel into the output profile in linear light (a real BT.2020→sRGB gamut change,
//! not a raw copy). The transfer-function/primaries/HDR math itself is covered by the in-crate fixtures
//! in `handlers::color`. Runs headlessly on Linux (libxkbcommon present) and macOS.

use hl_compositor::handlers::color::{ColorDescription, ColorPrimaries, ColorTransfer};
use hl_compositor::{ClientState, HlState};
use hl_display::present::{PresentError, PresentOutcome, Presenter, SurfaceBuffer};
use hl_display::wire::{Conn, Message};
use smithay::reexports::wayland_server::Display;
use std::collections::HashMap;
use std::os::unix::io::{FromRawFd, RawFd};
use std::sync::Arc;

const WL_DISPLAY: u32 = 1;
const WL_REGISTRY: u32 = 2;

struct CountingPresenter {
    frames: u32,
}
impl Presenter for CountingPresenter {
    fn present(&mut self, _surf: &SurfaceBuffer) -> Result<PresentOutcome, PresentError> {
        self.frames += 1;
        Ok(PresentOutcome::Delivered { serial: self.frames as u64, timing: None })
    }
    fn frame_count(&self) -> u32 {
        self.frames
    }
}

struct Cli {
    conn: Conn,
    next_id: u32,
    globals: HashMap<String, (u32, u32)>,
    events: Vec<(u32, u16)>,
}
impl Cli {
    fn new(fd: RawFd) -> Cli {
        Cli { conn: Conn::new(fd), next_id: 2, globals: HashMap::new(), events: Vec::new() }
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
            if m.opcode == 0 && m.object == WL_REGISTRY {
                let mut r = m.reader();
                let name = r.u32();
                let iface = r.string();
                let ver = r.u32();
                self.globals.insert(iface, (name, ver));
            }
        }
    }
    fn saw(&self, object: u32, opcode: u16) -> bool {
        self.events.contains(&(object, opcode))
    }
    fn had_protocol_error(&self) -> bool {
        self.saw(WL_DISPLAY, 0)
    }
}

fn socketpair_nonblocking() -> (RawFd, RawFd) {
    let mut sv = [0i32; 2];
    assert_eq!(unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, sv.as_mut_ptr()) }, 0);
    for fd in sv {
        unsafe {
            let fl = libc::fcntl(fd, libc::F_GETFL);
            libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
        }
    }
    (sv[0], sv[1])
}

#[test]
fn color_management_surface_description_and_output_profile_conversion() {
    let mut display: Display<HlState> = Display::new().unwrap();
    let mut dh = display.handle();
    let mut state = HlState::new(dh.clone(), Box::new(CountingPresenter { frames: 0 }));

    let (client_fd, server_fd) = socketpair_nonblocking();
    dh.insert_client(
        unsafe { std::os::unix::net::UnixStream::from_raw_fd(server_fd) },
        Arc::new(ClientState::default()),
    )
    .unwrap();
    let mut c = Cli::new(client_fd);

    macro_rules! pump {
        () => {{
            c.flush();
            display.dispatch_clients(&mut state).unwrap();
            display.flush_clients().unwrap();
            c.drain();
        }};
    }

    let reg = c.alloc();
    c.conn.send(&Message::new(WL_DISPLAY, 1).u32(reg));
    pump!();
    for iface in ["wp_color_manager_v1", "wl_compositor", "wl_output"] {
        assert!(c.globals.contains_key(iface), "global {iface} not advertised; got {:?}", c.globals.keys().collect::<Vec<_>>());
    }
    let bind = |c: &mut Cli, iface: &str, ver: u32| -> u32 {
        let id = c.alloc();
        let name = c.globals[iface].0;
        c.conn.send(&Message::new(WL_REGISTRY, 0).u32(name).string(iface).u32(ver).u32(id));
        id
    };

    // Bind the manager → it advertises its supported intents/features/tf/primaries then `done` (opcode 4).
    let mgr = bind(&mut c, "wp_color_manager_v1", 1);
    pump!();
    assert!(c.saw(mgr, 4), "color manager must advertise capabilities then done; saw {:?}", c.events);

    // Build a parametric image description: BT.2020 primaries + sRGB transfer.
    let creator = c.alloc();
    c.conn.send(&Message::new(mgr, 5).u32(creator)); // create_parametric_creator
    c.conn.send(&Message::new(creator, 1).u32(9)); // set_tf_named(srgb = 9)
    c.conn.send(&Message::new(creator, 3).u32(6)); // set_primaries_named(bt2020 = 6)
    let image_desc = c.alloc();
    c.conn.send(&Message::new(creator, 0).u32(image_desc)); // create(image_description) — destructor
    pump!();
    assert!(c.saw(image_desc, 1), "image description must become ready (opcode 1); saw {:?}", c.events);

    // Attach the description to a surface.
    let comp = bind(&mut c, "wl_compositor", 4);
    let surface = c.alloc();
    c.conn.send(&Message::new(comp, 0).u32(surface)); // create_surface (sid 1)
    let surf_color = c.alloc();
    c.conn.send(&Message::new(mgr, 2).u32(surf_color).u32(surface)); // get_surface(id, surface)
    c.conn.send(&Message::new(surf_color, 1).u32(image_desc).u32(0)); // set_image_description(desc, perceptual)
    pump!();
    assert!(!c.had_protocol_error(), "color negotiation must not error; saw {:?}", c.events);

    // (1) The compositor stored the surface's negotiated color description.
    let expected = ColorDescription {
        primaries: ColorPrimaries::Bt2020,
        transfer: ColorTransfer::Srgb,
        peak_luminance: 80.0,
        icc: Vec::new(),
    };
    assert_eq!(state.surface_color(1).as_ref(), Some(&expected), "surface's declared color must be stored");

    // The output advertises its color profile (sRGB), delivered as a ready image description.
    let out = bind(&mut c, "wl_output", 2);
    let out_color = c.alloc();
    c.conn.send(&Message::new(mgr, 1).u32(out_color).u32(out)); // get_output(id, wl_output)
    let out_desc = c.alloc();
    c.conn.send(&Message::new(out_color, 1).u32(out_desc)); // get_image_description
    pump!();
    assert!(c.saw(out_desc, 1), "output color profile must be delivered ready; saw {:?}", c.events);
    assert_eq!(state.output_color().primaries, ColorPrimaries::Srgb, "output profile is sRGB");

    // (2) The compositor converts a surface pixel to the output profile in linear light: the BT.2020
    // surface value is gamut-converted, NOT copied through.
    let input = [0.5_f64, 0.4, 0.3];
    let converted = state.convert_surface_pixel_to_output(1, input);
    assert!(
        (0..3).any(|i| (converted[i] - input[i]).abs() > 1e-2),
        "surface color must be converted to the output profile, got {converted:?} for {input:?}"
    );
    // A surface WITHOUT a declared color is assumed already in the output profile (identity).
    assert_eq!(state.convert_surface_pixel_to_output(999, input), input, "undeclared surface is identity");
}
