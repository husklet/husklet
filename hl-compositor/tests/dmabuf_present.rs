//! Integration proof for the `zwp_linux_dmabuf_v1` GPU present path on the Smithay compositor.
//!
//! A minimal in-process Wayland client (built on `hl_display::wire`) connects over a `socketpair`
//! handed to Smithay's `Display`, and drives the accelerated path end to end:
//!   1. `get_registry` → assert `zwp_linux_dmabuf_v1` is advertised;
//!   2. bind it (v3) → assert the compositor sends the dd IOSurface `modifier` for ARGB/XRGB8888;
//!   3. `create_params` + `add(fd, …, modifier=dd-tag|iosurface-id)` + `create_immed` → a dmabuf
//!      `wl_buffer`;
//!   4. attach it to a `wl_surface` and commit.
//! We assert the committed frame reaches the `Presenter` as a zero-copy `SurfaceBuffer` carrying
//! exactly the IOSurface id we encoded in the modifier (and no CPU pixels) — i.e. a GLES client
//! (glmark2/es2tri) can present on the Smithay path. This is the dmabuf analogue of the shm
//! `client_roundtrip` proof; it needs no real IOSurface/Metal because the mock Presenter records the
//! resolved `SurfaceBuffer` rather than compositing it.

use std::sync::{Arc, Mutex};

use hl_compositor::{ClientState, DdState};
use hl_display::present::{
    IOSurfaceMetadata, PresentError, PresentOutcome, Presenter, SurfaceBuffer,
};
use hl_display::wire::{Conn, Message};
use smithay::reexports::wayland_server::Display;

const WL_DISPLAY: u32 = 1;

// DRM fourccs (little-endian 'AR24' / 'XR24').
const DRM_FMT_ARGB8888: u32 = 0x3432_5241;
const DRM_FMT_XRGB8888: u32 = 0x3432_5258;
// dd-private modifier tag (must match handlers/dmabuf.rs + legacy server.rs).
const DD_DMABUF_MOD_MAGIC: u32 = 0x6464;

/// The fields of the last presented `SurfaceBuffer` the test asserts on (`SurfaceBuffer` is not
/// `Clone`, so we snapshot what we need): iosurface id, whether it carried CPU pixels, texture size,
/// format, and the gpu_render flag.
#[derive(Clone)]
struct Presented {
    iosurface_id: Option<u32>,
    bgra_empty: bool,
    texture_width: i32,
    texture_height: i32,
    format: u32,
    gpu_render: bool,
}

/// Records the most recent frame the compositor presented, so the test can assert the dmabuf commit
/// arrived as an IOSurface-backed (zero-copy) buffer.
struct RecordingPresenter {
    frames: u32,
    last: Arc<Mutex<Option<Presented>>>,
}
impl Presenter for RecordingPresenter {
    fn present(&mut self, surf: &SurfaceBuffer) -> Result<PresentOutcome, PresentError> {
        self.frames += 1;
        *self.last.lock().unwrap() = Some(Presented {
            iosurface_id: surf.iosurface_id,
            bgra_empty: surf.bgra.is_empty(),
            texture_width: surf.texture_width,
            texture_height: surf.texture_height,
            format: surf.format,
            gpu_render: surf.gpu_render,
        });
        Ok(PresentOutcome::Delivered {
            serial: self.frames as u64,
            timing: None,
        })
    }
    fn frame_count(&self) -> u32 {
        self.frames
    }
    /// Authenticate the id this test imports as a live 64x64 BGRA allocation so it passes the import
    /// metadata check. Generation 0 (unversioned) matches the test's unversioned modifier.
    fn iosurface_metadata(&self, id: u32) -> Option<IOSurfaceMetadata> {
        (id == 0x00AB_CDEF).then_some(IOSurfaceMetadata {
            width: 64,
            height: 64,
            bytes_per_row: 64 * 4,
            pixel_format: 0x4247_5241, // 'BGRA'
            generation: 0,
        })
    }
}

fn socketpair_nonblocking() -> (i32, i32) {
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

struct Client {
    conn: Conn,
    next_id: u32,
    globals: std::collections::HashMap<String, (u32, u32)>,
    /// `(fourcc, modifier_hi, modifier_lo)` from every `zwp_linux_dmabuf_v1.modifier` (opcode 1).
    dmabuf_modifiers: Vec<(u32, u32, u32)>,
    /// The bound `zwp_linux_dmabuf_v1` id, so `drain` decodes its modifier events.
    dmabuf_id: u32,
    events: Vec<(u32, u16)>,
}
impl Client {
    fn new(fd: i32) -> Client {
        Client {
            conn: Conn::new(fd),
            next_id: 2,
            globals: Default::default(),
            dmabuf_modifiers: Vec::new(),
            dmabuf_id: 0,
            events: Vec::new(),
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
                self.globals.insert(iface, (name, ver));
            } else if self.dmabuf_id != 0 && m.object == self.dmabuf_id && m.opcode == 1 {
                // zwp_linux_dmabuf_v1.modifier(format, modifier_hi, modifier_lo)
                let mut r = m.reader();
                let fourcc = r.u32();
                let hi = r.u32();
                let lo = r.u32();
                self.dmabuf_modifiers.push((fourcc, hi, lo));
            }
        }
    }
}

