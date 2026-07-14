//! CPU-path composition-correctness proof (codex-rendering §11 rows
//! `compositor_child_blend_honors_viewport_scaling_and_premultiplied_argb` and
//! `compositor_applies_all_buffer_transforms_to_geometry_sampling_and_damage`).
//!
//! Drives the real Smithay compositor over a socketpair and captures the composited `SurfaceBuffer` the
//! compositor hands the `Presenter`, then asserts the actual pixels — no Metal needed (the composition
//! math runs on the CPU/software path). Two scenarios, ONE `Display`/client (wayland-server keeps
//! process-global state; see `client_roundtrip.rs`):
//!   1. a semi-transparent PREMULTIPLIED ARGB subsurface over an opaque parent composites with correct
//!      Porter-Duff "over" (`dst = src + dst·(1-a)`), NOT the old double-multiply (`src·a + dst·(1-a)`);
//!   2. a `wl_surface.buffer_transform` of 90° rotates BOTH the presented geometry (w/h swapped) and the
//!      sampled pixels (an asymmetric 2×1 buffer presents as an upright 1×2 with the two texels swapped).

use hl_compositor::{ClientState, HlState};
use hl_display::present::{PresentError, PresentOutcome, Presenter, SurfaceBuffer};
use hl_display::wire::{Conn, Message};
use smithay::reexports::wayland_server::Display;
use std::collections::HashMap;
use std::os::unix::io::{FromRawFd, RawFd};
use std::sync::{Arc, Mutex};

const WL_DISPLAY: u32 = 1;

/// One presented frame's fields the tests assert on (`SurfaceBuffer` is not `Clone`).
#[derive(Clone)]
struct Shot {
    tex_w: i32,
    tex_h: i32,
    bgra: Vec<u8>,
}

struct RecordingPresenter {
    frames: u32,
    shots: Arc<Mutex<HashMap<u32, Shot>>>,
}
impl Presenter for RecordingPresenter {
    fn present(&mut self, surf: &SurfaceBuffer) -> Result<PresentOutcome, PresentError> {
        self.frames += 1;
        self.shots.lock().unwrap().insert(
            surf.sid,
            Shot {
                tex_w: surf.texture_width,
                tex_h: surf.texture_height,
                bgra: surf.bgra.clone(),
            },
        );
        Ok(PresentOutcome::Delivered {
            serial: self.frames as u64,
            timing: None,
        })
    }
    fn frame_count(&self) -> u32 {
        self.frames
    }
}