// One test, one Display/client (wayland-server keeps process-global state; see client_roundtrip.rs).
#[test]
fn dmabuf_global_and_iosurface_commit_presents() {
    // The dmabuf global is advertised only under `DD_DISPLAY_DMABUF` (parity with legacy `server.rs`;
    // the default software path must not advertise it — see handlers/dmabuf.rs::new_dmabuf_state).
    std::env::set_var("DD_DISPLAY_DMABUF", "1");
    // Accelerated import now REQUIRES a healthy host executor (dmabuf.rs's readiness gate rejects a
    // dd-tagged dmabuf when none is running, so the client can't render into a surface nothing presents).
    // This test simulates that healthy environment; without it the import would be correctly rejected.
    hl_compositor::gpu::set_executor_health(true);
    let mut display: Display<DdState> = Display::new().unwrap();
    let mut dh = display.handle();
    let last = Arc::new(Mutex::new(None));
    let mut state = DdState::new(
        dh.clone(),
        Box::new(RecordingPresenter {
            frames: 0,
            last: last.clone(),
        }),
    );

    use std::os::unix::io::FromRawFd;
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

    // (1) Registry advertises zwp_linux_dmabuf_v1.
    let reg = c.alloc();
    c.conn.send(&Message::new(WL_DISPLAY, 1).u32(reg));
    pump!();
    assert!(
        c.globals.contains_key("zwp_linux_dmabuf_v1"),
        "zwp_linux_dmabuf_v1 not advertised; got {:?}",
        c.globals.keys().collect::<Vec<_>>()
    );

    let bind = |c: &mut Client, iface: &str, ver: u32| -> u32 {
        let id = c.alloc();
        let name = c.globals[iface].0;
        c.conn
            .send(&Message::new(2, 0).u32(name).string(iface).u32(ver).u32(id));
        id
    };
    let comp = bind(&mut c, "wl_compositor", 4);
    // Bind dmabuf at v3 so the compositor sends deprecated `format`/`modifier` events (v4 uses feedback).
    let dmabuf = bind(&mut c, "zwp_linux_dmabuf_v1", 3);
    c.dmabuf_id = dmabuf;
    pump!();

    // (2) The compositor advertised the dd IOSurface modifier (modifier_hi low-16 == magic) for both
    // ARGB8888 and XRGB8888 — the tag a GLES client's buffer must carry to be resolved to an IOSurface.
    let has_dd_mod = |fourcc: u32| {
        c.dmabuf_modifiers
            .iter()
            .any(|&(f, hi, _)| f == fourcc && (hi & 0xffff) == DD_DMABUF_MOD_MAGIC)
    };
    assert!(
        has_dd_mod(DRM_FMT_ARGB8888) && has_dd_mod(DRM_FMT_XRGB8888),
        "expected dd IOSurface modifier for AR24 + XR24; got {:?}",
        c.dmabuf_modifiers
    );

    // (3) Create an IOSurface-backed dmabuf buffer. The plane fd is a placeholder (dd resolves the
    // IOSurface by id, not by the fd), sized to pass Smithay's stride*height <= size validation.
    const IOSURFACE_ID: u32 = 0x00AB_CDEF;
    let (w, h): (i32, i32) = (64, 64);
    let stride = (w * 4) as u32;
    let plane = hl_display::keymap::anon_fd_with(&vec![0u8; (stride as i32 * h) as usize])
        .expect("anon plane fd");

    let params = c.alloc();
    c.conn.send(&Message::new(dmabuf, 1).u32(params)); // zwp_linux_dmabuf_v1.create_params(params)
                                                       // zwp_linux_buffer_params_v1.add(fd, plane_idx, offset, stride, modifier_hi, modifier_lo)
    c.conn.queue_fd(plane);
    c.conn.send(
        &Message::new(params, 1)
            .u32(0) // plane_idx
            .u32(0) // offset
            .u32(stride)
            .u32(DD_DMABUF_MOD_MAGIC) // modifier_hi: dd tag
            .u32(IOSURFACE_ID), // modifier_lo: IOSurface id
    );
    let buffer = c.alloc();
    // create_immed(buffer_id, width, height, format, flags)
    c.conn.send(
        &Message::new(params, 3)
            .u32(buffer)
            .i32(w)
            .i32(h)
            .u32(DRM_FMT_XRGB8888)
            .u32(0),
    );
    pump!();
    unsafe { libc::close(plane) };

    // (4) Attach the dmabuf buffer to a plain wl_surface and commit → present.
    let surface = c.alloc();
    c.conn.send(&Message::new(comp, 0).u32(surface)); // create_surface
    c.conn.send(&Message::new(surface, 1).u32(buffer).i32(0).i32(0)); // attach
    c.conn.send(&Message::new(surface, 2).i32(0).i32(0).i32(w).i32(h)); // damage
    c.conn.send(&Message::new(surface, 6)); // commit
    pump!();

    // The commit reached the Presenter as a zero-copy IOSurface SurfaceBuffer.
    assert!(
        state.presenter.frame_count() >= 1,
        "dmabuf commit did not reach the presenter"
    );
    let sb = last.lock().unwrap().clone().expect("no SurfaceBuffer presented");
    assert_eq!(
        sb.iosurface_id,
        Some(IOSURFACE_ID),
        "presented buffer should carry the IOSurface id decoded from the dmabuf modifier"
    );
    assert!(
        sb.bgra_empty,
        "an IOSurface-backed buffer is zero-copy — it must carry no CPU pixels"
    );
    assert_eq!(sb.texture_width, w, "IOSurface texture width");
    assert_eq!(sb.texture_height, h, "IOSurface texture height");
    assert_eq!(sb.format, 1, "XR24 dmabuf should present as opaque (format == 1)");
    assert!(!sb.gpu_render, "no render bit was set in the modifier");
}