struct Cli {
    conn: Conn,
    next_id: u32,
    globals: HashMap<String, (u32, u32)>,
    xdg_id: u32,
    last_xdg_serial: Option<u32>,
}
impl Cli {
    fn new(fd: RawFd) -> Cli {
        Cli {
            conn: Conn::new(fd),
            next_id: 2,
            globals: HashMap::new(),
            xdg_id: 0,
            last_xdg_serial: None,
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
            if m.opcode == 0 && m.object == 2 {
                let mut r = m.reader();
                let name = r.u32();
                let iface = r.string();
                let ver = r.u32();
                self.globals.insert(iface, (name, ver));
            } else if self.xdg_id != 0 && m.object == self.xdg_id && m.opcode == 0 {
                self.last_xdg_serial = Some(m.reader().u32());
            }
        }
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
fn cpu_composition_premultiplied_blend_and_buffer_transform_are_correct() {
    let mut display: Display<HlState> = Display::new().unwrap();
    let mut dh = display.handle();
    let shots = Arc::new(Mutex::new(HashMap::new()));
    let mut state = HlState::new(
        dh.clone(),
        Box::new(RecordingPresenter {
            frames: 0,
            shots: shots.clone(),
        }),
    );
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
    let bind = |c: &mut Cli, iface: &str, ver: u32| -> u32 {
        let id = c.alloc();
        let name = c.globals[iface].0;
        c.conn
            .send(&Message::new(2, 0).u32(name).string(iface).u32(ver).u32(id));
        id
    };
    let comp = bind(&mut c, "wl_compositor", 4);
    let subc = bind(&mut c, "wl_subcompositor", 1);
    let shm = bind(&mut c, "wl_shm", 1);
    let wm = bind(&mut c, "xdg_wm_base", 1);

    // Helper: back a buffer with tight BGRA pixels via an anon shm fd, return its wl_buffer id.
    // format: 1 = XRGB8888 (opaque), 0 = ARGB8888 (premultiplied alpha).
    macro_rules! make_buffer {
        ($c:expr, $w:expr, $h:expr, $fmt:expr, $pixels:expr) => {{
            let (w, h): (i32, i32) = ($w, $h);
            let stride = w * 4;
            let size = (stride * h) as usize;
            let fd = hl_display::keymap::anon_fd_with($pixels).expect("anon fd");
            let pool = $c.alloc();
            $c.conn.send(&Message::new(shm, 0).u32(pool).u32(size as u32));
            $c.conn.queue_fd(fd);
            pump!();
            unsafe { libc::close(fd) };
            let buffer = $c.alloc();
            $c.conn.send(
                &Message::new(pool, 0)
                    .u32(buffer)
                    .i32(0)
                    .i32(w)
                    .i32(h)
                    .i32(stride)
                    .u32($fmt),
            );
            buffer
        }};
    }

    // Helper: map a toplevel for `surface`, completing the configure/ack handshake.
    macro_rules! map_toplevel {
        ($c:expr, $surface:expr) => {{
            let xdg = $c.alloc();
            $c.conn.send(&Message::new(wm, 2).u32(xdg).u32($surface));
            let toplevel = $c.alloc();
            $c.conn.send(&Message::new(xdg, 1).u32(toplevel));
            $c.xdg_id = xdg;
            $c.conn.send(&Message::new($surface, 6)); // commit → configure
            pump!();
            let serial = $c.last_xdg_serial.expect("configure serial");
            $c.conn.send(&Message::new(xdg, 4).u32(serial)); // ack_configure
            pump!();
        }};
    }

    // ============================================================================================
    // (1) Premultiplied source-over. Parent: opaque red (XRGB) 4×4. Child subsurface: green at 50%
    // alpha, PREMULTIPLIED (green·0.5 → G=128, A=128) ARGB 2×2 at (0,0). Correct "over" gives, under
    // the child: B=0, G=128 (src premult added directly), R=200·127/255≈99, A=255. The OLD double-
    // multiply bug would give G=(128·128)/255≈64 — so asserting G≈128 distinguishes the fix.
    let parent = c.alloc();
    c.conn.send(&Message::new(comp, 0).u32(parent)); // create_surface
    let (pw, ph) = (4, 4);
    let mut ppix = vec![0u8; (pw * ph * 4) as usize];
    for px in ppix.chunks_exact_mut(4) {
        px[0] = 0; // B
        px[1] = 0; // G
        px[2] = 200; // R (opaque red)
        px[3] = 255; // A (ignored for XRGB)
    }
    let pbuf = make_buffer!(c, pw, ph, 1, &ppix); // XRGB opaque
    map_toplevel!(c, parent);
    c.conn.send(&Message::new(parent, 1).u32(pbuf).i32(0).i32(0)); // attach
    c.conn.send(&Message::new(parent, 2).i32(0).i32(0).i32(pw).i32(ph)); // damage

    // Child subsurface (premultiplied green @ 50% alpha).
    let child = c.alloc();
    c.conn.send(&Message::new(comp, 0).u32(child)); // create_surface
    let sub = c.alloc();
    c.conn
        .send(&Message::new(subc, 1).u32(sub).u32(child).u32(parent)); // get_subsurface(id, surface, parent)
    c.conn.send(&Message::new(sub, 1).i32(0).i32(0)); // set_position(0,0)
    let (cw, ch) = (2, 2);
    let mut cpix = vec![0u8; (cw * ch * 4) as usize];
    for px in cpix.chunks_exact_mut(4) {
        px[0] = 0; // B
        px[1] = 128; // G premultiplied (green 255 · alpha 128/255 = 128)
        px[2] = 0; // R
        px[3] = 128; // A = 50%
    }
    let cbuf = make_buffer!(c, cw, ch, 0, &cpix); // ARGB premultiplied
    c.conn.send(&Message::new(child, 1).u32(cbuf).i32(0).i32(0)); // attach
    c.conn.send(&Message::new(child, 2).i32(0).i32(0).i32(cw).i32(ch)); // damage
    c.conn.send(&Message::new(child, 6)); // commit child (sync subsurface: applied on parent commit)
    c.conn.send(&Message::new(parent, 6)); // commit parent → composite
    pump!();

    // The presenter records each frame under the compositor's HOST surface id (a monotonic,
    // client-independent id — not the client's protocol object id), so locate the parent's composited
    // frame by its unique backing dimensions rather than by the client-local `parent` id.
    let shot = shots
        .lock()
        .unwrap()
        .values()
        .find(|s| (s.tex_w, s.tex_h) == (pw, ph))
        .cloned()
        .expect("parent presented");
    assert_eq!((shot.tex_w, shot.tex_h), (pw, ph), "parent backing size");
    // Pixel (0,0) is under the child: correct premultiplied "over".
    let (b, g, r) = (shot.bgra[0], shot.bgra[1], shot.bgra[2]);
    assert_eq!(b, 0, "blue channel under premultiplied green child");
    assert!(
        (g as i32 - 128).abs() <= 1,
        "green must be the premultiplied source added directly (~128), not the double-multiplied ~64; got {g}"
    );
    assert!(
        (r as i32 - 99).abs() <= 1,
        "red must be the parent attenuated by (1-a): 200·127/255≈99; got {r}"
    );
    // A pixel OUTSIDE the 2×2 child stays opaque parent red.
    let far = (3 * shot.tex_w + 3) as usize * 4;
    assert_eq!(
        (shot.bgra[far], shot.bgra[far + 1], shot.bgra[far + 2]),
        (0, 0, 200),
        "pixel outside the child must remain the parent's opaque red"
    );

    // ============================================================================================
    // (2) buffer_transform = 90°. Asymmetric 2×1 buffer: texel(0,0)=blue, texel(1,0)=red (XRGB). A 90°
    // transform swaps geometry (presented 1×2) and rotates pixels: presented row0 = buffer(1,0)=red,
    // row1 = buffer(0,0)=blue — proving orientation, not just a dimension swap.
    let tsurf = c.alloc();
    c.conn.send(&Message::new(comp, 0).u32(tsurf)); // create_surface
    let mut tpix = vec![0u8; (2 * 1 * 4) as usize];
    tpix[0] = 255; // texel(0,0) BLUE  (B=255,G=0,R=0)
    tpix[4] = 0; // texel(1,0) RED   (B=0,G=0,R=255)
    tpix[6] = 255;
    let tbuf = make_buffer!(c, 2, 1, 1, &tpix); // XRGB
    map_toplevel!(c, tsurf);
    c.conn.send(&Message::new(tsurf, 7).u32(1)); // set_buffer_transform(90 == 1)
    c.conn.send(&Message::new(tsurf, 1).u32(tbuf).i32(0).i32(0)); // attach
    c.conn.send(&Message::new(tsurf, 2).i32(0).i32(0).i32(2).i32(1)); // damage
    c.conn.send(&Message::new(tsurf, 6)); // commit → present
    pump!();

    // Same host-id vs client-id distinction: find the transformed surface's frame by its presented
    // 1×2 geometry (90° swap of the 2×1 buffer).
    let tshot = shots
        .lock()
        .unwrap()
        .values()
        .find(|s| (s.tex_w, s.tex_h) == (1, 2))
        .cloned()
        .expect("transform surface presented");
    assert_eq!(
        (tshot.tex_w, tshot.tex_h),
        (1, 2),
        "90° buffer_transform must swap presented geometry to 1×2"
    );
    // row0 (presented 0,0) = buffer(1,0) = RED (R byte == 255, B byte == 0).
    assert_eq!(tshot.bgra[2], 255, "row0 should be red (R=255) after 90° rotation");
    assert_eq!(tshot.bgra[0], 0, "row0 blue channel should be 0");
    // row1 (presented 0,1) = buffer(0,0) = BLUE (B byte == 255).
    assert_eq!(tshot.bgra[4], 255, "row1 should be blue (B=255) after 90° rotation");
    assert_eq!(tshot.bgra[6], 0, "row1 red channel should be 0");
}
